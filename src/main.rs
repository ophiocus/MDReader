#![windows_subsystem = "windows"]

mod git_update;

use eframe::egui;
use egui::{Color32, RichText, ScrollArea};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use git_update::{check_latest_release, UpdateAvailable, UpdateState};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
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
    fn from_path(path: &Path, order: &OrderConfig) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_string();

        if path.is_dir() {
            let mut children: Vec<FileNode> = fs::read_dir(path)
                .ok()?
                .filter_map(|e| e.ok())
                .filter_map(|e| FileNode::from_path(&e.path(), order))
                .collect();

            if children.is_empty() {
                return None;
            }

            // Default: dirs first, then alphabetical
            children.sort_by(|a, b| match (a.is_dir(), b.is_dir()) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });

            // Apply custom ordering (overrides alphabetical if present)
            order.apply(path, &mut children);

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

/// An entry in the document collection — either a directory group heading
/// or an actual markdown file.
#[derive(Debug, Clone)]
enum DocEntry {
    /// Directory group heading (no content to render, just TOC structure).
    Section { name: String, #[allow(dead_code)] dir_path: PathBuf, depth: usize },
    /// A markdown document to render.
    Document { path: PathBuf, depth: usize },
}

/// Collect all entries from the tree: directories as Section headings,
/// files as Documents.  Depth tracks nesting level.
fn collect_doc_entries(nodes: &[FileNode], depth: usize) -> Vec<DocEntry> {
    let mut out = Vec::new();
    for node in nodes {
        match &node.kind {
            NodeKind::File => {
                out.push(DocEntry::Document { path: node.path.clone(), depth });
            }
            NodeKind::Dir(children) => {
                out.push(DocEntry::Section {
                    name: node.name.clone(),
                    dir_path: node.path.clone(),
                    depth,
                });
                out.extend(collect_doc_entries(children, depth + 1));
            }
        }
    }
    out
}

fn build_tree(root: &str, order: &OrderConfig) -> Vec<FileNode> {
    let path = Path::new(root);
    if !path.is_dir() {
        return vec![];
    }

    let mut nodes: Vec<FileNode> = fs::read_dir(path)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| FileNode::from_path(&e.path(), order))
                .collect()
        })
        .unwrap_or_default();

    // Default alphabetical
    nodes.sort_by(|a, b| match (a.is_dir(), b.is_dir()) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    // Apply custom ordering
    order.apply(path, &mut nodes);

    nodes
}

/// Sidebar action returned from rendering.
enum SidebarAction {
    LoadFile(PathBuf),
    Reorder { parent: PathBuf, from: usize, to: usize },
}

