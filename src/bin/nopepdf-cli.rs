//! nopepdf-cli: prints PDFs, lists printers, shows document info and renders pages without
//! any window or dialog, for scripts and AI agents. Messages and JSON output are always in
//! English, whatever the UI language, so scripts can rely on them.

use nopepdf::pdf::{self, OpenError};
use nopepdf::print::{self, Duplex, PrintError, PrintRequest};
use pdfium_render::prelude::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
nopepdf-cli: print and inspect PDF files without a window.

Usage:
  nopepdf-cli print <file.pdf> [--printer NAME] [--pages 1-3,7,10-] [--copies N]
                               [--duplex long|short|none] [--output FILE] [--dry-run]
  nopepdf-cli printers
  nopepdf-cli info <file.pdf>
  nopepdf-cli render <file.pdf> --page N --out page.png [--dpi 150]
  nopepdf-cli --help | --version

Options:
  --json        machine-readable output on stdout (any command; errors too)
  --password    password of a protected PDF
  --printer     printer name as shown by `printers`; default: the system default printer
  --pages       1-based pages and ranges, printed in page order; \"10-\" means page 10 to
                the end; default: all pages
  --copies      number of collated copies, 1-999 (default 1)
  --duplex      two-sided printing: long (flip on long edge), short or none;
                default: the printer's setting
  --output      write the job to FILE instead of printing it (Windows only; for printers
                such as \"Microsoft Print to PDF\")
  --dry-run     check the file, pages and printer and show what would be printed
  --page        page to render (1-based)
  --out         PNG file to write
  --dpi         render resolution, 10-1200 (default 150)

Exit codes:
  0  success (print: the whole job is in the print queue)
  1  usage error (bad arguments or page range)
  2  file not found or unreadable
  3  not a valid PDF, or password needed
  4  printer not found
  5  option not supported by the printer or this platform
  6  printing or rendering failed";

const EXIT_USAGE: u8 = 1;
const EXIT_FILE: u8 = 2;
const EXIT_PDF: u8 = 3;
const EXIT_PRINTER: u8 = 4;
const EXIT_UNSUPPORTED: u8 = 5;
const EXIT_FAILED: u8 = 6;

/// An error with the exit code it maps to.
#[derive(Debug, PartialEq)]
struct Failure {
    code: u8,
    message: String,
}

fn fail<T>(code: u8, message: impl Into<String>) -> Result<T, Failure> {
    Err(Failure { code, message: message.into() })
}

fn usage<T>(message: impl Into<String>) -> Result<T, Failure> {
    fail(EXIT_USAGE, format!("{} (see nopepdf-cli --help)", message.into()))
}

#[derive(Debug, PartialEq)]
enum Command {
    Print {
        file: PathBuf,
        printer: Option<String>,
        pages: Option<String>,
        copies: u16,
        duplex: Option<Duplex>,
        output: Option<PathBuf>,
        dry_run: bool,
    },
    Printers,
    Info {
        file: PathBuf,
    },
    Render {
        file: PathBuf,
        page: usize,
        out: PathBuf,
        dpi: f32,
    },
    Help,
    Version,
}

#[derive(Debug, PartialEq)]
struct Args {
    command: Command,
    json: bool,
    password: Option<String>,
}

/// Hand-written parser (no `clap`, to keep the binary small). Options take their value as
/// the next argument or after `=`.
fn parse_args(args: &[String]) -> Result<Args, Failure> {
    let mut positional = Vec::new();
    let mut opts: Vec<(String, Option<String>)> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            positional.extend(it.by_ref().cloned());
        } else if let Some(name) = a.strip_prefix("--") {
            let (name, inline) = match name.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (name.to_string(), None),
            };
            let takes_value = matches!(
                name.as_str(),
                "printer" | "pages" | "copies" | "duplex" | "output" | "page" | "out" | "dpi" | "password"
            );
            let flag = matches!(name.as_str(), "json" | "dry-run" | "help" | "version");
            let value = match (takes_value, flag, inline) {
                (true, _, Some(v)) => Some(v),
                (true, _, None) => match it.next() {
                    Some(v) => Some(v.clone()),
                    None => return usage(format!("--{name} needs a value")),
                },
                (false, true, None) => None,
                (false, true, Some(_)) => return usage(format!("--{name} takes no value")),
                (false, false, _) => return usage(format!("unknown option --{name}")),
            };
            if opts.iter().any(|(n, _)| *n == name) {
                return usage(format!("--{name} given twice"));
            }
            opts.push((name, value));
        } else if a == "-h" {
            opts.push(("help".into(), None));
        } else {
            positional.push(a.clone());
        }
    }

    let take = |name: &str, opts: &mut Vec<(String, Option<String>)>| -> Option<Option<String>> {
        let i = opts.iter().position(|(n, _)| n == name)?;
        Some(opts.remove(i).1)
    };
    let json = take("json", &mut opts).is_some();
    let password = take("password", &mut opts).flatten();
    if take("help", &mut opts).is_some() {
        return Ok(Args { command: Command::Help, json, password });
    }
    if take("version", &mut opts).is_some() {
        return Ok(Args { command: Command::Version, json, password });
    }

    let mut pos = positional.into_iter();
    let Some(cmd) = pos.next() else { return usage("missing command") };
    let mut file = || -> Result<PathBuf, Failure> {
        match pos.next() {
            Some(f) => Ok(PathBuf::from(f)),
            None => usage(format!("`{cmd}` needs a PDF file")),
        }
    };
    let command = match cmd.as_str() {
        "print" => {
            let file = file()?;
            let copies = match take("copies", &mut opts).flatten() {
                None => 1,
                Some(c) => match c.parse::<u16>() {
                    Ok(n @ 1..=999) => n,
                    _ => return usage(format!("--copies must be a number from 1 to 999, not '{c}'")),
                },
            };
            let duplex = match take("duplex", &mut opts).flatten().as_deref() {
                None => None,
                Some("long") => Some(Duplex::Long),
                Some("short") => Some(Duplex::Short),
                Some("none") => Some(Duplex::None),
                Some(d) => return usage(format!("--duplex must be long, short or none, not '{d}'")),
            };
            Command::Print {
                file,
                printer: take("printer", &mut opts).flatten(),
                pages: take("pages", &mut opts).flatten(),
                copies,
                duplex,
                output: take("output", &mut opts).flatten().map(PathBuf::from),
                dry_run: take("dry-run", &mut opts).is_some(),
            }
        }
        "printers" => Command::Printers,
        "info" => Command::Info { file: file()? },
        "render" => {
            let file = file()?;
            let page = match take("page", &mut opts).flatten() {
                None => return usage("render needs --page"),
                Some(p) => match p.parse::<usize>() {
                    Ok(n) if n >= 1 => n,
                    _ => return usage(format!("--page must be a page number, not '{p}'")),
                },
            };
            let Some(out) = take("out", &mut opts).flatten() else { return usage("render needs --out") };
            let dpi = match take("dpi", &mut opts).flatten() {
                None => 150.0,
                Some(d) => match d.parse::<f32>() {
                    Ok(v) if (10.0..=1200.0).contains(&v) => v,
                    _ => return usage(format!("--dpi must be a number from 10 to 1200, not '{d}'")),
                },
            };
            Command::Render { file, page, out: PathBuf::from(out), dpi }
        }
        other => return usage(format!("unknown command '{other}'")),
    };
    if let Some(extra) = pos.next() {
        return usage(format!("unexpected argument '{extra}'"));
    }
    if let Some((name, _)) = opts.first() {
        return usage(format!("--{name} does not apply to `{cmd}`"));
    }
    Ok(Args { command, json, password })
}

