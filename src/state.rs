//! État partagé entre le cœur réseau (qui le modifie) et l'interface (qui l'affiche).

use std::collections::VecDeque;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use crate::config::{Config, ServerAddr};
use crate::dcc::Offer;
use crate::irc;

pub type ServerId = u32;
pub type TransferId = u64;
pub type QueueId = u64;

pub type Shared = Arc<Mutex<State>>;

pub fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct State {
    pub config: Config,
    pub servers: Vec<Server>,
    pub transfers: Vec<Transfer>,
    pub queue: Vec<QueueItem>,
    /// Fenêtre active (renseignée par l'interface) : celle de la fenêtre principale, ou celle
    /// ouverte à part qui a le focus ; les notices et réponses y sont écrites.
    pub active: Option<(ServerId, String)>,
    /// La fenêtre active est remontée dans son historique (renseigné par l'interface).
    pub scrolled_back: bool,
    /// Fenêtre à afficher (nouvelle connexion, canal rejoint, `/query`…).
    pub focus: Option<Focus>,
    /// Fenêtres renommées (conversation privée dont le correspondant change de pseudo), ancien et
    /// nouveau nom : l'interface les garde où elles étaient.
    pub renamed: Vec<(Focus, Focus)>,
    /// Un message nous concerne : faire clignoter la fenêtre si elle n'a pas le focus.
    pub attention: bool,
    /// `/help` : ouvrir la fenêtre d'aide.
    pub help: bool,
    next_id: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Focus {
    Buffer(ServerId, String),
    Transfers,
}

impl State {
    pub fn new(config: Config) -> Self {
        State {
            config,
            servers: Vec::new(),
            transfers: Vec::new(),
            queue: Vec::new(),
            active: None,
            scrolled_back: false,
            focus: None,
            renamed: Vec::new(),
            attention: false,
            help: false,
            next_id: 1,
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn server(&self, id: ServerId) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }

    pub fn server_mut(&mut self, id: ServerId) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.id == id)
    }

    pub fn transfer_mut(&mut self, id: TransferId) -> Option<&mut Transfer> {
        self.transfers.iter_mut().find(|t| t.id == id)
    }

    pub fn queue_item_mut(&mut self, id: QueueId) -> Option<&mut QueueItem> {
        self.queue.iter_mut().find(|q| q.id == id)
    }
}

#[derive(Clone, PartialEq)]
pub enum ConnStatus {
    Connecting,
    /// Connecté, enregistrement (NICK / USER / SASL) en cours.
    Registering,
    Connected,
    Disconnected,
    /// Reconnexion automatique prévue.
    Waiting(Instant),
}

pub struct Server {
    pub id: ServerId,
    /// Nom du réseau (configuration, ou annoncé par le serveur), sinon l'hôte.
    pub label: String,
    /// Réseau de la configuration utilisé pour l'identité, le perform et les favoris.
    pub network: Option<String>,
    pub addr: ServerAddr,
    pub nick: String,
    pub status: ConnStatus,
    /// Tampons : `buffers[0]` est la fenêtre d'état du serveur.
    pub buffers: Vec<Buffer>,
    /// Modes de rang des utilisateurs (`o` → `@`…), du plus haut au plus bas.
    pub prefixes: Vec<(char, char)>,
    pub chantypes: String,
    /// Modes de canal avec paramètre (toujours, ou seulement quand ils sont activés).
    pub chanmodes_param: String,
    pub chanmodes_param_set: String,
    /// Adresse IP vue du serveur (DCC passif) et adresse locale de la connexion.
    pub public_ip: Option<IpAddr>,
    pub local_ip: Option<IpAddr>,
}

impl Server {
    pub fn new(id: ServerId, label: String, network: Option<String>, addr: ServerAddr, nick: String) -> Self {
        Server {
            id,
            buffers: vec![Buffer::new(&label, BufKind::Status)],
            label,
            network,
            addr,
            nick,
            status: ConnStatus::Connecting,
            prefixes: vec![('o', '@'), ('v', '+')],
            chantypes: "#&".into(),
            chanmodes_param: "beIkL".into(),
            chanmodes_param_set: "lfj".into(),
            public_ip: None,
            local_ip: None,
        }
    }

    pub fn is_channel(&self, name: &str) -> bool {
        name.starts_with(|c| self.chantypes.contains(c))
    }

    pub fn buffer(&self, name: &str) -> Option<&Buffer> {
        if name.is_empty() {
            return self.buffers.first();
        }
        let key = irc::lower(name);
        self.buffers.iter().skip(1).find(|b| b.key == key)
    }