/// Renders a list of sibling nodes with drag-reorder support.
/// Returns any action triggered (file click or reorder).
fn render_siblings(
    ui: &mut egui::Ui,
    nodes: &mut [FileNode],
    selected: &Option<PathBuf>,
    parent_path: &Path,
    drag_source: &mut Option<(PathBuf, usize)>, // (parent, index)
) -> Option<SidebarAction> {
    let mut action: Option<SidebarAction> = None;
    let node_count = nodes.len();

    for idx in 0..node_count {
        let node = &mut nodes[idx];
        let _is_being_dragged = drag_source
            .as_ref()
            .map(|(p, i)| p == parent_path && *i == idx)
            .unwrap_or(false);

        // ── drag handle + node label ──
        let id = ui.make_persistent_id((&node.path, "drag"));

        match &mut node.kind {
            NodeKind::Dir(children) => {
                let dir_path = node.path.clone();
                let header_text = dir_display_name(&node.name);

                // Folder as collapsing header — prominent style, with move arrows
                let child_resp = egui::CollapsingHeader::new(
                    RichText::new(format!("{header_text}"))
                        .size(13.0)
                        .strong()
                        .color(Color32::from_rgb(200, 180, 120)),
                )
                .id_source(id)
                .default_open(true)
                .show(ui, |ui| {
                    // Move arrows at the top of the folder's children
                    ui.horizontal(|ui| {
                        if idx > 0 {
                            if ui.small_button("^").on_hover_text("Move up").clicked() {
                                action = Some(SidebarAction::Reorder {
                                    parent: parent_path.to_path_buf(),
                                    from: idx,
                                    to: idx - 1,
                                });
                            }
                        }
                        if idx + 1 < node_count {
                            if ui.small_button("v").on_hover_text("Move down").clicked() {
                                action = Some(SidebarAction::Reorder {
                                    parent: parent_path.to_path_buf(),
                                    from: idx,
                                    to: idx + 1,
                                });
                            }
                        }
                    });
                    render_siblings(ui, children, selected, &dir_path, drag_source)
                });

                if let Some(Some(child_action)) = child_resp.body_returned {
                    if action.is_none() {
                        action = Some(child_action);
                    }
                }
            }

            NodeKind::File => {
                let is_selected = selected.as_ref() == Some(&node.path);
                let node_path = node.path.clone();

                ui.horizontal(|ui| {
                    // Move arrows
                    if idx > 0 {
                        if ui.small_button("^").on_hover_text("Move up").clicked() {
                            action = Some(SidebarAction::Reorder {
                                parent: parent_path.to_path_buf(),
                                from: idx,
                                to: idx - 1,
                            });
                        }
                    }
                    if idx + 1 < node_count {
                        if ui.small_button("v").on_hover_text("Move down").clicked() {
                            action = Some(SidebarAction::Reorder {
                                parent: parent_path.to_path_buf(),
                                from: idx,
                                to: idx + 1,
                            });
                        }
                    }

                    // File label
                    let label = if is_selected {
                        RichText::new(&node.name)
                            .color(Color32::from_rgb(80, 170, 255))
                    } else {
                        RichText::new(&node.name)
                    };

                    if ui.selectable_label(is_selected, label).clicked() {
                        action = Some(SidebarAction::LoadFile(node_path));
                    }
                });
            }
        }
    }

    action
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

// ─── ordering config ─────────────────────────────────────────────────────────

/// Persisted custom ordering for siblings within each directory.
/// Maps a canonical directory path (as a string) to an ordered list of
/// child file/folder names.  Directories not present use alphabetical order.
#[derive(Debug, Default, Serialize, Deserialize)]
struct OrderConfig {
    #[serde(default)]
    orders: HashMap<String, Vec<String>>,
}

impl OrderConfig {
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
            .join("ordering.json")
    }

    /// Apply custom ordering to a list of children for a given parent dir.
    /// Children not in the saved order are appended at the end in their
    /// original (alphabetical) position.
    fn apply(&self, parent: &Path, children: &mut Vec<FileNode>) {
        let key = parent.to_string_lossy().to_string();
        if let Some(order) = self.orders.get(&key) {
            let name_to_pos: HashMap<&str, usize> = order
                .iter()
                .enumerate()
                .map(|(i, n)| (n.as_str(), i))
                .collect();
            children.sort_by(|a, b| {
                let pa = name_to_pos.get(a.name.as_str()).copied().unwrap_or(usize::MAX);
                let pb = name_to_pos.get(b.name.as_str()).copied().unwrap_or(usize::MAX);
                pa.cmp(&pb)
            });
        }
    }

    /// Save the current child order for a parent directory.
    fn set_order(&mut self, parent: &Path, children: &[FileNode]) {
        let key = parent.to_string_lossy().to_string();
        let names: Vec<String> = children.iter().map(|n| n.name.clone()).collect();
        self.orders.insert(key, names);
        self.save();
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
  figure {{ margin: 1.5em auto; text-align: center; page-break-inside: avoid; }}
  figure img {{ max-width: 100%; display: block; margin: 0 auto; }}
  figcaption {{ font-style: italic; color: #555; font-size: 0.92em;
                margin-top: 0.4em; text-align: center; }}
  .toc {{ margin-bottom: 2em; }}
  .toc h1 {{ font-size: 1.6em; border-bottom: 2px solid #333; padding-bottom: .3em; }}
  .toc ul {{ padding-left: 1.4em; list-style: none; margin: 0.2em 0; }}
  .toc .toc-root {{ padding-left: 0; }}
  .toc li {{ margin: 0.25em 0; line-height: 1.5; }}
  .toc .toc-section {{ margin-top: 0.8em; font-size: 1.05em; }}
  .toc a {{ color: #1a73e8; text-decoration: none; }}
  .toc a:hover {{ text-decoration: underline; }}
  .doc-title {{ page-break-after: avoid; }}
</style>
</head>
<body>
{body}
</body>
</html>"#
    )
}

/// Locate Chromium for headless PDF rendering.
///
/// Resolution order:
///   1. Bundled Chromium next to the installed exe:
///      `<exe_dir>\..\chromium\chrome-win\chrome.exe`
///      (MSI installs `mdreader.exe` under `bin\` and Chromium under `chromium\`)
///   2. Bundled Chromium next to the exe itself (dev/portable layout):
///      `<exe_dir>\chromium\chrome-win\chrome.exe`
///   3. System-wide Chrome or Edge.
///
/// Bundled Chromium is preferred so the app works fully offline.
fn find_chrome() -> Option<PathBuf> {
    // 1 + 2: bundled
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let bundled_sibling = exe_dir.join("chromium").join("chrome-win").join("chrome.exe");
            if bundled_sibling.exists() {
                return Some(bundled_sibling);
            }
            if let Some(install_root) = exe_dir.parent() {
                let bundled_installed =
                    install_root.join("chromium").join("chrome-win").join("chrome.exe");
                if bundled_installed.exists() {
                    return Some(bundled_installed);
                }
            }
        }
    }

    // 3: system-wide fallback
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

// ─── image uri resolution ────────────────────────────────────────────────────

fn has_uri_scheme(uri: &str) -> bool {
    uri.starts_with("http://")
        || uri.starts_with("https://")
        || uri.starts_with("file://")
        || uri.starts_with("data:")
        || uri.starts_with('#')
}

fn to_file_url(path: &str, base_dir: &Path) -> String {
    let trimmed = path.trim();
    let p = Path::new(trimmed);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base_dir.join(trimmed)
    };
    // Normalize: forward slashes, then `file:///C:/...` on Windows vs
    // `file:///path/...` on Unix.
    let s = abs.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// Rewrite relative image URIs in markdown to absolute `file://` URLs so
/// egui's image loader (which receives the raw URI string) can find them.
///
/// Handles both inline `![alt](path)` syntax and reference-style
/// `![alt][id]` ... `[id]: path` definitions.  URIs that already carry a
/// scheme (http, https, file, data) are left untouched.
///
/// Line-oriented implementation — assumes the `![..](..)` construct doesn't
/// cross newlines, which matches every real-world markdown document.  The
/// PDF export pipeline applies the same transform so Chromium headless can
/// also resolve local images.
fn resolve_image_uris(content: &str, base_dir: &Path) -> String {
    let mut out = String::with_capacity(content.len() + 64);
    for (idx, raw_line) in content.split_inclusive('\n').enumerate() {
        let _ = idx;
        // Reference-style definition: `[id]: path "title"`
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with('[') {
            if let Some(bracket_end) = trimmed.find("]:") {
                let indent_len = raw_line.len() - trimmed.len();
                let id_part = &trimmed[..=bracket_end + 1];
                let rest_with_nl = &trimmed[bracket_end + 2..];
                // Strip trailing newline for parsing, remember it
                let (rest, tail) = match rest_with_nl.strip_suffix("\r\n") {
                    Some(r) => (r, "\r\n"),
                    None => match rest_with_nl.strip_suffix('\n') {
                        Some(r) => (r, "\n"),
                        None => (rest_with_nl, ""),
                    },
                };
                let rest = rest.trim_start();
                let (url_raw, title) = match rest.find(char::is_whitespace) {
                    Some(sp) => (rest[..sp].trim(), rest[sp..].trim()),
                    None => (rest.trim(), ""),
                };
                if !url_raw.is_empty() && !has_uri_scheme(url_raw) {
                    out.push_str(&raw_line[..indent_len]);
                    out.push_str(id_part);
                    out.push(' ');
                    out.push_str(&to_file_url(url_raw, base_dir));
                    if !title.is_empty() {
                        out.push(' ');
                        out.push_str(title);
                    }
                    out.push_str(tail);
                    continue;
                }
            }
        }

        // Inline images on this line: scan for `![` ... `](` ... `)`.
        let mut rewritten = String::with_capacity(raw_line.len());
        let mut cursor = 0usize;
        let line = raw_line;
        while let Some(bang) = line[cursor..].find("![") {
            let bang_abs = cursor + bang;
            // Not escaped?
            if bang_abs > 0 && line.as_bytes()[bang_abs - 1] == b'\\' {
                rewritten.push_str(&line[cursor..bang_abs + 2]);
                cursor = bang_abs + 2;
                continue;
            }
            let after_bang = bang_abs + 2;
            // Find `]` closing the alt text.
            let alt_close = match line[after_bang..].find(']') {
                Some(p) => after_bang + p,
                None => break,
            };
            // Must be followed immediately by `(`
            if line.as_bytes().get(alt_close + 1) != Some(&b'(') {
                rewritten.push_str(&line[cursor..alt_close + 1]);
                cursor = alt_close + 1;
                continue;
            }
            let url_start = alt_close + 2;
            let url_close = match line[url_start..].find(')') {
                Some(p) => url_start + p,
                None => break,
            };
            let alt = &line[after_bang..alt_close];
            let inside = &line[url_start..url_close];
            let (url_raw, title) = match inside.find(char::is_whitespace) {
                Some(sp) => (inside[..sp].trim(), inside[sp..].trim()),
                None => (inside.trim(), ""),
            };
            let new_url = if url_raw.is_empty() || has_uri_scheme(url_raw) {
                url_raw.to_string()
            } else {
                to_file_url(url_raw, base_dir)
            };
            // Emit everything before this image, then the rewritten image.
            rewritten.push_str(&line[cursor..bang_abs]);
            rewritten.push_str("![");
            rewritten.push_str(alt);
            rewritten.push_str("](");
            rewritten.push_str(&new_url);
            if !title.is_empty() {
                rewritten.push(' ');
                rewritten.push_str(title);
            }
            rewritten.push(')');
            cursor = url_close + 1;
        }
        rewritten.push_str(&line[cursor..]);
        out.push_str(&rewritten);
    }
    out
}

// ─── figure captions ─────────────────────────────────────────────────────────
//
// Markdown has no first-class figure syntax, but CommonMark already allows a
// *title attribute* on images:
//
//     ![alt](path.png "My caption")
//
// By default the title just becomes a tooltip.  We treat a paragraph-only
// image with a title as a figure and expand it:
//
//   - GUI viewer: rewrite to `![alt](path.png)\n\n*caption*` — renders the
//     caption as an italic line immediately below the image.  egui_commonmark
//     has no <figure> primitive and no per-node rendering hook at 0.17, so
//     this is the lightest path that yields the same visual reading without
//     touching the renderer.
//
//   - PDF export: emit raw HTML `<figure><img><figcaption>...</figcaption></figure>`
//     so Chromium produces a properly grouped, styled, and semantic figure.
//
// Both branches run AFTER `resolve_image_uris` so the URL is already absolute.

/// If `line` is a standalone image with a title attribute, return
/// `(alt, url, caption)`.  Otherwise None.
///
/// "Standalone" = nothing on the line except the image expression (leading
/// and trailing whitespace is fine).  Inline images with titles inside a
/// wider paragraph continue to behave as ordinary markdown (title → tooltip).
fn parse_figure_line(line: &str) -> Option<(&str, &str, String)> {
    let t = line.trim();
    if !t.starts_with("![") || !t.ends_with(')') {
        return None;
    }
    let alt_end = t.find("](")?;
    let alt = &t[2..alt_end];
    let inside = &t[alt_end + 2..t.len() - 1];
    // Title syntax requires whitespace between url and quoted title.
    let sp = inside.find(char::is_whitespace)?;
    let url = inside[..sp].trim();
    if url.is_empty() {
        return None;
    }
    let rest = inside[sp..].trim();
    if rest.len() < 2 {
        return None;
    }
    let quote = rest.as_bytes()[0];
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    if !rest.ends_with(quote as char) {
        return None;
    }
    let caption = rest[1..rest.len() - 1].to_string();
    Some((alt, url, caption))
}

/// GUI-side expansion: rewrite figure paragraphs to image + italic caption.
fn expand_figures_md(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + 64);
    for line in content.split_inclusive('\n') {
        let (body, tail) = match line.strip_suffix("\r\n") {
            Some(b) => (b, "\r\n"),
            None => match line.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (line, ""),
            },
        };
        if let Some((alt, url, caption)) = parse_figure_line(body) {
            // Escape `*` inside the caption so it doesn't collide with the
            // italic delimiters we're about to add.
            let caption_esc = caption.replace('*', r"\*");
            out.push_str("![");
            out.push_str(alt);
            out.push_str("](");
            out.push_str(url);
            out.push_str(")");
            out.push_str(tail);
            out.push_str(tail); // blank line before caption
            out.push('*');
            out.push_str(&caption_esc);
            out.push('*');
            out.push_str(tail);
        } else {
            out.push_str(line);
        }
    }
    out
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// PDF-side expansion: rewrite figure paragraphs to an HTML `<figure>` block.
/// Requires the markdown renderer to have raw-HTML passthrough enabled.
fn expand_figures_html(content: &str) -> String {
    let mut out = String::with_capacity(content.len() + 128);
    for line in content.split_inclusive('\n') {
        let (body, tail) = match line.strip_suffix("\r\n") {
            Some(b) => (b, "\r\n"),
            None => match line.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (line, ""),
            },
        };
        if let Some((alt, url, caption)) = parse_figure_line(body) {
            // Markdown requires a blank line before and after raw HTML blocks
            // for the parser to treat them as block-level.
            out.push_str(tail);
            out.push_str("<figure>");
            out.push_str(&format!(
                r#"<img src="{}" alt="{}">"#,
                escape_html(url),
                escape_html(alt)
            ));
            out.push_str(&format!(
                "<figcaption>{}</figcaption>",
                escape_html(&caption)
            ));
            out.push_str("</figure>");
            out.push_str(tail);
            out.push_str(tail);
        } else {
            out.push_str(line);
        }
    }
    out
}

// ─── smart TOC helpers (all fallible → default on failure) ───────────────────

/// Try to extract the first `# heading` from markdown content.
/// Falls back to the file stem (kebab/snake → title case).
fn display_name_for(content: &str, path: &Path) -> String {
    // Try: first line matching `# Some Title`
    if let Some(title) = content.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") && !trimmed.starts_with("##") {
            Some(trimmed.trim_start_matches("# ").trim().to_string())
        } else {
            None
        }
    }) {
        if !title.is_empty() {
            return title;
        }
    }
    // Default: file stem → title case
    slug_to_title(&path.file_stem().unwrap_or_default().to_string_lossy())
}

