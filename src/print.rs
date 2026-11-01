//! Printing, split over three threads so the viewer never freezes:
//! - the UI thread shows the system print dialog, owned by the main window;
//! - the PDFium worker renders one page at a time for the printer between view renders
//!   (`render_page`, shared by all platforms);
//! - a print thread per job owns the printer and feeds it the pages (`spawn_printer`). It is
//!   the only one that may block, e.g. while spooling or while a PDF printer shows its
//!   "Save as" dialog.
//!
//! The printer backend is Windows GDI; other platforms report `PrintError::Unsupported`
//! until a CUPS backend exists (see docs/plans/headless-printing.md).

use crate::i18n::{tr, tr_args};
use pdfium_render::prelude::*;

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
        }
    }
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
    pub from: usize,
    pub to: usize,
    pub name: String,
    /// Print to this file instead of the device (used by tests).
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

/// The native window handle used as the owner of the print dialog.
pub fn owner_of(window: &impl raw_window_handle::HasWindowHandle) -> Option<isize> {
    match window.window_handle().ok()?.as_raw() {
        raw_window_handle::RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
        _ => None,
    }
}

#[cfg(windows)]
mod imp {
    use super::{Device, PageImage, PageResult, PrintError, PrintJob};
    use std::sync::mpsc;
    use windows::Win32::Foundation::{ERROR_CANCELLED, GetLastError, GlobalFree, HGLOBAL, HWND};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows::Win32::UI::Controls::Dialogs::*;
    use windows::core::PCWSTR;

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
            from,
            to: to.min(page_count - 1),
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
        // If the thread cannot start, `pages` is dropped and the worker stops rendering.
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

    /// Prints the whole range or nothing: any failure aborts the job instead of
    /// silently leaving pages out.
    unsafe fn print_job(job: &PrintJob, pages: &mpsc::Receiver<PageResult>) -> Result<(), PrintError> {
        let hdc = HDC(job.hdc as *mut _);
        let name: Vec<u16> = job.name.encode_utf16().chain(Some(0)).collect();
        let output: Option<Vec<u16>> = job.output.as_ref().map(|o| o.encode_utf16().chain(Some(0)).collect());
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
        let printed = (job.from..=job.to).try_for_each(|_| {
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
        // GDI wants BGRA; convert from the known RGBA layout rather than trusting the raw order.
        let mut bgra = page.rgba.clone();
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        if unsafe { StartPage(hdc) } <= 0 {
            return Err(PrintError::Printer);
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
        if copied <= 0 || unsafe { EndPage(hdc) } <= 0 {
            return Err(PrintError::Printer);
        }
        Ok(())
    }
}

#[cfg(windows)]
pub use imp::{show_dialog, spawn_printer};

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

/// Test helper: a job for "Microsoft Print to PDF" writing to `output`. Tests never print
/// to a physical printer.
#[cfg(all(test, windows))]
pub fn pdf_printer_job(from: usize, to: usize, output: &std::path::Path) -> PrintJob {
    use windows::Win32::Graphics::Gdi::CreateDCW;
    use windows::core::{PCWSTR, w};
    const VIRTUAL_PRINTER: &str = "Microsoft Print to PDF";
    let name: Vec<u16> = VIRTUAL_PRINTER.encode_utf16().chain(Some(0)).collect();
    let hdc = unsafe { CreateDCW(w!("WINSPOOL"), PCWSTR(name.as_ptr()), None, None) };
    assert!(!hdc.is_invalid(), "{VIRTUAL_PRINTER} is not available");
    let _ = std::fs::remove_file(output);
    PrintJob {
        hdc: hdc.0 as isize,
        from,
        to,
        name: "nopepdf test".into(),
        output: Some(output.to_string_lossy().into_owned()),
        device: imp::device_of(hdc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{lock, pdfium, sample};

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
    }

    #[cfg(windows)]
    mod windows_printer {
        use super::*;
        use std::sync::mpsc;

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
        fn print_to_pdf_printer() {
            let _g = lock();
            let (Some(input), Some(pdfium)) = (sample(), pdfium()) else { return };
            let doc = pdfium.load_pdf_from_file(&input, None).unwrap();
            let out = std::env::temp_dir().join("nopepdf-print-test.pdf");
            let job = pdf_printer_job(0, 2, &out);
            let dev = job.device;
            print_range(job, (0..=2).map(|i| render_page(&doc, i, &dev))).unwrap();
            let printed = pdfium.load_pdf_from_file(&out, None).unwrap();
            assert_eq!(printed.pages().len(), 3);
        }

        #[test]
        fn closed_channel_cancels_job() {
            let _g = lock();
            let (Some(input), Some(pdfium)) = (sample(), pdfium()) else { return };
            let doc = pdfium.load_pdf_from_file(&input, None).unwrap();
            let out = std::env::temp_dir().join("nopepdf-print-cancel-test.pdf");
            let job = pdf_printer_job(0, 2, &out);
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
            let out = std::env::temp_dir().join("nopepdf-print-error-test.pdf");
            let job = pdf_printer_job(0, 1, &out);
            let dev = job.device;
            let pages = [render_page(&doc, 0, &dev), render_page(&doc, 9999, &dev)];
            assert_eq!(print_range(job, pages.into_iter()), Err(PrintError::Page(9999)));
        }
    }
}
