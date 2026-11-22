//! Codes de mise en forme IRC (couleurs, gras, italique, souligné…) et liens cliquables.

use std::ops::Range;

use eframe::egui::text::{ByteIndex, LayoutJob, LayoutSection};
use eframe::egui::{Color32, Stroke, TextFormat};

const BOLD: char = '\x02';
const COLOR: char = '\x03';
const HEX_COLOR: char = '\x04';
const RESET: char = '\x0f';
const MONOSPACE: char = '\x11';
const REVERSE: char = '\x16';
const ITALIC: char = '\x1d';
const STRIKE: char = '\x1e';
const UNDERLINE: char = '\x1f';

/// Les 99 couleurs IRC (0-15 classiques, 16-98 étendues).
#[rustfmt::skip]
const PALETTE: [u32; 99] = [
    0xffffff, 0x000000, 0x00007f, 0x009300, 0xff0000, 0x7f0000, 0x9c009c, 0xfc7f00,
    0xffff00, 0x00fc00, 0x009393, 0x00ffff, 0x0000fc, 0xff00ff, 0x7f7f7f, 0xd2d2d2,
    0x470000, 0x472100, 0x474700, 0x324700, 0x004700, 0x00472c, 0x004747, 0x002747, 0x000047, 0x2e0047, 0x470047, 0x47002a,
    0x740000, 0x743a00, 0x747400, 0x517400, 0x007400, 0x007449, 0x007474, 0x004074, 0x000074, 0x4b0074, 0x740074, 0x740045,
    0xb50000, 0xb56300, 0xb5b500, 0x7db500, 0x00b500, 0x00b571, 0x00b5b5, 0x0063b5, 0x0000b5, 0x7500b5, 0xb500b5, 0xb5006b,
    0xff0000, 0xff8c00, 0xffff00, 0xb2ff00, 0x00ff00, 0x00ffa0, 0x00ffff, 0x008cff, 0x0000ff, 0xa500ff, 0xff00ff, 0xff0098,
    0xff5959, 0xffb459, 0xffff71, 0xcfff60, 0x6fff6f, 0x65ffc9, 0x6dffff, 0x59b4ff, 0x5959ff, 0xc459ff, 0xff66ff, 0xff59bc,
    0xff9c9c, 0xffd39c, 0xffff9c, 0xe2ff9c, 0x9cff9c, 0x9cffdb, 0x9cffff, 0x9cd3ff, 0x9c9cff, 0xdc9cff, 0xff9cff, 0xff94d3,
    0x000000, 0x131313, 0x282828, 0x363636, 0x4d4d4d, 0x656565, 0x818181, 0x9f9f9f, 0xbcbcbc, 0xe2e2e2, 0xffffff,
];

pub fn irc_color(index: usize) -> Color32 {
    rgb(PALETTE[index.min(PALETTE.len() - 1)])
}

fn rgb(value: u32) -> Color32 {
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    reverse: bool,
    fg: Option<Color32>,
    bg: Option<Color32>,
}

/// Une instruction de mise en forme, ou du texte.
enum Token<'a> {
    Text(&'a str),
    Toggle(char),
    Color(Option<Color32>, Option<Color32>),
    ResetColors,
    Reset,
}

fn digits(s: &str, max: usize, radix: u32) -> usize {
    s.chars().take(max).take_while(|c| c.is_digit(radix)).count()
}

fn tokens(text: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let end = rest
            .find(|c: char| c.is_control() && c != '\t' || invisible(c))
            .unwrap_or(rest.len());
        if end > 0 {
            out.push(Token::Text(&rest[..end]));
            rest = &rest[end..];
            continue;
        }
        let c = rest.chars().next().expect("caractère");
        rest = &rest[c.len_utf8()..];
        match c {
            MONOSPACE => {}
            BOLD | ITALIC | UNDERLINE | STRIKE | REVERSE => out.push(Token::Toggle(c)),
            RESET => out.push(Token::Reset),
            COLOR => {
                let n = digits(rest, 2, 10);
                if n == 0 {
                    out.push(Token::ResetColors);
                    continue;
                }
                let fg = color_code(&rest[..n]);
                rest = &rest[n..];
                let mut bg = None;
                if let Some(after) = rest.strip_prefix(',') {
                    let m = digits(after, 2, 10);
                    if m > 0 {
                        bg = color_code(&after[..m]);
                        rest = &after[m..];
                    }
                }
                out.push(Token::Color(fg, bg));
            }
            HEX_COLOR => {
                if digits(rest, 6, 16) < 6 {
                    out.push(Token::ResetColors);
                    continue;
                }
                let fg = u32::from_str_radix(&rest[..6], 16).ok().map(rgb);
                rest = &rest[6..];
                let mut bg = None;
                if let Some(after) = rest.strip_prefix(',')
                    && digits(after, 6, 16) == 6
                {
                    bg = u32::from_str_radix(&after[..6], 16).ok().map(rgb);
                    rest = &after[6..];
                }
                out.push(Token::Color(fg, bg));
            }
            // Autres caractères de contrôle, caractères invisibles : ignorés.
            _ => {}
        }
    }
    out
}

