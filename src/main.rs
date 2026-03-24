#![windows_subsystem = "windows"]

use eframe::egui;
use egui::{Color32, RichText, ScrollArea};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
};

// ─── update checker ───────────────────────────────────────────────────────────

struct UpdateAvailable {
    version: String,
    url: String,
}

enum UpdateState {
    Checking,
    Idle,
    Available(UpdateAvailable),
    Downloading,
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32) {
        let mut p = s.splitn(3, '.');
        let a = p.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let b = p.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        let c = p.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        (a, b, c)
    };
    parse(latest) > parse(current)
}

fn check_latest_release() -> Option<UpdateAvailable> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("MDReader/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let resp: serde_json::Value = client
        .get("https://api.github.com/repos/ophiocus/MDReader/releases/latest")
        .send().ok()?.json().ok()?;
    let tag = resp["tag_name"].as_str()?.trim_start_matches('v').to_string();
    if !is_newer(&tag, env!("CARGO_PKG_VERSION")) { return None; }
    let url = resp["assets"].as_array()?.iter()
        .find(|a| a["name"].as_str().unwrap_or("").ends_with(".msi"))?
        ["browser_download_url"].as_str()?.to_string();
    Some(UpdateAvailable { version: tag, url })
}

fn download_and_install(url: &str, version: &str) {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("MDReader/", env!("CARGO_PKG_VERSION")))
        .build();
    if let Ok(client) = client {
        if let Ok(bytes) = client.get(url).send().and_then(|r| r.bytes()) {
            let path = std::env::temp_dir().join(format!("MDReader-{version}.msi"));
            if std::fs::write(&path, &bytes).is_ok() {
                let _ = std::process::Command::new("msiexec")
                    .args(["/i", path.to_str().unwrap_or(""), "/passive", "/norestart"])
                    .spawn();
                std::process::exit(0);
            }
        }
    }
}

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

/// Renders one tree node; returns the path of any file clicked.
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
        v.widgets.noninteractive.bg_stroke =
            egui::Stroke::new(1.0, Color32::from_rgb(55, 55, 65));
        ctx.set_visuals(v);
    } else {
        let mut v = egui::Visuals::light();
        v.window_rounding = egui::Rounding::same(6.0);
        ctx.set_visuals(v);
    }
}

// ─── pdf export ───────────────────────────────────────────────────────────────

