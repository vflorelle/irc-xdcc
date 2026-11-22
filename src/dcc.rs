//! DCC : analyse des offres SEND / ACCEPT et réception des fichiers (DCC actif et passif).

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::config::{Dcc, IgnoreMode};

/// `DCC SEND fichier ip port [taille [jeton]]` ; un port 0 avec jeton = DCC passif (« reverse »).
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub filename: String,
    pub ip: Option<IpAddr>,
    pub port: u16,
    pub size: Option<u64>,
    pub token: Option<String>,
}

impl Offer {
    /// L'émetteur ne peut pas recevoir de connexion : c'est à nous d'écouter.
    pub fn passive(&self) -> bool {
        self.port == 0 && self.token.is_some()
    }
}

/// `DCC ACCEPT fichier port position [jeton]` : réponse à notre demande de reprise.
#[derive(Debug, Clone, PartialEq)]
pub struct Accept {
    pub port: u16,
    pub position: u64,
    pub token: Option<String>,
}

/// Sépare le nom de fichier (entre guillemets ou non) des paramètres numériques qui le suivent.
fn split_filename(args: &str, max_tail: usize, is_tail: impl Fn(&[&str]) -> bool) -> Option<(String, Vec<String>)> {
    let args = args.trim();
    if let Some(quoted) = args.strip_prefix('"') {
        let (name, rest) = quoted.split_once('"')?;
        return Some((name.to_owned(), rest.split_whitespace().map(String::from).collect()));
    }
    let words: Vec<&str> = args.split_whitespace().collect();
    // Le plus grand nombre de paramètres finaux plausibles, en laissant au moins un mot au nom.
    let tail = (1..=max_tail.min(words.len().saturating_sub(1)))
        .rev()
        .find(|&n| is_tail(&words[words.len() - n..]))?;
    let (name, tail) = words.split_at(words.len() - tail);
    Some((name.join(" "), tail.iter().map(|s| s.to_string()).collect()))
}

fn numeric(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

pub fn parse_send(args: &str) -> Option<Offer> {
    let (filename, tail) = split_filename(args, 4, |t| {
        parse_ip(t[0]).is_some()
            && t.len() >= 2
            && numeric(t[1])
            && t.get(2).is_none_or(|s| numeric(s))
            // Un jeton (4ᵉ paramètre) n'existe qu'en DCC passif (port 0).
            && (t.len() < 4 || t[1] == "0")
    })?;
    if filename.is_empty() || tail.len() < 2 {
        return None;
    }
    Some(Offer {
        filename,
        ip: parse_ip(&tail[0]),
        port: tail[1].parse().ok()?,
        size: tail.get(2).and_then(|s| s.parse().ok()).filter(|&s| s > 0),
        token: tail.get(3).cloned(),
    })
}

pub fn parse_accept(args: &str) -> Option<Accept> {
    let (_, tail) = split_filename(args, 3, |t| {
        t.len() >= 2 && numeric(t[0]) && numeric(t[1]) && (t.len() < 3 || t[0] == "0")
    })?;
    Some(Accept {
        port: tail.first()?.parse().ok()?,
        position: tail.get(1)?.parse().ok()?,
        token: tail.get(2).cloned(),
    })
}

/// Adresse DCC : entier (IPv4 historique) ou texte (IPv6, IPv4 pointée).
pub fn parse_ip(s: &str) -> Option<IpAddr> {
    if numeric(s) {
        s.parse::<u32>().ok().map(|n| IpAddr::V4(Ipv4Addr::from(n)))
    } else {
        s.parse().ok()
    }
}

pub fn format_ip(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => u32::from(v4).to_string(),
        IpAddr::V6(v6) => v6.to_string(),
    }
}

/// Nom de fichier entre guillemets s'il contient des espaces.
pub fn quote(filename: &str) -> String {
    if filename.contains(' ') {
        format!("\"{filename}\"")
    } else {
        filename.to_owned()
    }
}