/// Caractères sans dessin (espace sans chasse, trait d'union conditionnel…), glissés par exemple
/// dans un pseudo pour qu'il ne déclenche pas de mise en évidence. Une police qui ne les connaît
/// pas les afficherait comme un blanc au milieu du mot.
fn invisible(c: char) -> bool {
    matches!(c, '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{2060}'..='\u{2064}' | '\u{feff}')
}

/// 99 = couleur par défaut.
fn color_code(code: &str) -> Option<Color32> {
    code.parse::<usize>().ok().filter(|&n| n < 99).map(irc_color)
}

/// Texte sans codes de mise en forme.
pub fn strip(text: &str) -> String {
    tokens(text)
        .into_iter()
        .filter_map(|t| match t {
            Token::Text(s) => Some(s),
            _ => None,
        })
        .collect()
}

/// Ajoute un texte IRC (avec ses codes de mise en forme) à la mise en page, sur la base du
/// format donné ; `ground` : couleur du fond de la fenêtre. Renvoie les plages en gras (en
/// octets dans `job`), à dessiner une seconde fois décalées d'un pixel (voir [`bold_job`]).
pub fn append(job: &mut LayoutJob, text: &str, base: &TextFormat, colors: bool, ground: Color32) -> Vec<Range<usize>> {
    let mut style = Style::default();
    let mut bold = Vec::new();
    for token in tokens(text) {
        match token {
            Token::Text(s) => {
                let start = job.text.len();
                job.append(s, 0.0, apply(base, style, ground));
                if style.bold {
                    bold.push(start..job.text.len());
                }
            }
            Token::Toggle(c) => match c {
                BOLD => style.bold = !style.bold,
                ITALIC => style.italic = !style.italic,
                UNDERLINE => style.underline = !style.underline,
                STRIKE => style.strike = !style.strike,
                _ => style.reverse = !style.reverse,
            },
            Token::Color(fg, bg) if colors => {
                style.fg = fg;
                if bg.is_some() {
                    style.bg = bg;
                }
            }
            Token::Color(..) => {}
            Token::ResetColors => (style.fg, style.bg) = (None, None),
            Token::Reset => style = Style::default(),
        }
    }
    bold
}

fn apply(base: &TextFormat, style: Style, ground: Color32) -> TextFormat {
    let mut format = base.clone();
    if let Some(fg) = style.fg {
        format.color = fg;
    }
    if let Some(bg) = style.bg {
        format.background = bg;
    }
    if style.reverse {
        let bg = if format.background == Color32::TRANSPARENT {
            base.color
        } else {
            format.background
        };
        format.background = format.color;
        format.color = if bg == base.color { contrast(base.color) } else { bg };
    }
    // Texte de la couleur du fond (caché, ou pensé pour l'autre thème) : rendu lisible.
    let behind = if format.background == Color32::TRANSPARENT {
        ground
    } else {
        format.background
    };
    let gap = |a: u8, b: u8| a.abs_diff(b);
    let (c, g) = (format.color, behind);
    if gap(c.r(), g.r()).max(gap(c.g(), g.g())).max(gap(c.b(), g.b())) < 48 {
        format.color = contrast(behind);
    }
    format.italics = style.italic;
    if style.underline {
        format.underline = Stroke::new(1.0, format.color);
    }
    if style.strike {
        format.strikethrough = Stroke::new(1.0, format.color);
    }
    format
}

