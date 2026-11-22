//! Configuration (identité, serveurs, favoris, DCC, XDCC, affichage), enregistrée en TOML.
//!
//! Emplacement : `irc-xdcc.toml` à côté de l'exécutable s'il existe (mode portable),
//! sinon `%APPDATA%\irc-xdcc\config.toml` (Windows) ou `~/.config/irc-xdcc/config.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub identity: Identity,
    pub connection: Connection,
    pub networks: Vec<Network>,
    pub favorites: Vec<Favorite>,
    pub dcc: Dcc,
    pub xdcc: Xdcc,
    pub sounds: Sounds,
    pub display: Display,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Identity {
    pub nick: String,
    /// Pseudo de secours s'il est déjà pris (puis `pseudo_`, `pseudo__`…).
    pub alt_nick: String,
    /// Nom d'utilisateur (ident).
    pub username: String,
    pub realname: String,
    /// Vide : déconnexion sans message.
    pub quit_message: String,
    /// Réponse aux demandes CTCP VERSION (logiciel utilisé) ; vide : aucune réponse.
    pub version_reply: String,
}

impl Identity {
    /// Nom d'utilisateur envoyé au serveur (et au serveur ident).
    pub fn user(&self) -> &str {
        match self.username.trim() {
            "" => "user",
            user => user,
        }
    }

    /// Nom réel envoyé au serveur (obligatoire : le nom d'utilisateur s'il est vide).
    pub fn real(&self) -> &str {
        match self.realname.trim() {
            "" => self.user(),
            real => real,
        }
    }
}

/// Valeurs neutres, qui ne disent rien du logiciel utilisé.
impl Default for Identity {
    fn default() -> Self {
        Identity {
            nick: String::new(),
            alt_nick: String::new(),
            username: "user".into(),
            realname: String::new(),
            quit_message: String::new(),
            version_reply: String::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Connection {
    /// Reconnexion automatique après une déconnexion non demandée.
    pub reconnect: bool,
    pub reconnect_delay_secs: u64,
    /// Rejoindre les canaux ouverts après une reconnexion.
    pub rejoin_channels: bool,
    /// Mots (en plus du pseudo) qui mettent un message en évidence.
    pub highlight_words: Vec<String>,
    /// Serveur ident (port 113) : le serveur IRC lui demande notre nom
    /// d'utilisateur ; sans réponse, il le préfixe d'un `~`.
    pub identd: bool,
    /// Mode invisible (+i) à la connexion : absent de /who et des listes des utilisateurs pour qui
    /// ne partage aucun canal avec nous.
    pub invisible: bool,
}

impl Default for Connection {
    fn default() -> Self {
        Connection {
            reconnect: true,
            reconnect_delay_secs: 15,
            rejoin_channels: true,
            highlight_words: Vec::new(),
            identd: true,
            invisible: true,
        }
    }
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerAddr {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    /// Certificat TLS accepté sans vérification (auto-signé, expiré…).
    pub accept_invalid_cert: bool,
}

impl ServerAddr {
    pub fn new(host: &str, port: u16, tls: bool) -> Self {
        ServerAddr {
            host: host.to_owned(),
            port,
            tls,
            accept_invalid_cert: false,
        }
    }

    /// Syntaxe de `/server` : `hôte`, `hôte:port` ou `hôte:+port` (TLS).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (host, port) = match text.rsplit_once(':') {
            // IPv6 sans port : plusieurs « : » et pas de crochets.
            Some((h, _)) if h.contains(':') && !h.ends_with(']') => (text, None),
            Some((h, p)) => (h, Some(p)),
            None => (text, None),
        };
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if host.is_empty() || host.contains(char::is_whitespace) {
            return None;
        }
        let (tls, port) = match port {
            Some(p) => match p.strip_prefix('+') {
                Some(p) => (true, p.parse().ok()?),
                None => (false, p.parse().ok()?),
            },
            None => (false, 6667),
        };
        Some(ServerAddr::new(host, port, tls))
    }
}

impl std::fmt::Display for ServerAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let plus = if self.tls { "+" } else { "" };
        if self.host.contains(':') {
            write!(f, "[{}]:{plus}{}", self.host, self.port)
        } else {
            write!(f, "{}:{plus}{}", self.host, self.port)
        }
    }
}

/// Réseau IRC : serveurs (essayés à tour de rôle) et réglages propres à ce réseau.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    pub name: String,
    pub servers: Vec<ServerAddr>,
    pub connect_on_startup: bool,
    /// Vides : l'identité générale est utilisée.
    pub nick: String,
    pub alt_nick: String,
    /// Mot de passe du serveur (commande PASS).
    pub password: String,
    /// Authentification SASL PLAIN si le compte est renseigné.
    pub sasl_account: String,
    pub sasl_password: String,
    /// Commandes exécutées à la connexion, une par ligne (ex. `/ns identify motdepasse`).
    pub perform: Vec<String>,
    /// Délai entre ces commandes et l'entrée dans les canaux favoris (le temps d'être identifié).
    pub join_delay_secs: u64,
}

