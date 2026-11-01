//! Shared core of NopePDF, used by the viewer (`nopepdf`) and the command-line tool
//! (`nopepdf-cli`): PDFium loading and document opening, printing, translations.

pub mod i18n;
pub mod pdf;
pub mod print;

#[cfg(test)]
mod testutil;