/// Convert a kebab-case or snake_case slug to Title Case.
/// E.g. "part-1-vision" → "Part 1 Vision", "ux-analysis" → "Ux Analysis".
fn slug_to_title(slug: &str) -> String {
    slug.split(|c: char| c == '-' || c == '_')
        .filter(|s| !s.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    format!("{upper}{}", chars.as_str())
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Clean a directory name for use as a TOC group heading.
/// Detects `part-N-slug` pattern → "Part N — Slug".
/// Falls back to slug_to_title.
fn dir_display_name(dir_name: &str) -> String {
    // Try: "part-N-rest" or "part-N-rest-of-name"
    let lower = dir_name.to_lowercase();
    if lower.starts_with("part-") {
        let rest = &dir_name[5..]; // after "part-"
        if let Some(idx) = rest.find('-') {
            let num = &rest[..idx];
            let slug = &rest[idx + 1..];
            if num.chars().all(|c| c.is_ascii_digit()) {
                let title = slug_to_title(slug);
                return format!("Part {num} — {title}");
            }
        }
    }
    // Default: plain title case
    slug_to_title(dir_name)
}

/// Detect whether a markdown file is purely a navigation index (almost entirely
/// `.md` link lists with minimal prose).  Very conservative: only returns true
/// when the file is named README.md AND ≥70% of content lines are link-list
/// entries.  Anything ambiguous is treated as real content.
fn is_nav_only(content: &str, path: &Path) -> bool {
    // Only READMEs can be nav-only; regular docs are never stripped.
    let is_readme = path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase() == "readme.md")
        .unwrap_or(false);
    if !is_readme {
        return false;
    }

    let mut link_lines = 0u32;
    let mut content_lines = 0u32;

    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t == "---" || t.starts_with('#') {
            continue;
        }
        content_lines += 1;
        // Only count lines that START with `- [` and end with `.md)` — strict
        if (t.starts_with("- [") || t.starts_with("* ["))
            && t.contains("](")
            && t.ends_with(".md)")
        {
            link_lines += 1;
        }
    }

    // Very conservative threshold: 70% link lines, minimum 3 content lines
    content_lines >= 3 && link_lines * 10 > content_lines * 7
}

