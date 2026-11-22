//! Police du texte : Fixedsys Excelsior 3.02 (github.com/kika/fixedsys, domaine public), en deux
//! variantes, ou un fichier de police au choix.
//!
//! Fixedsys est une police « pixel » 8×16 : elle n'est nette qu'à 16 pixels d'écran (ou un multiple).

use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use eframe::epaint::text::{FallbackRequest, FontInsert, FontProvider};

use crate::config::{Display, FontVariant};

const STANDARD: &[u8] = include_bytes!("../fonts/FSEX302.ttf");
const ALT: &[u8] = include_bytes!("../fonts/FSEX302-alt.ttf");

/// Symboles des petits boutons, présents dans les polices intégrées (Fixedsys, Hack) : la police
/// proportionnelle d'egui n'a pas les trois premiers, ils s'afficheraient en carré.
pub const SYMBOL_UP: &str = "▲";
pub const SYMBOL_DOWN: &str = "▼";
pub const SYMBOL_PLAY: &str = "▶";
pub const SYMBOL_REMOVE: &str = "×";

/// Famille réservée aux symboles, indépendante de la police choisie pour le texte.
const SYMBOLS: &str = "symboles";

/// Petit bouton à symbole, avec sa bulle d'aide.
pub fn symbol_button(ui: &mut egui::Ui, symbol: &str, hover: &str) -> bool {
    let text = egui::RichText::new(symbol).family(FontFamily::Name(SYMBOLS.into()));
    ui.small_button(text).on_hover_text(hover).clicked()
}

/// Police du texte à chasse fixe (discussions, liste des pseudos, saisie) ; les polices d'egui
/// (et celles du système) prennent le relais pour les caractères absents. Une police
/// personnalisée illisible est remplacée par Fixedsys, avec le message d'erreur.
pub fn install(ctx: &egui::Context, display: &Display) -> Result<(), String> {
    let mut result = Ok(());
    let bytes = match display.font {
        FontVariant::Standard => STANDARD.to_vec(),
        FontVariant::Alt => ALT.to_vec(),
        FontVariant::Custom => {
            // egui s'arrête net sur un fichier qui n'est pas une police : on le vérifie avant.
            let bytes = std::fs::read(&display.custom_font)
                .map_err(|e| e.to_string())
                .and_then(|bytes| match skrifa::FontRef::from_index(&bytes, 0) {
                    Ok(_) => Ok(bytes),
                    Err(e) => Err(tr!("police illisible ({e})", "unreadable font ({e})")),
                });
            bytes.unwrap_or_else(|e| {
                let path = display.custom_font.display();
                result = Err(tr!("Police {path} : {e}", "Font {path}: {e}"));
                STANDARD.to_vec()
            })
        }
    };
    let font = |bytes: Vec<u8>| Arc::new(FontData::from_owned(without_bitmaps(&bytes).unwrap_or(bytes)));
    let mut fonts = FontDefinitions::default();
    let monospace = fonts.families.entry(FontFamily::Monospace).or_default();
    let mut symbols = monospace.clone();
    monospace.insert(0, "texte".into());
    fonts.font_data.insert("texte".into(), font(bytes));
    // Symboles : Fixedsys puis les polices à chasse fixe d'egui, sans la police choisie.
    symbols.insert(0, SYMBOLS.into());
    fonts.font_data.insert(SYMBOLS.into(), font(STANDARD.to_vec()));
    fonts.families.insert(FontFamily::Name(SYMBOLS.into()), symbols);
    ctx.set_fonts(fonts);
    result
}

/// Police sans ses bitmaps intégrés (tables EBDT, EBLC, EBSC), seul le dessin vectoriel servant ;
/// `None` si elle n'en a pas. egui traite ces bitmaps comme des emojis en couleur, dessinés en
/// blanc quelle que soit la couleur du texte : Fixedsys Excelsior en a un, pour « A », qui
/// devenait invisible sur fond blanc. Les tables sont renommées (étiquettes inconnues, donc
/// ignorées), le répertoire des tables restant trié.
fn without_bitmaps(font: &[u8]) -> Option<Vec<u8>> {
    let read = |data: &[u8], at: usize, len: usize| -> Option<usize> {
        let bytes = data.get(at..at + len)?;
        Some(bytes.iter().fold(0, |n, &b| n << 8 | usize::from(b)))
    };
    // Collection (.ttc) : plusieurs polices, chacune avec son répertoire.
    let starts: Vec<usize> = if font.starts_with(b"ttcf") {
        let count = read(font, 8, 4).unwrap_or(0);
        (0..count).filter_map(|i| read(font, 12 + 4 * i, 4)).collect()
    } else {
        vec![0]
    };
    let mut data = None;
    for start in starts {
        let Some(count) = read(font, start + 4, 2) else {
            continue;
        };
        let directory = start + 12..start + 12 + 16 * count;
        let Some(records) = font.get(directory.clone()) else {
            continue;
        };
        let mut sorted: Vec<[u8; 16]> = records.as_chunks::<16>().0.to_vec();
        let mut found = false;
        for record in &mut sorted {
            if [b"EBDT", b"EBLC", b"EBSC"].iter().any(|tag| record[..4] == tag[..]) {
                record[..4].make_ascii_lowercase();
                found = true;
            }
        }
        if found {
            sorted.sort_by(|a, b| a[..4].cmp(&b[..4]));
            let data = data.get_or_insert_with(|| font.to_vec());
            data[directory].copy_from_slice(&sorted.concat());
        }
    }
    data
}

