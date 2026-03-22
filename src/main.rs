#![windows_subsystem = "windows"]

use eframe::egui;
use egui::{Color32, RichText, ScrollArea};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

// ─── file tree ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct FileNode {
    name: String,
    path: PathBuf,
    kind: NodeKind,
}

#[derive(Debug, Clone)]
enum NodeKind {
    File,
    Dir(Vec<FileNode>),
}

impl FileNode {
    fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_string();

        if path.is_dir() {
            let mut children: Vec<FileNode> = fs::read_dir(path)
                .ok()?
                .filter_map(|e| e.ok())
                .filter_map(|e| FileNode::from_path(&e.path()))
                .collect();

            if children.is_empty() {
                return None;
            }

            children.sort_by(|a, b| match (a.is_dir(), b.is_dir()) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });

            Some(FileNode { name, path: path.to_path_buf(), kind: NodeKind::Dir(children) })
        } else if name.to_lowercase().ends_with(".md") {
            Some(FileNode { name, path: path.to_path_buf(), kind: NodeKind::File })
        } else {
            None
        }
    }

    fn is_dir(&self) -> bool {
        matches!(self.kind, NodeKind::Dir(_))
    }
}

fn build_tree(root: &str) -> Vec<FileNode> {
    let path = Path::new(root);
    if !path.is_dir() {
        return vec![];
    }

    let mut nodes: Vec<FileNode> = fs::read_dir(path)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| FileNode::from_path(&e.path()))
                .collect()
        })
        .unwrap_or_default();

    nodes.sort_by(|a, b| match (a.is_dir(), b.is_dir()) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    nodes
}

/// Returns the path of any file clicked in the tree.
fn render_node(
    ui: &mut egui::Ui,
    node: &mut FileNode,
    selected: &Option<PathBuf>,
) -> Option<PathBuf> {
    match &mut node.kind {
        NodeKind::Dir(children) => {
            let resp = egui::CollapsingHeader::new(
                RichText::new(format!("📁 {}", node.name)).strong(),
            )
            .show(ui, |ui| {
                let mut clicked = None;
                for child in children.iter_mut() {
                    if let Some(p) = render_node(ui, child, selected) {
                        clicked = Some(p);
                    }
                }
                clicked
            });
            resp.body_returned.flatten()
        }

        NodeKind::File => {
            let is_selected = selected.as_ref() == Some(&node.path);
            // Don't override color for unselected — let the theme decide.
            let label = if is_selected {
                RichText::new(format!("  📄 {}", node.name))
                    .color(Color32::from_rgb(80, 170, 255))
            } else {
                RichText::new(format!("  📄 {}", node.name))
            };
            if ui.selectable_label(is_selected, label).clicked() {
                Some(node.path.clone())
            } else {
                None
            }
        }
    }
}

// ─── config ───────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    root_path: String,
    dark_mode: bool,
    zoom: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root_path: dirs::document_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .to_string_lossy()
                .to_string(),
            dark_mode: true,
            zoom: 1.0,
        }
    }
}

impl Config {
    fn load() -> Self {
        if let Ok(s) = fs::read_to_string(Self::path()) {
            serde_json::from_str(&s).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = fs::write(p, s);
        }
    }

    fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("MDReader")
            .join("config.json")
    }
}

// ─── theme ────────────────────────────────────────────────────────────────────

fn apply_theme(ctx: &egui::Context, dark: bool) {
    if dark {
        let mut v = egui::Visuals::dark();
        v.panel_fill = Color32::from_rgb(22, 22, 26);
        v.window_fill = Color32::from_rgb(30, 30, 36);
        v.window_rounding = egui::Rounding::same(6.0);
        ctx.set_visuals(v);
    } else {
        let mut v = egui::Visuals::light();
        v.window_rounding = egui::Rounding::same(6.0);
        ctx.set_visuals(v);
    }
}

// ─── app state ────────────────────────────────────────────────────────────────

