# Changelog

All notable changes to MDReader are documented here.

---

## [0.3.0] — 2026-04-21

### Added

- **Bundled Chromium** — the Windows MSI installer now ships a pinned
  Chromium snapshot under `C:\Program Files\mdreader\chromium\`. PDF
  export works fully offline with no system Chrome/Edge required and no
  internet connection. Installer size grows from ~5 MB to ~180 MB as a
  result; auto-update delta is proportional. Resolution order:
  bundled → system Chrome → system Edge.
- **CLI markdown-to-PDF converter** — `mdreader.exe` is now both a GUI
  app and a headless CLI tool. New flags:
    - `--to-pdf <input>` — convert a single `.md` file or a directory
      tree to PDF. Output defaults to `<stem>.pdf` in the current
      working directory.
    - `-o` / `--output <file>` — explicit output path.
    - `--help` / `-h`, `--version` / `-V`.
  Uses the same comrak + Chromium pipeline as the GUI, so output is
  visually identical. Works from any `cmd.exe` or PowerShell prompt
  because the MSI adds the install directory to `PATH`. Attaches to the
  parent console via `AttachConsole(ATTACH_PARENT_PROCESS)` so stdout
  and exit codes behave correctly under `windows_subsystem = "windows"`.
- **`render_version_button()` + `render_update_status()`** — update
  checker extracted into `src/git_update.rs` with a UI matching
  ExamHelper. Click the version label in the status bar to manually
  re-check for updates; failed updates now surface in a dedicated
  red-text error region instead of the shared status message slot.

### Changed

- **Renderer switched from wgpu to glow** — eframe now uses the OpenGL
  backend rather than wgpu. The wgpu default spins up a full
  Vulkan/DX12 pipeline which was overkill for a 2D text viewer and
  produced noticeable idle CPU use. Glow drops idle CPU to near zero.
- **Idle repaint throttle extended to `UpdateState::Downloading`** —
  previously only `Checking` was covered, so the app would repaint at
  uncapped FPS while downloading an MSI update.

### Fixed

- **High idle CPU usage** — combined effect of the glow switch and the
  repaint-throttle fix. The static document viewer now goes fully idle
  after a paint when no user input is arriving.

---

## [0.2.5] — 2026-03-30

### Added

- **Drag-to-reorder sidebar items** — each node in the sidebar now has a
  drag handle (⠿). Drag any sibling to reorder it within its parent
  directory. The order is persisted to `%APPDATA%\MDReader\ordering.json`
  and applied on every tree build, including PDF export. This fixes the
  alphabetical ordering issue where "appendices" appeared before "part-1".

### Changed

- **Folder names visually prominent** — directory entries in the sidebar
  are now rendered with a larger font (13px), gold coloring, and the
  `dir_display_name` formatter (`part-1-vision` → "Part 1 — Vision").
  File entries remain compact.

---

## [0.2.4] — 2026-03-30

### Added

- **Desktop shortcut** — MSI installer now creates an "MD Reader"
  shortcut on the desktop with the app icon.

### Fixed

- **PDF export mangling content** — conservative TOC transforms with
  safety net. Nav detection only on READMEs, strict inline TOC strip,
  code-block-aware link rewriting, 40% content-loss fallback to raw.

- **Updater silent failure** — `msiexec` now launched elevated via
  `Start-Process -Verb RunAs` to trigger UAC for per-machine installs.

---

## [0.2.3] — 2026-03-30

### Changed

- **Smart hierarchical TOC** — PDF export now generates a nested table of
  contents that mirrors the directory structure instead of a flat list.
  All transformations use a try/default pattern so non-compliant content
  is never mangled:

  - **Heading extraction**: display names are pulled from the first
    `# heading` in each file; falls back to file stem → title case
  - **Directory name cleaning**: `part-1-vision` → "Part 1 — Vision",
    kebab-case → Title Case; falls back to raw name
  - **Nav-only README detection**: READMEs that are mostly `.md` link
    lists (>50% of content lines) are folded into the TOC as group
    headings with only their intro prose rendered; falls back to treating
    them as regular content
  - **Inline TOC stripping**: "In this part:" / "Table of Contents"
    sections followed by `.md` link lists are removed since the generated
    TOC replaces them; falls back to keeping the block
  - **Internal link rewriting**: `[text](relative/path.md)` links are
    rewritten to `#doc-N` anchors so cross-references work within the
    concatenated PDF; non-matching links are left untouched
  - **Hierarchical nesting**: TOC entries are nested with `<ul>` based
    on directory depth, producing a book-like structure

---

## [0.2.2] — 2026-03-30

### Changed

- **Export as PDF** now concatenates every markdown file in the sidebar
  tree into a single PDF with a clickable **Table of Contents** on the
  first page. Each file starts on a new page with its filename as an
  `<h1>` heading. Previously only the single selected file was exported.

### Fixed

- **Auto-updater silent failure** — `download_and_install` previously
  ran in a fire-and-forget thread with no error feedback; if the download
  or `msiexec` launch failed, the app stayed stuck on "downloading" or
  the click did nothing visible. Now the thread reports success/failure
  via a channel, the app only calls `process::exit(0)` after the
  installer has actually launched, and errors are surfaced in the status
  bar (e.g. "✘ Update failed: Download failed: …").
- **GitHub Actions Node.js 20 deprecation** — bumped `actions/checkout`,
  `upload-artifact`, and `download-artifact` from v4 → v5

---

## [0.2.1] — 2026-03-26

### Fixed

- **Context menu "Application not found" error** — the WiX template used
  `[INSTALLDIR]` as the path prefix for all four registry values (the
  `Directory\shell` and `Directory\Background\shell` command entries plus
  their icon declarations). `[INSTALLDIR]` is not a valid WiX/MSI property;
  it expanded to an empty string at install time, leaving the registry with
  bare `"mdreader.exe" "%1"` entries that Windows could not resolve.

  The correct property is `[Bin]`, the WiX `Directory` element whose
  `Id='Bin'` maps to `C:\Program Files\mdreader\bin\` — the actual
  location of the installed executable. All four references replaced.

  **Root cause in brief:** cargo-wix places the binary one level deeper
  than `APPLICATIONFOLDER` (in a `bin\` subdirectory) to allow the optional
  PATH component to add just that subdirectory. The auto-generated template
  comment does not make this distinction obvious. Any registry value
  referencing the executable must use `[Bin]`, not `[APPLICATIONFOLDER]`
  or any assumed `INSTALLDIR` alias.

- **Product name in Add/Remove Programs** was `mdreader` (Cargo crate name);
  corrected to `MD Reader`
- **MSI icon** not shown in Add/Remove Programs; wired `assets\icon.ico`
  to the `ARPPRODUCTICON` property
- **No help link** in Add/Remove Programs; `ARPHELPLINK` set to
  `https://github.com/ophiocus/MDReader`

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
