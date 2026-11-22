// Pas de console noire derrière la fenêtre sous Windows (en release).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

// En premier : la macro `tr!` sert dans tous les modules.
#[macro_use]
mod i18n;

mod app;
mod config;
mod core;
mod dcc;
#[cfg(windows)]
mod explorer;
mod fonts;
mod format;
mod ident;
mod irc;
mod net;
mod options;
#[cfg(test)]
mod scenario;
mod sound;
mod state;
mod transfers;
mod upnp;

use eframe::egui;

fn main() {
    let loaded = config::Config::load();
    i18n::set(loaded.0.display.language);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app::APP_NAME)
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([500.0, 300.0]),
        // Les polices du système sont ajoutées par l'application (fonts::SystemFonts).
        system_font_fallback: false,
        ..Default::default()
    };
    let result = eframe::run_native(
        app::APP_NAME,
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc, loaded)))),
    );
    if let Err(e) = result {
        // En mode fenêtré il n'y a pas de console : on affiche l'erreur dans une boîte de dialogue.
        rfd::MessageDialog::new()
            .set_title(app::APP_NAME)
            .set_description(tr!("Impossible de démarrer : {e}", "Unable to start: {e}"))
            .set_level(rfd::MessageLevel::Error)
            .show();
        std::process::exit(1);
    }
}