/// Parses 1-based pages and ranges ("1-3,7,10-") into sorted, unique 0-based indices.
/// Pages outside the document are an error, not silently dropped.
fn parse_pages(spec: &str, page_count: usize) -> Result<Vec<usize>, String> {
    let mut pages = Vec::new();
    for part in spec.split(',').map(str::trim) {
        let num = |s: &str| -> Result<usize, String> {
            match s.trim().parse::<usize>() {
                Ok(n) if n >= 1 => Ok(n),
                _ => Err(format!("'{part}' is not a page or range")),
            }
        };
        let (first, last) = match part.split_once('-') {
            Some((a, b)) if b.trim().is_empty() => (num(a)?, page_count),
            Some((a, b)) => (num(a)?, num(b)?),
            None => (num(part)?, num(part)?),
        };
        if first > last {
            return Err(format!("range '{part}' runs backwards"));
        }
        if last > page_count || first > page_count {
            return Err(format!("page {} is past the end of the document ({page_count} pages)", first.max(last)));
        }
        pages.extend(first - 1..last);
    }
    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

fn open_error(e: OpenError, file: &Path) -> Failure {
    let code = if matches!(e, OpenError::Read(_)) { EXIT_FILE } else { EXIT_PDF };
    let hint = if e == OpenError::NeedsPassword { " (use --password)" } else { "" };
    Failure { code, message: format!("{}: {}{hint}", file.display(), e.message()) }
}

fn print_error(e: PrintError, printer: &str) -> Failure {
    let code = match e {
        PrintError::PrinterNotFound => EXIT_PRINTER,
        PrintError::DuplexUnsupported | PrintError::Unsupported => EXIT_UNSUPPORTED,
        _ => EXIT_FAILED,
    };
    Failure { code, message: format!("cannot print to \"{printer}\": {}", e.message()) }
}

fn load_pdfium() -> Result<Pdfium, Failure> {
    pdf::load_pdfium().or_else(|e| fail(EXIT_FAILED, format!("{} {e}", nopepdf::i18n::tr("error.pdfium_missing"))))
}

fn duplex_name(d: Option<Duplex>) -> &'static str {
    match d {
        None => "printer default",
        Some(Duplex::None) => "none",
        Some(Duplex::Long) => "long",
        Some(Duplex::Short) => "short",
    }
}