/// Wraps rendered HTML in a styled page template suitable for printing.
fn html_page(body: &str, title: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>{title}</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
         max-width: 860px; margin: 40px auto; padding: 0 20px;
         line-height: 1.6; color: #1a1a1a; }}
  h1,h2,h3,h4 {{ margin-top: 1.4em; }}
  h1 {{ border-bottom: 2px solid #e0e0e0; padding-bottom: .3em; }}
  h2 {{ border-bottom: 1px solid #eee; padding-bottom: .2em; }}
  code {{ background: #f4f4f4; border-radius: 3px;
          padding: 0.1em 0.35em; font-size: 0.9em; }}
  pre  {{ background: #f4f4f4; border-radius: 5px;
          padding: 1em; overflow-x: auto; }}
  pre code {{ background: none; padding: 0; }}
  table {{ border-collapse: collapse; width: 100%; margin: 1em 0; }}
  th, td {{ border: 1px solid #ddd; padding: 6px 12px; text-align: left; }}
  th {{ background: #f0f0f0; font-weight: 600; }}
  tr:nth-child(even) {{ background: #fafafa; }}
  blockquote {{ border-left: 4px solid #ccc; margin: 0;
                padding-left: 1em; color: #555; }}
  hr {{ border: none; border-top: 1px solid #ddd; margin: 2em 0; }}
  img {{ max-width: 100%; }}
</style>
</head>
<body>
{body}
</body>
</html>"#
    )
}

/// Locate Chrome or Edge for headless PDF rendering.
fn find_chrome() -> Option<PathBuf> {
    let candidates = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

/// Export the current markdown file to PDF.
/// Returns `Ok(path)` on success or `Err(message)` on failure.
fn export_pdf(content: &str, source_path: &Path) -> Result<PathBuf, String> {
    // ── 1. pick save location ──────────────────────────────────────────────
    let stem = source_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let default_name = format!("{stem}.pdf");

    let pdf_path = rfd::FileDialog::new()
        .add_filter("PDF", &["pdf"])
        .set_file_name(&default_name)
        .save_file()
        .ok_or_else(|| "Export cancelled.".to_string())?;

    // ── 2. markdown → HTML ─────────────────────────────────────────────────
    let opts = comrak::Options::default();
    let body = comrak::markdown_to_html(content, &opts);
    let title = stem.as_ref();
    let html = html_page(&body, title);

    // ── 3. write temp HTML ─────────────────────────────────────────────────
    let tmp_html = std::env::temp_dir().join("mdreader_export.html");
    fs::write(&tmp_html, &html)
        .map_err(|e| format!("Failed to write temp HTML: {e}"))?;

    // ── 4. Chrome / Edge headless → PDF ───────────────────────────────────
    let chrome = find_chrome()
        .ok_or_else(|| "Chrome/Edge not found — saved as HTML instead.".to_string())?;

    let out = Command::new(&chrome)
        .args([
            "--headless=new",
            "--disable-gpu",
            "--no-sandbox",
            "--disable-software-rasterizer",
            &format!("--print-to-pdf={}", pdf_path.display()),
            &format!("file:///{}", tmp_html.display().to_string().replace('\\', "/")),
        ])
        .output()
        .map_err(|e| format!("Failed to launch browser: {e}"))?;

    let _ = fs::remove_file(&tmp_html);

    if out.status.success() || pdf_path.exists() {
        Ok(pdf_path)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("Browser error: {stderr}"))
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
    /// Zoom preview while dragging — committed to `config.zoom` on release.
    drag_zoom: Option<f32>,
    /// Transient status message shown in the status bar (e.g. after PDF export).
    status_msg: Option<String>,
    /// Receives the result of the background update check.
    update_rx: mpsc::Receiver<Option<UpdateAvailable>>,
    /// Current state of the update workflow.
    update_state: UpdateState,
}

impl MDReaderApp {
    fn new(cc: &eframe::CreationContext<'_>, cli_root: Option<String>) -> Self {
        let mut config = Config::load();
        let native_ppp = cc.egui_ctx.pixels_per_point();

        if let Some(root) = cli_root {
            config.root_path = root;
            config.save();
        }

        apply_theme(&cc.egui_ctx, config.dark_mode);
        cc.egui_ctx.set_pixels_per_point(native_ppp * config.zoom);

        let tree = build_tree(&config.root_path);
        let root_input = config.root_path.clone();

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || { let _ = tx.send(check_latest_release()); });

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
            status_msg: None,
            update_rx: rx,
            update_state: UpdateState::Checking,
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

    /// Root folder display name for the sidebar title.
    fn root_display_name(&self) -> String {
        Path::new(&self.config.root_path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    }
}

// ─── ui ───────────────────────────────────────────────────────────────────────

impl eframe::App for MDReaderApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ── poll update channel ──────────────────────────────────────────────
        if let Ok(result) = self.update_rx.try_recv() {
            self.update_state = match result {
                Some(avail) => UpdateState::Available(avail),
                None => UpdateState::Idle,
            };
        }

        // ── window title ─────────────────────────────────────────────────────
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "MD Reader — {}",
            self.root_display_name()
        )));

        // ── settings modal ───────────────────────────────────────────────────
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

        // ── menu bar ─────────────────────────────────────────────────────────
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

                    // Export to PDF — only enabled when a file is open.
                    let can_export = self.selected_file.is_some() && !self.file_content.is_empty();
                    if ui
                        .add_enabled(can_export, egui::Button::new("⬇  Export as PDF…"))
                        .clicked()
                    {
                        if let Some(ref path) = self.selected_file.clone() {
                            match export_pdf(&self.file_content, path) {
                                Ok(out) => {
                                    self.status_msg =
                                        Some(format!("✔ PDF saved → {}", out.display()));
                                }
                                Err(e) => {
                                    self.status_msg = Some(format!("✘ {e}"));
                                }
                            }
                        }
                        ui.close_menu();
                    }

                    ui.separator();

                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                ui.menu_button("View", |ui| {
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

                // Current filename — right-aligned.
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

        // ── status bar ───────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Update state UI — shown before status_msg / file path.
                match &self.update_state {
                    UpdateState::Checking => {
                        ui.label(RichText::new("⟳ checking for updates…").weak().size(11.0));
                        ui.separator();
                    }
                    UpdateState::Available(avail) => {
                        let label = RichText::new(format!("↑ v{} available — click to install", avail.version))
                            .size(11.0)
                            .color(Color32::from_rgb(80, 210, 110));
                        if ui.add(egui::Label::new(label).sense(egui::Sense::click())).clicked() {
                            let url = avail.url.clone();
                            let version = avail.version.clone();
                            std::thread::spawn(move || download_and_install(&url, &version));
                            self.update_state = UpdateState::Downloading;
                        }
                        ui.separator();
                    }
                    UpdateState::Downloading => {
                        ui.label(RichText::new("⬇ downloading update…").weak().size(11.0));
                        ui.separator();
                    }
                    UpdateState::Idle => {}
                }

                // Status message (export result) or file path.
                if let Some(ref msg) = self.status_msg {
                    if ui
                        .label(RichText::new(msg).size(11.0).weak())
                        .clicked()
                    {
                        self.status_msg = None;
                    }
                } else if let Some(ref p) = self.selected_file {
                    ui.label(
                        RichText::new(p.to_string_lossy().as_ref())
                            .weak()
                            .size(11.0),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Draggable zoom label.
                    let display_zoom = self.drag_zoom.unwrap_or(self.config.zoom);
                    let pct = (display_zoom * 100.0).round() as i32;

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
                        // Accumulate into preview only — never touch ppp mid-drag.
                        let z = self.drag_zoom.get_or_insert(self.config.zoom);
                        *z = (*z + response.drag_delta().x * 0.003).clamp(0.25, 4.0);
                    }

                    if response.drag_stopped() {
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

        // ── left sidebar — TOC ───────────────────────────────────────────────
        let mut file_to_load: Option<PathBuf> = None;
        let sel = self.selected_file.clone();

        egui::SidePanel::left("sidebar")
            .min_width(200.0)
            .default_width(300.0)
            .show(ctx, |ui| {
                // Root folder as TOC title.
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(self.root_display_name())
                            .size(14.0)
                            .strong(),
                    );
                });
                ui.add_space(2.0);
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
    // Check if first argument is a valid directory (used by context menu).
    let cli_root: Option<String> = std::env::args().nth(1).and_then(|arg| {
        if std::path::Path::new(&arg).is_dir() {
            Some(arg)
        } else {
            None
        }
    });

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
        Box::new(|cc| Ok(Box::new(MDReaderApp::new(cc, cli_root)))),
    )
    .expect("Failed to launch MD Reader");
}