fn contrast(color: Color32) -> Color32 {
    let luma = 0.299 * color.r() as f32 + 0.587 * color.g() as f32 + 0.114 * color.b() as f32;
    if luma > 128.0 { Color32::BLACK } else { Color32::WHITE }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Link {
    Url(String),
    Channel(String),
}

/// Adresses web et noms de canaux dans un texte (plages en octets).
pub fn links(text: &str, chantypes: &str) -> Vec<(Range<usize>, Link)> {
    let mut found = Vec::new();
    // Position réelle de chaque mot : les espaces Unicode (insécable…) font plusieurs octets.
    let words = text
        .split(char::is_whitespace)
        .map(|w| (w.as_ptr() as usize - text.as_ptr() as usize, w));
    for (begin, word) in words {
        let trimmed = word.trim_start_matches(['(', '<', '[', '"', '\'']);
        let offset = begin + word.len() - trimmed.len();
        let trimmed = trimmed.trim_end_matches(['.', ',', ')', '>', ']', '"', '\'', '!', '?', ':', ';']);
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        let range = offset..offset + trimmed.len();
        if ["http://", "https://", "ftp://"]
            .iter()
            .any(|p| lower.starts_with(p) && lower.len() > p.len())
        {
            found.push((range, Link::Url(trimmed.to_owned())));
        } else if lower.starts_with("www.") && lower.len() > 4 {
            found.push((range, Link::Url(format!("http://{trimmed}"))));
        } else if offset == begin
            && let Some(channel) = channel_name(trimmed, chantypes)
        {
            // Le rang éventuel (« @#canal » d'un /whois) ne fait pas partie du lien.
            let start = range.end - channel.len();
            found.push((start..range.end, Link::Channel(channel.to_owned())));
        }
    }
    found
}

/// Nom de canal d'un mot, sans le rang qui peut le précéder (`~`, `&`, `@`, `%`, `+`, comme
/// dans « @#canal ») ; « #5 » est un numéro de pack XDCC, pas un canal.
fn channel_name<'a>(word: &'a str, chantypes: &str) -> Option<&'a str> {
    const RANKS: [char; 5] = ['~', '&', '@', '%', '+'];
    let is_channel = |s: &str| s.starts_with(|c| chantypes.contains(c));
    let mut name = word;
    // « &canal » peut être un canal local (& dans CHANTYPES) : le rang n'est retiré que s'il est
    // suivi d'un canal ou d'un autre rang.
    while let Some(rest) = name.strip_prefix(RANKS)
        && (is_channel(rest) || rest.starts_with(RANKS))
    {
        name = rest;
    }
    (name.len() > 1 && is_channel(name) && !name[1..].bytes().all(|b| b.is_ascii_digit())).then_some(name)
}

/// Souligne les plages données (liens) dans la mise en page.
pub fn underline(job: &mut LayoutJob, ranges: &[Range<usize>]) {
    if !ranges.is_empty() {
        restyle(job, ranges, |format, inside| {
            if inside {
                format.underline = Stroke::new(1.0, format.color);
            }
        });
    }
}

/// Copie de la mise en page où seul le texte en gras (`bold`, plages de [`append`]) est visible :
/// dessinée un pixel à droite de l'originale, elle l'épaissit. Fixedsys n'a pas de variante
/// grasse, et c'est ainsi que Windows met en gras les polices bitmap.
pub fn bold_job(job: &LayoutJob, bold: &[Range<usize>]) -> Option<LayoutJob> {
    if bold.is_empty() {
        return None;
    }
    let mut copy = job.clone();
    restyle(&mut copy, bold, |format, inside| {
        format.background = Color32::TRANSPARENT;
        if !inside {
            format.color = Color32::TRANSPARENT;
            format.underline = Stroke::NONE;
            format.strikethrough = Stroke::NONE;
        }
    });
    Some(copy)
}

