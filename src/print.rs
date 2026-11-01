//! Printing.
//!
//! In the viewer, printing is split over three threads so the window never freezes:
//! - the UI thread shows the system print dialog, owned by the main window;
//! - the PDFium worker renders one page at a time for the printer between view renders
//!   (`render_entry`, shared by all platforms);
//! - a print thread per job owns the printer and feeds it the pages (`spawn_printer`). It is
//!   the only one that may block, e.g. while spooling or while a PDF printer shows its
//!   "Save as" dialog.
//!
//! The command-line tool prints without any dialog through `print_document`.
//!
//! Backends: Windows GDI (pages are rasterized at up to 300 DPI) and CUPS `lp` on macOS and
//! Linux (the PDF file goes to CUPS as it is). The viewer's print dialog exists only on
//! Windows so far; elsewhere it reports `PrintError::Unsupported`.

use crate::i18n::{tr, tr_args};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PrintError {
    NotOpen,
    Start,
    /// Rendering page (0-based) failed.
    Page(usize),
    Printer,
    /// The user cancelled (e.g. the "Save as" dialog) or the document was closed.
    Cancelled,
    Unsupported,
    PrinterNotFound,
    DuplexUnsupported,
}

impl PrintError {
    pub fn message(&self) -> String {
        match self {
            PrintError::NotOpen => tr("error.not_open").into(),
            PrintError::Start => tr("error.print_start").into(),
            PrintError::Page(i) => tr_args("error.print_page", &[("page", &(i + 1))]),
            PrintError::Printer => tr("error.printer").into(),
            PrintError::Cancelled => tr("print.cancelled").into(),
            PrintError::Unsupported => tr("error.print_unsupported").into(),
            PrintError::PrinterNotFound => tr("error.printer_not_found").into(),
            PrintError::DuplexUnsupported => tr("error.duplex_unsupported").into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Duplex {
    /// One-sided.
    None,
    /// Two-sided, flipped on the long edge.
    Long,
    /// Two-sided, flipped on the short edge.
    Short,
}

/// A print job without a dialog (command-line tool, tests).
#[derive(Clone, Debug)]
pub struct PrintRequest {
    pub printer: String,
    /// 0-based page indices in print order.
    pub pages: Vec<usize>,
    pub copies: u16,
    /// `None` keeps the printer's default.
    pub duplex: Option<Duplex>,
    /// Write the job to this file instead of the device (Windows only; meant for PDF and
    /// XPS printers, which otherwise ask for a file name).
    pub output: Option<PathBuf>,
    /// Job name shown in the print queue.
    pub name: String,
}

impl PrintRequest {
    pub fn new(printer: &str, pages: Vec<usize>) -> Self {
        PrintRequest { printer: printer.into(), pages, copies: 1, duplex: None, output: None, name: "NopePDF".into() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Printer {
    pub name: String,
    pub is_default: bool,
}

/// Printable area of the printer: resolution and size in device pixels.
#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub dpi_x: f32,
    pub dpi_y: f32,
    pub width: f32,
    pub height: f32,
}

#[cfg_attr(not(windows), allow(dead_code))]
pub struct PrintJob {
    pub hdc: isize,
    /// Pages in print order; `None` is an empty page (keeps copies apart when two-sided).
    pub pages: Vec<Option<usize>>,
    pub name: String,
    /// Print to this file instead of the device.
    pub output: Option<String>,
    pub device: Device,
}

/// The printer device context belongs to the job and is released however the job ends.
#[cfg(windows)]
impl Drop for PrintJob {
    fn drop(&mut self) {
        if self.hdc != 0 {
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::DeleteDC(windows::Win32::Graphics::Gdi::HDC(self.hdc as *mut _));
            }
        }
    }
}

/// A page rendered for the printer and where it goes on the printable area (device pixels).
/// A zero-sized image is an empty page.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct PageImage {
    pub width: i32,
    pub height: i32,
    pub rgba: Vec<u8>,
    pub x: i32,
    pub y: i32,
    pub dest_w: i32,
    pub dest_h: i32,
}

/// What the worker sends to the print thread for each page, in order.
pub type PageResult = Result<PageImage, PrintError>;

/// The pages to send for `copies` collated copies. When printing two-sided, an odd page
/// count gets an empty page between copies, so no copy starts on the back of a sheet.
/// Copies are made here rather than by the driver, because drivers handle them unevenly.
pub fn sheet_sequence(pages: &[usize], copies: u16, two_sided: bool) -> Vec<Option<usize>> {
    let mut out = Vec::new();
    for copy in 0..copies.max(1) {
        if copy > 0 && two_sided && pages.len() % 2 == 1 {
            out.push(None);
        }
        out.extend(pages.iter().map(|&p| Some(p)));
    }
    out
}

/// 0-based pages as compact 1-based ranges, e.g. `[0, 1, 2, 6]` → `"1-3,7"`.
pub fn page_ranges(pages: &[usize]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let mut j = i;
        while j + 1 < pages.len() && pages[j + 1] == pages[j] + 1 {
            j += 1;
        }
        parts.push(if i == j { format!("{}", pages[i] + 1) } else { format!("{}-{}", pages[i] + 1, pages[j] + 1) });
        i = j + 1;
    }
    parts.join(",")
}

/// Renders page `index` at (up to) 300 DPI, fitted to the printable area and centered.
/// Landscape pages are turned onto portrait paper and vice versa; pages are never enlarged.
pub fn render_page(doc: &PdfDocument, index: usize, dev: &Device) -> PageResult {
    let page = doc.pages().get(index as PdfPageIndex).map_err(|_| PrintError::Page(index))?;
    let (pw, ph) = (page.width().value, page.height().value);
    let rotate = (pw > ph) != (dev.width / dev.dpi_x > dev.height / dev.dpi_y);
    let (pw_r, ph_r) = if rotate { (ph, pw) } else { (pw, ph) };
    let fit = (dev.width / (pw_r / 72.0 * dev.dpi_x)).min(dev.height / (ph_r / 72.0 * dev.dpi_y)).min(1.0);
    let dest_w = pw_r / 72.0 * dev.dpi_x * fit;
    let dest_h = ph_r / 72.0 * dev.dpi_y * fit;
    let scale = dev.dpi_x.max(dev.dpi_y).min(300.0) / 72.0 * fit;

    let cfg = PdfRenderConfig::new()
        .scale_page_by_factor(scale)
        .rotate(if rotate { PdfPageRenderRotation::Degrees90 } else { PdfPageRenderRotation::None }, true)
        .render_form_data(true)
        .set_clear_color(PdfColor::WHITE);
    let bmp = page.render_with_config(&cfg).map_err(|_| PrintError::Page(index))?;
    Ok(PageImage {
        width: bmp.width(),
        height: bmp.height(),
        rgba: bmp.as_rgba_bytes(),
        x: ((dev.width - dest_w) / 2.0) as i32,
        y: ((dev.height - dest_h) / 2.0) as i32,
        dest_w: dest_w as i32,
        dest_h: dest_h as i32,
    })
}

/// Renders one entry of `PrintJob::pages`: a page, or an empty page for `None`.
pub fn render_entry(doc: &PdfDocument, entry: Option<usize>, dev: &Device) -> PageResult {
    match entry {
        Some(i) => render_page(doc, i, dev),
        None => Ok(PageImage { width: 0, height: 0, rgba: Vec::new(), x: 0, y: 0, dest_w: 0, dest_h: 0 }),
    }
}

/// The native window handle used as the owner of the print dialog.
pub fn owner_of(window: &impl raw_window_handle::HasWindowHandle) -> Option<isize> {
    match window.window_handle().ok()?.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
        _ => None,
    }
}

/// Prints without any dialog and returns once the whole job is with the spooler. `doc` is
/// the document at `path`, already opened (the Windows backend renders it; CUPS prints
/// the file itself).
pub fn print_document(doc: &PdfDocument, path: &Path, req: &PrintRequest) -> Result<(), PrintError> {
    #[cfg(windows)]
    {
        let _ = path;
        let job = imp::open_printer(req)?;
        let (dev, entries) = (job.device, job.pages.clone());
        let (tx, rx) = std::sync::mpsc::sync_channel(0);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        imp::spawn_printer(job, rx, move |r| {
            let _ = done_tx.send(r);
        });
        for entry in entries {
            let page = render_entry(doc, entry, &dev);
            let failed = page.is_err();
            if tx.send(page).is_err() || failed {
                break;
            }
        }
        drop(tx);
        done_rx.recv().unwrap_or(Err(PrintError::Printer))
    }
    #[cfg(not(windows))]
    {
        let _ = doc;
        cups::print_file(path, req)
    }
}

/// Installed printers; the system default is marked.
pub fn printers() -> Result<Vec<Printer>, PrintError> {
    #[cfg(windows)]
    return imp::printers();
    #[cfg(not(windows))]
    return cups::printers();
}

#[cfg(windows)]
mod imp {
    use super::{Device, Duplex, PageImage, PageResult, PrintError, PrintJob, PrintRequest, Printer};
    use std::sync::mpsc;
    use windows::Win32::Foundation::{ERROR_CANCELLED, GetLastError, GlobalFree, HGLOBAL, HWND};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Graphics::Printing::{
        ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW, PRINTER_ENUM_CONNECTIONS,
        PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W,
    };
    use windows::Win32::Storage::Xps::{
        AbortDoc, DC_DUPLEX, DOCINFOW, DeviceCapabilitiesW, EndDoc, EndPage, StartDocW, StartPage,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::UI::Controls::Dialogs::*;
    use windows::core::{PCWSTR, PWSTR, w};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub fn device_of(hdc: HDC) -> Device {
        unsafe {
            Device {
                dpi_x: GetDeviceCaps(Some(hdc), LOGPIXELSX).max(72) as f32,
                dpi_y: GetDeviceCaps(Some(hdc), LOGPIXELSY).max(72) as f32,
                width: GetDeviceCaps(Some(hdc), HORZRES).max(1) as f32,
                height: GetDeviceCaps(Some(hdc), VERTRES).max(1) as f32,
            }
        }
    }

    pub fn printers() -> Result<Vec<Printer>, PrintError> {
        let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
        let (mut needed, mut count) = (0u32, 0u32);
        // The first call only reports the buffer size it needs.
        let _ = unsafe { EnumPrintersW(flags, PCWSTR::null(), 4, None, &mut needed, &mut count) };
        if needed == 0 {
            return Ok(Vec::new());
        }
        // u64 storage keeps the PRINTER_INFO_4W records properly aligned.
        let mut buf = vec![0u64; needed as usize / 8 + 1];
        let bytes = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, needed as usize) };
        unsafe { EnumPrintersW(flags, PCWSTR::null(), 4, Some(bytes), &mut needed, &mut count) }
            .map_err(|_| PrintError::Printer)?;
        let infos = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const PRINTER_INFO_4W, count as usize) };
        let default = default_printer();
        Ok(infos
            .iter()
            .filter_map(|i| unsafe { i.pPrinterName.to_string() }.ok())
            .map(|name| Printer { is_default: default.as_deref() == Some(name.as_str()), name })
            .collect())
    }

    fn default_printer() -> Option<String> {
        let mut len = 0u32;
        let _ = unsafe { GetDefaultPrinterW(None, &mut len) };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        if !unsafe { GetDefaultPrinterW(Some(PWSTR(buf.as_mut_ptr())), &mut len) }.as_bool() {
            return None;
        }
        Some(String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(buf.len())]))
    }

    /// Opens the named printer with the requested two-sided setting and one copy per job
    /// (copies are repeated by `sheet_sequence`).
    pub fn open_printer(req: &PrintRequest) -> Result<PrintJob, PrintError> {
        let list = printers()?;
        let Some(printer) = list.iter().find(|p| p.name.eq_ignore_ascii_case(&req.printer)) else {
            return Err(PrintError::PrinterNotFound);
        };
        let name = wide(&printer.name);
        let name_p = PCWSTR(name.as_ptr());
        let wants_two_sided = matches!(req.duplex, Some(Duplex::Long | Duplex::Short));
        if wants_two_sided && unsafe { DeviceCapabilitiesW(name_p, PCWSTR::null(), DC_DUPLEX, None, None) } != 1 {
            return Err(PrintError::DuplexUnsupported);
        }

        let mut handle = PRINTER_HANDLE::default();
        unsafe { OpenPrinterW(name_p, &mut handle, None) }.map_err(|_| PrintError::PrinterNotFound)?;
        let devmode = unsafe { job_devmode(handle, name_p, req.duplex) };
        let _ = unsafe { ClosePrinter(handle) };
        let devmode = devmode.ok_or(PrintError::Printer)?;
        let dm = devmode.as_ptr() as *const DEVMODEW;

        let two_sided =
            unsafe { (*dm).dmFields.contains(DM_DUPLEX) && (*dm).dmDuplex != DMDUP_SIMPLEX && (*dm).dmDuplex.0 != 0 };
        let hdc = unsafe { CreateDCW(w!("WINSPOOL"), name_p, PCWSTR::null(), Some(dm)) };
        if hdc.is_invalid() {
            return Err(PrintError::Printer);
        }
        Ok(PrintJob {
            hdc: hdc.0 as isize,
            pages: super::sheet_sequence(&req.pages, req.copies, two_sided),
            name: req.name.clone(),
            output: req.output.as_ref().map(|p| p.to_string_lossy().into_owned()),
            device: device_of(hdc),
        })
    }

    /// The printer's default DEVMODE with the duplex setting merged in and copies set to 1,
    /// validated by the driver. Stored as u64s for alignment.
    unsafe fn job_devmode(handle: PRINTER_HANDLE, name: PCWSTR, duplex: Option<Duplex>) -> Option<Vec<u64>> {
        let size = unsafe { DocumentPropertiesW(None, handle, name, None, None, 0) };
        if size <= 0 {
            return None;
        }
        let words = size as usize / 8 + 1;
        let mut base = vec![0u64; words];
        let base_p = base.as_mut_ptr() as *mut DEVMODEW;
        if unsafe { DocumentPropertiesW(None, handle, name, Some(base_p), None, DM_OUT_BUFFER.0) } < 0 {
            return None;
        }
        unsafe {
            (*base_p).dmFields |= DM_COPIES;
            (*base_p).Anonymous1.Anonymous1.dmCopies = 1;
            if let Some(d) = duplex {
                (*base_p).dmFields |= DM_DUPLEX;
                (*base_p).dmDuplex = match d {
                    Duplex::None => DMDUP_SIMPLEX,
                    Duplex::Long => DMDUP_VERTICAL,
                    Duplex::Short => DMDUP_HORIZONTAL,
                };
            }
        }
        let mut merged = vec![0u64; words];
        let merged_p = merged.as_mut_ptr() as *mut DEVMODEW;
        let flags = DM_IN_BUFFER.0 | DM_OUT_BUFFER.0;
        if unsafe { DocumentPropertiesW(None, handle, name, Some(merged_p), Some(base_p), flags) } < 0 {
            return None;
        }
        Some(merged)
    }

    /// Shows the print dialog, modal to `owner`. Runs on the UI thread.
    pub fn show_dialog(owner: Option<isize>, page_count: usize, current: usize, name: &str) -> Option<PrintJob> {
        let mut pd = PRINTDLGW {
            lStructSize: std::mem::size_of::<PRINTDLGW>() as u32,
            hwndOwner: owner.map_or(HWND::default(), |h| HWND(h as *mut _)),
            Flags: PD_RETURNDC | PD_NOSELECTION | PD_USEDEVMODECOPIESANDCOLLATE,
            nFromPage: (current + 1) as u16,
            nToPage: (current + 1) as u16,
            nMinPage: 1,
            nMaxPage: page_count.min(u16::MAX as usize) as u16,
            nCopies: 1,
            ..Default::default()
        };
        let ok = unsafe { PrintDlgW(&mut pd) }.as_bool();
        unsafe {
            if !pd.hDevMode.is_invalid() {
                let _ = GlobalFree(Some(HGLOBAL(pd.hDevMode.0)));
            }
            if !pd.hDevNames.is_invalid() {
                let _ = GlobalFree(Some(HGLOBAL(pd.hDevNames.0)));
            }
        }
        if !ok || pd.hDC.is_invalid() {
            return None;
        }
        let (from, to) = if pd.Flags.contains(PD_PAGENUMS) {
            (pd.nFromPage.max(1) as usize - 1, pd.nToPage.max(1) as usize - 1)
        } else {
            (0, page_count.saturating_sub(1))
        };
        Some(PrintJob {
            hdc: pd.hDC.0 as isize,
            // Copies and collation are applied by the driver (PD_USEDEVMODECOPIESANDCOLLATE).
            pages: (from..=to.min(page_count - 1)).map(Some).collect(),
            name: name.to_string(),
            output: None,
            device: device_of(pd.hDC),
        })
    }

    pub fn spawn_printer(
        job: PrintJob,
        pages: mpsc::Receiver<PageResult>,
        done: impl FnOnce(Result<(), PrintError>) + Send + 'static,
    ) {
        // If the thread cannot start, `pages` is dropped and the sender stops rendering.
        let _ = std::thread::Builder::new().name("print".into()).spawn(move || {
            // PDF and XPS printers open a file dialog inside StartDoc, which needs COM.
            let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
            let res = unsafe { print_job(&job, &pages) };
            drop(job);
            if com {
                unsafe { CoUninitialize() };
            }
            done(res);
        });
    }

    /// Prints the whole job or nothing: any failure aborts the job instead of silently
    /// leaving pages out.
    unsafe fn print_job(job: &PrintJob, pages: &mpsc::Receiver<PageResult>) -> Result<(), PrintError> {
        let hdc = HDC(job.hdc as *mut _);
        let name = wide(&job.name);
        let output: Option<Vec<u16>> = job.output.as_deref().map(wide);
        let di = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: PCWSTR(name.as_ptr()),
            lpszOutput: output.as_ref().map_or(PCWSTR::null(), |o| PCWSTR(o.as_ptr())),
            ..Default::default()
        };
        if unsafe { StartDocW(hdc, &di) } <= 0 {
            return Err(if unsafe { GetLastError() } == ERROR_CANCELLED {
                PrintError::Cancelled
            } else {
                PrintError::Start
            });
        }
        let printed = job.pages.iter().try_for_each(|_| {
            // A closed channel before the last page means the job was cancelled.
            let page = pages.recv().map_err(|_| PrintError::Cancelled)??;
            unsafe { print_page(hdc, &page) }
        });
        match printed {
            Ok(()) if unsafe { EndDoc(hdc) } > 0 => Ok(()),
            Ok(()) => Err(PrintError::Printer),
            Err(e) => {
                unsafe { AbortDoc(hdc) };
                Err(e)
            }
        }
    }

    unsafe fn print_page(hdc: HDC, page: &PageImage) -> Result<(), PrintError> {
        if unsafe { StartPage(hdc) } <= 0 {
            return Err(PrintError::Printer);
        }
        if page.width > 0 && page.height > 0 {
            // GDI wants BGRA; convert from the known RGBA layout rather than trusting the raw order.
            let mut bgra = page.rgba.clone();
            for px in bgra.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: page.width,
                    biHeight: -page.height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let copied = unsafe {
                SetStretchBltMode(hdc, HALFTONE);
                StretchDIBits(
                    hdc,
                    page.x,
                    page.y,
                    page.dest_w,
                    page.dest_h,
                    0,
                    0,
                    page.width,
                    page.height,
                    Some(bgra.as_ptr() as *const _),
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                )
            };
            if copied <= 0 {
                return Err(PrintError::Printer);
            }
        }
        if unsafe { EndPage(hdc) } <= 0 {
            return Err(PrintError::Printer);
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use imp::{open_printer, show_dialog, spawn_printer};

#[cfg(not(windows))]
pub fn show_dialog(_owner: Option<isize>, _page_count: usize, _current: usize, _name: &str) -> Option<PrintJob> {
    None
}

#[cfg(not(windows))]
pub fn spawn_printer(
    _job: PrintJob,
    _pages: std::sync::mpsc::Receiver<PageResult>,
    done: impl FnOnce(Result<(), PrintError>) + Send + 'static,
) {
    done(Err(PrintError::Unsupported));
}

/// CUPS backend: the PDF file goes to `lp` unchanged, so nothing is rasterized.
#[cfg(not(windows))]
mod cups {
    use super::{Duplex, PrintError, PrintRequest, Printer};
    use std::path::Path;
    use std::process::Command;

    fn run(cmd: &str, args: &[&str]) -> Result<String, PrintError> {
        // A missing command means CUPS is not installed.
        let out = Command::new(cmd).args(args).output().map_err(|_| PrintError::Unsupported)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(PrintError::Printer)
        }
    }

    pub fn printers() -> Result<Vec<Printer>, PrintError> {
        // `lpstat -e` fails when no printer is configured; that is an empty list.
        let names = run("lpstat", &["-e"]).unwrap_or_default();
        // "system default destination: NAME"
        let default = run("lpstat", &["-d"])
            .ok()
            .and_then(|s| s.split_once(':').map(|(_, n)| n.trim().to_string()))
            .filter(|n| !n.is_empty());
        Ok(names
            .lines()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| Printer { name: n.to_string(), is_default: default.as_deref() == Some(n) })
            .collect())
    }

    pub fn print_file(path: &Path, req: &PrintRequest) -> Result<(), PrintError> {
        if req.output.is_some() {
            return Err(PrintError::Unsupported);
        }
        if !printers()?.iter().any(|p| p.name == req.printer) {
            return Err(PrintError::PrinterNotFound);
        }
        let copies = req.copies.max(1).to_string();
        let ranges = super::page_ranges(&req.pages);
        let mut args = vec!["-d", &req.printer, "-n", &copies, "-P", &ranges, "-t", &req.name];
        match req.duplex {
            Some(Duplex::None) => args.extend(["-o", "sides=one-sided"]),
            Some(Duplex::Long) => args.extend(["-o", "sides=two-sided-long-edge"]),
            Some(Duplex::Short) => args.extend(["-o", "sides=two-sided-short-edge"]),
            None => {}
        }
        if req.copies > 1 {
            args.extend(["-o", "collate=true"]);
        }
        let path = path.to_string_lossy();
        args.extend(["--", &path]);
        run("lp", &args).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{lock, pdfium, sample};

    #[test]
    fn sheet_sequence_repeats_and_pads() {
        assert_eq!(sheet_sequence(&[0, 1], 1, false), vec![Some(0), Some(1)]);
        assert_eq!(sheet_sequence(&[0, 1, 2], 2, false), vec![Some(0), Some(1), Some(2), Some(0), Some(1), Some(2)]);
        // Two-sided, odd count: copy 2 starts on a new sheet.
        assert_eq!(
            sheet_sequence(&[4, 5, 6], 2, true),
            vec![Some(4), Some(5), Some(6), None, Some(4), Some(5), Some(6)]
        );
        // Even count needs no padding; zero copies means one.
        assert_eq!(sheet_sequence(&[0, 1], 2, true).len(), 4);
        assert_eq!(sheet_sequence(&[3], 0, false), vec![Some(3)]);
    }

    #[test]
    fn page_ranges_are_compact() {
        assert_eq!(page_ranges(&[0, 1, 2, 6]), "1-3,7");
        assert_eq!(page_ranges(&[4]), "5");
        assert_eq!(page_ranges(&[0, 2, 3, 9, 10, 11]), "1,3-4,10-12");
        assert_eq!(page_ranges(&[]), "");
    }

    #[test]
    fn landscape_page_is_rotated_and_fits() {
        let _g = lock();
        let (Some(input), Some(pdfium)) = (sample(), pdfium()) else { return };
        let doc = pdfium.load_pdf_from_file(&input, None).unwrap();
        // A4 portrait at 600 DPI and its landscape version.
        let portrait = Device { dpi_x: 600.0, dpi_y: 600.0, width: 4960.0, height: 7016.0 };
        let landscape = Device { width: portrait.height, height: portrait.width, ..portrait };
        for dev in [portrait, landscape] {
            let img = render_page(&doc, 0, &dev).unwrap();
            assert!(img.dest_w as f32 <= dev.width && img.dest_h as f32 <= dev.height);
            assert!(img.x >= 0 && img.y >= 0);
            // Rendered at 300 DPI, so the bitmap is about half the device size.
            assert!(img.width <= img.dest_w / 2 + 2);
            assert_eq!(img.width > img.height, dev.width > dev.height);
        }
        assert_eq!(render_page(&doc, 9999, &portrait).err(), Some(PrintError::Page(9999)));
        assert_eq!(render_entry(&doc, None, &portrait).unwrap().width, 0);
    }

    /// These print only to "Microsoft Print to PDF" with an output file, never to a
    /// physical printer.
    #[cfg(windows)]
    mod windows_printer {
        use super::*;
        use std::sync::mpsc;

        const PDF_PRINTER: &str = "Microsoft Print to PDF";

        fn request(pages: Vec<usize>, out: &str) -> PrintRequest {
            let out = std::env::temp_dir().join(out);
            let _ = std::fs::remove_file(&out);
            PrintRequest { output: Some(out), ..PrintRequest::new(PDF_PRINTER, pages) }
        }

        /// Feeds `pages` to a print thread the way the worker does and waits for the result.
        fn print_range(job: PrintJob, pages: impl Iterator<Item = PageResult>) -> Result<(), PrintError> {
            let (tx, rx) = mpsc::sync_channel(0);
            let (done_tx, done_rx) = mpsc::channel();
            spawn_printer(job, rx, move |r| done_tx.send(r).unwrap());
            for p in pages {
                if tx.send(p).is_err() {
                    break;
                }
            }
            drop(tx);
            done_rx.recv_timeout(std::time::Duration::from_secs(60)).expect("print thread did not finish")
        }

        #[test]
        fn lists_the_pdf_printer() {
            let list = printers().unwrap();
            assert!(list.iter().any(|p| p.name == PDF_PRINTER), "{list:?}");
            assert!(list.iter().filter(|p| p.is_default).count() <= 1);
        }

        #[test]
        fn unknown_printer_and_duplex_are_reported() {
            let req = PrintRequest::new("No Such Printer 1234", vec![0]);
            assert_eq!(open_printer(&req).err(), Some(PrintError::PrinterNotFound));
            let req = PrintRequest { duplex: Some(Duplex::Long), ..request(vec![0], "nopepdf-duplex-test.pdf") };
            assert_eq!(open_printer(&req).err(), Some(PrintError::DuplexUnsupported));
        }

        #[test]
        fn print_document_with_copies() {
            let _g = lock();
            let (Some(input), Some(pdfium)) = (sample(), pdfium()) else { return };
            let doc = pdfium.load_pdf_from_file(&input, None).unwrap();
            let req = PrintRequest { copies: 2, ..request(vec![0, 2], "nopepdf-copies-test.pdf") };
            print_document(&doc, &input, &req).unwrap();
            let printed = pdfium.load_pdf_from_file(req.output.as_ref().unwrap(), None).unwrap();
            assert_eq!(printed.pages().len(), 4);
        }

        #[test]
        fn closed_channel_cancels_job() {
            let _g = lock();
            let (Some(input), Some(pdfium)) = (sample(), pdfium()) else { return };
            let doc = pdfium.load_pdf_from_file(&input, None).unwrap();
            let job = open_printer(&request(vec![0, 1, 2], "nopepdf-print-cancel-test.pdf")).unwrap();
            let dev = job.device;
            // Only one of three pages arrives.
            let res = print_range(job, (0..1).map(|i| render_page(&doc, i, &dev)));
            assert_eq!(res, Err(PrintError::Cancelled));
        }

        #[test]
        fn render_error_aborts_job() {
            let _g = lock();
            let (Some(input), Some(pdfium)) = (sample(), pdfium()) else { return };
            let doc = pdfium.load_pdf_from_file(&input, None).unwrap();
            let req = request(vec![0, 9999], "nopepdf-print-error-test.pdf");
            assert_eq!(print_document(&doc, &input, &req), Err(PrintError::Page(9999)));
        }
    }
}