/// Extract introductory prose from a navigation README (everything before the
/// first list/link block). Returns empty string if extraction fails.
fn extract_intro(content: &str) -> String {
    let mut intro = String::new();
    let mut hit_link_block = false;

    for line in content.lines() {
        let t = line.trim();
        // Skip the title heading — we use our own
        if !hit_link_block && t.starts_with("# ") && !t.starts_with("##") && intro.is_empty() {
            continue;
        }
        // Once we hit a link-list line, stop collecting intro
        if (t.starts_with("- [") || t.starts_with("## [")) && t.contains(".md)") {
            hit_link_block = true;
            continue;
        }
        if hit_link_block {
            continue;
        }
        intro.push_str(line);
        intro.push('\n');
    }
    intro.trim().to_string()
}

/// Strip inline TOC blocks: only removes a heading + link-list block when
/// the heading is EXACTLY "In this part:" (case-insensitive) or
/// "Table of Contents" — and every subsequent non-blank line is a markdown
/// list item linking to a `.md` file.  If the block doesn't match this
/// strict pattern it is left untouched.
fn strip_inline_toc(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;

    while i < lines.len() {
        let t = lines[i].trim();

        // Only match headings that are exactly TOC-like labels
        if t.starts_with('#') && i + 1 < lines.len() {
            // Strip the #'s and ** bold markers to get the heading text
            let heading_text = t.trim_start_matches('#').trim()
                .trim_start_matches("**").trim_end_matches("**").trim()
                .to_lowercase();

            let is_toc_heading = heading_text == "in this part:"
                || heading_text == "in this part"
                || heading_text == "table of contents";

            if is_toc_heading {
                // Peek ahead: only skip if ALL following non-blank lines are
                // `.md` link-list items.  If any line isn't, abort and keep
                // everything.
                let mut j = i + 1;
                let all_links = true;
                let mut found_links = false;

                while j < lines.len() {
                    let lt = lines[j].trim();
                    if lt.is_empty() {
                        j += 1;
                        continue;
                    }
                    if (lt.starts_with("- [") || lt.starts_with("* ["))
                        && lt.contains("](")
                        && lt.contains(".md)")
                    {
                        found_links = true;
                        j += 1;
                    } else {
                        // Non-link content line → stop scanning
                        break;
                    }
                }

                if found_links && all_links {
                    // Skip the heading + link block
                    i = j;
                    continue;
                }
            }
        }

        out.push(lines[i]);
        i += 1;
    }

    out.join("\n")
}

/// Build a path→anchor lookup from document entries.
fn build_path_map(entries: &[DocEntry]) -> HashMap<PathBuf, String> {
    let mut map = HashMap::new();
    let mut doc_idx = 0usize;
    for entry in entries {
        if let DocEntry::Document { path, .. } = entry {
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            map.insert(canon, format!("doc-{doc_idx}"));
            doc_idx += 1;
        }
    }
    map
}