    pub fn buffer_mut(&mut self, name: &str) -> Option<&mut Buffer> {
        if name.is_empty() {
            return self.buffers.first_mut();
        }
        let key = irc::lower(name);
        self.buffers.iter_mut().skip(1).find(|b| b.key == key)
    }

    /// Tampon du canal ou de la conversation privée, créé au besoin.
    pub fn open_buffer(&mut self, name: &str) -> &mut Buffer {
        let kind = if self.is_channel(name) {
            BufKind::Channel
        } else {
            BufKind::Query
        };
        let key = irc::lower(name);
        match self.buffers.iter().skip(1).position(|b| b.key == key) {
            Some(i) => &mut self.buffers[i + 1],
            None => {
                self.buffers.push(Buffer::new(name, kind));
                self.buffers.last_mut().expect("tampon ajouté")
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum BufKind {
    Status,
    Channel,
    Query,
}

/// Activité non lue, pour colorer l'entrée dans l'arborescence.
#[derive(Clone, Copy, PartialEq, PartialOrd)]
pub enum Activity {
    None,
    Event,
    Message,
    Highlight,
}

/// Numéros de ligne uniques dans toute l'application : un tampon fermé puis rouvert ne peut pas
/// être confondu avec l'ancien dans le cache d'affichage.
static NEXT_LINE: AtomicU64 = AtomicU64::new(0);

/// Clé de comparaison ; vide pour la fenêtre d'état (comme dans `Focus`).
fn buffer_key(name: &str, kind: BufKind) -> String {
    if kind == BufKind::Status {
        String::new()
    } else {
        irc::lower(name)
    }
}

pub struct Buffer {
    pub name: String,
    /// Nom en casse IRC (vide pour la fenêtre d'état), pour les comparaisons.
    pub key: String,
    pub kind: BufKind,
    pub lines: VecDeque<Line>,
    pub activity: Activity,
    pub topic: String,
    /// Utilisateurs du canal, triés par rang puis par pseudo.
    pub users: Vec<User>,
    /// La réponse NAMES en cours remplace la liste.
    pub names_pending: bool,
    pub joined: bool,
    /// Dernière ligne affichée quand on a quitté la fenêtre : un trait la sépare des nouvelles.
    pub read_marker: Option<u64>,
}

impl Buffer {
    pub fn new(name: &str, kind: BufKind) -> Self {
        Buffer {
            name: name.to_owned(),
            key: buffer_key(name, kind),
            kind,
            lines: VecDeque::new(),
            activity: Activity::None,
            topic: String::new(),
            users: Vec::new(),
            names_pending: false,
            joined: false,
            read_marker: None,
        }
    }

    pub fn rename(&mut self, name: &str) {
        self.name = name.to_owned();
        self.key = buffer_key(name, self.kind);
    }

    pub fn push(&mut self, line: Line, limit: usize, activity: Activity) {
        let mut line = line;
        line.id = NEXT_LINE.fetch_add(1, Ordering::Relaxed);
        self.lines.push_back(line);
        while self.lines.len() > limit.max(100) {
            self.lines.pop_front();
        }
        if activity > self.activity {
            self.activity = activity;
        }
    }

    pub fn user_index(&self, nick: &str) -> Option<usize> {
        self.users.iter().position(|u| irc::eq(&u.nick, nick))
    }
}

#[derive(Clone)]
pub struct User {
    pub nick: String,
    /// Préfixes de rang détenus (`@`, `+`…), du plus haut au plus bas.
    pub prefixes: String,
}

#[derive(Clone, Copy, PartialEq)]
pub enum LineKind {
    Normal,
    Own,
    Action,
    Notice,
    Ctcp,
    Join,
    Part,
    Quit,
    Mode,
    Topic,
    Info,
    /// En vert, sans « * » : hôte local, message de fin du serveur.
    Info2,
    Error,
    /// Texte du serveur (accueil, MOTD, statistiques, réponses).
    Server,
    /// Ligne « - » qui sépare deux groupes de réponses du serveur.
    Separator,
}

pub struct Line {
    /// Numéro croissant (cache d'affichage de l'interface).
    pub id: u64,
    pub time: String,
    pub kind: LineKind,
    /// Mis en évidence (pseudo ou mot surveillé).
    pub highlight: bool,
    pub text: String,
    /// Groupe de réponses du serveur : dans la fenêtre d'état, pas de séparateur à l'intérieur.
    pub group: Option<ServerGroup>,
}

/// Réponses du serveur affichées ensemble dans la fenêtre d'état.
#[derive(Clone, Copy, PartialEq)]
pub enum ServerGroup {
    /// Accueil (001 à 003).
    Welcome,
    /// Fonctions prises en charge par le serveur (005).
    Features,
    /// Statistiques d'utilisateurs, d'opérateurs, de canaux (250 à 255).
    Stats,
    /// Utilisateurs locaux et globaux (265, 266).
    Counts,
    /// Lignes du message du jour (entre son titre et sa fin, affichés à part).
    Motd,
    /// Réponse à une commande (/whois, /who, /list…).
    Reply,
}

impl LineKind {
    /// Ligne écrite par le logiciel (évènement, action, information, échec) : précédée de « * ».
    /// Le texte du serveur, les messages des utilisateurs, les lignes CTCP et DCC n'en ont pas.
    fn starred(self) -> bool {
        matches!(
            self,
            LineKind::Action
                | LineKind::Join
                | LineKind::Part
                | LineKind::Quit
                | LineKind::Mode
                | LineKind::Topic
                | LineKind::Info
                | LineKind::Error
        )
    }
}

impl Line {
    pub fn new(time: String, kind: LineKind, text: String) -> Self {
        Line {
            id: 0,
            time,
            kind,
            highlight: false,
            text: if kind.starred() { format!("* {text}") } else { text },
            group: None,
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum TransferStatus {
    /// Envoi non sollicité : en attente de l'accord de l'utilisateur.
    AskAccept,
    /// Le fichier existe : reprendre, écraser ou renommer ?
    AskExists,
    /// Demande de reprise envoyée, en attente de DCC ACCEPT.
    Resuming,
    Connecting,
    /// DCC passif : en attente de la connexion de l'émetteur.
    Listening,
    Receiving,
    Done,
    Failed(String),
    Cancelled,
}

impl TransferStatus {
    pub fn finished(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_) | Self::Cancelled)
    }
}

pub struct Transfer {
    pub id: TransferId,
    pub server: ServerId,
    pub nick: String,
    pub offer: Offer,
    pub path: PathBuf,
    /// Position de départ (reprise).
    pub offset: u64,
    /// Position atteinte, mise à jour par la tâche de réception.
    pub position: Arc<AtomicU64>,
    pub status: TransferStatus,
    pub started: Option<Instant>,
    pub ended: Option<Instant>,
    /// Pack XDCC à l'origine de ce transfert.
    pub queue_item: Option<QueueId>,
    /// Demande de reprise envoyée (sans réponse, le transfert échoue après le délai DCC).
    pub resume_sent: Option<Instant>,
    /// DCC passif : adresse et port annoncés à l'émetteur.
    pub announced: Option<std::net::SocketAddr>,
    /// Fichier déjà complet sur le disque : seul le dernier octet est redemandé, pour que
    /// l'émetteur clôture son envoi.
    pub complete: bool,
}

#[derive(Clone, PartialEq)]
pub enum QueueStatus {
    /// En file : sera demandé quand le pack précédent du même bot sera terminé.
    Waiting,
    /// Demandé au bot, en attente de son envoi.
    Requested,
    /// Placé dans la file d'attente du bot (position annoncée par le bot, si connue).
    BotQueue(String),
    Transferring(TransferId),
    Done,
    Failed(String),
    Cancelled,
}

impl QueueStatus {
    /// Demande en cours auprès du bot : les packs suivants du même bot attendent.
    pub fn active(&self) -> bool {
        matches!(self, Self::Requested | Self::BotQueue(_) | Self::Transferring(_))
    }

    /// Demandé au bot, envoi pas encore commencé.
    pub fn awaiting_bot(&self) -> bool {
        matches!(self, Self::Requested | Self::BotQueue(_))
    }

    pub fn finished(&self) -> bool {
        matches!(self, Self::Done | Self::Failed(_) | Self::Cancelled)
    }
}

pub struct QueueItem {
    pub id: QueueId,
    pub server: ServerId,
    pub bot: String,
    /// Numéro de pack, avec `#`.
    pub pack: String,
    pub status: QueueStatus,
    /// Demande envoyée, ou dernière réponse du bot.
    pub since: Instant,
    /// Prochaine tentative après un échec.
    pub retry_at: Option<Instant>,
    pub attempts: u32,
    /// Déjà demandé au bot au moins une fois.
    pub asked: bool,
    /// Fichier reçu (une fois l'envoi du bot arrivé).
    pub filename: String,
    /// Fenêtre d'où le pack a été demandé : les messages du bot et du transfert y sont écrits.
    pub window: String,
}
