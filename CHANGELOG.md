# Changelog

All notable changes to MDReader are documented here.

---

## [0.2.0] — 2026-03-23

#### Added
- **Windows MSI installer** via `cargo-wix` — produces a standard Windows
  Installer package with Start Menu shortcut, per-machine installation, and
  automatic major-upgrade handling
- **Explorer context menu** — two registry components registered by the MSI:
  - `Directory\shell\MDReader` — "Open with MD Reader" appears when
    right-clicking any folder in Explorer; passes the folder path as `%1`
  - `Directory\Background\shell\MDReader` — same entry for right-clicking
    the background inside an open folder; passes `%V`
  Both entries are cleanly removed on uninstall
- **CLI argument support** — `main()` reads `argv[1]`; if it is a valid
  directory path the app overrides `config.root_path` and saves, enabling
  the context menu integration to open the app rooted at the clicked folder
- **Auto-update checker** — on startup a background thread queries the
  GitHub Releases API (`/repos/ophiocus/MDReader/releases/latest`).
  If the latest tag is newer than `CARGO_PKG_VERSION`, the status bar shows
  a green "↑ vX.Y available — click to install" label. Clicking it
  downloads the `.msi` to `%TEMP%` and runs `msiexec /passive /norestart`,
  then exits — the update completes with a brief Windows Installer progress
  dialog and no further interaction. States: `Checking → Available |
  Idle → Downloading`
- **`reqwest 0.12`** dependency added (blocking, json, rustls-tls features)
  for both the update check and MSI download
- **README Installation section** — covers Windows MSI, Windows portable,
  Linux `.deb`, macOS `.app`, and auto-update behaviour
- **GitHub Actions** — `build-windows` job now also installs `cargo-wix`,
  builds the MSI (`cargo wix --nocapture`), uploads it as
  `mdreader-windows-msi`, and includes `*.msi` in the GitHub Release

---

## [0.1.0] — 2026-03-22

### Session 1 — Initial build

**Zero-shot implementation of the full application in a single pass.**

#### Added
- `src/main.rs` — complete single-file Rust application (~500 LOC)
- Left sidebar file tree rooted at a configurable directory; recursively
  scans for `.md` files, omits empty directories, sorts folders-first then
  alphabetically at every level, with unlimited nesting depth
- Right panel renders markdown via `egui_commonmark` (CommonMark spec):
  headings, tables, blockquotes, inline code, bullet lists, bold/italic