/// Rewrite relative `.md` links in markdown content to `#anchor` references.
/// Only rewrites links whose resolved path exists in the path map.
/// Non-matching links are left untouched.
/// Skips code blocks (``` fenced) and inline code (` backtick) entirely.
fn rewrite_md_links(
    content: &str,
    file_dir: &Path,
    path_map: &std::collections::HashMap<PathBuf, String>,
) -> String {
    let mut out_lines = Vec::new();
    let mut in_code_block = false;

    for line in content.lines() {
        // Track fenced code blocks — never touch content inside them
        if line.trim().starts_with("```") {
            in_code_block = !in_code_block;
            out_lines.push(line.to_string());
            continue;
        }
        if in_code_block {
            out_lines.push(line.to_string());
            continue;
        }

        // Process this line: find markdown links `[text](target.md)`
        // Skip anything inside inline backticks
        let mut result = String::with_capacity(line.len());
        let mut chars = line.char_indices().peekable();
        let bytes = line.as_bytes();

        while let Some((pos, ch)) = chars.next() {
            // Skip inline code spans
            if ch == '`' {
                result.push(ch);
                for (_, c) in chars.by_ref() {
                    result.push(c);
                    if c == '`' { break; }
                }
                continue;
            }

            // Look for `](` pattern — the `]` must be preceded by a `[` somewhere
            if ch == ']' && pos + 1 < line.len() && bytes[pos + 1] == b'(' {
                // Find the opening `[` by scanning backwards in result
                if !result.contains('[') {
                    result.push(ch);
                    continue;
                }

                // Consume the `(`
                chars.next(); // skip `(`

                // Collect the link target up to `)`
                let mut target = String::new();
                let mut found_close = false;
                for (_, c) in chars.by_ref() {
                    if c == ')' { found_close = true; break; }
                    // Bail on spaces/newlines — not a valid markdown link
                    if c == ' ' || c == '\n' { target.push(c); break; }
                    target.push(c);
                }

                if !found_close {
                    // Not a valid link — dump what we collected
                    result.push(']');
                    result.push('(');
                    result.push_str(&target);
                    continue;
                }

                // Only rewrite .md links
                if target.ends_with(".md") || target.contains(".md#") {
                    let path_part = target.split('#').next().unwrap_or(&target);
                    let resolved = file_dir.join(path_part);

                    if let Some(anchor) = resolved.canonicalize().ok()
                        .and_then(|c| path_map.get(&c))
                    {
                        result.push_str("](#");
                        result.push_str(anchor);
                        result.push(')');
                        continue;
                    }
                }

                // Default: leave link untouched
                result.push(']');
                result.push('(');
                result.push_str(&target);
                result.push(')');
            } else {
                result.push(ch);
            }
        }

        out_lines.push(result);
    }

    out_lines.join("\n")
}

// ─── pdf export ──────────────────────────────────────────────────────────────

/// GUI wrapper: prompts for output path via native dialog, then calls `build_pdf`.
fn export_pdf(tree: &[FileNode], root_name: &str) -> Result<PathBuf, String> {
    let entries = collect_doc_entries(tree, 0);
    if entries.is_empty() {
        return Err("No markdown files to export.".to_string());
    }

    let default_name = format!("{root_name}.pdf");
    let pdf_path = rfd::FileDialog::new()
        .add_filter("PDF", &["pdf"])
        .set_file_name(&default_name)
        .save_file()
        .ok_or_else(|| "Export cancelled.".to_string())?;

    build_pdf(&entries, &pdf_path, root_name)
}