struct MDReaderApp {
    config: Config,
    tree: Vec<FileNode>,
    selected_file: Option<PathBuf>,
    file_content: String,
    md_cache: CommonMarkCache,
    show_settings: bool,
    root_input: String,
    /// System-native pixels-per-point captured at startup; zoom is applied on top.
    native_ppp: f32,
    /// Zoom value being previewed while the user is actively dragging.
    /// `None` when not dragging. Committed to `config.zoom` on release.
    drag_zoom: Option<f32>,
}

impl MDReaderApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config = Config::load();
        let native_ppp = cc.egui_ctx.pixels_per_point();

        apply_theme(&cc.egui_ctx, config.dark_mode);
        cc.egui_ctx.set_pixels_per_point(native_ppp * config.zoom);

        let tree = build_tree(&config.root_path);
        let root_input = config.root_path.clone();

        Self {
            config,
            tree,
            selected_file: None,
            file_content: String::new(),
            md_cache: CommonMarkCache::default(),
            show_settings: false,
            root_input,
            native_ppp,
            drag_zoom: None,
        }
    }

    fn load_file(&mut self, path: PathBuf) {
        match fs::read_to_string(&path) {
            Ok(c) => {
                self.file_content = c;
                self.selected_file = Some(path);
            }
            Err(e) => {
                self.file_content = format!("> **Error reading file:** {e}");
                self.selected_file = Some(path);
            }
        }
    }

    fn refresh_tree(&mut self) {
        self.tree = build_tree(&self.config.root_path);
    }
}

// ─── ui ───────────────────────────────────────────────────────────────────────

