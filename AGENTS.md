# AGENTS.md

Guide for AI coding agents (and humans) working on NopePDF. Read README.md for what the
app does; this file covers how to work on it.

## Project in one paragraph

NopePDF is a lightweight PDF reader in Rust: eframe/egui 0.37 for the UI (OpenGL `glow`
backend), PDFium through `pdfium-render` 0.9 for parsing and rendering. Rendering, text
extraction and search run on one worker thread; the UI thread only lays out pages and
draws textures. Windows is the tested platform; the code must stay portable to macOS and
Linux.

## Layout

| Path | Purpose |
|---|---|
| `src/main.rs` | Startup: PDFium loading, fonts, single-instance check, window options |
| `src/app.rs` | `App`: tabs, toolbar, shortcuts, search bar, reload-on-change bar, welcome screen, theme |
| `src/view.rs` | `Doc`: per-document view state (layout, scroll, zoom, fit, rotation, selection, links, outline, thumbnails) |
| `src/worker.rs` | Worker thread owning PDFium; `Cmd`/`Job` in, `Event` out; render queue replaced every frame so stale jobs drop |
| `src/print.rs` | Printing (Windows GDI; other platforms return an error) |
| `src/instance.rs` | Single instance: file lock + localhost socket with a random token |
| `src/settings.rs` | TOML settings and per-file history in the user config dir |
| `scripts/` | `build.ps1` (release + portable + installer), `fetch-pdfium.*` (pinned PDFium download), `register.ps1` |
| `installer/nopepdf.iss` | Inno Setup script |
| `docs/plans/` | Work plans for upcoming changes |

## Build and test

- Windows: `.\scripts\build.ps1`. It fetches PDFium into `vendor/pdfium` when missing.
  For a quick check: `scripts\fetch-pdfium.ps1` once, then `cargo build` / `cargo test`.
- macOS/Linux: `scripts/fetch-pdfium.sh`, then `cargo build`.
- GNU toolchain on Windows needs MinGW binutils (WinLibs) on `PATH`; `build.ps1` adds it.
  When calling `cargo` directly from PowerShell, prepend the WinLibs `mingw64\bin` dir.
- `cargo test` must pass on a fresh clone. Tests that need local sample files
  (`testdata/`, not committed) must skip when the file is missing.
- Run `cargo fmt` before committing (style in `rustfmt.toml`: 120 columns, compact
  heuristics). `cargo clippy` should not report new warnings; a few existing style
  warnings (collapsible `if`s and similar) are known.

## Rules

- **Portability first.** Design every feature for Windows, macOS and Linux. Put OS-specific
  code behind small `#[cfg(...)]` modules with one shared interface (see `print.rs`). If
  something is Windows-only, say why in the code and in the plan.
- **Keep it light.** The app competes on start time, RAM and binary size. Avoid heavy
  dependencies (no `clap`, no web views, no async runtimes). Measure RAM and exe size
  before and after anything that could change them.
- **Do not switch eframe to `wgpu`** without re-measuring: `glow` uses ~130 MB RAM and a
  7.5 MB exe versus ~355 MB and 35 MB for `wgpu`.
- **Keep the eframe `clipboard` feature.** Default features are disabled; without it
  Ctrl+C silently stops working.
- **Never lock the user's file.** Documents are read into memory; keep it that way.
- **Language:** code, comments, docs, commit messages and the changelog are in English.
  The UI is meant to be multilingual (see `docs/plans/localization.md`). Until that lands,
  UI strings are hard-coded Polish: keep new ones consistent with the existing strings
  and collect them in one place where possible. Once `locales/` exists, never hard-code
  UI text: add the key to every language file, English first.
- **Never commit** `testdata/` (copyrighted sample PDFs), `vendor/pdfium/` (downloaded),
  `dist/` or `target/`.
- **Licenses:** NopePDF is MIT. Any new dependency must have a permissive license. Release
  packages must ship `LICENSE.txt` and the PDFium `licenses/` folder.

## Documentation upkeep

When you change behavior:

- Add an entry under `[Unreleased]` in `CHANGELOG.md`.
- Update `ROADMAP.md`: move finished items out of *Next*/*Backlog*, and add a dated entry
  to *Plan history* when a plan is made or changes direction.
- Larger features get a plan in `docs/plans/<name>.md` (goal, design, steps, status)
  before implementation; update its status as work progresses.
- Keep the shortcut table and feature list in `README.md` in sync with the code.

## Releasing

1. Bump `version` in `Cargo.toml`; move `[Unreleased]` entries in `CHANGELOG.md` under the
   new version with the date and update the compare links.
2. `.\scripts\build.ps1` → `dist\NopePDF-setup-<version>.exe` and `dist\NopePDF\`.
3. Tag `v<version>` and attach the installer and a zip of the portable folder to the
   GitHub release.