/// Runs a command; returns the text or JSON to print on success.
fn run(args: Args) -> Result<Output, Failure> {
    let password = args.password.as_deref();
    match args.command {
        Command::Help => Ok(Output::text(USAGE)),
        Command::Version => Ok(Output {
            text: format!("nopepdf-cli {}", env!("CARGO_PKG_VERSION")),
            json: json!({ "version": env!("CARGO_PKG_VERSION") }),
        }),
        Command::Printers => {
            let list =
                print::printers().or_else(|e| fail(EXIT_FAILED, format!("cannot list printers: {}", e.message())))?;
            let text = if list.is_empty() {
                "No printers found.".to_string()
            } else {
                list.iter()
                    .map(|p| format!("{} {}", if p.is_default { "*" } else { " " }, p.name))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let json = list.iter().map(|p| json!({ "name": p.name, "default": p.is_default })).collect();
            Ok(Output { text, json: Value::Array(json) })
        }
        Command::Info { file } => {
            let pdfium = load_pdfium()?;
            let doc = pdf::open_document(&pdfium, &file, password).map_err(|e| open_error(e, &file))?;
            Ok(info(&doc, &file))
        }
        Command::Render { file, page, out, dpi } => {
            let pdfium = load_pdfium()?;
            let doc = pdf::open_document(&pdfium, &file, password).map_err(|e| open_error(e, &file))?;
            let count = doc.pages().len() as usize;
            if page > count {
                return usage(format!("page {page} is past the end of the document ({count} pages)"));
            }
            let p = doc.pages().get((page - 1) as PdfPageIndex).or_else(|e| fail(EXIT_FAILED, format!("{e:?}")))?;
            let cfg = PdfRenderConfig::new()
                .scale_page_by_factor(dpi / 72.0)
                .render_form_data(true)
                .set_clear_color(PdfColor::WHITE);
            let img = p
                .render_with_config(&cfg)
                .and_then(|b| b.as_image())
                .or_else(|e| fail(EXIT_FAILED, format!("cannot render page {page}: {e:?}")))?;
            img.save_with_format(&out, image::ImageFormat::Png)
                .or_else(|e| fail(EXIT_FAILED, format!("cannot write {}: {e}", out.display())))?;
            let (w, h) = (img.width(), img.height());
            Ok(Output {
                text: format!("Wrote {} ({w} x {h} px, page {page})", out.display()),
                json: json!({ "file": out.display().to_string(), "page": page, "width": w, "height": h, "dpi": dpi }),
            })
        }
        Command::Print { file, printer, pages, copies, duplex, output, dry_run } => {
            let pdfium = load_pdfium()?;
            let doc = pdf::open_document(&pdfium, &file, password).map_err(|e| open_error(e, &file))?;
            let count = doc.pages().len() as usize;
            let pages = match &pages {
                Some(spec) => parse_pages(spec, count).or_else(|e| usage(format!("--pages: {e}")))?,
                None => (0..count).collect(),
            };
            if pages.is_empty() {
                return usage("nothing to print");
            }
            let list = print::printers().map_err(|e| print_error(e, printer.as_deref().unwrap_or("?")))?;
            let printer = match printer {
                Some(name) => match list.iter().find(|p| p.name.eq_ignore_ascii_case(&name)) {
                    Some(p) => p.name.clone(),
                    None => {
                        return fail(EXIT_PRINTER, format!("printer not found: \"{name}\" (see nopepdf-cli printers)"));
                    }
                },
                None => match list.iter().find(|p| p.is_default) {
                    Some(p) => p.name.clone(),
                    None => return fail(EXIT_PRINTER, "no default printer; choose one with --printer"),
                },
            };
            let summary = json!({
                "file": file.display().to_string(),
                "printer": printer,
                "pages": print::page_ranges(&pages),
                "page_count": pages.len(),
                "copies": copies,
                "duplex": duplex_name(duplex),
                "output": output.as_ref().map(|o| o.display().to_string()),
                "dry_run": dry_run,
            });
            let what = format!(
                "{} to \"{printer}\": pages {} ({} pages), {copies} {}, duplex: {}",
                file.display(),
                print::page_ranges(&pages),
                pages.len(),
                if copies == 1 { "copy" } else { "copies" },
                duplex_name(duplex),
            );
            if dry_run {
                return Ok(Output { text: format!("Would print {what}"), json: summary });
            }
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "NopePDF".into());
            let req = PrintRequest { printer: printer.clone(), pages, copies, duplex, output, name };
            print::print_document(&doc, &file, &req).map_err(|e| print_error(e, &printer))?;
            Ok(Output { text: format!("Printed {what}"), json: summary })
        }
    }
}

