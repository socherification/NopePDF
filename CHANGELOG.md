# Changelog

All notable changes to NopePDF are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project
uses [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `scripts/fetch-pdfium.ps1` and `scripts/fetch-pdfium.sh` download a pinned PDFium build
  and verify its checksum, so PDFium binaries are no longer kept in the repository.
- README, roadmap, changelog and contributor/agent guide (`AGENTS.md`).

### Changed
- Portable build and installer now ship NopePDF's `LICENSE.txt` and the full set of PDFium
  third-party licenses in a `licenses` folder (previously only the pdfium-binaries build
  script license was included, under a misleading name).
- The printing test is skipped when the local sample file is missing.
- Code formatted with rustfmt (`rustfmt.toml`); `.gitattributes` keeps shell scripts LF.

## [0.2.0] - 2026-10-17

### Added
- Single window with tabs: PDFs opened from Explorer, "Open with" or the command line are
  added as tabs to the running instance, which comes to the front. Starting NopePDF again
  without a file just activates the existing window.
- Files are read fully into memory, so an open PDF is never locked on disk.
- Change detection: when the open file changes on disk, a bar offers *Reload (F5)*,
  *Always reload this file* and *Ignore*; changed background tabs show a ● marker.
- F5 reloads the current file.
- Windows installer (Inno Setup): per-user install without admin rights, PDF viewer
  registration, Start menu entry, optional desktop icon.
- Application icon embedded in the executable.

### Fixed
- Empty or directory command-line arguments no longer produce an "unable to open" error.
- The error screen uses the theme background instead of black.
- Invalid or damaged files show a readable message instead of an internal PDFium error.

## [0.1.0] - 2026-10-17

### Added
- First version: PDFium rendering on a background thread with visible pages first,
  continuous scrolling, zoom, fit width / fit page, rotation.
- Search with highlighted hits, text selection and copy, double-click word selection.
- Sidebar with outline and thumbnails; internal and external links.
- Per-file page and zoom memory, recent files on the start screen.
- Dark mode and inverted page colors.
- Password-protected PDFs, full screen, go to page.
- Printing on Windows through the system print dialog (rendered at up to 300 DPI).

[Unreleased]: https://github.com/socherification/NopePDF/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/socherification/NopePDF/releases/tag/v0.2.0
[0.1.0]: https://github.com/socherification/NopePDF/releases/tag/v0.1.0