/// Build a single PDF from a prepared list of doc entries to a specified path.
/// Shared by the GUI "Export as PDF…" action and the `--to-pdf` CLI flag.
/// Every transformation is wrapped in a try/default pattern: if detection or
/// rewriting fails for a given file the original content is used verbatim.
fn build_pdf(entries: &[DocEntry], pdf_path: &Path, root_name: &str) -> Result<PathBuf, String> {
    if entries.is_empty() {
        return Err("No markdown files to export.".to_string());
    }
    let pdf_path = pdf_path.to_path_buf();

    // ── 2. build path→anchor map ───────────────────────────────────────────
    let path_map = build_path_map(entries);

    // ── 3. comrak options ──────────────────────────────────────────────────
    let mut opts = comrak::Options::default();
    opts.extension.table = true;
    opts.extension.strikethrough = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;
    // Let raw HTML blocks through — needed for the <figure>/<figcaption>
    // markup emitted by `expand_figures_html`.  The source is the user's own
    // markdown, loaded locally, so the usual XSS concerns don't apply here.
    opts.render.unsafe_ = true;

    // ── 4. build TOC + body in a single pass ───────────────────────────────
    //
    // TOC entries:  (depth, display_name, anchor_or_none)
    //   - Section  → bold group heading, no link
    //   - Document → clickable link to #anchor
    //
    // Body sections: only Documents produce rendered HTML.

    struct TocItem {
        depth: usize,
        display: String,
        anchor: Option<String>, // None = section heading
    }

    let mut toc_items: Vec<TocItem> = Vec::new();
    let mut body_html = String::new();
    let mut doc_idx = 0usize;

    for entry in entries {
        match entry {
            DocEntry::Section { name, depth, .. } => {
                toc_items.push(TocItem {
                    depth: *depth,
                    display: dir_display_name(name),
                    anchor: None,
                });
            }
            DocEntry::Document { path, depth } => {
                let raw_disk = fs::read_to_string(path)
                    .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
                // Rewrite relative image paths to absolute file:// URLs so
                // Chromium can locate them when rendering the temp HTML, then
                // expand figure-style paragraphs (image + title attribute) to
                // semantic <figure>/<figcaption> HTML.
                let base_dir = path.parent().unwrap_or(Path::new(""));
                let resolved = resolve_image_uris(&raw_disk, base_dir);
                let raw_content = expand_figures_html(&resolved);

                let anchor = format!("doc-{doc_idx}");
                doc_idx += 1;

                // Display name: first # heading, fallback to file stem
                let display = display_name_for(&raw_content, path);

                // Detect nav-only READMEs
                let nav = is_nav_only(&raw_content, path);

                toc_items.push(TocItem {
                    depth: *depth,
                    display: display.clone(),
                    anchor: if nav { None } else { Some(anchor.clone()) },
                });

                // Prepare content
                let processed = if nav {
                    extract_intro(&raw_content)
                } else {
                    let stripped = strip_inline_toc(&raw_content);
                    let file_dir = path.parent().unwrap_or(path);
                    let rewritten = rewrite_md_links(&stripped, file_dir, &path_map);
                    // Safety net
                    if rewritten.len() < raw_content.len() * 60 / 100 {
                        raw_content.clone()
                    } else {
                        rewritten
                    }
                };

                if processed.trim().is_empty() {
                    continue;
                }

                // Strip duplicate first # heading
                let final_content = {
                    let mut found = false;
                    let mut out = String::new();
                    for line in processed.lines() {
                        if !found && line.trim().starts_with("# ")
                            && !line.trim().starts_with("##")
                        {
                            found = true;
                            continue;
                        }
                        out.push_str(line);
                        out.push('\n');
                    }
                    if found { out } else { processed }
                };

                // Page break before each section except the first
                if !body_html.is_empty() {
                    body_html.push_str(
                        "<div style=\"page-break-before:always\"></div>\n",
                    );
                }

                // Section heading level: h1 for depth 0-1, h2 for depth 2+
                let tag = if *depth <= 1 { "h1" } else { "h2" };
                body_html.push_str(&format!(
                    "<{tag} id=\"{anchor}\" class=\"doc-title\">{display}</{tag}>\n"
                ));
                body_html.push_str(&comrak::markdown_to_html(&final_content, &opts));
            }
        }
    }

    // ── 5. build hierarchical TOC HTML ─────────────────────────────────────
    let mut toc_html = String::from(
        "<nav class=\"toc\">\n<h1>Table of Contents</h1>\n",
    );
    let mut current_depth: Option<usize> = None;
    let mut open_uls = 0u32;

    // Open the root list
    toc_html.push_str("<ul class=\"toc-root\">\n");

    for item in &toc_items {
        let d = item.depth;

        match current_depth {
            None => {
                current_depth = Some(d);
            }
            Some(cd) => {
                if d > cd {
                    for _ in 0..(d - cd) {
                        toc_html.push_str("<ul>\n");
                        open_uls += 1;
                    }
                } else if d < cd {
                    let close = (cd - d).min(open_uls as usize);
                    for _ in 0..close {
                        toc_html.push_str("</ul></li>\n");
                        open_uls -= 1;
                    }
                }
                current_depth = Some(d);
            }
        }

        match &item.anchor {
            Some(anchor) => {
                toc_html.push_str(&format!(
                    "  <li><a href=\"#{anchor}\">{}</a></li>\n",
                    item.display
                ));
            }
            None => {
                // Section heading — bold, not a link
                toc_html.push_str(&format!(
                    "  <li class=\"toc-section\"><strong>{}</strong>\n",
                    item.display
                ));
                // Don't close </li> — nested <ul> will follow
            }
        }
    }

    // Close all remaining open lists
    for _ in 0..open_uls {
        toc_html.push_str("</ul></li>\n");
    }
    toc_html.push_str("</ul>\n</nav>\n<div style=\"page-break-after:always\"></div>\n");

    let full_body = format!("{toc_html}{body_html}");
    let html = html_page(&full_body, root_name);

    // ── 5. write temp HTML ─────────────────────────────────────────────────
    let tmp_html = std::env::temp_dir().join("mdreader_export.html");
    fs::write(&tmp_html, &html)
        .map_err(|e| format!("Failed to write temp HTML: {e}"))?;

    // ── 6. Chrome / Edge headless → PDF ───────────────────────────────────
    let chrome = find_chrome()
        .ok_or_else(|| "Chrome/Edge not found — cannot generate PDF.".to_string())?;

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
    order: OrderConfig,
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
    pub update_rx: Option<mpsc::Receiver<Option<UpdateAvailable>>>,
    /// Current state of the update workflow.
    pub update_state: UpdateState,
    /// Persistent error message from a failed update attempt.
    pub update_error: Option<String>,
    /// Drag-reorder state: (parent path, source index within siblings).
    drag_reorder: Option<(PathBuf, usize)>,
    /// Cached window title to avoid sending viewport commands every frame.
    last_title: String,
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

        // Register image loaders so `![alt](foo.png)` in markdown renders
        // actual bitmaps instead of silently displaying nothing.  Handles
        // PNG/JPEG/WebP/GIF/BMP via the `image` crate, SVG via resvg, and
        // http(s) URLs via ehttp.
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let order = OrderConfig::load();
        let tree = build_tree(&config.root_path, &order);
        let root_input = config.root_path.clone();

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || { let _ = tx.send(check_latest_release()); });

        Self {
            config,
            order,
            tree,
            selected_file: None,
            file_content: String::new(),
            md_cache: CommonMarkCache::default(),
            show_settings: false,
            root_input,
            native_ppp,
            drag_zoom: None,
            status_msg: None,
            update_rx: Some(rx),
            update_state: UpdateState::Checking,
            update_error: None,
            drag_reorder: None,
            last_title: String::new(),
        }
    }

    fn load_file(&mut self, path: PathBuf) {
        match fs::read_to_string(&path) {
            Ok(c) => {
                let base_dir = path.parent().unwrap_or(Path::new(""));
                let resolved = resolve_image_uris(&c, base_dir);
                self.file_content = expand_figures_md(&resolved);
                self.selected_file = Some(path);
            }
            Err(e) => {
                self.file_content = format!("> **Error reading file:** {e}");
                self.selected_file = Some(path);
            }
        }
    }

    fn refresh_tree(&mut self) {
        self.order = OrderConfig::load();
        self.tree = build_tree(&self.config.root_path, &self.order);
    }

    /// Reorder siblings: move item at `from` to `to` within the children
    /// of `parent`, then persist the new order.
    fn reorder(&mut self, parent: &Path, from: usize, to: usize) {
        // Find the sibling list to reorder — either the root tree or a dir's children
        let root_path = PathBuf::from(&self.config.root_path);
        let siblings = if parent == root_path {
            Some(&mut self.tree)
        } else {
            Self::find_children(&mut self.tree, parent)
        };

        if let Some(nodes) = siblings {
            if from < nodes.len() && to < nodes.len() && from != to {
                let node = nodes.remove(from);
                nodes.insert(to, node);
                self.order.set_order(parent, nodes);
            }
        }
    }

    /// Recursively find the mutable children Vec for a given directory path.
    fn find_children<'a>(
        nodes: &'a mut Vec<FileNode>,
        target: &Path,
    ) -> Option<&'a mut Vec<FileNode>> {
        for node in nodes.iter_mut() {
            if node.path == target {
                if let NodeKind::Dir(ref mut children) = node.kind {
                    return Some(children);
                }
            }
            if let NodeKind::Dir(ref mut children) = node.kind {
                if let Some(found) = Self::find_children(children, target) {
                    return Some(found);
                }
            }
        }
        None
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
        if let Some(ref rx) = self.update_rx {
            if let Ok(result) = rx.try_recv() {
                self.update_state = match result {
                    Some(avail) => UpdateState::Available(avail),
                    None => UpdateState::Idle,
                };
                self.update_rx = None;
            }
        }

        // ── window title (only send when changed to avoid repaint loop) ────
        let title = format!("MD Reader — {}", self.root_display_name());
        if title != self.last_title {
            self.last_title = title.clone();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }

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

                    // Export all files to a single PDF with TOC.
                    let can_export = !self.tree.is_empty();
                    if ui
                        .add_enabled(can_export, egui::Button::new("⬇  Export as PDF…"))
                        .clicked()
                    {
                        let root_name = self.root_display_name();
                        match export_pdf(&self.tree, &root_name) {
                            Ok(out) => {
                                self.status_msg =
                                    Some(format!("✔ PDF saved → {}", out.display()));
                            }
                            Err(e) => {
                                self.status_msg = Some(format!("✘ {e}"));
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
                // Version label — clickable to check for updates
                self.render_version_button(ui);

                ui.separator();

                self.render_update_status(ui);

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
        let mut sidebar_action: Option<SidebarAction> = None;
        let sel = self.selected_file.clone();
        let root_path = PathBuf::from(&self.config.root_path);

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
                            .size(15.0)
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
                            sidebar_action = render_siblings(
                                ui,
                                &mut self.tree,
                                &sel,
                                &root_path,
                                &mut self.drag_reorder,
                            );
                        }
                    });
            });

        // Handle sidebar actions
        match sidebar_action {
            Some(SidebarAction::LoadFile(p)) => self.load_file(p),
            Some(SidebarAction::Reorder { parent, from, to }) => {
                self.reorder(&parent, from, to);
            }
            None => {}
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

        // ── idle repaint throttle ────────────────────────────────────────────
        // Only repaint when the user interacts (mouse, keyboard, scroll) or
        // when we're waiting on the update checker.  For everything else the
        // app is a static document viewer — zero repaints needed.
        if matches!(self.update_state, UpdateState::Checking | UpdateState::Downloading(_)) {
            // Background thread still running — poll once per second
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        // Otherwise: no request_repaint at all.  egui will still wake on any
        // user input (mouse move, click, scroll, key press, window resize)
        // automatically — that's built into the winit/eframe event loop.
    }
}

