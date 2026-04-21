use crate::MDReaderApp;
use eframe::egui;
use egui::{Color32, RichText};
use std::path::PathBuf;
use std::sync::mpsc;

// ── types ────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct UpdateAvailable {
    pub version: String,
    pub url: String,
}

pub enum UpdateState {
    Checking,
    Idle,
    Available(UpdateAvailable),
    Downloading(mpsc::Receiver<Result<PathBuf, String>>),
}

// ── version comparison ───────────────────────────────────────────────────────

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

// ── GitHub release check ─────────────────────────────────────────────────────

pub fn check_latest_release() -> Option<UpdateAvailable> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("MDReader/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;
    let resp: serde_json::Value = client
        .get("https://api.github.com/repos/ophiocus/MDReader/releases/latest")
        .send()
        .ok()?
        .json()
        .ok()?;
    let tag = resp["tag_name"]
        .as_str()?
        .trim_start_matches('v')
        .to_string();
    if !is_newer(&tag, env!("CARGO_PKG_VERSION")) {
        return None;
    }
    let url = resp["assets"]
        .as_array()?
        .iter()
        .find(|a| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .ends_with(".msi")
        })?["browser_download_url"]
        .as_str()?
        .to_string();
    Some(UpdateAvailable { version: tag, url })
}

// ── download + install ───────────────────────────────────────────────────────

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
    std::fs::write(&path, &bytes).map_err(|e| format!("Failed to write MSI: {e}"))?;

    let msi_str = path.to_string_lossy();
    std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Start-Process msiexec -ArgumentList '/i \"{msi_str}\" /passive /norestart' -Verb RunAs"
            ),
        ])
        .spawn()
        .map_err(|e| format!("Failed to launch installer: {e}"))?;

    Ok(path)
}

// ── UI rendering ─────────────────────────────────────────────────────────────

impl MDReaderApp {
    /// Clickable version label in the bottom-left corner.
    /// Clicking triggers a manual update check.
    pub fn render_version_button(&mut self, ui: &mut egui::Ui) {
        let version_text = format!("v{}", env!("CARGO_PKG_VERSION"));
        let label = RichText::new(&version_text).weak().size(11.0);
        let response = ui.add(egui::Label::new(label).sense(egui::Sense::click()));
        if response.clicked() && matches!(self.update_state, UpdateState::Idle) {
            self.update_error = None;
            self.update_state = UpdateState::Checking;
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(check_latest_release());
            });
            self.update_rx = Some(rx);
        }
        response.on_hover_text("Click to check for updates");
    }

    pub fn render_update_status(&mut self, ui: &mut egui::Ui) {
        match &self.update_state {
            UpdateState::Checking => {
                ui.label(RichText::new("checking for updates...").weak().size(11.0));
                ui.separator();
            }
            UpdateState::Available(avail) => {
                let label = RichText::new(format!(
                    "v{} available — click to install",
                    avail.version
                ))
                .size(11.0)
                .color(Color32::from_rgb(80, 210, 110));
                if ui
                    .add(egui::Label::new(label).sense(egui::Sense::click()))
                    .clicked()
                {
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
                            std::process::exit(0);
                        }
                        Err(e) => {
                            self.update_error = Some(format!("Update failed: {e}"));
                            self.update_state = UpdateState::Idle;
                        }
                    }
                } else {
                    ui.label(RichText::new("downloading update...").weak().size(11.0));
                }
                ui.separator();
            }
            UpdateState::Idle => {
                if let Some(ref err) = self.update_error {
                    ui.colored_label(Color32::from_rgb(255, 80, 80), err);
                    ui.separator();
                }
            }
        }
    }
}
