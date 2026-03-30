#![windows_subsystem = "windows"]

use eframe::egui;
use egui::{Color32, RichText, ScrollArea};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
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
    Downloading(mpsc::Receiver<Result<PathBuf, String>>),
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

fn download_and_install(url: &str, version: &str) -> Result<PathBuf, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("MDReader/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let bytes = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .map_err(|e| format!("Download failed: {e}"))?;

    let path = std::env::temp_dir().join(format!("MDReader-{version}.msi"));
    std::fs::write(&path, &bytes)
        .map_err(|e| format!("Failed to write MSI: {e}"))?;

    // Launch installer elevated via powershell Start-Process -Verb RunAs.
    // Plain `msiexec` from a non-elevated process silently fails on
    // per-machine installs because /passive suppresses the UAC prompt.
    let msi_str = path.to_string_lossy();
    std::process::Command::new("powershell")
        .args([
            "-NoProfile", "-Command",
            &format!(
                "Start-Process msiexec -ArgumentList '/i \"{msi_str}\" /passive /norestart' -Verb RunAs"
            ),
        ])
        .spawn()
        .map_err(|e| format!("Failed to launch installer: {e}"))?;

    Ok(path)
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

/// Collect all markdown file paths from a tree in depth-first order,
/// paired with their nesting depth (0 = root level).
fn collect_md_entries(nodes: &[FileNode], depth: usize) -> Vec<(PathBuf, usize)> {
    let mut out = Vec::new();
    for node in nodes {
        match &node.kind {
            NodeKind::File => out.push((node.path.clone(), depth)),
            NodeKind::Dir(children) => out.extend(collect_md_entries(children, depth + 1)),
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
  .toc {{ margin-bottom: 2em; }}
  .toc h2 {{ border-bottom: 2px solid #e0e0e0; padding-bottom: .3em; }}
  .toc ul {{ padding-left: 1.5em; list-style: none; }}
  .toc .toc-root {{ padding-left: 0; }}
  .toc li {{ margin: 0.3em 0; }}
  .toc a {{ color: #1a73e8; text-decoration: none; }}
  .toc a:hover {{ text-decoration: underline; }}
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

/// Build a path→anchor lookup from the collected entries.
/// Used to rewrite relative `[text](path.md)` links to `#doc-N`.
fn build_path_map(entries: &[(PathBuf, usize)]) -> std::collections::HashMap<PathBuf, String> {
    entries
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (p.clone(), format!("doc-{i}")))
        .collect()
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

/// Export all markdown files in the tree as a single PDF with a smart
/// hierarchical table of contents.  Every transformation is wrapped in a
/// try/default pattern: if detection or rewriting fails for a given file
/// the original content is used verbatim.
fn export_pdf(tree: &[FileNode], root_name: &str) -> Result<PathBuf, String> {
    let entries = collect_md_entries(tree, 0);
    if entries.is_empty() {
        return Err("No markdown files to export.".to_string());
    }

    // ── 1. pick save location ──────────────────────────────────────────────
    let default_name = format!("{root_name}.pdf");
    let pdf_path = rfd::FileDialog::new()
        .add_filter("PDF", &["pdf"])
        .set_file_name(&default_name)
        .save_file()
        .ok_or_else(|| "Export cancelled.".to_string())?;

    // ── 2. build path→anchor map (best-effort canonicalization) ────────────
    let canon_entries: Vec<(PathBuf, usize)> = entries
        .iter()
        .map(|(p, d)| (p.canonicalize().unwrap_or_else(|_| p.clone()), *d))
        .collect();
    let path_map = build_path_map(&canon_entries);

    // ── 3. process each file ───────────────────────────────────────────────
    let mut opts = comrak::Options::default();
    opts.extension.table = true;
    opts.extension.strikethrough = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;

    struct TocEntry {
        anchor: String,
        display: String,
        depth: usize,
        is_nav: bool,
    }

    let mut toc: Vec<TocEntry> = Vec::new();
    let mut sections = String::new();

    for (i, (path, depth)) in entries.iter().enumerate() {
        let raw_content = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

        let anchor = format!("doc-{i}");

        // Try: for README.md files, use the parent directory's cleaned name
        // as the display; for regular files, extract the first # heading.
        // Default: file stem → title case.
        let is_readme = path.file_name()
            .map(|n| n.to_string_lossy().to_lowercase() == "readme.md")
            .unwrap_or(false);
        let display = if is_readme {
            // Try dir name cleaning; fall back to heading extraction
            path.parent()
                .and_then(|p| p.file_name())
                .map(|n| dir_display_name(&n.to_string_lossy()))
                .unwrap_or_else(|| display_name_for(&raw_content, path))
        } else {
            display_name_for(&raw_content, path)
        };

        // Try: detect nav-only README; default: false (treat as content)
        let nav = is_nav_only(&raw_content, path);

        toc.push(TocEntry {
            anchor: anchor.clone(),
            display: display.clone(),
            depth: *depth,
            is_nav: nav,
        });

        // Prepare content for this section.
        // Safety net: if any transformation loses >40% of the content length,
        // fall back to the raw markdown — better to have duplicate TOC entries
        // than mangled content.
        let processed = if nav {
            let intro = extract_intro(&raw_content);
            // Nav-only with no intro → will be skipped below
            intro
        } else {
            let stripped = strip_inline_toc(&raw_content);
            let file_dir = path.parent().unwrap_or(path);
            let rewritten = rewrite_md_links(&stripped, file_dir, &path_map);

            // Safety net: if we lost too much content, use raw
            if rewritten.len() < raw_content.len() * 60 / 100 {
                raw_content.clone()
            } else {
                rewritten
            }
        };

        // Skip entirely empty sections (nav READMEs with no intro prose)
        if processed.trim().is_empty() {
            continue;
        }

        // Strip the first `# heading` line from the content to avoid a
        // duplicate title (we already inject our own <h1>).
        let final_content = {
            let mut lines = processed.lines();
            let mut skipped = false;
            let mut out = String::new();
            for line in &mut lines {
                if !skipped && line.trim().starts_with("# ") && !line.trim().starts_with("##") {
                    skipped = true;
                    continue;
                }
                out.push_str(line);
                out.push('\n');
            }
            if skipped { out } else { processed }
        };

        // Page break before each section except the first
        if !sections.is_empty() {
            sections.push_str("<div style=\"page-break-before:always\"></div>\n");
        }
        sections.push_str(&format!(
            "<h1 id=\"{anchor}\" style=\"margin-top:0.5em\">{display}</h1>\n"
        ));
        sections.push_str(&comrak::markdown_to_html(&final_content, &opts));
    }

    // ── 4. build hierarchical TOC ──────────────────────────────────────────
    let mut toc_html = String::from(
        "<nav class=\"toc\">\n<h2>Table of Contents</h2>\n<ul class=\"toc-root\">\n",
    );
    let mut current_depth: Option<usize> = None;
    let mut open_uls = 0u32;

    for entry in &toc {
        // Skip nav-only entries that produced no content
        if entry.is_nav {
            // Still include as a group heading if it has a nice name
            // but don't make it a link — it has no rendered content worth jumping to
        }

        let target_depth = entry.depth;

        match current_depth {
            None => {
                current_depth = Some(target_depth);
            }
            Some(cd) => {
                if target_depth > cd {
                    for _ in 0..(target_depth - cd) {
                        toc_html.push_str("<ul>\n");
                        open_uls += 1;
                    }
                } else if target_depth < cd {
                    for _ in 0..(cd - target_depth) {
                        toc_html.push_str("</ul>\n");
                        if open_uls > 0 {
                            open_uls -= 1;
                        }
                    }
                }
                current_depth = Some(target_depth);
            }
        }

        if entry.is_nav {
            toc_html.push_str(&format!(
                "  <li><strong>{}</strong></li>\n",
                entry.display
            ));
        } else {
            toc_html.push_str(&format!(
                "  <li><a href=\"#{}\">{}</a></li>\n",
                entry.anchor, entry.display
            ));
        }
    }

    // Close any remaining nested <ul>s
    for _ in 0..open_uls {
        toc_html.push_str("</ul>\n");
    }
    toc_html.push_str("</ul>\n</nav>\n<div style=\"page-break-after:always\"></div>\n");

    let full_body = format!("{toc_html}{sections}");
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
    update_rx: mpsc::Receiver<Option<UpdateAvailable>>,
    /// Current state of the update workflow.
    update_state: UpdateState,
    /// Drag-reorder state: (parent path, source index within siblings).
    drag_reorder: Option<(PathBuf, usize)>,
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
            update_rx: rx,
            update_state: UpdateState::Checking,
            drag_reorder: None,
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
                            let (tx, rx) = mpsc::channel();
                            std::thread::spawn(move || {
                                let result = download_and_install(&url, &version);
                                let _ = tx.send(result);
                            });
                            self.update_state = UpdateState::Downloading(rx);
                        }
                        ui.separator();
                    }
                    UpdateState::Downloading(rx) => {
                        if let Ok(result) = rx.try_recv() {
                            match result {
                                Ok(_) => {
                                    // Installer launched — exit so it can replace the binary.
                                    std::process::exit(0);
                                }
                                Err(e) => {
                                    self.status_msg = Some(format!("✘ Update failed: {e}"));
                                    self.update_state = UpdateState::Idle;
                                }
                            }
                        } else {
                            ui.label(RichText::new("⬇ downloading update…").weak().size(11.0));
                        }
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
