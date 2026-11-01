//! Shared helpers for tests that need PDFium. PDFium is not thread-safe, so these tests
//! hold `lock()` while they use it.

use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static LOCK: Mutex<()> = Mutex::new(());

pub fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// PDFium from vendor/pdfium (run scripts/fetch-pdfium.* first), or None to skip.
pub fn pdfium() -> Option<Pdfium> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in ["vendor/pdfium/bin", "vendor/pdfium/lib"] {
        let lib = Pdfium::pdfium_platform_library_name_at_path(&root.join(dir));
        if lib.exists() {
            return Some(match Pdfium::bind_to_library(lib) {
                Ok(b) => Pdfium::new(b),
                // The library binds once per process; later instances reuse the binding.
                Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Pdfium::default(),
                Err(e) => panic!("cannot load PDFium: {e:?}"),
            });
        }
    }
    eprintln!("skipped: PDFium not found in vendor/pdfium");
    None
}

/// testdata/attention.pdf: any PDF with at least 15 pages works; not in the repo.
pub fn sample() -> Option<PathBuf> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/attention.pdf");
    if p.exists() {
        Some(p)
    } else {
        eprintln!("skipped: {} not found", p.display());
        None
    }
}
