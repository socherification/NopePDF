#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod instance;
mod print;
mod settings;
mod view;
mod worker;

use eframe::egui;
use pdfium_render::prelude::*;
use std::path::PathBuf;

/// Look for the PDFium library next to the executable first, then (debug builds
/// only) in the vendor directory of the source tree, then on the system search path.
/// Release builds must not embed or search the build machine's paths.
fn load_pdfium() -> Result<Pdfium, String> {
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
        if lib.exists() {
            if let Ok(b) = Pdfium::bind_to_library(&lib) {
                return Ok(Pdfium::new(b));
            }
        }
    }
    Pdfium::bind_to_system_library().map(Pdfium::new).map_err(|e| format!("{e:?}"))
}

/// Use the platform UI font when available for a native look; egui's
/// bundled fonts stay as fallback (they carry the icon glyphs).
fn setup_fonts(ctx: &egui::Context) {
    let candidates = [
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "/System/Library/Fonts/SFNS.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
    ];
    let mut fonts = egui::FontDefinitions::default();
    let mut added = false;
    for c in candidates {
        if let Ok(bytes) = std::fs::read(c) {
            fonts.font_data.insert("system".into(), egui::FontData::from_owned(bytes).into());
            fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "system".into());
            added = true;
            break;
        }
    }
    // Symbol font for toolbar glyphs not covered by egui's bundled fonts.
    if let Ok(bytes) = std::fs::read("C:\\Windows\\Fonts\\seguisym.ttf") {
        fonts.font_data.insert("symbols".into(), egui::FontData::from_owned(bytes).into());
        fonts.families.entry(egui::FontFamily::Proportional).or_default().push("symbols".into());
        added = true;
    }
    if added {
        ctx.set_fonts(fonts);
    }
}

fn main() -> eframe::Result {
    let cwd = std::env::current_dir().unwrap_or_default();
    let files: Vec<PathBuf> = std::env::args_os().skip(1).filter(|a| !a.is_empty()).map(|a| cwd.join(a)).collect();
    let primary = match instance::start(&files) {
        instance::Startup::Forwarded => return Ok(()),
        instance::Startup::Primary(p) => p,
    };
    let settings = settings::Settings::load();

    let pdfium = match load_pdfium() {
        Ok(p) => p,
        Err(e) => {
            rfd::MessageDialog::new()
                .set_title("NopePDF")
                .set_description(format!("Nie znaleziono biblioteki PDFium (pdfium.dll).\n\n{e}"))
                .set_level(rfd::MessageLevel::Error)
                .show();
            std::process::exit(1);
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("NopePDF")
            .with_inner_size([1100.0, 850.0])
            .with_min_inner_size([400.0, 300.0])
            .with_drag_and_drop(true)
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon256.png")).unwrap_or_default()),
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "NopePDF",
        options,
        Box::new(move |cc| {
            setup_fonts(&cc.egui_ctx);
            app::apply_theme(&cc.egui_ctx, settings.dark);
            cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
            let worker = worker::spawn(pdfium, cc.egui_ctx.clone());
            let ipc = primary.map(|p| instance::serve(p, cc.egui_ctx.clone()));
            Ok(Box::new(app::App::new(worker, settings, files, ipc)))
        }),
    )
}