// ─── entry point ──────────────────────────────────────────────────────────────

/// Try to attach to the parent console on Windows so stdout/stderr/exit codes
/// work when the app was launched from cmd.exe or PowerShell.  Necessary
/// because `windows_subsystem = "windows"` hides the console by default.
#[cfg(windows)]
fn attach_parent_console() {
    extern "system" {
        fn AttachConsole(dwProcessId: u32) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

fn print_cli_help() {
    println!("MD Reader v{} — read-only markdown viewer with PDF export", env!("CARGO_PKG_VERSION"));
    println!();
    println!("USAGE:");
    println!("    mdreader                         Launch GUI");
    println!("    mdreader <DIR>                   Launch GUI with <DIR> as root");
    println!("    mdreader --to-pdf <IN> [OPTS]    Convert markdown to PDF (headless)");
    println!("    mdreader --help | -h             Show this help");
    println!("    mdreader --version | -V          Show version");
    println!();
    println!("PDF EXPORT OPTIONS:");
    println!("    <IN>                             Path to a .md file or a directory");
    println!("                                     tree containing markdown files.");
    println!("    -o, --output <FILE>              Output PDF path");
    println!("                                     (default: <IN stem>.pdf in CWD)");
    println!();
    println!("EXAMPLES:");
    println!("    mdreader --to-pdf notes.md");
    println!("    mdreader --to-pdf ./docs -o book.pdf");
    println!();
    println!("PDF export uses bundled Chromium (no internet required).");
}

/// Collect doc entries from a single-file OR directory input for CLI PDF export.
fn collect_cli_entries(input: &Path) -> Result<(Vec<DocEntry>, String), String> {
    if input.is_file() {
        let name = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("document")
            .to_string();
        Ok((
            vec![DocEntry::Document { path: input.to_path_buf(), depth: 0 }],
            name,
        ))
    } else if input.is_dir() {
        let order = OrderConfig::default();
        let root = input.to_string_lossy().into_owned();
        let tree = build_tree(&root, &order);
        let entries = collect_doc_entries(&tree, 0);
        if entries.is_empty() {
            return Err(format!("No markdown files found in {}", input.display()));
        }
        let name = input
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("export")
            .to_string();
        Ok((entries, name))
    } else {
        Err(format!("Input not found: {}", input.display()))
    }
}

/// Handle `--to-pdf` CLI invocation.  Returns exit code.
fn run_cli_to_pdf(args: &[String]) -> i32 {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: {} requires a value", args[i - 1]);
                    return 2;
                }
                output = Some(PathBuf::from(&args[i]));
            }
            a if input.is_none() => {
                input = Some(PathBuf::from(a));
            }
            a => {
                eprintln!("error: unexpected argument: {a}");
                return 2;
            }
        }
        i += 1;
    }

    let input = match input {
        Some(p) => p,
        None => {
            eprintln!("error: --to-pdf requires an input path");
            return 2;
        }
    };

    let (entries, stem) = match collect_cli_entries(&input) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let output = output.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(format!("{stem}.pdf"))
    });

    match build_pdf(&entries, &output, &stem) {
        Ok(p) => {
            println!("PDF written: {}", p.display());
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn main() {
    // ── CLI argument handling ────────────────────────────────────────────────
    let raw: Vec<String> = std::env::args().skip(1).collect();

    // Help / version flags
    if raw.iter().any(|a| a == "--help" || a == "-h") {
        attach_parent_console();
        print_cli_help();
        std::process::exit(0);
    }
    if raw.iter().any(|a| a == "--version" || a == "-V") {
        attach_parent_console();
        println!("mdreader {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }

    // --to-pdf headless mode
    if let Some(pos) = raw.iter().position(|a| a == "--to-pdf") {
        attach_parent_console();
        let code = run_cli_to_pdf(&raw[pos + 1..]);
        std::process::exit(code);
    }

    // GUI mode: first arg (if any) is a directory to open.
    let cli_root: Option<String> = raw.into_iter().next().and_then(|arg| {
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

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> PathBuf {
        // Use forward slashes to make expected strings portable.
        PathBuf::from("C:/docs")
    }

    #[test]
    fn resolves_relative_inline_image() {
        let src = "See ![diagram](img/flow.png) below.";
        let got = resolve_image_uris(src, &base());
        assert!(got.contains("![diagram](file:///C:/docs/img/flow.png)"),
            "got: {got}");
    }

    #[test]
    fn preserves_http_url() {
        let src = "![logo](https://example.com/logo.png)";
        let got = resolve_image_uris(src, &base());
        assert_eq!(got, "![logo](https://example.com/logo.png)");
    }

    #[test]
    fn preserves_title_attribute() {
        let src = r#"![alt](foo.png "caption text")"#;
        let got = resolve_image_uris(src, &base());
        assert!(got.contains(r#"(file:///C:/docs/foo.png "caption text")"#),
            "got: {got}");
    }

    #[test]
    fn rewrites_reference_definition() {
        let src = "![pic][p1]\n\n[p1]: assets/a.jpg\n";
        let got = resolve_image_uris(src, &base());
        assert!(got.contains("[p1]: file:///C:/docs/assets/a.jpg"),
            "got: {got}");
    }

    #[test]
    fn leaves_non_image_links_alone() {
        let src = "See [the doc](other.md) for details.";
        let got = resolve_image_uris(src, &base());
        assert_eq!(got, src);
    }

    #[test]
    fn handles_no_images() {
        let src = "# Plain text\n\nNo images here.\n";
        let got = resolve_image_uris(src, &base());
        assert_eq!(got, src);
    }

    // ── figure captions ──────────────────────────────────────────────────

    #[test]
    fn detects_standalone_figure_with_title() {
        let line = r#"![schematic](foo.png "Figure 1")"#;
        let got = parse_figure_line(line);
        assert_eq!(got, Some(("schematic", "foo.png", "Figure 1".to_string())));
    }

    #[test]
    fn detects_figure_with_surrounding_whitespace() {
        let line = r#"   ![a](x.png "cap")   "#;
        assert_eq!(parse_figure_line(line), Some(("a", "x.png", "cap".to_string())));
    }

    #[test]
    fn rejects_image_without_title() {
        assert_eq!(parse_figure_line("![a](x.png)"), None);
    }

    #[test]
    fn rejects_inline_image_in_paragraph() {
        let line = r#"See ![a](x "c") for details."#;
        assert_eq!(parse_figure_line(line), None);
    }

    #[test]
    fn rejects_non_image_line() {
        assert_eq!(parse_figure_line("# Heading"), None);
        assert_eq!(parse_figure_line("just text"), None);
    }

    #[test]
    fn expand_figures_md_adds_italic_caption() {
        let src = "Intro.\n\n![diag](foo.png \"Figure 1: the flow\")\n\nMore.\n";
        let got = expand_figures_md(src);
        assert!(got.contains("![diag](foo.png)"), "got: {got}");
        assert!(got.contains("*Figure 1: the flow*"), "got: {got}");
    }

    #[test]
    fn expand_figures_md_escapes_asterisks_in_caption() {
        let src = "![a](x.png \"see the * algorithm\")\n";
        let got = expand_figures_md(src);
        assert!(got.contains(r"*see the \* algorithm*"), "got: {got}");
    }

    #[test]
    fn expand_figures_md_leaves_plain_images_alone() {
        let src = "![a](x.png)\n";
        assert_eq!(expand_figures_md(src), src);
    }

    #[test]
    fn expand_figures_html_emits_figure_tag() {
        let src = "![diag](foo.png \"Figure 1\")\n";
        let got = expand_figures_html(src);
        assert!(got.contains("<figure>"), "got: {got}");
        assert!(got.contains(r#"<img src="foo.png" alt="diag">"#), "got: {got}");
        assert!(got.contains("<figcaption>Figure 1</figcaption>"), "got: {got}");
        assert!(got.contains("</figure>"), "got: {got}");
    }

    #[test]
    fn expand_figures_html_escapes_special_chars_in_caption() {
        let src = "![a](x.png \"A & B <c>\")\n";
        let got = expand_figures_html(src);
        assert!(got.contains("A &amp; B &lt;c&gt;"), "got: {got}");
    }

    #[test]
    fn figure_expanders_are_idempotent_on_non_figures() {
        let src = "# Title\n\nParagraph with ![inline](x.png) image.\n";
        assert_eq!(expand_figures_md(src), src);
        assert_eq!(expand_figures_html(src), src);
    }
}