/// Découpe les sections aux bords des plages et modifie chaque morceau (`inside` : dans une
/// plage).
fn restyle(job: &mut LayoutJob, ranges: &[Range<usize>], change: impl Fn(&mut TextFormat, bool)) {
    let mut sections = Vec::with_capacity(job.sections.len() + ranges.len() * 2);
    for section in job.sections.drain(..) {
        let (start, end) = (section.byte_range.start.0, section.byte_range.end.0);
        let mut cuts = vec![start, end];
        for r in ranges {
            for b in [r.start, r.end] {
                if b > start && b < end {
                    cuts.push(b);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for (i, pair) in cuts.windows(2).enumerate() {
            let mut part = LayoutSection {
                leading_space: if i == 0 { section.leading_space } else { 0.0 },
                byte_range: ByteIndex(pair[0])..ByteIndex(pair[1]),
                format: section.format.clone(),
            };
            let inside = ranges.iter().any(|r| r.start <= pair[0] && pair[1] <= r.end);
            change(&mut part.format, inside);
            sections.push(part);
        }
    }
    job.sections = sections;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stripping() {
        assert_eq!(strip("\x02gras\x02 \x0304,01rouge\x03 \x1fsou\x0f"), "gras rouge sou");
        assert_eq!(strip("\x0312345"), "345");
        assert_eq!(strip("\x04ff0000hex\x04"), "hex");
        assert_eq!(strip("\x03,5virgule"), ",5virgule");
        assert_eq!(strip("Bo\u{200b}b\u{ad}by"), "Bobby");
    }

    #[test]
    fn colors() {
        let mut job = LayoutJob::default();
        let base = TextFormat {
            color: Color32::BLACK,
            ..Default::default()
        };
        let white = Color32::WHITE;
        append(&mut job, "a\x0304b\x03c", &base, true, white);
        assert_eq!(job.text, "abc");
        assert_eq!(job.sections.len(), 3);
        assert_eq!(job.sections[1].format.color, irc_color(4));
        assert_eq!(job.sections[2].format.color, base.color);

        let mut job = LayoutJob::default();
        append(&mut job, "a\x0304b", &base, false, white);
        assert!(job.sections.iter().all(|s| s.format.color == base.color));

        // Blanc sur le fond blanc, noir sur fond noir : lisibles.
        let mut job = LayoutJob::default();
        append(&mut job, "\x0300blanc\x0301,01noir", &base, true, white);
        assert_eq!(job.sections[0].format.color, Color32::BLACK);
        assert_eq!(job.sections[1].format.color, Color32::WHITE);
        assert_eq!(job.sections[1].format.background, Color32::BLACK);
    }

    /// Gras : ses plages, et une copie où seul le gras reste visible.
    #[test]
    fn bold() {
        let mut job = LayoutJob::default();
        let base = TextFormat::default();
        let bold = append(&mut job, "a \x02gras\x02 b \x02x", &base, true, Color32::WHITE);
        assert_eq!(bold, [2..6, 9..10]);
        let copy = bold_job(&job, &bold).unwrap();
        let visible: String = copy
            .sections
            .iter()
            .filter(|s| s.format.color != Color32::TRANSPARENT)
            .map(|s| &copy.text[s.byte_range.start.0..s.byte_range.end.0])
            .collect();
        assert_eq!(visible, "grasx");
        assert!(bold_job(&job, &[]).is_none());
    }

    #[test]
    fn link_detection() {
        let found = links("voir (https://example.com/a?b=1). sur #canal et www.x.org", "#");
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].1, Link::Url("https://example.com/a?b=1".into()));
        assert_eq!(
            &"voir (https://example.com/a?b=1). sur"[found[0].0.clone()],
            "https://example.com/a?b=1"
        );
        assert_eq!(found[1].1, Link::Channel("#canal".into()));
        assert_eq!(found[2].1, Link::Url("http://www.x.org".into()));
        assert!(links("pack #5", "#").is_empty());
        let text = "voir\u{a0}https://x.org\u{3000}#chan";
        let found = links(text, "#");
        assert_eq!(&text[found[0].0.clone()], "https://x.org");
        assert_eq!(&text[found[1].0.clone()], "#chan");
        // Rangs d'un /whois : le lien ne couvre que le canal ; « &canal » local reste un canal.
        let text = "MyNick on @#un +#deux %#trois &local";
        let found = links(text, "#&");
        let names: Vec<&str> = found.iter().map(|(r, _)| &text[r.clone()]).collect();
        assert_eq!(names, ["#un", "#deux", "#trois", "&local"]);
        assert_eq!(found[0].1, Link::Channel("#un".into()));
    }

    #[test]
    fn underlining() {
        let mut job = LayoutJob::default();
        job.append("hello world", 0.0, TextFormat::default());
        let link = 6..11;
        underline(&mut job, std::slice::from_ref(&link));
        assert_eq!(job.sections.len(), 2);
        assert_eq!(job.sections[1].byte_range, ByteIndex(6)..ByteIndex(11));
        assert_ne!(job.sections[1].format.underline, Stroke::NONE);
    }
}