/// Nom de fichier sans chemin ni caractères interdits sous Windows.
pub fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base
        .chars()
        .map(|c| {
            if c.is_control() || "<>:\"|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let clean = clean.trim().trim_end_matches('.').trim_start_matches('.');
    let stem = clean.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if clean.is_empty() {
        "fichier".into()
    } else if reserved {
        format!("_{clean}")
    } else {
        clean.to_owned()
    }
}

/// Nom du fichier (sans le dossier), pour l'affichage.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Délai d'inactivité des transferts (et d'attente d'une réponse de reprise).
pub fn idle_timeout(config: &Dcc) -> Duration {
    Duration::from_secs(config.timeout_secs.max(10))
}

/// Premier nom libre : `fichier (1).ext`, `fichier (2).ext`…
pub fn unique_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (1..)
        .map(|i| path.with_file_name(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_owned())
}

/// Correspondance insensible à la casse avec les jokers `*` et `?`.
pub fn wildcard(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Raison du refus d'un fichier d'après la liste d'ignorés, s'il est refusé.
pub fn ignored(config: &Dcc, nick: &str, filename: &str) -> Option<String> {
    if config.ignore_nicks.iter().any(|n| wildcard(n, nick)) {
        return Some(tr!("pseudo ignoré", "ignored nick"));
    }
    let listed = config.ignore_files.iter().any(|p| wildcard(p.trim(), filename));
    match config.ignore_mode {
        IgnoreMode::Disabled => None,
        IgnoreMode::IgnoreListed => listed.then(|| tr!("type de fichier ignoré", "ignored file type")),
        IgnoreMode::AcceptOnlyListed => (!listed).then(|| tr!("type de fichier non autorisé", "file type not allowed")),
    }
}

/// Ouvre un port d'écoute dans la plage configurée (DCC passif).
pub fn listen(config: &Dcc) -> Result<TcpListener, String> {
    let (first, last) = (config.port_first.max(1), config.port_last.max(config.port_first.max(1)));
    for port in first..=last {
        if let Ok(listener) = std::net::TcpListener::bind(("0.0.0.0", port)) {
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
            return TcpListener::from_std(listener).map_err(|e| e.to_string());
        }
    }
    Err(tr!(
        "aucun port libre entre {first} et {last}",
        "no free port between {first} and {last}"
    ))
}

pub enum Endpoint {
    Connect(SocketAddr),
    Listen(TcpListener),
}

pub struct Job {
    pub endpoint: Endpoint,
    pub path: PathBuf,
    /// Position de départ (reprise).
    pub offset: u64,
    pub size: Option<u64>,
    pub timeout: Duration,
    /// Position atteinte dans le fichier, lue par l'interface.
    pub position: Arc<AtomicU64>,
}

/// Reçoit le fichier ; `connected` est appelé quand la connexion est établie.
pub async fn receive(job: Job, connected: impl FnOnce()) -> Result<(), String> {
    let mut stream = match job.endpoint {
        Endpoint::Connect(addr) => timeout(job.timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| tr!("délai de connexion dépassé", "connection timed out"))?
            .map_err(|e| tr!("connexion impossible : {e}", "unable to connect: {e}"))?,
        Endpoint::Listen(listener) => {
            timeout(job.timeout, listener.accept())
                .await
                .map_err(|_| tr!("l'émetteur ne s'est pas connecté", "the sender did not connect"))?
                .map_err(|e| e.to_string())?
                .0
        }
    };
    connected();

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(job.offset == 0)
        .open(&job.path)
        .await
        .map_err(|e| {
            let path = job.path.display();
            tr!("{path} : {e}", "{path}: {e}")
        })?;
    if job.offset > 0 {
        // Reprise : tout ce qui suit la position demandée est réécrit.
        file.set_len(job.offset).await.map_err(|e| e.to_string())?;
        file.seek(std::io::SeekFrom::Start(job.offset))
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut file = BufWriter::with_capacity(1 << 20, file);
    let mut position = job.offset;
    job.position.store(position, Ordering::Relaxed);
    let mut buf = vec![0; 1 << 16];
    let result = loop {
        if job.size.is_some_and(|size| position >= size) {
            break Ok(());
        }
        let n = match timeout(job.timeout, stream.read(&mut buf)).await {
            Err(_) => {
                break Err(tr!(
                    "plus de données reçues (délai dépassé)",
                    "no more data received (timed out)"
                ));
            }
            Ok(Err(e)) => break Err(e.to_string()),
            Ok(Ok(0)) => {
                break match job.size {
                    Some(size) if position < size => Err(tr!(
                        "connexion fermée par l'émetteur",
                        "connection closed by the sender"
                    )),
                    _ => Ok(()),
                };
            }
            Ok(Ok(n)) => n,
        };
        if let Err(e) = file.write_all(&buf[..n]).await {
            break Err(tr!("écriture : {e}", "write error: {e}"));
        }
        position += n as u64;
        job.position.store(position, Ordering::Relaxed);
        // Accusé de réception : position sur 32 bits (modulo 4 Gio au-delà), ignoré par beaucoup de bots.
        let _ = stream.write_all(&(position as u32).to_be_bytes()).await;
    };
    let flushed = file
        .flush()
        .await
        .map_err(|e| tr!("écriture : {e}", "write error: {e}"));
    result.and(flushed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Option<IpAddr> {
        s.parse().ok()
    }

    #[test]
    fn send_offers() {
        let o = parse_send("file.mkv 3232235777 5000 123456").unwrap();
        assert_eq!(o.filename, "file.mkv");
        assert_eq!(o.ip, ip("192.168.1.1"));
        assert_eq!((o.port, o.size, o.token.as_deref()), (5000, Some(123456), None));
        assert!(!o.passive());

        let o = parse_send("\"my file.mkv\" 3232235777 0 99 42").unwrap();
        assert_eq!(o.filename, "my file.mkv");
        assert_eq!((o.port, o.size, o.token.as_deref()), (0, Some(99), Some("42")));
        assert!(o.passive());

        let o = parse_send("Show.S01E01.mkv 3232235777 0 99 7").unwrap();
        assert_eq!(o.filename, "Show.S01E01.mkv");
        assert!(o.passive());

        let o = parse_send("a.txt ::1 4000 10").unwrap();
        assert_eq!((o.ip, o.port, o.size), (ip("::1"), 4000, Some(10)));

        let o = parse_send("old.txt 3232235777 4000").unwrap();
        assert_eq!((o.filename.as_str(), o.size), ("old.txt", None));

        // Nom non entre guillemets avec espaces.
        let o = parse_send("my file 2.txt 3232235777 4000 10").unwrap();
        assert_eq!(o.filename, "my file 2.txt");

        assert!(parse_send("3232235777 4000").is_none());
        assert!(parse_send("").is_none());
    }

    #[test]
    fn accepts() {
        let a = parse_accept("file.mkv 5000 1024").unwrap();
        assert_eq!((a.port, a.position, a.token), (5000, 1024, None));
        let a = parse_accept("\"my file\" 0 1024 42").unwrap();
        assert_eq!((a.port, a.position, a.token.as_deref()), (0, 1024, Some("42")));
        let a = parse_accept("file.mkv 0 1024 42").unwrap();
        assert_eq!(a.token.as_deref(), Some("42"));
    }

    #[test]
    fn ips() {
        assert_eq!(format_ip(ip("192.168.1.1").unwrap()), "3232235777");
        assert_eq!(parse_ip("192.168.1.1"), ip("192.168.1.1"));
        assert_eq!(parse_ip("nope"), None);
    }

    #[test]
    fn filenames() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("C:\\Windows\\a?b.txt"), "a_b.txt");
        assert_eq!(sanitize("con.txt"), "_con.txt");
        assert_eq!(sanitize(".."), "fichier");
        assert_eq!(sanitize("COM1"), "_COM1");
        assert_eq!(sanitize("Comics.cbz"), "Comics.cbz");
        assert_eq!(quote("a b"), "\"a b\"");
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("*.exe", "Setup.EXE"));
        assert!(!wildcard("*.exe", "file.exe.mkv"));
        assert!(wildcard("a?c*", "abcdef"));
        assert!(wildcard("*", ""));
        assert!(!wildcard("a", "ab"));
    }

    #[test]
    fn ignore_list() {
        let mut config = Dcc::default();
        assert!(ignored(&config, "bot", "virus.exe").is_some());
        assert!(ignored(&config, "bot", "movie.mkv").is_none());
        config.ignore_mode = IgnoreMode::AcceptOnlyListed;
        assert!(ignored(&config, "bot", "movie.mkv").is_some());
        config.ignore_mode = IgnoreMode::Disabled;
        config.ignore_nicks = vec!["bad*".into()];
        assert!(ignored(&config, "BadBot", "movie.mkv").is_some());
    }

    #[tokio::test]
    async fn receive_with_resume() {
        let dir = std::env::temp_dir().join(format!("irc-xdcc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.bin");
        std::fs::write(&path, b"hello?????").unwrap();

        let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        let sender = tokio::spawn(async move {
            let (mut s, _) = server.accept().await.unwrap();
            s.write_all(b" world").await.unwrap();
            let mut ack = [0; 4];
            s.read_exact(&mut ack).await.unwrap();
            u32::from_be_bytes(ack)
        });
        let position = Arc::new(AtomicU64::new(0));
        let job = Job {
            endpoint: Endpoint::Connect(addr),
            path: path.clone(),
            offset: 5,
            size: Some(11),
            timeout: Duration::from_secs(5),
            position: position.clone(),
        };
        receive(job, || {}).await.unwrap();
        assert_eq!(sender.await.unwrap(), 11);
        assert_eq!(std::fs::read(&path).unwrap(), b"hello world");
        assert_eq!(position.load(Ordering::Relaxed), 11);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
