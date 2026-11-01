//! PDFium loading and document opening, shared by the viewer and the command-line tool.

use crate::i18n::{tr, tr_args};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

/// Look for the PDFium library next to the executable first, then (debug builds
/// only) in the vendor directory of the source tree, then on the system search path.
/// Release builds must not embed or search the build machine's paths.
pub fn load_pdfium() -> Result<Pdfium, String> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())) {
        dirs.push(dir);
    }
    #[cfg(debug_assertions)]
    {
        dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/pdfium/bin"));
        dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor/pdfium/lib"));
    }
    for d in &dirs {
        let lib = Pdfium::pdfium_platform_library_name_at_path(d);
        if lib.exists()
            && let Ok(b) = Pdfium::bind_to_library(&lib)
        {
            return Ok(Pdfium::new(b));
        }
    }
    Pdfium::bind_to_system_library().map(Pdfium::new).map_err(|e| format!("{e:?}"))
}

/// Why a document could not be opened. The viewer translates it when drawing, so a
/// language switch also changes messages already on screen.
#[derive(Clone, Debug, PartialEq)]
pub enum OpenError {
    Read(String),
    InvalidPdf,
    UnsupportedSecurity,
    NeedsPassword,
    Other(String),
}

impl OpenError {
    fn from_pdfium(e: PdfiumError) -> Self {
        match e {
            PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => OpenError::NeedsPassword,
            PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::FormatError) => OpenError::InvalidPdf,
            PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::SecurityError) => {
                OpenError::UnsupportedSecurity
            }
            e => OpenError::Other(format!("{e:?}")),
        }
    }

    pub fn message(&self) -> String {
        match self {
            OpenError::Read(e) => tr_args("error.read", &[("error", e)]),
            OpenError::InvalidPdf => tr("error.invalid_pdf").into(),
            OpenError::UnsupportedSecurity => tr("error.unsupported_security").into(),
            OpenError::NeedsPassword => tr("doc.password_title").into(),
            OpenError::Other(e) => tr_args("error.other", &[("error", e)]),
        }
    }
}

/// Opens a document from memory: the whole file is read first so no handle stays open,
/// and the file can be overwritten, renamed or deleted while it is being viewed.
pub fn open_document<'a>(
    pdfium: &'a Pdfium,
    path: &Path,
    password: Option<&str>,
) -> Result<PdfDocument<'a>, OpenError> {
    let bytes = std::fs::read(path).map_err(|e| OpenError::Read(e.to_string()))?;
    pdfium.load_pdf_from_byte_vec(bytes, password).map_err(OpenError::from_pdfium)
}