/// Polices du système, pour les caractères absents des autres (idéogrammes…), sans leurs bitmaps
/// intégrés (voir [`without_bitmaps`]).
pub struct SystemFonts(egui_system_font_provider::SystemFontProvider);

impl SystemFonts {
    pub fn new() -> Self {
        SystemFonts(egui_system_font_provider::SystemFontProvider::new())
    }
}

impl FontProvider for SystemFonts {
    fn font_for(&self, request: &FallbackRequest<'_>) -> Option<FontInsert> {
        let mut font = self.0.font_for(request)?;
        if let Some(bytes) = without_bitmaps(font.data.bytes()) {
            font.data.font = Arc::new(bytes);
        }
        Some(font)
    }
}

/// Taille en points : celle choisie pour une police personnalisée ; pour Fixedsys, 16 pixels
/// physiques × `font_scale` (0 : multiple le plus proche de l'échelle de l'écran).
pub fn size(display: &Display, pixels_per_point: f32) -> f32 {
    if display.font == FontVariant::Custom {
        return display.custom_font_size.clamp(6.0, 72.0);
    }
    let factor = if display.font_scale == 0 {
        pixels_per_point.round().max(1.0)
    } else {
        f32::from(display.font_scale)
    };
    16.0 * factor / pixels_per_point
}

/// Polices à chasse fixe de Windows proposées dans les options (celles présentes sur ce PC).
pub fn windows_fonts() -> Vec<(&'static str, std::path::PathBuf)> {
    let Some(dir) = std::env::var_os("WINDIR") else {
        return Vec::new();
    };
    let dir = std::path::PathBuf::from(dir).join("Fonts");
    [
        ("Consolas", "consola.ttf"),
        ("Lucida Console", "lucon.ttf"),
        ("Courier New", "cour.ttf"),
        ("Cascadia Mono", "CascadiaMono.ttf"),
    ]
    .into_iter()
    .map(|(name, file)| (name, dir.join(file)))
    .filter(|(_, path)| path.is_file())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crisp_sizes() {
        let mut display = Display::default();
        assert_eq!(size(&display, 1.0), 16.0);
        assert_eq!(size(&display, 2.0), 16.0);
        // 125 % : 16 pixels physiques.
        assert_eq!(size(&display, 1.25) * 1.25, 16.0);
        display.font_scale = 2;
        assert_eq!(size(&display, 1.0), 32.0);
        display.font = FontVariant::Custom;
        assert_eq!(size(&display, 1.25), 13.0);
    }

    /// Les symboles des petits boutons existent dans les polices intégrées (sinon : un carré).
    #[test]
    fn button_symbols() {
        let ctx = egui::Context::default();
        install(&ctx, &Display::default()).unwrap();
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        let font = egui::FontId::new(14.0, FontFamily::Name(SYMBOLS.into()));
        ctx.fonts_mut(|f| {
            for symbol in [SYMBOL_UP, SYMBOL_DOWN, SYMBOL_PLAY, SYMBOL_REMOVE] {
                assert!(f.has_glyphs(&font, symbol), "{symbol}");
            }
        });
    }

    /// Le « A » de Fixedsys Excelsior a un bitmap intégré, dessiné en blanc par egui : retiré,
    /// il reste le contour vectoriel, et les autres tables sont toujours trouvées.
    #[test]
    fn embedded_bitmaps() {
        use skrifa::MetadataProvider;
        let bitmap_a = |font: &skrifa::FontRef| {
            let glyph = font.charmap().map('A').unwrap();
            font.bitmap_strikes()
                .glyph_for_size(skrifa::instance::Size::new(16.0), glyph)
                .is_some()
        };
        for data in [STANDARD, ALT] {
            assert!(bitmap_a(&skrifa::FontRef::new(data).unwrap()));
            let stripped = without_bitmaps(data).unwrap();
            // Plus rien à retirer.
            assert!(without_bitmaps(&stripped).is_none());
            let font = skrifa::FontRef::new(&stripped).unwrap();
            assert!(!bitmap_a(&font));
            let glyph = font.charmap().map('A').unwrap();
            assert!(font.outline_glyphs().get(glyph).is_some());
            assert!(
                font.metrics(
                    skrifa::instance::Size::new(16.0),
                    skrifa::instance::LocationRef::default()
                )
                .ascent
                    > 0.0
            );
        }
    }

    /// Texte noir : aucun caractère dessiné dans une autre couleur (le « A » était blanc).
    #[test]
    fn text_color() {
        let ctx = egui::Context::default();
        install(&ctx, &Display::default()).unwrap();
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            ui.label(
                egui::RichText::new("ABCDEFGHIJKLMNOPQRSTUVWXYZ abcdefghijklmnopqrstuvwxyz 0123456789")
                    .monospace()
                    .color(egui::Color32::BLACK),
            );
        });
        output.textures_delta.clear();
        for primitive in ctx.tessellate(output.shapes, 1.0) {
            if let egui::epaint::Primitive::Mesh(mesh) = primitive.primitive {
                assert!(mesh.vertices.iter().all(|v| v.color == egui::Color32::BLACK));
            }
        }
    }

    #[test]
    fn missing_custom_font() {
        let mut display = Display {
            font: FontVariant::Custom,
            custom_font: "/introuvable.ttf".into(),
            ..Default::default()
        };
        assert!(install(&egui::Context::default(), &display).is_err());
        // Un fichier qui n'est pas une police : refusé, sans planter egui.
        display.custom_font = file!().into();
        let ctx = egui::Context::default();
        assert!(install(&ctx, &display).is_err());
        ctx.run_ui(Default::default(), |ui| {
            ui.monospace("texte");
        })
        .textures_delta
        .clear();
    }
}
