//! Protocole IRC : analyse des lignes, CTCP, comparaison des pseudos, découpage des messages longs.

/// Ligne IRC analysée (les tags IRCv3 éventuels sont ignorés).
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub prefix: Option<String>,
    pub command: String,
    pub params: Vec<String>,
}

impl Message {
    pub fn parse(line: &str) -> Option<Message> {
        let mut rest = line.trim_end_matches(['\r', '\n']);
        if let Some(tagged) = rest.strip_prefix('@') {
            rest = tagged.split_once(' ')?.1;
        }
        rest = rest.trim_start_matches(' ');
        let mut prefix = None;
        if let Some(p) = rest.strip_prefix(':') {
            let (p, r) = p.split_once(' ')?;
            prefix = Some(p.to_owned());
            rest = r.trim_start_matches(' ');
        }
        let (command, mut rest) = rest.split_once(' ').unwrap_or((rest, ""));
        if command.is_empty() {
            return None;
        }
        let mut params = Vec::new();
        loop {
            rest = rest.trim_start_matches(' ');
            if rest.is_empty() {
                break;
            }
            if let Some(trailing) = rest.strip_prefix(':') {
                params.push(trailing.to_owned());
                break;
            }
            let (p, r) = rest.split_once(' ').unwrap_or((rest, ""));
            params.push(p.to_owned());
            rest = r;
        }
        Some(Message {
            prefix,
            command: command.to_ascii_uppercase(),
            params,
        })
    }

    /// Pseudo de l'émetteur (`nick!user@host`), ou nom du serveur.
    pub fn nick(&self) -> &str {
        self.prefix.as_deref().map(nick_of).unwrap_or("")
    }

    /// `user@host` de l'émetteur, s'il est connu.
    pub fn userhost(&self) -> &str {
        self.prefix
            .as_deref()
            .and_then(|p| p.split_once('!'))
            .map(|(_, uh)| uh)
            .unwrap_or("")
    }

    /// Message envoyé par un serveur (pas de `!` dans le préfixe).
    pub fn by_server(&self) -> bool {
        self.prefix.as_deref().is_none_or(|p| !p.contains('!'))
    }

    pub fn param(&self, i: usize) -> &str {
        self.params.get(i).map(String::as_str).unwrap_or("")
    }

    /// Dernier paramètre (le texte, en général).
    pub fn last(&self) -> &str {
        self.params.last().map(String::as_str).unwrap_or("")
    }
}

pub fn nick_of(prefix: &str) -> &str {
    prefix.split('!').next().unwrap_or(prefix)
}

/// Clé de comparaison insensible à la casse IRC (rfc1459 : `[]\~` sont les majuscules de `{}|^`).
pub fn lower(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '[' => '{',
            ']' => '}',
            '\\' => '|',
            '~' => '^',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

pub fn eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && lower(a) == lower(b)
}

/// Contenu d'un message CTCP (`\x01COMMANDE arguments\x01`) : (commande en majuscules, arguments).
pub fn ctcp(text: &str) -> Option<(String, &str)> {
    let inner = text.strip_prefix('\x01')?;
    let inner = inner.strip_suffix('\x01').unwrap_or(inner);
    let (cmd, args) = inner.split_once(' ').unwrap_or((inner, ""));
    Some((cmd.to_ascii_uppercase(), args))
}

/// Découpe un texte en morceaux d'au plus `max` octets, de préférence sur un espace.
pub fn split_text(text: &str, max: usize) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = text;
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        if let Some(space) = rest[..cut].rfind(' ').filter(|&i| i > max / 2) {
            cut = space;
        }
        parts.push(&rest[..cut]);
        rest = rest[cut..].strip_prefix(' ').unwrap_or(&rest[cut..]);
    }
    if !rest.is_empty() || parts.is_empty() {
        parts.push(rest);
    }
    parts
}

/// Décode une ligne reçue : UTF-8, et Windows-1252 (vieux clients IRC) pour les seuls octets
/// qui ne sont pas de l'UTF-8 valide. Une ligne qui mélange les deux (texte UTF-8 cité par un
/// client en Windows-1252…) garde ainsi ses accents au lieu de devenir « Ã© » ou « ï¿½ ».
pub fn decode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        text.extend(chunk.invalid().iter().map(|&b| cp1252(b)));
    }
    text
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘',
        '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9f => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// Encodage base64 (authentification SASL PLAIN).
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lines() {
        let m = Message::parse(":nick!user@host PRIVMSG #chan :hello world\r\n").unwrap();
        assert_eq!(m.nick(), "nick");
        assert_eq!(m.userhost(), "user@host");
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.params, ["#chan", "hello world"]);
        assert!(!m.by_server());

        let m = Message::parse("@time=2024-01-01T00:00:00Z :irc.example.net 001 me :Welcome").unwrap();
        assert_eq!(m.nick(), "irc.example.net");
        assert!(m.by_server());
        assert_eq!(m.params, ["me", "Welcome"]);

        let m = Message::parse("PING :12345").unwrap();
        assert_eq!(m.prefix, None);
        assert_eq!(m.last(), "12345");

        let m = Message::parse(":s 353 me = #c :@op +voice  user").unwrap();
        assert_eq!(m.params, ["me", "=", "#c", "@op +voice  user"]);
        assert_eq!(Message::parse(""), None);
    }

    #[test]
    fn casemapping() {
        assert!(eq("Nick[a]", "nick{A}"));
        assert!(!eq("nick", "nick_"));
    }

    #[test]
    fn ctcp_parsing() {
        assert_eq!(ctcp("\x01ACTION waves\x01"), Some(("ACTION".into(), "waves")));
        assert_eq!(ctcp("\x01version\x01"), Some(("VERSION".into(), "")));
        assert_eq!(ctcp("hello"), None);
    }

    #[test]
    fn splitting() {
        assert_eq!(split_text("hello", 10), ["hello"]);
        assert_eq!(split_text("aaaa bbbb cccc", 10), ["aaaa bbbb", "cccc"]);
        assert_eq!(split_text("ééééé", 5), ["éé", "éé", "é"]);
        assert_eq!(split_text("", 5), [""]);
    }

    #[test]
    fn decoding() {
        assert_eq!(decode("é".as_bytes()), "é");
        assert_eq!(decode(b"caf\xe9 \x80"), "café €");
        assert_eq!(decode(b"\xc3\xa9t\xe9"), "été");
    }

    #[test]
    fn base64_encoding() {
        assert_eq!(base64(b"user\0user\0pass"), "dXNlcgB1c2VyAHBhc3M=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b""), "");
    }
}
