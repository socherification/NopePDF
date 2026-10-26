//! Printing. On Windows we use the system print dialog and GDI: each page is
//! rendered by PDFium to a bitmap at (up to) 300 DPI and blitted to the
//! printer DC. Other platforms are not supported yet.

#[cfg_attr(not(windows), allow(dead_code))]
pub struct PrintJob {
    pub hdc: isize,
    pub from: usize,
    pub to: usize,
    pub name: String,
    /// Print to this file instead of the device (used by tests).
    pub output: Option<String>,
}

#[cfg(windows)]
mod imp {
    use super::PrintJob;
    use pdfium_render::prelude::*;
    use windows::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Storage::Xps::{DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
    use windows::Win32::UI::Controls::Dialogs::*;
    use windows::core::PCWSTR;

    pub fn show_dialog(page_count: usize, current: usize, name: &str) -> Option<PrintJob> {
        let mut pd = PRINTDLGW {
            lStructSize: std::mem::size_of::<PRINTDLGW>() as u32,
            hwndOwner: HWND::default(),
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
        })
    }

    pub fn print_document(doc: &PdfDocument, job: PrintJob) -> Result<(), String> {
        let hdc = HDC(job.hdc as *mut _);
        let res = unsafe { print_pages(doc, hdc, &job) };
        unsafe {
            let _ = DeleteDC(hdc);
        }
        res
    }

    unsafe fn print_pages(doc: &PdfDocument, hdc: HDC, job: &PrintJob) -> Result<(), String> {
        let name: Vec<u16> = job.name.encode_utf16().chain(Some(0)).collect();
        let output: Option<Vec<u16>> = job.output.as_ref().map(|o| o.encode_utf16().chain(Some(0)).collect());
        let di = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: PCWSTR(name.as_ptr()),
            lpszOutput: output.as_ref().map_or(PCWSTR::null(), |o| PCWSTR(o.as_ptr())),
            ..Default::default()
        };
        if unsafe { StartDocW(hdc, &di) } <= 0 {
            return Err("Nie udało się rozpocząć wydruku".into());
        }
        let dpi = unsafe { GetDeviceCaps(Some(hdc), LOGPIXELSX) }.max(72) as f32;
        let area_w = unsafe { GetDeviceCaps(Some(hdc), HORZRES) } as f32;
        let area_h = unsafe { GetDeviceCaps(Some(hdc), VERTRES) } as f32;
        let render_dpi = dpi.min(300.0);

        for i in job.from..=job.to {
            let Ok(page) = doc.pages().get(i as PdfPageIndex) else { continue };
            let (pw, ph) = (page.width().value, page.height().value);
            // Auto-rotate landscape pages onto portrait paper (and vice versa).
            let rotate = (pw > ph) != (area_w > area_h);
            let (pw_r, ph_r) = if rotate { (ph, pw) } else { (pw, ph) };
            // Fit to printable area, never enlarge beyond 100%.
            let fit = (area_w / (pw_r / 72.0 * dpi)).min(area_h / (ph_r / 72.0 * dpi)).min(1.0);
            let dest_w = pw_r / 72.0 * dpi * fit;
            let dest_h = ph_r / 72.0 * dpi * fit;
            let scale = render_dpi / 72.0 * fit;

            let cfg = PdfRenderConfig::new()
                .scale_page_by_factor(scale)
                .rotate(if rotate { PdfPageRenderRotation::Degrees90 } else { PdfPageRenderRotation::None }, true)
                .render_form_data(true)
                .set_clear_color(PdfColor::WHITE);
            let Ok(bmp) = page.render_with_config(&cfg) else { continue };
            let (bw, bh) = (bmp.width(), bmp.height());
            // GDI wants BGRA; convert from a known RGBA layout rather than trusting the raw order.
            let mut bgra = bmp.as_rgba_bytes();
            for px in bgra.chunks_exact_mut(4) {
                px.swap(0, 2);
            }

            if unsafe { StartPage(hdc) } <= 0 {
                unsafe { EndDoc(hdc) };
                return Err("Błąd drukarki".into());
            }
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: bw,
                    biHeight: -bh,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let x = ((area_w - dest_w) / 2.0) as i32;
            let y = ((area_h - dest_h) / 2.0) as i32;
            unsafe {
                SetStretchBltMode(hdc, HALFTONE);
                StretchDIBits(
                    hdc,
                    x,
                    y,
                    dest_w as i32,
                    dest_h as i32,
                    0,
                    0,
                    bw,
                    bh,
                    Some(bgra.as_ptr() as *const _),
                    &bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                EndPage(hdc);
            }
        }
        unsafe { EndDoc(hdc) };
        Ok(())
    }
}

#[cfg(windows)]
pub use imp::{print_document, show_dialog};

#[cfg(not(windows))]
pub fn show_dialog(_page_count: usize, _current: usize, _name: &str) -> Option<PrintJob> {
    None
}

#[cfg(not(windows))]
pub fn print_document(_doc: &pdfium_render::prelude::PdfDocument, _job: PrintJob) -> Result<(), String> {
    Err("Drukowanie nie jest jeszcze obsługiwane na tej platformie".into())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use pdfium_render::prelude::*;
    use windows::Win32::Graphics::Gdi::CreateDCW;
    use windows::core::w;

    /// Prints the first pages of testdata/attention.pdf (any PDF with at least
    /// 3 pages, not in the repo) through "Microsoft Print to PDF" into a file.
    #[test]
    fn print_to_pdf_printer() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let input = root.join("testdata/attention.pdf");
        if !input.exists() {
            eprintln!("skipped: {} not found", input.display());
            return;
        }
        let pdfium = Pdfium::new(
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&root.join("vendor/pdfium/bin")))
                .unwrap(),
        );
        let doc = pdfium.load_pdf_from_file(&input, None).unwrap();
        let hdc = unsafe { CreateDCW(w!("WINSPOOL"), w!("Microsoft Print to PDF"), None, None) };
        assert!(!hdc.is_invalid(), "printer not available");
        let out = std::env::temp_dir().join("nopepdf-print-test.pdf");
        let _ = std::fs::remove_file(&out);
        let job = PrintJob {
            hdc: hdc.0 as isize,
            from: 0,
            to: 2,
            name: "test".into(),
            output: Some(out.to_string_lossy().into_owned()),
        };
        print_document(&doc, job).unwrap();
        let printed = pdfium.load_pdf_from_file(&out, None).unwrap();
        assert_eq!(printed.pages().len(), 3);
        let text = printed.pages().get(0).unwrap().text().unwrap().all();
        println!("printed {} bytes, page 1 text chars: {}", std::fs::metadata(&out).unwrap().len(), text.len());
    }
}
