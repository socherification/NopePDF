# NopePDF

A lightweight, fast PDF reader. Opens instantly, scrolls smoothly, stays out of your way.

NopePDF is built for reading: no accounts, no cloud, no upsell panels, and it never locks
the file you have open. It uses [PDFium](https://pdfium.googlesource.com/pdfium/) (the PDF
engine from Chromium) for rendering and [egui](https://github.com/emilk/egui) for the UI,
and is written in Rust so the same code base can run on Windows, macOS and Linux.

> **Status:** early (v0.2). Windows is the primary, tested platform. The code is portable,
> but macOS and Linux builds are not tested yet and printing there is not implemented.
> The user interface is currently in Polish; a multilingual UI (English first) is planned.

## Features

- Continuous scrolling, zoom (Ctrl+wheel, Ctrl+/−), fit width, fit page, rotation
- Tabs: every PDF opened from Explorer or the command line joins the running window
- Search with highlighted hits
- Text selection and copy, double-click selects a word
- Sidebar with the document outline and page thumbnails
- Internal and external links
- Remembers the page and zoom of every file; recent files on the start screen
- Dark mode, plus optional inverted page colors for night reading
- Files are read into memory, so the PDF on disk is never locked
  (you can overwrite, rename or delete it while it is open)
- When the file changes on disk, asks whether to reload (or always reloads it, handy with LaTeX)
- Password-protected PDFs
- Printing via the system print dialog (Windows)

Measured on Windows: ~7.5 MB executable plus ~7.5 MB PDFium, ~130 MB RAM with a typical
paper open, start-up with a file in under a second.

## Install (Windows)

Download from [Releases](https://github.com/socherification/NopePDF/releases):

- **Installer** `NopePDF-setup-<version>.exe`: installs for the current user (no admin
  rights needed) and registers NopePDF as a PDF viewer. Windows does not let apps make
  themselves the default, so the last page of the installer offers to open
  *Settings › Default apps* where you pick NopePDF for `.pdf`.
- **Portable** folder: `nopepdf.exe` + `pdfium.dll`, run from anywhere.
  `scripts/register.ps1` registers a portable copy as a PDF viewer for the current user.

The installer is not code-signed yet, so SmartScreen shows an "unknown publisher" warning.

## Keyboard shortcuts

| Action | Keys |
|---|---|
| Open file | Ctrl+O |
| Close tab | Ctrl+W, Ctrl+F4 |
| Next / previous tab | Ctrl+Tab / Ctrl+Shift+Tab |
| Find, next / previous hit | Ctrl+F, F3 / Shift+F3 |
| Go to page | Ctrl+G |
| Zoom in / out | Ctrl++ / Ctrl+−, Ctrl+wheel |
| Fit page / actual size / fit width | Ctrl+0 / Ctrl+1 / Ctrl+2 |
| Rotate clockwise / counter-clockwise | Ctrl+R / Ctrl+Shift+R |
| Toggle sidebar | Ctrl+B |
| Copy selected text | Ctrl+C |
| Print | Ctrl+P |
| Reload from disk | F5 |
| Full screen | F11 |
| Leave full screen / close search / clear selection | Esc |
| Scroll | Arrows, PgUp/PgDn, Space/Shift+Space, Home/End |

## Building from source

Requirements: a recent stable [Rust](https://rustup.rs) toolchain (edition 2024).

PDFium is not stored in the repository. The fetch scripts download a pinned build from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) into
`vendor/pdfium` and verify its SHA-256 (see `scripts/pdfium.lock`).

### Windows

```powershell
.\scripts\build.ps1
```

This fetches PDFium if needed, builds a release, assembles the portable folder in
`dist\NopePDF\` and, if [Inno Setup 6](https://jrsoftware.org/isinfo.php) is installed
(`winget install JRSoftware.InnoSetup`), the installer in `dist\`.

Both the MSVC and the GNU Rust toolchains work. With the GNU toolchain
(`x86_64-pc-windows-gnu`) you also need MinGW binutils on `PATH`, e.g.
`winget install BrechtSanders.WinLibs.POSIX.UCRT`; `build.ps1` finds a WinLibs install
automatically.

### macOS / Linux (untested)

```sh
scripts/fetch-pdfium.sh
cargo build --release
```

At run time NopePDF looks for the PDFium library next to the executable, then on the
system library path. Debug builds also look in `vendor/pdfium/bin` and `vendor/pdfium/lib`
of the source tree, so `cargo run` works; for a release build, put the library next to
the executable (`build.ps1` does this in `dist\NopePDF\`).

### Tests

```sh
cargo test
```

The printing test needs the *Microsoft Print to PDF* printer and a sample file at
`testdata/attention.pdf` (any PDF with 3+ pages; sample PDFs are not committed). It is
skipped when the file is missing.

## Settings

Stored as TOML in the user config directory (`%APPDATA%\NopePDF\settings.toml` on Windows):
theme, inverted page colors, sidebar state and the per-file history (page, zoom, fit mode).

## Roadmap

See [ROADMAP.md](ROADMAP.md) for what is planned and [CHANGELOG.md](CHANGELOG.md) for what
changed. Detailed work plans live in [docs/plans](docs/plans).

## License

NopePDF is released under the [MIT License](LICENSE).

Binary releases include PDFium and the libraries built into it (FreeType, libjpeg-turbo,
OpenJPEG, libpng, zlib, ICU, HarfBuzz, Little CMS and others), all under permissive
licenses. Their license texts ship in the `licenses` folder next to the executable.