- Fenced code blocks with language tags (` ```rust `, ` ```python `,
  ` ```bash `, etc.) rendered with full syntax colouring via the `syntect`
  backend bundled inside `egui_commonmark`
- Menu bar — **File** (Settings, Refresh tree, Quit) and **View**
  (Light/Dark mode toggle, zoom presets 75 / 100 / 125 / 150 / 200 %,
  Reset zoom)
- Bottom status bar: drag the `%` label left/right to continuously adjust
  UI scale from 25 % to 400 %; zoom persisted on drag release
- Settings modal with native Windows folder picker (`rfd`)
- Config persisted to `%APPDATA%\MDReader\config.json` (root path, theme,
  zoom); defaults to Documents folder on first run
- No-console binary via `#![windows_subsystem = "windows"]`
- `Cargo.toml` with `eframe 0.28`, `egui_commonmark 0.17`
  (`better_syntax_highlighting` feature), `serde_json`, `dirs`, `rfd`

---

### Session 2 — Fixes, polish, icon, branding, GitHub

#### Fixed
- **Haywire zoom drag** — calling `set_pixels_per_point` every drag frame
  shifted the logical coordinate system mid-gesture, causing `drag_delta()`
  to diverge wildly on the next frame (runaway feedback loop).
  Fix: accumulate drag into `drag_zoom: Option<f32>` without touching `ppp`.
  Commit the value with a single `set_pixels_per_point` call on
  `drag_stopped()`. The `%` label shows a live preview during the drag and
  the correct committed value at all other times — including immediately
  after a View-menu zoom preset is applied. *(commit `a0a5afe`)*

#### Added — icon pipeline
- Custom app icon generated externally and saved to `assets/`
- Square variant (`assets/icon_sqr.png`) selected as the definitive icon
- `ffmpeg` used to produce a multi-resolution `assets/icon.ico`
  (16 × 16, 32 × 32, 48 × 48, 256 × 256) from the PNG source
- `build.rs` added — uses `winres` to embed the `.ico` into the `.exe` at
  compile time; guarded by `CARGO_CFG_TARGET_OS == "windows"` so Linux and
  macOS builds are unaffected
- Windows icon cache flushed post-build (`ie4uinit.exe`) to surface the
  new icon in Explorer and the taskbar immediately

#### Added — branding & docs
- `assets/md_reader_logo.png` — branded card used as README header seal
- `assets/sample_1.png` — annotated screenshot illustrating tree population,
  deep nesting, and the markdown renderer
- `README.md` — full project documentation: features, interface walkthrough,
  build instructions, configuration reference, dependency table, roadmap
- `.gitignore` — excludes `target/`, compiled artifacts, and editor noise

#### Added — UI polish
- Sidebar default width increased to 300 px (min 200 px)
- `---` horizontal rules now visible in dark mode (stroke override on
  `widgets.noninteractive.bg_stroke`)
- Window title reflects the active root folder:
  `MD Reader — <folder name>`
- Sidebar header changed from generic `FILES` label to the root folder
  basename, styled as a bold TOC title

#### Added — infrastructure
- Git repository initialised; all commits signed with author
  `ophiocus <csantanad@gmail.com>`
- Remote `origin` created at **https://github.com/ophiocus/MDReader**
  and all commits pushed

#### Added — new features (end of session 2)
- **Root folder as TOC title** — sidebar displays the root directory
  basename as a bold header, making the panel read as a named table of
  contents for the documentation set
- **Export as PDF** — File › Export as PDF… converts the current markdown
  file to a print-ready PDF:
  1. `comrak` renders the markdown to HTML with a clean CSS stylesheet
     (paper-white, max-width 860 px, styled tables and code blocks)
  2. A temp HTML file is written to `%TEMP%`
  3. Chrome or Edge is launched headless (`--headless=new --print-to-pdf`)
     to produce the final PDF
  4. Result path (or error) is shown in the status bar; click to dismiss
  - Menu item is greyed out when no file is open
  - `comrak 0.28` added as a dependency

- **GitHub Actions release workflow** (`.github/workflows/release.yml`) —
  triggers on version tags (`v*`), three parallel jobs:

  | Job | Runner | Output |
  |---|---|---|
  | `build-windows` | `windows-latest` | `mdreader.exe` (icon embedded) |
  | `build-linux` | `ubuntu-latest` | `.deb` via `cargo-deb` |
  | `build-macos` | `macos-latest` | `.app` bundle via `cargo-bundle`, zipped |

  A final `release` job collects all artifacts and publishes them to a
  GitHub Release with auto-generated notes.

- `Cargo.toml` gains `[package.metadata.deb]` and
  `[package.metadata.bundle]` sections for the packaging tools

---

## Commits

| Hash | Description |
|---|---|
| `771390f` | Initial commit: read-only Windows markdown viewer |
| `a0a5afe` | Fix haywire zoom drag and stale percent display |
| `a1cd283` | Embed custom app icon into Windows executable |
| `43a038b` | Replace icon with square variant (icon_sqr.png) |
| `2ee849e` | Add README, .gitignore, and retain source icon asset |
| `0fe87a2` | Add brand seal to README header |
| `cdbcb2d` | Sidebar wider, HR separators visible, title shows root folder |
| `2a20b33` | Add annotated interface screenshot to README |
| `13f0387` | Add PDF export, root folder TOC title, and sample screenshot |
| `380c4ee` | Add GitHub Actions release workflow for Windows, Linux, macOS |
