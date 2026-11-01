//! End-to-end tests of the `nopepdf-cli` executable: output, JSON and exit codes.
//! Printing goes only to "Microsoft Print to PDF" with `--output`, never to a physical
//! printer, and never to the default printer.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nopepdf-cli")).args(args).output().expect("run nopepdf-cli")
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn json(o: &Output) -> Value {
    serde_json::from_slice(&o.stdout)
        .unwrap_or_else(|e| panic!("bad JSON ({e}): {}", String::from_utf8_lossy(&o.stdout)))
}

/// The sample document, or None (test skipped) when it or PDFium is missing.
fn sample() -> Option<String> {
    let pdf = root().join("testdata/attention.pdf");
    let pdfium = root().join("vendor/pdfium");
    if pdf.exists() && pdfium.exists() {
        Some(pdf.to_string_lossy().into_owned())
    } else {
        eprintln!("skipped: needs testdata/attention.pdf and vendor/pdfium");
        None
    }
}

fn temp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(name);
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn help_version_and_usage_errors() {
    let o = cli(&["--help"]);
    assert_eq!(code(&o), 0);
    assert!(String::from_utf8_lossy(&o.stdout).contains("Exit codes"));
    let o = cli(&["--version", "--json"]);
    assert_eq!(json(&o)["version"], env!("CARGO_PKG_VERSION"));
    for args in [&[][..], &["print"], &["info", "a.pdf", "--copies", "2"], &["bogus"]] {
        assert_eq!(code(&cli(args)), 1, "{args:?}");
    }
    let o = cli(&["bogus", "--json"]);
    assert_eq!(json(&o)["exit_code"], 1);
}

#[test]
fn file_errors() {
    let missing = root().join("testdata/no-such-file.pdf");
    assert_eq!(code(&cli(&["info", &missing.to_string_lossy()])), 2);
    if sample().is_none() {
        return;
    }
    let not_pdf = root().join("Cargo.toml");
    let o = cli(&["info", &not_pdf.to_string_lossy(), "--json"]);
    assert_eq!(code(&o), 3);
    assert!(json(&o)["error"].as_str().unwrap().contains("not a valid PDF"));
}

#[test]
fn info_reports_pages_and_metadata() {
    let Some(pdf) = sample() else { return };
    let o = cli(&["info", &pdf, "--json"]);
    assert_eq!(code(&o), 0);
    let v = json(&o);
    let pages = v["pages"].as_u64().unwrap();
    assert!(pages >= 15);
    assert_eq!(v["page_sizes"].as_array().unwrap().len() as u64, pages);
    let text = String::from_utf8_lossy(&cli(&["info", &pdf]).stdout).into_owned();
    assert!(text.contains(&format!("Pages:     {pages}")), "{text}");
}

#[test]
fn render_writes_png() {
    let Some(pdf) = sample() else { return };
    let out = temp("nopepdf-cli-render.png");
    let o = cli(&["render", &pdf, "--page", "1", "--out", &out.to_string_lossy(), "--dpi", "72", "--json"]);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(json(&o)["width"], 612);
    assert_eq!(code(&cli(&["render", &pdf, "--page", "999", "--out", &out.to_string_lossy()])), 1);
}

#[cfg(windows)]
mod printing {
    use super::*;

    const PDF_PRINTER: &str = "Microsoft Print to PDF";

    fn page_count(file: &Path) -> u64 {
        let o = cli(&["info", &file.to_string_lossy(), "--json"]);
        assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
        json(&o)["pages"].as_u64().unwrap()
    }

    #[test]
    fn printers_lists_the_pdf_printer() {
        let o = cli(&["printers", "--json"]);
        assert_eq!(code(&o), 0);
        let list = json(&o);
        assert!(list.as_array().unwrap().iter().any(|p| p["name"] == PDF_PRINTER), "{list}");
    }

    #[test]
    fn prints_pages_and_copies_to_file() {
        let Some(pdf) = sample() else { return };
        let out = temp("nopepdf-cli-print.pdf");
        let o = cli(&[
            "print",
            &pdf,
            "--printer",
            PDF_PRINTER,
            "--pages",
            "1-2,5",
            "--copies",
            "2",
            "--output",
            &out.to_string_lossy(),
            "--json",
        ]);
        assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(json(&o)["pages"], "1-2,5");
        assert_eq!(page_count(&out), 6);
    }

    #[test]
    fn dry_run_prints_nothing() {
        let Some(pdf) = sample() else { return };
        let out = temp("nopepdf-cli-dry-run.pdf");
        let o = cli(&[
            "print",
            &pdf,
            "--printer",
            PDF_PRINTER,
            "--pages",
            "3-",
            "--output",
            &out.to_string_lossy(),
            "--dry-run",
        ]);
        assert_eq!(code(&o), 0);
        let text = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(text.starts_with("Would print") && text.contains("pages 3-"), "{text}");
        assert!(!out.exists());
    }

    #[test]
    fn print_exit_codes() {
        let Some(pdf) = sample() else { return };
        let out = temp("nopepdf-cli-errors.pdf");
        let out = out.to_string_lossy();
        let run = |extra: &[&str]| {
            let mut args = vec!["print", pdf.as_str(), "--output", &out];
            args.extend_from_slice(extra);
            code(&cli(&args))
        };
        assert_eq!(run(&["--printer", "No Such Printer 1234"]), 4);
        assert_eq!(run(&["--printer", PDF_PRINTER, "--pages", "0"]), 1);
        assert_eq!(run(&["--printer", PDF_PRINTER, "--pages", "1-999"]), 1);
        // The PDF printer cannot print two-sided.
        assert_eq!(run(&["--printer", PDF_PRINTER, "--duplex", "long"]), 5);
        let missing = root().join("testdata/no-such-file.pdf");
        assert_eq!(code(&cli(&["print", &missing.to_string_lossy(), "--printer", PDF_PRINTER])), 2);
    }
}