struct Output {
    text: String,
    json: Value,
}

impl Output {
    fn text(s: &str) -> Self {
        Output { text: s.to_string(), json: json!({ "help": s }) }
    }
}

fn info(doc: &PdfDocument, file: &Path) -> Output {
    let sizes: Vec<(f32, f32)> = doc
        .pages()
        .page_sizes()
        .map(|v| v.iter().map(|r| (r.width().value, r.height().value)).collect())
        .unwrap_or_default();
    let meta = doc.metadata();
    let tags = [
        ("title", PdfDocumentMetadataTagType::Title),
        ("author", PdfDocumentMetadataTagType::Author),
        ("subject", PdfDocumentMetadataTagType::Subject),
        ("keywords", PdfDocumentMetadataTagType::Keywords),
        ("creator", PdfDocumentMetadataTagType::Creator),
        ("producer", PdfDocumentMetadataTagType::Producer),
        ("created", PdfDocumentMetadataTagType::CreationDate),
        ("modified", PdfDocumentMetadataTagType::ModificationDate),
    ];
    let values: Vec<(&str, String)> = tags
        .iter()
        .filter_map(|(k, t)| meta.get(*t).map(|v| (*k, v.value().trim().to_string())))
        .filter(|(_, v)| !v.is_empty())
        .collect();

    let mm = |pt: f32| pt / 72.0 * 25.4;
    let size_text = |(w, h): (f32, f32)| format!("{w:.0} x {h:.0} pt ({:.0} x {:.0} mm)", mm(w), mm(h));
    let mut text = vec![format!("File:      {}", file.display()), format!("Pages:     {}", sizes.len())];
    match sizes.first() {
        Some(&first) if sizes.iter().all(|&s| (s.0 - first.0).abs() < 0.5 && (s.1 - first.1).abs() < 0.5) => {
            text.push(format!("Page size: {}", size_text(first)))
        }
        Some(&first) => text.push(format!("Page size: mixed, first page {}", size_text(first))),
        None => {}
    }
    for (k, v) in &values {
        let mut label = k.to_string();
        label[..1].make_ascii_uppercase();
        text.push(format!("{:<10} {v}", format!("{label}:")));
    }

    let mut json = json!({
        "file": file.display().to_string(),
        "pages": sizes.len(),
        "page_sizes": sizes.iter().map(|(w, h)| json!({ "width": w, "height": h })).collect::<Vec<_>>(),
    });
    for (k, v) in values {
        json[k] = Value::String(v);
    }
    Output { text: text.join("\n"), json }
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // --json must also shape errors, including usage errors found while parsing.
    let json = raw.iter().any(|a| a == "--json");
    let result = parse_args(&raw).and_then(run);
    match result {
        Ok(out) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&out.json).unwrap_or_default());
            } else {
                println!("{}", out.text);
            }
            ExitCode::SUCCESS
        }
        Err(f) => {
            if json {
                println!("{}", json!({ "error": f.message, "exit_code": f.code }));
            }
            eprintln!("nopepdf-cli: {}", f.message);
            ExitCode::from(f.code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Result<Args, Failure> {
        parse_args(&s.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn pages_parse() {
        assert_eq!(parse_pages("1-3,7,10-", 12), Ok(vec![0, 1, 2, 6, 9, 10, 11]));
        assert_eq!(parse_pages("5", 5), Ok(vec![4]));
        assert_eq!(parse_pages(" 3 , 1-2 , 2 ", 5), Ok(vec![0, 1, 2]));
        assert!(parse_pages("0", 5).is_err());
        assert!(parse_pages("6", 5).is_err());
        assert!(parse_pages("4-9", 5).is_err());
        assert!(parse_pages("3-1", 5).is_err());
        assert!(parse_pages("a", 5).is_err());
        assert!(parse_pages("", 5).is_err());
        assert!(parse_pages("1,,2", 5).is_err());
        assert!(parse_pages("-3", 5).is_err());
    }

    #[test]
    fn print_args() {
        let a =
            args(&["print", "a.pdf", "--printer", "P", "--pages=2-3", "--copies", "2", "--duplex", "long"]).unwrap();
        assert_eq!(
            a.command,
            Command::Print {
                file: "a.pdf".into(),
                printer: Some("P".into()),
                pages: Some("2-3".into()),
                copies: 2,
                duplex: Some(Duplex::Long),
                output: None,
                dry_run: false,
            }
        );
        assert!(!a.json);
        let a = args(&["--json", "print", "a.pdf", "--dry-run", "--password", "x"]).unwrap();
        assert!(a.json);
        assert_eq!(a.password.as_deref(), Some("x"));
        assert!(matches!(a.command, Command::Print { dry_run: true, copies: 1, duplex: None, .. }));
    }

    #[test]
    fn other_commands() {
        assert_eq!(args(&["printers"]).unwrap().command, Command::Printers);
        assert_eq!(args(&["info", "x.pdf"]).unwrap().command, Command::Info { file: "x.pdf".into() });
        assert_eq!(
            args(&["render", "x.pdf", "--page", "2", "--out", "p.png"]).unwrap().command,
            Command::Render { file: "x.pdf".into(), page: 2, out: "p.png".into(), dpi: 150.0 }
        );
        assert_eq!(args(&["--help"]).unwrap().command, Command::Help);
        assert_eq!(args(&["print", "-h"]).unwrap().command, Command::Help);
        assert_eq!(args(&["--version"]).unwrap().command, Command::Version);
    }

    #[test]
    fn usage_errors() {
        for bad in [
            &[][..],
            &["frobnicate"],
            &["print"],
            &["print", "a.pdf", "--copies", "0"],
            &["print", "a.pdf", "--copies", "1000"],
            &["print", "a.pdf", "--duplex", "both"],
            &["print", "a.pdf", "--printer"],
            &["print", "a.pdf", "--dry-run=yes"],
            &["print", "a.pdf", "--bogus"],
            &["print", "a.pdf", "b.pdf"],
            &["print", "a.pdf", "--page", "2"],
            &["info", "a.pdf", "--printer", "P"],
            &["render", "a.pdf", "--out", "x.png"],
            &["render", "a.pdf", "--page", "0", "--out", "x.png"],
            &["render", "a.pdf", "--page", "1", "--out", "x.png", "--dpi", "5"],
            &["printers", "--copies", "2", "--copies", "3"],
        ] {
            assert_eq!(args(bad).err().map(|f| f.code), Some(EXIT_USAGE), "{bad:?}");
        }
    }
}