impl eframe::App for MDReaderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── settings modal ──────────────────────────────────────────────────
        if self.show_settings {
            egui::Window::new("⚙  Settings")
                .resizable(false)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .min_width(500.0)
                .show(ctx, |ui| {
                    ui.add_space(6.0);
                    ui.label("Root directory for markdown files:");
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.root_input)
                                .desired_width(360.0),
                        );
                        if ui.button("Browse…").clicked() {
                            if let Some(f) = rfd::FileDialog::new().pick_folder() {
                                self.root_input = f.to_string_lossy().to_string();
                            }
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);

                    ui.horizontal(|ui| {
                        if ui.button("  Apply  ").clicked() {
                            self.config.root_path = self.root_input.clone();
                            self.config.save();
                            self.refresh_tree();
                            self.selected_file = None;
                            self.file_content.clear();
                            self.show_settings = false;
                        }
                        if ui.button("  Cancel  ").clicked() {
                            self.root_input = self.config.root_path.clone();
                            self.show_settings = false;
                        }
                    });
                });
        }

        // ── menu bar ────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("topbar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.label(RichText::new("📖").size(15.0));

                ui.menu_button("File", |ui| {
                    if ui.button("⚙  Settings…").clicked() {
                        self.show_settings = !self.show_settings;
                        self.root_input = self.config.root_path.clone();
                        ui.close_menu();
                    }
                    if ui.button("↺  Refresh tree").clicked() {
                        self.refresh_tree();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                ui.menu_button("View", |ui| {
                    // ── light / dark ──
                    let theme_label = if self.config.dark_mode {
                        "☀  Light mode"
                    } else {
                        "🌙  Dark mode"
                    };
                    if ui.button(theme_label).clicked() {
                        self.config.dark_mode = !self.config.dark_mode;
                        apply_theme(ctx, self.config.dark_mode);
                        self.config.save();
                        ui.close_menu();
                    }

                    ui.separator();

                    // ── zoom presets ──
                    for &(label, factor) in &[
                        ("75%",  0.75_f32),
                        ("100%", 1.00_f32),
                        ("125%", 1.25_f32),
                        ("150%", 1.50_f32),
                        ("200%", 2.00_f32),
                    ] {
                        if ui.button(label).clicked() {
                            self.config.zoom = factor;
                            ctx.set_pixels_per_point(self.native_ppp * factor);
                            self.config.save();
                            ui.close_menu();
                        }
                    }

                    ui.separator();
                    if ui.button("Reset zoom").clicked() {
                        self.config.zoom = 1.0;
                        ctx.set_pixels_per_point(self.native_ppp);
                        self.config.save();
                        ui.close_menu();
                    }
                });

                // current filename right-aligned
                if let Some(ref p) = self.selected_file {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(
                                p.file_name().unwrap_or_default().to_string_lossy().as_ref(),
                            )
                            .weak(),
                        );
                    });
                }
            });
        });

        // ── status bar — zoom drag ───────────────────────────────────────────
        //
        // Drag the percentage label left/right to shrink/expand the UI scale.
        // Markdown code fences support language tags (```rust, ```python, etc.)
        // and egui_commonmark's syntect backend renders them with full syntax
        // colouring automatically.
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // full path on the left
                if let Some(ref p) = self.selected_file {
                    ui.label(
                        RichText::new(p.to_string_lossy().as_ref())
                            .weak()
                            .size(11.0),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Show the live preview during drag, otherwise the committed value.
                    let display_zoom = self.drag_zoom.unwrap_or(self.config.zoom);
                    let pct = (display_zoom * 100.0).round() as i32;

                    // Draggable zoom label — horizontal drag adjusts scale.
                    // IMPORTANT: we never call set_pixels_per_point while dragging.
                    // Doing so shifts the logical coordinate system mid-drag, which
                    // makes drag_delta() diverge wildly on subsequent frames.
                    let response = ui
                        .add(
                            egui::Label::new(
                                RichText::new(format!(" {pct}% "))
                                    .monospace()
                                    .size(11.0),
                            )
                            .sense(egui::Sense::drag()),
                        )
                        .on_hover_text("Drag ← → to zoom");

                    if response.hovered() || response.dragged() {
                        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }

                    if response.dragged() {
                        // Accumulate into the preview; leave ppp (and the coordinate
                        // system) completely untouched until the drag finishes.
                        let z = self.drag_zoom.get_or_insert(self.config.zoom);
                        *z = (*z + response.drag_delta().x * 0.003).clamp(0.25, 4.0);
                    }

                    if response.drag_stopped() {
                        // Commit: apply scale and persist only once, at release.
                        if let Some(z) = self.drag_zoom.take() {
                            self.config.zoom = z;
                            ctx.set_pixels_per_point(self.native_ppp * z);
                            self.config.save();
                        }
                    }

                    ui.separator();
                    ui.label(RichText::new("zoom").weak().size(11.0));
                });
            });
        });

        // ── left sidebar ─────────────────────────────────────────────────────
        let mut file_to_load: Option<PathBuf> = None;
        let sel = self.selected_file.clone();

        egui::SidePanel::left("sidebar")
            .min_width(180.0)
            .default_width(260.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.label(RichText::new("  FILES").size(11.0).weak());
                ui.separator();

                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.tree.is_empty() {
                            ui.add_space(20.0);
                            ui.label(
                                RichText::new(
                                    "No markdown files found.\n\nFile › Settings to set root.",
                                )
                                .weak()
                                .size(12.0),
                            );
                        } else {
                            for node in &mut self.tree {
                                if let Some(p) = render_node(ui, node, &sel) {
                                    file_to_load = Some(p);
                                }
                            }
                        }
                    });
            });

        if let Some(p) = file_to_load {
            self.load_file(p);
        }

        // ── main content ─────────────────────────────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.file_content.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        RichText::new("Select a file from the sidebar")
                            .size(18.0)
                            .weak(),
                    );
                });
            } else {
                ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        CommonMarkViewer::new("mdviewer")
                            .show(ui, &mut self.md_cache, &self.file_content);
                    });
            }
        });
    }
}

// ─── entry point ──────────────────────────────────────────────────────────────

fn main() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MD Reader")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "MD Reader",
        options,
        Box::new(|cc| Ok(Box::new(MDReaderApp::new(cc)))),
    )
    .expect("Failed to launch MD Reader");
}
