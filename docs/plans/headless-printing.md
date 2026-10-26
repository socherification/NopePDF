# Plan: headless command-line printing

- **Status:** planned, not started
- **Written:** 2026-10-26
- **Goal:** let scripts and AI agents print a PDF, list printers, read document info and
  render page previews without any window or dialog, with a reliable exit code. It is a
  deterministic replacement for driving a GUI reader and killing it after a timeout,
  which truncated print jobs.
- **Constraint:** portable. The command line, options, exit codes and output are the same
  on every platform; only a thin printing backend differs per OS.

## Why it does not work today

1. `main.rs` treats every argument as a file to open and always starts the window.
2. Printing always goes through the interactive system dialog (`print::show_dialog`).
3. Single-instance mode forwards the arguments to an already running window and exits.
4. The release build uses the Windows GUI subsystem. PowerShell and cmd do not wait for
   such a program and do not get its exit code, and it has no console for output.

The rendering and printing core already exists: `print::print_document` prints a page
range to any printer device context (auto-rotation, fit to page, rendering at up to 300 DPI).

## Design

### Separate console executable

A second binary, `nopepdf-cli`, built from the same crate on all platforms (console
subsystem on Windows). The GUI `nopepdf` stays unchanged. Shared code moves into a
library target (or shared modules) so both binaries use the same PDFium loading,
rendering and printing code. On macOS/Linux the CLI could live in the main binary, but one
name everywhere keeps scripts portable.

The CLI never touches single-instance mode and never opens a window.

### Commands

```
nopepdf-cli print <file.pdf> [--printer NAME] [--pages 1-3,7,10-] [--copies N]
                             [--duplex long|short|none] [--dry-run] [--json]
nopepdf-cli printers [--json]                 # lists printers, marks the default one
nopepdf-cli info <file.pdf> [--json]          # page count, page sizes, metadata
nopepdf-cli render <file.pdf> --page N --out page.png [--dpi 150]
```

- No `--printer` means the system default printer.
- `--pages` accepts ranges and lists and is validated against the page count;
  out-of-range pages are an error, not silently clamped.
- `--dry-run` prints what would be sent (printer, pages, copies, duplex) without printing.
- Argument parsing is hand-written (no `clap`) to keep the binary small.
- CLI messages and JSON output are always in English, independent of the UI language
  (see [localization](localization.md)), so scripts can rely on them.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (for `print`: the whole job was handed to the spooler) |
| 1 | Usage error (bad arguments, bad page range) |
| 2 | File not found or unreadable |
| 3 | Not a valid PDF, or password required |
| 4 | Printer not found |
| 5 | Requested option not supported by the printer (e.g. duplex) |
| 6 | Printing failed (render or spooler error) |

Exit 0 means the job is fully spooled, not physically printed. That is enough to stop
truncated jobs, because the process ends only after every page was handed over.

### Shared core (all platforms)

- `PrintOptions { printer: Option<String>, pages: Vec<RangeInclusive<usize>>, copies, duplex }`
- `info` and `render` use only PDFium (`render` needs PNG encoding through the `image`
  crate, which is already a dependency of `pdfium-render`).
- `--json` output through `serde_json` (new, small dependency).

### Windows backend

- Printer listing: `EnumPrintersW` + `GetDefaultPrinterW`.
- Settings: `OpenPrinterW` → `DocumentPropertiesW` (query size, then fetch the default
  `DEVMODEW`) → set `dmDuplex` (`DMDUP_VERTICAL` = long edge, `DMDUP_HORIZONTAL` = short
  edge) and `dmCopies` with the matching `dmFields` → merge back with
  `DocumentPropertiesW(DM_IN_BUFFER | DM_OUT_BUFFER)` → `CreateDCW("WINSPOOL", name, None, devmode)`.
- Check duplex support with `DeviceCapabilitiesW(DC_DUPLEX)`. If the driver ignores
  `dmCopies`, print the range N times.
- Requires the `Win32_Graphics_Printing` feature of the `windows` crate.
- Printing still rasterizes pages (current `print_document`). Printing to
  *Microsoft Print to PDF* therefore produces image-only PDFs; fine for tests, not for
  converting documents.

### macOS / Linux backend (CUPS)

- Print by handing the PDF file to CUPS: `lp -d NAME -n COPIES -P PAGES
  -o sides=two-sided-long-edge|two-sided-short-edge|one-sided file.pdf`. CUPS accepts PDF
  directly, so nothing is rasterized and output is sharp.
- Printer listing: `lpstat -e` (names) and `lpstat -d` (default).
- The same backend can later provide GUI printing on macOS/Linux (an egui dialog listing
  CUPS printers), which is a separate backlog item.

### Changes needed in existing code

- `print_pages` must report errors instead of skipping pages: today a page that fails to
  render is skipped with `continue`, and the results of `StretchDIBits`, `EndPage` and
  `EndDoc` are not checked. In headless mode this would silently produce incomplete prints.
- `main.rs`: move PDFium loading into the shared code.

## Steps

1. Shared code into a library target; add the `nopepdf-cli` binary, argument parsing,
   exit codes. Update `build.ps1` and the installer to ship it.
2. `print` with the default settings, `printers`, error reporting in `print_pages`;
   Windows and CUPS backends.
3. Duplex and copies (DEVMODE on Windows, `lp` options on CUPS).
4. `info`, `render`, `--json`, `--dry-run`.
5. Tests: argument and page-range parsing (unit), an end-to-end print to
   *Microsoft Print to PDF* checking the page count and exit codes.

## Estimate

About 600 lines in total. The Windows DEVMODE part (~150 lines of unsafe Win32 code) is
the riskiest piece; the CUPS backend is ~60–80 lines.

## Verification that needs a person

- Duplex and copies on a real printer (Windows).
- The CUPS backend on a macOS or Linux machine.
