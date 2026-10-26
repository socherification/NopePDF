# Roadmap

What NopePDF is aiming for, what is next, and how the plans evolved.
Shipped changes are listed in [CHANGELOG.md](CHANGELOG.md); detailed work plans live in
[docs/plans](docs/plans).

## Goals

- **A replacement for heavyweight PDF readers** for everyday reading: fast start,
  smooth scrolling, low memory use.
- **Simple but not bare:** no "fancy" features, but everything a reader expects
  (search, outline, thumbnails, links, tabs, printing).
- **Portable:** Windows, macOS and Linux from one code base. Platform-specific code stays
  behind small, per-platform modules; a feature designed for one OS only is the exception
  and must say why.
- **Respect the user's files:** never lock or modify the PDF being read.
- **Multilingual:** the UI follows the system language and can be switched in the app.

## Next

| Item | Status | Plan |
|---|---|---|
| Headless command-line printing (`nopepdf-cli --print`), printer listing, duplex and copies, document info and page rendering for scripts and AI agents; CUPS backend for macOS/Linux | Planned | [headless-printing](docs/plans/headless-printing.md) |
| Multilingual UI: English and Polish first, system language by default, switchable in the app, new languages by adding a translation file | Planned | [localization](docs/plans/localization.md) |

## Backlog

- Tiled rendering for very high zoom (above ~800%), so only the visible part of a page is
  rendered.
- Printing from the GUI on macOS and Linux (can reuse the CUPS backend from the
  headless-printing plan).
- Build and test on macOS and Linux; CI for all three platforms.
- Release automation (GitHub Actions building the installer and portable zip).
- Code signing for the Windows installer (avoids the SmartScreen warning).

## Ideas (not decided)

- Indicator when the open file was deleted or moved on disk (today the document simply
  stays readable from memory).
- Forms and annotations: low priority, NopePDF is mainly for reading.

## Plan history

### 2026-10-26: multilingual UI
- Decision: the UI becomes multilingual (it was Polish only). English is the source and
  fallback language, Polish the first translation.
- Lightweight approach chosen: TOML translation files embedded in the binary and a small
  in-house lookup with per-language plural rules, instead of Fluent/ICU.

### 2026-10-26: headless printing plan
- Request: let scripts and AI agents print a PDF without a dialog and get a reliable
  result (exit code), as a deterministic replacement for driving a GUI reader.
- Planned as a separate console executable (`nopepdf-cli`) sharing the code base, because
  a Windows GUI-subsystem program does not return its exit code to PowerShell or cmd.
- Requirement added the same day: design it for all platforms (Windows GDI backend,
  CUPS backend for macOS/Linux) rather than Windows only.

### 2026-10-17: initial scope and technology
- Stack: Rust + PDFium (`pdfium-render`) + egui/eframe. Rejected: Electron/Tauri with
  pdf.js (slow and memory-hungry on heavy PDFs), C++/Qt (heavy to build and ship),
  MuPDF (AGPL; kept as a fallback if PDFium proves too slow).
- Smoothness by architecture: rendering on a worker thread with visible pages first,
  stale jobs dropped, the old texture scaled as a preview until the sharp render arrives,
  textures of pages far from the viewport freed, tiles at high zoom (tiles still to do).
- v1 scope: reading features listed in the README. Printing was moved into v1 at the
  user's request; forms and annotations were deprioritized because the main use is reading.
- Rendering backend: eframe's `glow` (OpenGL) instead of `wgpu`. Measured on Windows:
  ~130 MB RAM / 7.5 MB exe with glow versus ~355 MB / 35 MB with wgpu.
- After v0.1: single-window tabs, no file locking and reload-on-change (v0.2.0),
  installer and icon.
- License: MIT. All dependencies (egui, pdfium-render, PDFium and the libraries built
  into it) are under permissive licenses.