/// Canal favori, rattaché à un réseau et éventuellement à un serveur précis (port TLS particulier…).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Favorite {
    pub channel: String,
    pub key: String,
    /// Nom du réseau (vide : seul le serveur compte).
    pub network: String,
    /// Hôte vide : les serveurs du réseau, à tour de rôle.
    pub server: ServerAddr,
    /// Rejoint automatiquement à chaque connexion à ce réseau / serveur.
    pub auto_join: bool,
}

/// Envoi DCC non sollicité (les packs XDCC demandés sont toujours acceptés).
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum OfferAction {
    Ask,
    Accept,
    Ignore,
}

/// Le fichier reçu existe déjà dans le dossier de téléchargement.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ExistsAction {
    Ask,
    Resume,
    Overwrite,
    Rename,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum IgnoreMode {
    Disabled,
    /// Refuse les fichiers dont le nom correspond à la liste.
    IgnoreListed,
    /// N'accepte que les fichiers dont le nom correspond à la liste.
    AcceptOnlyListed,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Dcc {
    /// Dossier local, lecteur réseau ou chemin réseau (`\\serveur\partage\…`).
    pub download_dir: PathBuf,
    pub on_offer: OfferAction,
    pub if_exists: ExistsAction,
    pub ignore_mode: IgnoreMode,
    /// Motifs avec jokers `*` et `?` (ex. `*.exe`).
    pub ignore_files: Vec<String>,
    /// Pseudos dont les envois non sollicités sont refusés.
    pub ignore_nicks: Vec<String>,
    /// Ports d'écoute pour les DCC passifs (l'émetteur ne peut pas recevoir de connexion).
    pub port_first: u16,
    pub port_last: u16,
    /// Adresse IP annoncée pour les DCC passifs ; vide : donnée par la box (UPnP) ou demandée au
    /// serveur IRC.
    pub external_ip: String,
    /// UPnP : la box ouvre d'elle-même les ports des DCC passifs et du serveur ident.
    pub upnp: bool,
    /// Abandon d'un transfert sans données reçues pendant ce délai.
    pub timeout_secs: u64,
}

impl Default for Dcc {
    fn default() -> Self {
        Dcc {
            download_dir: default_download_dir(),
            on_offer: OfferAction::Ask,
            if_exists: ExistsAction::Resume,
            ignore_mode: IgnoreMode::IgnoreListed,
            // Fichiers exécutables ou de script.
            ignore_files: [
                "*.exe", "*.com", "*.bat", "*.dll", "*.ini", "*.mrc", "*.vbs", "*.js", "*.pif", "*.scr", "*.lnk",
                "*.pl", "*.shs", "*.htm", "*.html",
            ]
            .map(String::from)
            .into(),
            ignore_nicks: Vec::new(),
            port_first: 1024,
            port_last: 5000,
            external_ip: String::new(),
            upnp: true,
            timeout_secs: 120,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Xdcc {
    /// Nouvelles tentatives (avec reprise) après un transfert interrompu.
    pub retries: u32,
    pub retry_delay_secs: u64,
    /// Demande abandonnée si le bot ne répond rien pendant ce délai (0 : jamais).
    pub request_timeout_secs: u64,
}

impl Default for Xdcc {
    fn default() -> Self {
        Xdcc {
            retries: 3,
            retry_delay_secs: 30,
            request_timeout_secs: 300,
        }
    }
}

/// Son joué lors d'un évènement.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sound {
    pub enabled: bool,
    /// Fichier audio (WAV, MP3…) ; vide : son du système.
    pub file: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sounds {
    /// Interrupteur général de tous les sons.
    pub enabled: bool,
    /// Bip à chaque message dans un canal, une conversation privée qui n'est pas la fenêtre
    /// affichée, ou dans la fenêtre affichée quand on remonte l'historique.
    pub beep_channel: bool,
    pub beep_query: bool,
    pub beep_scrolled: bool,
    pub started: Sound,
    pub completed: Sound,
    pub failed: Sound,
    /// Le bot ne répond pas, n'est pas (ou plus) sur le réseau.
    pub bot_offline: Sound,
    /// Connexion au serveur perdue.
    pub disconnected: Sound,
}

impl Default for Sounds {
    fn default() -> Self {
        Sounds {
            enabled: true,
            beep_channel: false,
            beep_query: false,
            beep_scrolled: false,
            started: Sound::default(),
            completed: Sound::default(),
            failed: Sound::default(),
            bot_offline: Sound::default(),
            disconnected: Sound::default(),
        }
    }
}

/// Police du texte : Fixedsys Excelsior (deux variantes qui ne diffèrent que par les ligatures
/// `<=` et `=<`) ou un fichier de police au choix.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum FontVariant {
    /// `<=` devient une flèche, comme `=>` : le sens le plus courant dans une discussion.
    Standard,
    /// `<=` devient « inférieur ou égal » (pensée pour le code).
    Alt,
    /// Fichier `.ttf` / `.otf` (`custom_font`).
    Custom,
}

/// Langue de l'interface.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Language {
    /// Celle du système : français ou anglais selon le système, anglais pour toute autre langue.
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "en-US")]
    English,
    #[serde(rename = "fr-FR")]
    French,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Theme {
    /// Fond blanc et couleurs IRC classiques.
    Light,
    Dark,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    pub language: Language,
    pub font: FontVariant,
    /// Multiple des 16 pixels natifs de Fixedsys (0 : selon l'échelle de l'écran).
    pub font_scale: u8,
    /// Police personnalisée et sa taille en points.
    pub custom_font: PathBuf,
    pub custom_font_size: f32,
    pub theme: Theme,
    /// Format `strftime` (vide : pas d'horodatage).
    pub timestamp: String,
    pub show_joins_parts: bool,
    pub strip_colors: bool,
    pub scrollback: usize,
}

impl Default for Display {
    fn default() -> Self {
        Display {
            language: Language::Auto,
            font: FontVariant::Standard,
            font_scale: 0,
            custom_font: PathBuf::new(),
            custom_font_size: 13.0,
            theme: Theme::Light,
            timestamp: "[%H:%M]".into(),
            show_joins_parts: true,
            strip_colors: false,
            scrollback: 5000,
        }
    }
}

impl Config {
    /// Configuration enregistrée, ou par défaut ; `true` si elle n'existait pas encore, et
    /// l'erreur de lecture éventuelle.
    pub fn load() -> (Config, bool, Option<String>) {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(config) => (config, false, None),
                Err(e) => {
                    let path = path.display();
                    let error = tr!(
                        "Configuration illisible ({path}) : {e}",
                        "Unreadable configuration ({path}): {e}"
                    );
                    (Config::first_run(), false, Some(error))
                }
            },
            Err(_) => (Config::first_run(), true, None),
        }
    }

    /// Configuration du premier lancement : quelques réseaux publics prêts à l'emploi.
    pub fn first_run() -> Config {
        let server = |host: &str| ServerAddr::new(host, 6697, true);
        Config {
            networks: vec![
                Network {
                    name: "Libera.Chat".into(),
                    servers: vec![server("irc.libera.chat")],
                    ..Default::default()
                },
                Network {
                    name: "Rizon".into(),
                    servers: vec![server("irc.rizon.net")],
                    ..Default::default()
                },
                Network {
                    name: "OFTC".into(),
                    servers: vec![server("irc.oftc.net")],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                let dir = dir.display();
                tr!("{dir} : {e}", "{dir}: {e}")
            })?;
        }
        std::fs::write(&path, text).map_err(|e| {
            let path = path.display();
            tr!("{path} : {e}", "{path}: {e}")
        })
    }

    pub fn network(&self, name: &str) -> Option<&Network> {
        self.networks.iter().find(|n| n.name.eq_ignore_ascii_case(name))
    }

    /// Réseau dont un serveur a cet hôte.
    pub fn network_of_host(&self, host: &str) -> Option<&Network> {
        self.networks
            .iter()
            .find(|n| n.servers.iter().any(|s| s.host.eq_ignore_ascii_case(host)))
    }
}

pub fn config_path() -> PathBuf {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
    {
        let portable = dir.join("irc-xdcc.toml");
        if portable.is_file() {
            return portable;
        }
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
    };
    base.unwrap_or_default().join("irc-xdcc").join("config.toml")
}

fn default_download_dir() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    home.map(|h| PathBuf::from(h).join("Downloads")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_syntax() {
        let s = ServerAddr::parse("irc.example.net:+6697").unwrap();
        assert_eq!((s.host.as_str(), s.port, s.tls), ("irc.example.net", 6697, true));
        let s = ServerAddr::parse("irc.example.net").unwrap();
        assert_eq!((s.port, s.tls), (6667, false));
        let s = ServerAddr::parse("[::1]:7000").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("::1", 7000));
        assert_eq!(s.to_string(), "[::1]:7000");
        assert!(ServerAddr::parse("host:abc").is_none());
        assert_eq!(ServerAddr::new("a.b", 6697, true).to_string(), "a.b:+6697");
    }

    #[test]
    fn roundtrip() {
        let mut config = Config::first_run();
        config.dcc.download_dir = r"\\NAS\partage\XDCC".into();
        let text = toml::to_string_pretty(&config).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.networks.len(), 3);
        assert_eq!(back.dcc.download_dir, config.dcc.download_dir);
        // Les champs absents prennent leur valeur par défaut.
        let partial: Config = toml::from_str("[identity]\nnick = \"moi\"").unwrap();
        assert_eq!(partial.identity.nick, "moi");
        assert_eq!(partial.xdcc.retries, 3);
        assert!(partial.sounds.enabled && partial.connection.identd);
        // Fichier d'une version précédente : les champs disparus sont ignorés.
        let old: Config = toml::from_str(
            "[[networks]]\nname = \"Réseau\"\ndownload_dir = \"D:\\\\XDCC\"\n\n[xdcc]\nmax_per_bot = 2\n\n\
             [sounds.completed]\nenabled = true\n\n[display]\nfont = \"Alt\"",
        )
        .unwrap();
        assert_eq!(old.networks[0].name, "Réseau");
        assert!(old.display.font == FontVariant::Alt);
        assert!(old.sounds.enabled && old.sounds.completed.enabled && !old.sounds.failed.enabled);
    }
}
