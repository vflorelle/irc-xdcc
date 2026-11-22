//! Cœur : connexions IRC, commandes saisies, DCC et file d'attente XDCC.
//!
//! Tourne dans une tâche tokio indépendante de l'interface : tout continue (réponses aux PING,
//! téléchargements, file XDCC) même quand la fenêtre est réduite.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use eframe::egui;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::task::AbortHandle;

use crate::config::{ExistsAction, OfferAction, ServerAddr};
use crate::dcc::{self, Endpoint, Job, Offer};
use crate::irc::{self, Message};
use crate::net::{self, Connection, NetEvent};
use crate::sound::{self, Event};
use crate::state::{
    Activity, BufKind, ConnStatus, Focus, Line, LineKind, QueueId, QueueItem, QueueStatus, Server, ServerGroup,
    ServerId, Shared, State, Transfer, TransferId, TransferStatus, User, lock,
};
use crate::upnp;

/// Réponse complète à VERSION, proposée dans les options.
pub const VERSION: &str = concat!("irc-xdcc ", env!("CARGO_PKG_VERSION"), " (Rust)");
/// Longueur maximale du texte d'un message (la ligne entière est limitée à 512 octets).
const MAX_TEXT: usize = 400;

pub enum CoreMsg {
    Ui(Command),
    /// Évènement d'une connexion (serveur, génération de la connexion).
    Net(ServerId, u64, NetEvent),
    Transfer(TransferId, TransferEvent),
    /// Adresse publique et nom de notre hôte, trouvés par DNS (DCC passif, affichage).
    PublicIp(ServerId, String, IpAddr),
    /// Port d'un DCC passif ouvert par la box (adresse publique qu'elle donne), ou échec.
    Upnp(TransferId, u16, Result<Option<IpAddr>, String>),
}

pub enum TransferEvent {
    Started,
    Ended(Result<(), String>),
}

/// Demandes de l'interface.
pub enum Command {
    ConnectNetwork(String),
    Connect(ServerAddr),
    JoinFavorite(usize),
    Input {
        server: ServerId,
        buffer: String,
        text: String,
    },
    Disconnect(ServerId),
    Reconnect(ServerId),
    CloseBuffer(ServerId, String),
    Transfer(TransferId, TransferAction),
    Queue(QueueId, QueueAction),
    ClearFinished,
    /// La configuration a changé (serveur ident, nouvelles tentatives…).
    ConfigChanged,
    Quit,
}

#[derive(Clone, Copy)]
pub enum TransferAction {
    Accept,
    Exists(ExistsAction),
    Cancel,
    Remove,
}

#[derive(Clone, Copy)]
pub enum QueueAction {
    Cancel,
    Retry,
    Remove,
    Up,
    Down,
}

/// Connexion d'un serveur et ce qui doit survivre à une reconnexion.
#[derive(Default)]
struct Session {
    link: Option<Connection>,
    generation: u64,
    nick_attempt: u32,
    /// SASL demandé pour cette connexion, et proposé par le serveur.
    sasl: bool,
    sasl_offered: bool,
    registered_at: Option<Instant>,
    performed: bool,
    join_at: Option<Instant>,
    /// Quelques secondes après l'entrée dans les canaux : les demandes XDCC peuvent partir (les
    /// bots exigent souvent la présence sur leur canal).
    ready_at: Option<Instant>,
    /// Canaux à rejoindre une fois connecté (favoris, reconnexion) : (canal, clé).
    pending_joins: Vec<(String, String)>,
    /// Canaux rejoints à la demande de l'utilisateur : leur fenêtre s'affiche.
    focus_joins: HashSet<String>,
    userhost_pending: bool,
    /// Déconnexion demandée : pas de reconnexion automatique.
    quitting: bool,
    /// Serveur du réseau essayé (on passe au suivant en cas d'échec).
    server_index: usize,
}

pub struct Core {
    state: Shared,
    ctx: egui::Context,
    tx: UnboundedSender<CoreMsg>,
    sessions: HashMap<ServerId, Session>,
    tasks: HashMap<TransferId, AbortHandle>,
    /// Serveur ident en cours, et redirection UPnP demandée.
    identd: Option<(AbortHandle, bool)>,
}

impl Core {
    pub fn new(state: Shared, ctx: egui::Context, tx: UnboundedSender<CoreMsg>) -> Self {
        Core {
            state,
            ctx,
            tx,
            sessions: HashMap::new(),
            tasks: HashMap::new(),
            identd: None,
        }
    }

    pub async fn run(mut self, mut rx: UnboundedReceiver<CoreMsg>) {
        let startup: Vec<String> = {
            let state = self.state.clone();
            let st = lock(&state);
            self.update_identd(&st);
            st.config
                .networks
                .iter()
                .filter(|n| n.connect_on_startup)
                .map(|n| n.name.clone())
                .collect()
        };
        for name in startup {
            self.handle(CoreMsg::Ui(Command::ConnectNetwork(name)));
        }
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(msg) => self.handle(msg),
                    None => return,
                },
                _ = tick.tick() => {
                    let state = self.state.clone();
                    let mut st = lock(&state);
                    self.on_tick(&mut st);
                }
            }
            self.ctx.request_repaint();
        }
    }

    fn handle(&mut self, msg: CoreMsg) {
        let state = self.state.clone();
        let mut st = lock(&state);
        match msg {
            CoreMsg::Ui(cmd) => self.on_command(&mut st, cmd),
            CoreMsg::Net(id, generation, event) => {
                if self.sessions.get(&id).is_some_and(|s| s.generation == generation) {
                    self.on_net(&mut st, id, event);
                }
            }
            CoreMsg::Transfer(id, event) => self.on_transfer(&mut st, id, event),
            CoreMsg::PublicIp(id, host, ip) => set_public_ip(&mut st, id, &host, ip),
            CoreMsg::Upnp(tid, port, result) => {
                let text = match &result {
                    Ok(_) => tr!(
                        "UPnP : port {port} ouvert sur la box",
                        "UPnP: port {port} opened on the router"
                    ),
                    Err(e) => tr!(
                        "UPnP : {e} ; le port {port} doit être redirigé à la main",
                        "UPnP: {e}; port {port} must be forwarded manually"
                    ),
                };
                print_transfer(&mut st, tid, LineKind::Info, text);
                self.passive_offer(&mut st, tid, port, result.ok().flatten());
            }
        }
    }

    /// Démarre, arrête ou relance (UPnP activé ou non) le serveur ident selon les options.
    fn update_identd(&mut self, st: &State) {
        let wanted = st.config.connection.identd.then_some(st.config.dcc.upnp);
        if self.identd.as_ref().map(|(_, upnp)| *upnp) == wanted {
            return;
        }
        if let Some((task, _)) = self.identd.take() {
            task.abort();
        }
        if let Some(upnp) = wanted {
            self.identd = Some((crate::ident::start(self.state.clone(), upnp), upnp));
        }
    }

    fn send(&self, id: ServerId, line: String) {
        if let Some(link) = self.sessions.get(&id).and_then(|s| s.link.as_ref()) {
            link.send(line);
        }
    }

    fn connected(&self, st: &State, id: ServerId) -> bool {
        st.server(id).is_some_and(|s| s.status == ConnStatus::Connected)
    }

    // ---------------------------------------------------------------- connexions

    /// Fenêtre du serveur de ce réseau (ou de cette adresse, hors réseau) : celle qui existe déjà,
    /// reconnectée si besoin, sinon une nouvelle. Un serveur n'est jamais ouvert deux fois.
    fn open_server(&mut self, st: &mut State, network: Option<String>, addr: ServerAddr) -> ServerId {
        let network = network.or_else(|| st.config.network_of_host(&addr.host).map(|n| n.name.clone()));
        let existing = st
            .servers
            .iter()
            .find(|s| match &network {
                Some(name) => {
                    s.network.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(name))
                        || s.label.eq_ignore_ascii_case(name)
                }
                None => s.addr.host.eq_ignore_ascii_case(&addr.host) && s.addr.port == addr.port,
            })
            .map(|s| (s.id, s.status.clone()));
        match existing {
            Some((id, status)) => {
                st.focus = Some(Focus::Buffer(id, String::new()));
                match status {
                    ConnStatus::Disconnected => self.start(st, id),
                    ConnStatus::Connected => self.join_missing(st, id, None),
                    _ => {}
                }
                id
            }
            None => self.new_server(st, network, addr),
        }
    }

    fn new_server(&mut self, st: &mut State, network: Option<String>, addr: ServerAddr) -> ServerId {
        let label = network.clone().unwrap_or_else(|| addr.host.clone());
        let id = st.next_id() as ServerId;
        let nick = self.nicks(st, network.as_deref()).0;
        st.servers.push(Server::new(id, label, network, addr, nick));
        self.sessions.insert(id, Session::default());
        st.focus = Some(Focus::Buffer(id, String::new()));
        self.start(st, id);
        id
    }

    fn start(&mut self, st: &mut State, id: ServerId) {
        let Some(server) = st.server_mut(id) else { return };
        server.status = ConnStatus::Connecting;
        let addr = server.addr.clone();
        print(
            st,
            id,
            "",
            LineKind::Info,
            tr!("Connexion à {addr}…", "Connecting to {addr}…"),
        );
        let session = self.sessions.entry(id).or_default();
        session.generation = st.next_id();
        session.link = Some(net::connect(id, session.generation, addr, self.tx.clone()));
        session.nick_attempt = 0;
        session.sasl_offered = false;
        session.registered_at = None;
        session.performed = false;
        session.join_at = None;
        session.ready_at = None;
        session.quitting = false;
    }

    /// Pseudo et pseudo de secours (réglages du réseau, sinon identité générale).
    fn nicks(&self, st: &State, network: Option<&str>) -> (String, String) {
        let identity = &st.config.identity;
        let net = network.and_then(|n| st.config.network(n));
        let pick = |own: Option<&String>, general: &String| {
            own.filter(|s| !s.trim().is_empty())
                .unwrap_or(general)
                .trim()
                .to_owned()
        };
        let mut nick = pick(net.map(|n| &n.nick), &identity.nick);
        if nick.is_empty() {
            nick = "Invite".into();
        }
        let alt = pick(net.map(|n| &n.alt_nick), &identity.alt_nick);
        (nick, alt)
    }

    fn on_net(&mut self, st: &mut State, id: ServerId, event: NetEvent) {
        match event {
            NetEvent::Connected { local_ip } => self.register(st, id, local_ip),
            NetEvent::Line(line) => {
                if let Some(msg) = Message::parse(&line) {
                    self.on_message(st, id, &msg);
                }
            }
            NetEvent::Closed(reason) => self.on_closed(st, id, &reason),
        }
    }

    fn register(&mut self, st: &mut State, id: ServerId, local_ip: IpAddr) {
        let Some(server) = st.server_mut(id) else { return };
        server.status = ConnStatus::Registering;
        server.local_ip = Some(local_ip);
        let network = server.network.clone();
        let (nick, _) = self.nicks(st, network.as_deref());
        let net = network
            .as_deref()
            .and_then(|n| st.config.network(n))
            .cloned()
            .unwrap_or_default();
        let identity = st.config.identity.clone();
        let sasl = !net.sasl_account.is_empty();
        if let Some(session) = self.sessions.get_mut(&id) {
            session.sasl = sasl;
        }
        print(
            st,
            id,
            "",
            LineKind::Info,
            tr!("Connecté, identification…", "Connected, logging in…"),
        );
        if !net.password.is_empty() {
            self.send(id, format!("PASS {}", net.password));
        }
        if sasl {
            self.send(id, "CAP LS 302".into());
        }
        self.send(id, format!("NICK {nick}"));
        self.send(id, format!("USER {} 0 * :{}", identity.user(), identity.real()));
        if let Some(server) = st.server_mut(id) {
            server.nick = nick;
        }
    }

    fn on_closed(&mut self, st: &mut State, id: ServerId, reason: &str) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        session.link = None;
        let registered = session.registered_at.is_some();
        let quitting = session.quitting;

        self.connection_lost(st, id, reason);
        if registered && !quitting {
            sound::notify(&st.config.sounds, Event::Disconnected);
        }
        let network = st.server(id).and_then(|s| s.network.clone());
        let config = &st.config.connection;
        let status = if quitting || !config.reconnect {
            ConnStatus::Disconnected
        } else {
            if !registered {
                // Échec avant l'enregistrement : serveur suivant du réseau.
                let servers = network
                    .as_deref()
                    .and_then(|n| st.config.network(n))
                    .map(|n| n.servers.clone());
                if let (Some(servers), Some(session)) = (servers, self.sessions.get_mut(&id))
                    && !servers.is_empty()
                {
                    session.server_index = (session.server_index + 1) % servers.len();
                    if let Some(server) = st.server_mut(id) {
                        server.addr = servers[session.server_index].clone();
                    }
                }
            }
            let delay = st.config.connection.reconnect_delay_secs.max(1);
            print(
                st,
                id,
                "",
                LineKind::Info,
                tr!("Reconnexion dans {delay} s", "Reconnecting in {delay} s"),
            );
            ConnStatus::Waiting(Instant::now() + Duration::from_secs(delay))
        };
        if let Some(server) = st.server_mut(id) {
            server.status = status;
        }
    }

    /// Connexion perdue ou abandonnée : canaux à rejoindre, demandes XDCC à refaire, reprises
    /// sans réponse possible.
    fn connection_lost(&mut self, st: &mut State, id: ServerId, reason: &str) {
        let rejoin = st.config.connection.rejoin_channels;
        let Some(server) = st.server_mut(id) else { return };
        let mut rejoins = Vec::new();
        for buffer in &mut server.buffers {
            if buffer.kind == BufKind::Channel && buffer.joined {
                rejoins.push((buffer.name.clone(), String::new()));
                buffer.joined = false;
                buffer.users.clear();
            }
        }
        let names: Vec<String> = server.buffers.iter().map(|b| b.name.clone()).collect();
        for (i, name) in names.iter().enumerate() {
            let target = if i == 0 { "" } else { name };
            print(
                st,
                id,
                target,
                LineKind::Info,
                tr!("Déconnecté ({reason})", "Disconnected ({reason})"),
            );
        }
        if rejoin && !rejoins.is_empty() {
            let session = self.sessions.entry(id).or_default();
            for join in rejoins {
                if !session.pending_joins.iter().any(|(c, _)| irc::eq(c, &join.0)) {
                    session.pending_joins.push(join);
                }
            }
        }
        // Les demandes en cours (et la place dans la file du bot) seront refaites à la reconnexion.
        for item in st
            .queue
            .iter_mut()
            .filter(|q| q.server == id && q.status.awaiting_bot())
        {
            item.status = QueueStatus::Waiting;
        }
        // Une réponse à une demande de reprise ne peut plus arriver.
        let resuming: Vec<TransferId> = st
            .transfers
            .iter()
            .filter(|t| t.server == id && t.status == TransferStatus::Resuming)
            .map(|t| t.id)
            .collect();
        for tid in resuming {
            self.end_transfer(st, tid, Err(tr!("déconnecté du serveur", "disconnected from server")));
        }
    }

    fn disconnect(&mut self, st: &mut State, id: ServerId, message: Option<&str>) {
        let quit = message
            .map(str::to_owned)
            .unwrap_or_else(|| st.config.identity.quit_message.clone());
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        session.quitting = true;
        session.pending_joins.clear();
        if session.link.is_some() {
            self.send(
                id,
                if quit.is_empty() {
                    "QUIT".into()
                } else {
                    format!("QUIT :{quit}")
                },
            );
        } else if let Some(server) = st.server_mut(id) {
            server.status = ConnStatus::Disconnected;
        }
    }

    fn on_tick(&mut self, st: &mut State) {
        let now = Instant::now();
        let waiting: Vec<ServerId> = st
            .servers
            .iter()
            .filter(|s| matches!(s.status, ConnStatus::Waiting(at) if at <= now))
            .map(|s| s.id)
            .collect();
        for id in waiting {
            self.start(st, id);
        }
        let ids: Vec<ServerId> = self.sessions.keys().copied().collect();
        for id in ids {
            let session = &self.sessions[&id];
            // Serveur sans fin de MOTD : le perform est lancé quand même.
            if !session.performed && session.registered_at.is_some_and(|t| now - t > Duration::from_secs(20)) {
                self.perform(st, id);
            }
            if self.sessions[&id].join_at.is_some_and(|t| t <= now) {
                self.auto_join(st, id);
            }
        }
        // Demande de reprise restée sans réponse.
        let limit = dcc::idle_timeout(&st.config.dcc);
        let stale: Vec<TransferId> = st
            .transfers
            .iter()
            .filter(|t| t.status == TransferStatus::Resuming && t.resume_sent.is_some_and(|s| s.elapsed() > limit))
            .map(|t| t.id)
            .collect();
        for tid in stale {
            // Fichier déjà complet : rien ne manque, même si l'émetteur n'a pas répondu.
            let complete = st.transfers.iter().any(|t| t.id == tid && t.complete);
            let result = if complete {
                Ok(())
            } else {
                Err(tr!(
                    "pas de réponse à la demande de reprise",
                    "no reply to the resume request"
                ))
            };
            self.end_transfer(st, tid, result);
        }
        self.check_requests(st);
        self.schedule(st);
    }

    /// Commandes de connexion du réseau, puis entrée dans les canaux.
    fn perform(&mut self, st: &mut State, id: ServerId) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        if session.performed {
            return;
        }
        session.performed = true;
        let network = st.server(id).and_then(|s| s.network.clone());
        let net = network.as_deref().and_then(|n| st.config.network(n)).cloned();
        let (commands, delay) = net.map(|n| (n.perform, n.join_delay_secs)).unwrap_or_default();
        let commands: Vec<String> = commands.into_iter().filter(|c| !c.trim().is_empty()).collect();
        for command in &commands {
            let command = command.trim();
            let command = if command.starts_with('/') {
                command.to_owned()
            } else {
                format!("/{command}")
            };
            self.input(st, id, "", &command);
        }
        let delay = if commands.is_empty() { 0 } else { delay };
        if let Some(session) = self.sessions.get_mut(&id) {
            session.join_at = Some(Instant::now() + Duration::from_secs(delay));
        }
    }

    fn auto_join(&mut self, st: &mut State, id: ServerId) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        session.join_at = None;
        session.ready_at = Some(Instant::now() + Duration::from_secs(3));
        let mut joins = std::mem::take(&mut session.pending_joins);
        if let Some(server) = st.server(id) {
            for fav in st
                .config
                .favorites
                .iter()
                .filter(|f| f.auto_join && favorite_matches(f, server))
            {
                if !joins.iter().any(|(c, _)| irc::eq(c, &fav.channel)) {
                    joins.push((fav.channel.clone(), fav.key.clone()));
                }
            }
        }
        for (channel, key) in joins {
            self.send(id, join_line(&channel, &key));
        }
    }

    /// Serveur déjà connecté, de nouveau demandé (menu *Serveurs*, autre serveur, favori…) : les
    /// favoris « auto » qui n'ont pas pu être rejoints (canal plein, clé changée, bannissement
    /// levé depuis…) sont redemandés, sauf `except` (déjà demandé). Rien tant que l'entrée
    /// automatique dans les canaux n'a pas eu lieu (perform, délai).
    fn join_missing(&mut self, st: &mut State, id: ServerId, except: Option<&str>) {
        if self.sessions.get(&id).is_none_or(|s| s.ready_at.is_none()) {
            return;
        }
        let Some(server) = st.server(id) else { return };
        let wanted = |channel: &str| {
            !server.buffer(channel).is_some_and(|b| b.joined) && except.is_none_or(|e| !irc::eq(e, channel))
        };
        let missing: Vec<String> = st
            .config
            .favorites
            .iter()
            .filter(|f| f.auto_join && favorite_matches(f, server) && wanted(&f.channel))
            .map(|f| join_line(&f.channel, &f.key))
            .collect();
        for line in missing {
            self.send(id, line);
        }
    }

    fn join_favorite(&mut self, st: &mut State, index: usize) {
        let Some(fav) = st.config.favorites.get(index).cloned() else {
            return;
        };
        let channel = (fav.channel.clone(), fav.key.clone());
        let connected = st
            .servers
            .iter()
            .find(|s| s.status == ConnStatus::Connected && favorite_matches(&fav, s))
            .map(|s| s.id);
        if let Some(id) = connected {
            if let Some(session) = self.sessions.get_mut(&id) {
                session.focus_joins.insert(irc::lower(&fav.channel));
            }
            self.join(st, id, &channel.0, &channel.1);
            self.join_missing(st, id, Some(&channel.0));
            return;
        }
        let network = (!fav.network.is_empty()).then(|| fav.network.clone());
        // Sans serveur précis : le premier du réseau (les suivants en cas d'échec).
        let addr = if fav.server.host.is_empty() {
            let first = network.as_deref().and_then(|n| st.config.network(n)?.servers.first());
            let Some(addr) = first.cloned() else { return };
            addr
        } else {
            fav.server.clone()
        };
        // Fenêtre du serveur déjà ouverte (connexion en cours, déconnectée…) ou nouvelle.
        let id = self.open_server(st, network, addr);
        let session = self.sessions.entry(id).or_default();
        session.focus_joins.insert(irc::lower(&channel.0));
        if !session.pending_joins.iter().any(|(c, _)| irc::eq(c, &channel.0)) {
            session.pending_joins.push(channel);
        }
    }

    fn join(&mut self, st: &mut State, id: ServerId, channel: &str, key: &str) {
        if let Some(buffer) = st.server(id).and_then(|s| s.buffer(channel))
            && buffer.joined
        {
            st.focus = Some(Focus::Buffer(id, buffer.key.clone()));
            return;
        }
        self.send(id, join_line(channel, key));
    }

    // ---------------------------------------------------------------- messages du serveur

    fn on_message(&mut self, st: &mut State, id: ServerId, m: &Message) {
        let Some(server) = st.server(id) else { return };
        let me = server.nick.clone();
        let from_me = irc::eq(m.nick(), &me);
        match m.command.as_str() {
            "PING" => self.send(id, format!("PONG :{}", m.last())),
            "PRIVMSG" => self.on_privmsg(st, id, m),
            "NOTICE" => self.on_notice(st, id, m),
            "JOIN" => {
                let channel = m.param(0).to_owned();
                if from_me {
                    let server = st.server_mut(id).expect("serveur");
                    let buffer = server.open_buffer(&channel);
                    buffer.joined = true;
                    buffer.users.clear();
                    buffer.topic.clear();
                    let key = buffer.key.clone();
                    print(
                        st,
                        id,
                        &channel,
                        LineKind::Join,
                        tr!("Vous avez rejoint {channel}", "You have joined {channel}"),
                    );
                    if self.sessions.get_mut(&id).is_some_and(|s| s.focus_joins.remove(&key)) {
                        st.focus = Some(Focus::Buffer(id, key));
                    }
                } else {
                    add_user(st.server_mut(id).expect("serveur"), &channel, m.nick(), "");
                    let (who, host) = (m.nick(), m.userhost());
                    let text = tr!(
                        "{who} ({host}) a rejoint {channel}",
                        "{who} ({host}) has joined {channel}"
                    );
                    print(st, id, &channel, LineKind::Join, text);
                }
            }
            "PART" => {
                let channel = m.param(0);
                let reason = if m.params.len() > 1 {
                    format!(" ({})", m.last())
                } else {
                    String::new()
                };
                let server = st.server_mut(id).expect("serveur");
                if let Some(buffer) = server.buffer_mut(channel) {
                    if from_me {
                        buffer.joined = false;
                        buffer.users.clear();
                    } else if let Some(i) = buffer.user_index(m.nick()) {
                        buffer.users.remove(i);
                    }
                }
                let text = if from_me {
                    tr!("Vous avez quitté {channel}{reason}", "You have left {channel}{reason}")
                } else {
                    let (who, host) = (m.nick(), m.userhost());
                    tr!(
                        "{who} ({host}) a quitté {channel}{reason}",
                        "{who} ({host}) has left {channel}{reason}"
                    )
                };
                print(st, id, channel, LineKind::Part, text);
            }
            "KICK" => {
                let (channel, victim) = (m.param(0), m.param(1));
                let server = st.server_mut(id).expect("serveur");
                let mine = irc::eq(victim, &me);
                if let Some(buffer) = server.buffer_mut(channel) {
                    if mine {
                        buffer.joined = false;
                        buffer.users.clear();
                    } else if let Some(i) = buffer.user_index(victim) {
                        buffer.users.remove(i);
                    }
                }
                let (by, reason) = (m.nick(), m.param(2));
                let text = tr!(
                    "{victim} a été expulsé par {by} ({reason})",
                    "{victim} was kicked by {by} ({reason})"
                );
                print(st, id, channel, LineKind::Part, text);
                if mine {
                    st.attention = true;
                }
            }
            "QUIT" => {
                let nick = m.nick();
                let server = st.server_mut(id).expect("serveur");
                let mut targets = Vec::new();
                for buffer in server.buffers.iter_mut().skip(1) {
                    if let Some(i) = buffer.user_index(nick) {
                        buffer.users.remove(i);
                        targets.push(buffer.name.clone());
                    } else if buffer.kind == BufKind::Query && irc::eq(&buffer.name, nick) {
                        targets.push(buffer.name.clone());
                    }
                }
                for target in targets {
                    let (host, reason) = (m.userhost(), m.last());
                    let text = tr!(
                        "{nick} ({host}) a quitté IRC ({reason})",
                        "{nick} ({host}) has quit IRC ({reason})"
                    );
                    print(st, id, &target, LineKind::Quit, text);
                }
                self.bot_unreachable(st, id, nick, &tr!("a quitté IRC", "has quit IRC"), false);
            }
            "NICK" => self.on_nick(st, id, m.nick(), m.param(0), from_me),
            "MODE" => self.on_mode(st, id, m),
            "TOPIC" => {
                let channel = m.param(0);
                if let Some(buffer) = st.server_mut(id).and_then(|s| s.buffer_mut(channel)) {
                    buffer.topic = m.param(1).to_owned();
                }
                let (who, topic) = (m.nick(), m.param(1));
                let text = tr!("{who} change le sujet : {topic}", "{who} changes topic to: {topic}");
                print(st, id, channel, LineKind::Topic, text);
            }
            "INVITE" => {
                let (who, channel) = (m.nick(), m.param(1));
                let text = tr!(
                    "{who} vous invite sur {channel} (/join {channel})",
                    "{who} invites you to {channel} (/join {channel})"
                );
                print_active(st, id, LineKind::Info, text);
                st.attention = true;
            }
            "ERROR" => print(st, id, "", LineKind::Info2, m.last()),
            "CAP" => self.on_cap(st, id, m),
            "AUTHENTICATE" => {
                if m.param(0) == "+" {
                    let network = st.server(id).and_then(|s| s.network.clone());
                    if let Some(net) = network.as_deref().and_then(|n| st.config.network(n)) {
                        let payload = format!("{0}\0{0}\0{1}", net.sasl_account, net.sasl_password);
                        self.send(id, format!("AUTHENTICATE {}", irc::base64(payload.as_bytes())));
                    }
                }
            }
            command if command.len() == 3 && command.bytes().all(|b| b.is_ascii_digit()) => {
                self.on_numeric(st, id, m);
            }
            _ => print(
                st,
                id,
                "",
                LineKind::Server,
                format!("{} {}", m.command, m.params.join(" ")),
            ),
        }
    }

    fn on_cap(&mut self, st: &mut State, id: ServerId, m: &Message) {
        let Some(session) = self.sessions.get_mut(&id) else {
            return;
        };
        match m.param(1).to_ascii_uppercase().as_str() {
            "LS" => {
                if m.last().split(' ').any(|c| c == "sasl" || c.starts_with("sasl=")) {
                    session.sasl_offered = true;
                }
                // « CAP * LS * :… » : la liste continue sur la ligne suivante.
                if m.param(2) == "*" && m.params.len() > 3 {
                    return;
                }
                if session.sasl && session.sasl_offered {
                    self.send(id, "CAP REQ :sasl".into());
                } else {
                    if session.sasl {
                        print(
                            st,
                            id,
                            "",
                            LineKind::Error,
                            tr!("Le serveur ne propose pas SASL", "The server does not offer SASL"),
                        );
                    }
                    self.send(id, "CAP END".into());
                }
            }
            "ACK" if m.last().split(' ').any(|c| c == "sasl") => self.send(id, "AUTHENTICATE PLAIN".into()),
            "NAK" => self.send(id, "CAP END".into()),
            _ => {}
        }
    }

    fn on_numeric(&mut self, st: &mut State, id: ServerId, m: &Message) {
        if let Some(text) = whois_text(&m.command, &m.params) {
            print_reply(st, id, &m.command, text);
            return;
        }
        let text = numeric_text(&m.params);
        match m.command.as_str() {
            "001" => {
                let nick = m.param(0).to_owned();
                let lookup = st.config.dcc.external_ip.trim().is_empty();
                if let Some(server) = st.server_mut(id) {
                    server.nick = nick.clone();
                    server.status = ConnStatus::Connected;
                }
                // Hôte réel du message de bienvenue (avant d'être masqué), sinon demandé au serveur.
                let welcome_host = welcome_host(m.last());
                let userhost = lookup && welcome_host.is_none();
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.registered_at = Some(Instant::now());
                    session.userhost_pending = userhost;
                }
                if let Some(host) = welcome_host.filter(|_| lookup) {
                    self.find_public_ip(id, host);
                }
                if userhost {
                    self.send(id, format!("USERHOST {nick}"));
                }
                if st.config.connection.invisible {
                    self.send(id, format!("MODE {nick} +i"));
                }
                print_server(st, id, ServerGroup::Welcome, false, text);
            }
            "005" => {
                let server = st.server_mut(id).expect("serveur");
                for token in m.params.get(1..m.params.len().saturating_sub(1)).unwrap_or_default() {
                    let (key, value) = token.split_once('=').unwrap_or((token, ""));
                    match key {
                        "PREFIX" => {
                            if let Some((modes, symbols)) = value.trim_start_matches('(').split_once(')') {
                                server.prefixes = modes.chars().zip(symbols.chars()).collect();
                            }
                        }
                        "CHANTYPES" => server.chantypes = value.to_owned(),
                        "CHANMODES" => {
                            let groups: Vec<&str> = value.split(',').collect();
                            if groups.len() >= 3 {
                                server.chanmodes_param = format!("{}{}", groups[0], groups[1]);
                                server.chanmodes_param_set = groups[2].to_owned();
                            }
                        }
                        "NETWORK" if server.network.is_none() && !value.is_empty() => {
                            server.label = value.to_owned();
                            server.buffers[0].rename(value);
                        }
                        _ => {}
                    }
                }
                print_server(st, id, ServerGroup::Features, false, text);
            }
            // Pseudo indisponible pendant l'enregistrement : pseudo de secours, puis variantes.
            "432" | "433" | "436" | "437" if !self.connected(st, id) => {
                let network = st.server(id).and_then(|s| s.network.clone());
                let (nick, alt) = self.nicks(st, network.as_deref());
                let Some(session) = self.sessions.get_mut(&id) else {
                    return;
                };
                session.nick_attempt += 1;
                let attempt = session.nick_attempt;
                if attempt > 10 {
                    print(
                        st,
                        id,
                        "",
                        LineKind::Error,
                        tr!("{text} — aucun pseudo accepté", "{text} — no nick accepted"),
                    );
                    return self.disconnect(st, id, None);
                }
                let suffix = 100 + (attempt * 37 + jiff::Timestamp::now().subsec_nanosecond().unsigned_abs()) % 900;
                let next = if attempt == 1 && !alt.is_empty() && !irc::eq(&alt, &nick) {
                    alt
                } else if m.command == "432" {
                    // Pseudo refusé (caractères, longueur) : ses variantes le seraient aussi.
                    format!("Invite{suffix}")
                } else if attempt <= 3 {
                    format!("{nick}{}", "_".repeat(attempt as usize))
                } else {
                    let base: String = nick.chars().take(7).collect();
                    format!("{base}{suffix}")
                };
                print(
                    st,
                    id,
                    "",
                    LineKind::Error,
                    tr!("{text} — essai avec {next}", "{text} — trying {next}"),
                );
                self.send(id, format!("NICK {next}"));
                if let Some(server) = st.server_mut(id) {
                    server.nick = next;
                }
            }
            "302" if self.sessions.get(&id).is_some_and(|s| s.userhost_pending) => {
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.userhost_pending = false;
                }
                // « pseudo=+user@hôte » : l'hôte vu par le serveur.
                let host = m.last().split('@').nth(1).unwrap_or("").trim().to_owned();
                self.find_public_ip(id, &host);
            }
            "332" => {
                let channel = m.param(1);
                if let Some(buffer) = st.server_mut(id).and_then(|s| s.buffer_mut(channel)) {
                    buffer.topic = m.last().to_owned();
                }
                let topic = m.last();
                print(
                    st,
                    id,
                    channel,
                    LineKind::Topic,
                    tr!("Sujet : {topic}", "Topic: {topic}"),
                );
            }
            "333" => {
                let date = m.param(3).parse::<i64>().ok().and_then(format_date).unwrap_or_default();
                let by = irc::nick_of(m.param(2));
                print(
                    st,
                    id,
                    m.param(1),
                    LineKind::Topic,
                    tr!("Défini par {by} {date}", "Set by {by} {date}"),
                );
            }
            "353" => {
                let channel = m.param(2).to_owned();
                let server = st.server_mut(id).expect("serveur");
                let symbols: String = server.prefixes.iter().map(|&(_, p)| p).collect();
                if server.buffer(&channel).is_none_or(|b| !b.joined) {
                    let names = m.last();
                    print_reply(st, id, "353", format!("{channel} {names}"));
                    return;
                }
                let names: Vec<(String, String)> = m
                    .last()
                    .split_whitespace()
                    .map(|n| {
                        let nick = n.trim_start_matches(|c| symbols.contains(c));
                        (nick.to_owned(), n[..n.len() - nick.len()].to_owned())
                    })
                    .collect();
                let buffer = server.buffer_mut(&channel).expect("tampon");
                if !buffer.names_pending {
                    buffer.users.clear();
                    buffer.names_pending = true;
                }
                for (nick, prefixes) in names {
                    add_user(server, &channel, &nick, &prefixes);
                }
            }
            "366" => match st.server_mut(id).and_then(|s| s.buffer_mut(m.param(1))) {
                Some(buffer) if buffer.joined => buffer.names_pending = false,
                _ => print_reply(st, id, "366", text),
            },
            // Accueil, statistiques et message du jour : fenêtre d'état, par groupes ; la version
            // du serveur (004), le titre et la fin du message du jour seuls.
            "002" | "003" => print_server(st, id, ServerGroup::Welcome, false, text),
            "004" | "375" => print(st, id, "", LineKind::Server, text),
            "250" | "251" | "252" | "253" | "254" | "255" => {
                print_server(st, id, ServerGroup::Stats, m.command == "255", text);
            }
            "265" | "266" => print_server(st, id, ServerGroup::Counts, m.command == "266", text),
            "372" => print_server(st, id, ServerGroup::Motd, false, text),
            "376" | "422" => {
                print(st, id, "", LineKind::Server, text);
                self.perform(st, id);
            }
            "900" | "903" => {
                print(st, id, "", LineKind::Server, m.last());
                if m.command == "903" {
                    self.send(id, "CAP END".into());
                }
            }
            "902" | "904" | "905" | "906" | "908" => {
                let reason = m.last();
                print(st, id, "", LineKind::Error, tr!("SASL : {reason}", "SASL: {reason}"));
                self.send(id, "CAP END".into());
            }
            // Pseudo inconnu : peut-être un bot à qui un pack vient d'être demandé.
            "401" => {
                print_active(st, id, LineKind::Server, text);
                let nick = m.param(1).to_owned();
                self.bot_unreachable(st, id, &nick, &tr!("absent du réseau", "is not on the network"), true);
            }
            // Refus (4xx, 5xx) : texte du serveur, seul, dans la fenêtre active.
            command if command.starts_with('4') || command.starts_with('5') => {
                print_active(st, id, LineKind::Server, text);
            }
            // Réponses aux autres commandes (/who, /list, /links…) : texte du serveur, groupé.
            code => print_reply(st, id, code, text),
        }
    }

    fn on_nick(&mut self, st: &mut State, id: ServerId, old: &str, new: &str, from_me: bool) {
        // Une conversation avec le nouveau pseudo existe déjà : l'ancienne garde son nom.
        let taken = !irc::eq(old, new) && st.server(id).is_some_and(|s| s.buffer(new).is_some());
        let server = st.server_mut(id).expect("serveur");
        if from_me {
            server.nick = new.to_owned();
        }
        let mut targets = vec![String::new()];
        let mut renamed = None;
        for buffer in server.buffers.iter_mut().skip(1) {
            if let Some(i) = buffer.user_index(old) {
                buffer.users[i].nick = new.to_owned();
                targets.push(buffer.name.clone());
            } else if buffer.kind == BufKind::Query && irc::eq(&buffer.name, old) && !taken {
                let key = buffer.key.clone();
                buffer.rename(new);
                renamed = Some((Focus::Buffer(id, key), Focus::Buffer(id, buffer.key.clone())));
                targets.push(buffer.name.clone());
            }
        }
        sort_users(server);
        st.renamed.extend(renamed);
        if !from_me {
            targets.retain(|t| !t.is_empty());
        }
        for target in targets {
            print(
                st,
                id,
                &target,
                LineKind::Mode,
                tr!("{old} s'appelle maintenant {new}", "{old} is now known as {new}"),
            );
        }
        for item in st.queue.iter_mut().filter(|q| q.server == id && irc::eq(&q.bot, old)) {
            item.bot = new.to_owned();
        }
    }

    fn on_mode(&mut self, st: &mut State, id: ServerId, m: &Message) {
        let target = m.param(0);
        let changes = m.params.get(1..).unwrap_or_default().join(" ");
        let server = st.server_mut(id).expect("serveur");
        if !server.is_channel(target) {
            // Notre mode, changé par nous, un service ou le serveur (« * NickServ met le mode : +r »).
            let setter = m.nick();
            let text = tr!("{setter} met le mode : {changes}", "{setter} sets mode: {changes}");
            print(st, id, "", LineKind::Mode, text);
            return;
        }
        let modes = m.param(1);
        let mut args = m.params.iter().skip(2);
        let mut adding = true;
        for c in modes.chars() {
            match c {
                '+' => adding = true,
                '-' => adding = false,
                c => {
                    if let Some(&(_, symbol)) = server.prefixes.iter().find(|&&(mode, _)| mode == c) {
                        let Some(nick) = args.next() else { break };
                        let ranks: Vec<char> = server.prefixes.iter().map(|&(_, p)| p).collect();
                        if let Some(buffer) = server.buffer_mut(target)
                            && let Some(i) = buffer.user_index(nick)
                        {
                            let user = &mut buffer.users[i];
                            user.prefixes.retain(|p| p != symbol);
                            if adding {
                                user.prefixes.push(symbol);
                            }
                            let mut sorted: Vec<char> = user.prefixes.chars().collect();
                            sorted.sort_by_key(|p| ranks.iter().position(|r| r == p));
                            user.prefixes = sorted.into_iter().collect();
                        }
                    } else if server.chanmodes_param.contains(c) || (adding && server.chanmodes_param_set.contains(c)) {
                        args.next();
                    }
                }
            }
        }
        sort_users(server);
        let who = m.nick();
        print(
            st,
            id,
            target,
            LineKind::Mode,
            tr!("{who} met le mode : {changes}", "{who} sets mode: {changes}"),
        );
    }

    fn on_privmsg(&mut self, st: &mut State, id: ServerId, m: &Message) {
        let (target, text, nick) = (m.param(0), m.last(), m.nick());
        let server = st.server(id).expect("serveur");
        let channel = server.is_channel(target);
        let me = server.nick.clone();
        if let Some((command, args)) = irc::ctcp(text) {
            match command.as_str() {
                "ACTION" => {
                    let buffer = if channel { target } else { nick };
                    st.server_mut(id).expect("serveur").open_buffer(buffer);
                    let highlight = !channel || highlights(st, &me, args);
                    print_message(st, id, buffer, LineKind::Action, format!("{nick} {args}"), highlight);
                }
                "DCC" => self.on_dcc(st, id, nick, args),
                _ => {
                    // Rien sur le logiciel sauf choix contraire, heure sans le fuseau horaire.
                    let version = st.config.identity.version_reply.trim();
                    let reply = match command.as_str() {
                        "VERSION" if !version.is_empty() => Some(version.to_owned()),
                        "PING" => Some(args.to_owned()),
                        "TIME" => Some(jiff::Timestamp::now().strftime("%a %b %d %H:%M:%S %Y UTC").to_string()),
                        "CLIENTINFO" if version.is_empty() => Some("ACTION CLIENTINFO DCC PING TIME".into()),
                        "CLIENTINFO" => Some("ACTION CLIENTINFO DCC PING TIME VERSION".into()),
                        _ => None,
                    };
                    if let Some(reply) = reply {
                        let reply = if reply.is_empty() {
                            command.clone()
                        } else {
                            format!("{command} {reply}")
                        };
                        self.send(id, format!("NOTICE {nick} :\x01{reply}\x01"));
                    }
                    let text = format!("[{nick} {command}] {args}");
                    print_active(st, id, LineKind::Ctcp, text.trim_end().to_owned());
                }
            }
            return;
        }
        if m.by_server() || !channel && !irc::eq(target, &me) {
            print(st, id, "", LineKind::Notice, format!("-{nick}- {text}"));
            return;
        }
        let buffer = if channel { target } else { nick };
        let prefix = st
            .server(id)
            .and_then(|s| s.buffer(buffer))
            .and_then(|b| b.user_index(nick).map(|i| b.users[i].prefixes.chars().next()))
            .flatten()
            .map(String::from)
            .unwrap_or_default();
        st.server_mut(id).expect("serveur").open_buffer(buffer);
        let highlight = !channel || highlights(st, &me, text);
        print_message(
            st,
            id,
            buffer,
            LineKind::Normal,
            format!("<{prefix}{nick}> {text}"),
            highlight,
        );
    }

    fn on_notice(&mut self, st: &mut State, id: ServerId, m: &Message) {
        let (target, text, nick) = (m.param(0), m.last(), m.nick());
        if let Some((command, args)) = irc::ctcp(text) {
            let args = if command == "PING" {
                let sent = args.parse::<i64>().unwrap_or(0);
                let now = jiff::Timestamp::now().as_millisecond();
                format!("{:.2} s", (now - sent) as f64 / 1000.0)
            } else {
                args.to_owned()
            };
            let text = tr!("[{nick} réponse {command}] {args}", "[{nick} {command} reply] {args}");
            print_active(st, id, LineKind::Ctcp, text.trim_end().to_owned());
            return;
        }
        let server = st.server(id).expect("serveur");
        let line = format!("-{nick}- {text}");
        if m.by_server() || server.status != ConnStatus::Connected {
            print(st, id, "", LineKind::Notice, line);
        } else if server.is_channel(target) {
            print(st, id, target, LineKind::Notice, line);
        } else {
            match common_channel(st, id, nick) {
                Some(channel) => print(st, id, &channel, LineKind::Notice, line),
                None => print_active(st, id, LineKind::Notice, line),
            }
            self.on_bot_notice(st, id, nick, text);
        }
    }

    // ---------------------------------------------------------------- saisie

    /// Ligne saisie dans une fenêtre (`buffer` vide : fenêtre d'état).
    fn input(&mut self, st: &mut State, id: ServerId, buffer: &str, text: &str) {
        for line in text.lines().filter(|l| !l.is_empty()) {
            match line.strip_prefix('/') {
                Some(command) if !command.starts_with('/') => self.command(st, id, buffer, command),
                _ => self.say(st, id, buffer, line.strip_prefix('/').unwrap_or(line)),
            }
        }
    }

    fn say(&mut self, st: &mut State, id: ServerId, buffer: &str, text: &str) {
        let Some(server) = st.server(id) else { return };
        let Some(buf) = server.buffer(buffer).filter(|b| b.kind != BufKind::Status) else {
            print(
                st,
                id,
                "",
                LineKind::Info,
                tr!(
                    "Pas de destinataire ici : rejoignez un canal ou utilisez /msg",
                    "No target here: join a channel or use /msg"
                ),
            );
            return;
        };
        if server.status != ConnStatus::Connected {
            print(
                st,
                id,
                buffer,
                LineKind::Info,
                tr!("Non connecté au serveur", "Not connected to a server"),
            );
            return;
        }
        let prefix = buf
            .user_index(&server.nick)
            .and_then(|i| buf.users[i].prefixes.chars().next())
            .map(String::from)
            .unwrap_or_default();
        let (target, nick) = (buf.name.clone(), server.nick.clone());
        for part in irc::split_text(text, MAX_TEXT) {
            self.send(id, format!("PRIVMSG {target} :{part}"));
            print(st, id, buffer, LineKind::Own, format!("<{prefix}{nick}> {part}"));
        }
    }

    fn command(&mut self, st: &mut State, id: ServerId, buffer: &str, line: &str) {
        let (name, args) = line.split_once(' ').unwrap_or((line, ""));
        let args = args.trim();
        let name = name.to_ascii_lowercase();
        let Some(server) = st.server(id) else { return };
        let current = server
            .buffer(buffer)
            .filter(|b| b.kind != BufKind::Status)
            .map(|b| b.name.clone());
        let current_channel = current.clone().filter(|c| server.is_channel(c));
        let nick = server.nick.clone();
        let (first, rest) = args.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((args, ""));
        let usage = |st: &mut State, text: String| {
            print(st, id, buffer, LineKind::Info, tr!("Usage : {text}", "Usage: {text}"))
        };
        let not_connected = |st: &mut State| {
            print(
                st,
                id,
                buffer,
                LineKind::Info,
                tr!(
                    "/{name} : non connecté au serveur",
                    "/{name}: not connected to a server"
                ),
            )
        };
        let linked = self.sessions.get(&id).is_some_and(|s| s.link.is_some());
        // Commandes sans connexion.
        match name.as_str() {
            "server" => {
                let new_window = first == "-m";
                let spec = if new_window { rest } else { args };
                let (addr, password) = spec.split_once(' ').unwrap_or((spec, ""));
                if addr.is_empty() && !new_window {
                    self.reconnect(st, id);
                    return;
                }
                let Some(addr) = ServerAddr::parse(addr) else {
                    usage(
                        st,
                        tr!(
                            "/server [-m] hôte[:port | :+port_tls] [mot de passe]",
                            "/server [-m] host[:port | :+tls_port] [password]"
                        ),
                    );
                    return;
                };
                if !password.is_empty() {
                    print(
                        st,
                        id,
                        buffer,
                        LineKind::Info,
                        tr!(
                            "Mot de passe : à définir dans Options → Serveurs",
                            "Password: set it in Options → Servers"
                        ),
                    );
                }
                if new_window {
                    self.open_server(st, None, addr);
                } else {
                    self.disconnect(st, id, None);
                    self.connection_lost(st, id, &tr!("changement de serveur", "changing server"));
                    self.sessions.remove(&id);
                    let network = st.config.network_of_host(&addr.host).map(|n| n.name.clone());
                    if let Some(server) = st.server_mut(id) {
                        server.label = network.clone().unwrap_or_else(|| addr.host.clone());
                        server.buffers[0].rename(&server.label.clone());
                        server.network = network;
                        server.addr = addr;
                    }
                    self.sessions.insert(id, Session::default());
                    self.start(st, id);
                }
                return;
            }
            "reconnect" => return self.reconnect(st, id),
            "disconnect" | "quit" => {
                // Sans connexion, annule seulement une reconnexion en attente.
                if !linked {
                    not_connected(st);
                }
                return self.disconnect(st, id, (!args.is_empty()).then_some(args));
            }
            "clear" => {
                if let Some(b) = st.server_mut(id).and_then(|s| s.buffer_mut(buffer)) {
                    b.lines.clear();
                }
                return;
            }
            "close" => return self.close_buffer(st, id, buffer),
            "query" | "q" => {
                if first.is_empty() {
                    return usage(st, tr!("/query pseudo [message]", "/query nick [message]"));
                }
                let key = st.server_mut(id).expect("serveur").open_buffer(first).key.clone();
                st.focus = Some(Focus::Buffer(id, key.clone()));
                if !rest.is_empty() {
                    self.say(st, id, &key, rest);
                }
                return;
            }
            "dcc" | "transfers" => {
                st.focus = Some(Focus::Transfers);
                return;
            }
            "help" => {
                st.help = true;
                return;
            }
            _ => {}
        }
        if !self.connected(st, id) && name != "raw" && name != "quote" {
            return not_connected(st);
        }
        match name.as_str() {
            "join" | "j" => {
                if first.is_empty() {
                    return usage(
                        st,
                        tr!("/join #canal[,#canal…] [clé]", "/join #channel[,#channel…] [key]"),
                    );
                }
                let chantypes = st.server(id).map(|s| s.chantypes.clone()).unwrap_or_default();
                let channels: Vec<String> = first
                    .split(',')
                    .map(|c| {
                        if c.starts_with(|ch| chantypes.contains(ch)) {
                            c.to_owned()
                        } else {
                            format!("#{c}")
                        }
                    })
                    .collect();
                if let Some(session) = self.sessions.get_mut(&id) {
                    session.focus_joins.extend(channels.iter().map(|c| irc::lower(c)));
                }
                let channels = channels.join(",");
                self.join(st, id, &channels, rest);
            }
            "part" | "leave" => {
                let (channel, reason) = match st.server(id) {
                    Some(s) if s.is_channel(first) => (first.to_owned(), rest),
                    _ => match &current_channel {
                        Some(c) => (c.clone(), args),
                        None => return usage(st, tr!("/part [#canal] [raison]", "/part [#channel] [reason]")),
                    },
                };
                self.send(id, format!("PART {channel} :{reason}"));
            }
            "msg" | "m" => {
                if first.is_empty() || rest.is_empty() {
                    return usage(st, tr!("/msg pseudo|#canal message", "/msg nick|#channel message"));
                }
                if let Some(packs) = xdcc_request(rest) {
                    self.enqueue(st, id, buffer, first, &packs);
                    return;
                }
                self.message(st, id, buffer, first, rest);
            }
            "ns" | "cs" | "ms" | "hs" | "bs" | "os" => {
                let service = match name.as_str() {
                    "ns" => "NickServ",
                    "cs" => "ChanServ",
                    "ms" => "MemoServ",
                    "hs" => "HostServ",
                    "bs" => "BotServ",
                    _ => "OperServ",
                };
                self.message(st, id, buffer, service, args);
            }
            "me" | "action" => match &current {
                Some(target) => {
                    self.send(id, format!("PRIVMSG {target} :\x01ACTION {args}\x01"));
                    print(st, id, buffer, LineKind::Action, format!("{nick} {args}"));
                }
                None => print(
                    st,
                    id,
                    buffer,
                    LineKind::Info,
                    tr!("Pas de destinataire ici", "No target here"),
                ),
            },
            "notice" => {
                if first.is_empty() || rest.is_empty() {
                    return usage(
                        st,
                        tr!("/notice pseudo|#canal message", "/notice nick|#channel message"),
                    );
                }
                self.send(id, format!("NOTICE {first} :{rest}"));
                print(st, id, buffer, LineKind::Notice, format!("-> -{first}- {rest}"));
            }
            "ctcp" => {
                if first.is_empty() || rest.is_empty() {
                    return usage(
                        st,
                        tr!("/ctcp pseudo commande [arguments]", "/ctcp nick command [arguments]"),
                    );
                }
                self.ctcp(st, id, buffer, first, rest);
            }
            "ping" | "version" | "time" | "clientinfo" if !first.is_empty() => {
                let request = if name == "ping" {
                    format!("PING {}", jiff::Timestamp::now().as_millisecond())
                } else {
                    name.to_ascii_uppercase()
                };
                self.ctcp(st, id, buffer, first, &request);
            }
            "nick" if !first.is_empty() => self.send(id, format!("NICK {first}")),
            "topic" => match st.server(id) {
                Some(s) if s.is_channel(first) => {
                    let line = if rest.is_empty() {
                        format!("TOPIC {first}")
                    } else {
                        format!("TOPIC {first} :{rest}")
                    };
                    self.send(id, line);
                }
                _ => match &current_channel {
                    Some(c) if args.is_empty() => self.send(id, format!("TOPIC {c}")),
                    Some(c) => self.send(id, format!("TOPIC {c} :{args}")),
                    None => usage(st, tr!("/topic [#canal] [sujet]", "/topic [#channel] [topic]")),
                },
            },
            "mode" => match &current_channel {
                Some(c) if args.starts_with(['+', '-']) => self.send(id, format!("MODE {c} {args}")),
                _ if args.is_empty() => usage(st, tr!("/mode [#canal|pseudo] modes", "/mode [#channel|nick] modes")),
                _ => self.send(id, format!("MODE {args}")),
            },
            "kick" | "k" => {
                let (channel, who) = match st.server(id) {
                    Some(s) if s.is_channel(first) => (first.to_owned(), rest),
                    _ => match &current_channel {
                        Some(c) => (c.clone(), args),
                        None => {
                            return usage(
                                st,
                                tr!("/kick [#canal] pseudo [raison]", "/kick [#channel] nick [reason]"),
                            );
                        }
                    },
                };
                let (victim, reason) = who.split_once(' ').unwrap_or((who, ""));
                self.send(id, format!("KICK {channel} {victim} :{reason}"));
            }
            "op" | "deop" | "voice" | "devoice" => {
                let Some(channel) = &current_channel else {
                    return print(
                        st,
                        id,
                        buffer,
                        LineKind::Info,
                        tr!("À utiliser dans un canal", "Use this in a channel"),
                    );
                };
                let nicks: Vec<&str> = args.split_whitespace().collect();
                let sign = if name.starts_with("de") { '-' } else { '+' };
                let mode = if name.ends_with("op") { 'o' } else { 'v' };
                for chunk in nicks.chunks(3) {
                    let modes: String = std::iter::once(sign).chain(chunk.iter().map(|_| mode)).collect();
                    self.send(id, format!("MODE {channel} {modes} {}", chunk.join(" ")));
                }
            }
            "invite" => {
                let channel = if rest.is_empty() {
                    current_channel.clone()
                } else {
                    Some(rest.to_owned())
                };
                match channel {
                    Some(c) if !first.is_empty() => self.send(id, format!("INVITE {first} {c}")),
                    _ => usage(st, tr!("/invite pseudo [#canal]", "/invite nick [#channel]")),
                }
            }
            "whois" | "w" if !first.is_empty() => self.send(id, format!("WHOIS {first}")),
            "away" => self.send(
                id,
                if args.is_empty() {
                    "AWAY".into()
                } else {
                    format!("AWAY :{args}")
                },
            ),
            "back" => self.send(id, "AWAY".into()),
            "names" => match current_channel.as_deref().filter(|_| args.is_empty()).or(Some(args)) {
                Some(c) if !c.is_empty() => self.send(id, format!("NAMES {c}")),
                _ => usage(st, tr!("/names #canal", "/names #channel")),
            },
            "raw" | "quote" => {
                if linked {
                    print(
                        st,
                        id,
                        buffer,
                        LineKind::Own,
                        tr!("-> serveur : {args}", "-> server: {args}"),
                    );
                    self.send(id, args.to_owned());
                } else {
                    not_connected(st);
                }
            }
            "xdcc" => self.xdcc_command(st, id, buffer, args),
            // Commande inconnue : envoyée telle quelle au serveur.
            _ => self.send(
                id,
                format!("{} {args}", name.to_ascii_uppercase()).trim_end().to_owned(),
            ),
        }
    }

    fn message(&mut self, st: &mut State, id: ServerId, buffer: &str, target: &str, text: &str) {
        let shown = mask_password(text);
        for part in irc::split_text(text, MAX_TEXT) {
            self.send(id, format!("PRIVMSG {target} :{part}"));
        }
        let open = st.server(id).and_then(|s| s.buffer(target)).is_some();
        if open && !irc::eq(target, buffer) {
            let nick = st.server(id).map(|s| s.nick.clone()).unwrap_or_default();
            print(st, id, target, LineKind::Own, format!("<{nick}> {shown}"));
        }
        print(st, id, buffer, LineKind::Own, format!("-> *{target}* {shown}"));
    }

    fn ctcp(&mut self, st: &mut State, id: ServerId, buffer: &str, target: &str, request: &str) {
        let (command, args) = request.split_once(' ').unwrap_or((request, ""));
        let request = format!("{} {args}", command.to_ascii_uppercase());
        let request = request.trim_end();
        self.send(id, format!("PRIVMSG {target} :\x01{request}\x01"));
        print(st, id, buffer, LineKind::Ctcp, format!("-> [{target}] {request}"));
    }

    fn reconnect(&mut self, st: &mut State, id: ServerId) {
        // L'ancienne connexion ne signalera pas sa fermeture (génération dépassée) : on le fait ici.
        if self.sessions.get(&id).is_some_and(|s| s.link.is_some()) {
            self.disconnect(st, id, None);
            self.connection_lost(st, id, &tr!("reconnexion", "reconnecting"));
        }
        self.start(st, id);
    }

    fn close_buffer(&mut self, st: &mut State, id: ServerId, buffer: &str) {
        if buffer.is_empty() {
            // Fenêtre d'état : déconnexion et fermeture de tout le serveur.
            self.disconnect(st, id, None);
            self.sessions.remove(&id);
            st.servers.retain(|s| s.id != id);
            let running: Vec<TransferId> = st
                .transfers
                .iter()
                .filter(|t| t.server == id && !t.status.finished())
                .map(|t| t.id)
                .collect();
            for tid in running {
                self.transfer_action(st, tid, TransferAction::Cancel);
            }
            for item in st.queue.iter_mut().filter(|q| q.server == id && !q.status.finished()) {
                item.status = QueueStatus::Cancelled;
            }
            return;
        }
        let Some(server) = st.server_mut(id) else { return };
        if let Some(b) = server.buffer(buffer)
            && b.kind == BufKind::Channel
            && b.joined
        {
            let name = b.name.clone();
            self.send(id, format!("PART {name}"));
        }
        let key = irc::lower(buffer);
        if let Some(server) = st.server_mut(id) {
            server.buffers.retain(|b| b.kind == BufKind::Status || b.key != key);
        }
    }

    fn on_command(&mut self, st: &mut State, cmd: Command) {
        match cmd {
            Command::ConnectNetwork(name) => {
                let Some(net) = st.config.network(&name).cloned() else {
                    return;
                };
                let Some(addr) = net.servers.first().cloned() else {
                    return;
                };
                self.open_server(st, Some(net.name), addr);
            }
            Command::Connect(addr) => {
                self.open_server(st, None, addr);
            }
            Command::JoinFavorite(index) => self.join_favorite(st, index),
            Command::Input { server, buffer, text } => self.input(st, server, &buffer, &text),
            Command::Disconnect(id) => self.disconnect(st, id, None),
            Command::Reconnect(id) => self.reconnect(st, id),
            Command::CloseBuffer(id, buffer) => self.close_buffer(st, id, &buffer),
            Command::Transfer(id, action) => self.transfer_action(st, id, action),
            Command::Queue(id, action) => self.queue_action(st, id, action),
            Command::ClearFinished => {
                st.transfers.retain(|t| !t.status.finished());
                st.queue.retain(|q| !q.status.finished());
            }
            Command::ConfigChanged => {
                self.update_identd(st);
                self.schedule(st);
            }
            Command::Quit => {
                let ids: Vec<ServerId> = self.sessions.keys().copied().collect();
                for id in ids {
                    self.disconnect(st, id, None);
                }
            }
        }
    }

    // ---------------------------------------------------------------- DCC

    fn on_dcc(&mut self, st: &mut State, id: ServerId, nick: &str, args: &str) {
        let (kind, rest) = args.split_once(' ').unwrap_or((args, ""));
        match kind.to_ascii_uppercase().as_str() {
            "SEND" => match dcc::parse_send(rest) {
                // Port 0 sans jeton : ni actif ni passif, inutilisable.
                Some(offer) if offer.port != 0 || offer.token.is_some() => self.on_offer(st, id, nick, offer),
                _ => print(
                    st,
                    id,
                    "",
                    LineKind::Ctcp,
                    tr!(
                        "DCC SEND de {nick} invalide : {rest}",
                        "Invalid DCC SEND from {nick}: {rest}"
                    ),
                ),
            },
            "ACCEPT" => match dcc::parse_accept(rest) {
                Some(accept) => {
                    let found = st.transfers.iter().find(|t| {
                        t.server == id
                            && irc::eq(&t.nick, nick)
                            && t.status == TransferStatus::Resuming
                            && if t.offer.passive() {
                                accept.token.is_some() && t.offer.token == accept.token
                            } else {
                                t.offer.port == accept.port
                            }
                    });
                    if let Some(t) = found {
                        let tid = t.id;
                        self.start_transfer(st, tid, accept.position);
                    }
                }
                None => print(
                    st,
                    id,
                    "",
                    LineKind::Ctcp,
                    tr!(
                        "DCC ACCEPT de {nick} invalide : {rest}",
                        "Invalid DCC ACCEPT from {nick}: {rest}"
                    ),
                ),
            },
            _ => print_active(
                st,
                id,
                LineKind::Ctcp,
                tr!(
                    "{nick} propose un DCC {kind} (non pris en charge)",
                    "{nick} offers a DCC {kind} (not supported)"
                ),
            ),
        }
    }

    fn on_offer(&mut self, st: &mut State, id: ServerId, nick: &str, offer: Offer) {
        let filename = dcc::sanitize(&offer.filename);
        let size = offer
            .size
            .map(human_size)
            .unwrap_or_else(|| tr!("taille inconnue", "unknown size"));
        // Pack demandé à ce bot ; à défaut, un pack déjà demandé puis remis en attente ou
        // abandonné : le bot muet trop longtemps a fini par l'envoyer (à ne pas redemander).
        let from_bot = |q: &&QueueItem| q.server == id && irc::eq(&q.bot, nick);
        let requested = st
            .queue
            .iter()
            .position(|q| from_bot(&q) && q.status.awaiting_bot())
            .or_else(|| {
                st.queue.iter().position(|q| {
                    from_bot(&q) && q.asked && matches!(q.status, QueueStatus::Waiting | QueueStatus::Failed(_))
                })
            });
        let mut ask = false;
        if requested.is_none() {
            if let Some(reason) = dcc::ignored(&st.config.dcc, nick, &filename) {
                let text = tr!(
                    "Envoi DCC de {nick} ignoré ({reason}) : {filename}",
                    "Ignored DCC send from {nick} ({reason}): {filename}"
                );
                print(st, id, "", LineKind::Ctcp, text);
                return;
            }
            match st.config.dcc.on_offer {
                OfferAction::Ignore => {
                    let text = tr!(
                        "Envoi DCC de {nick} ignoré : {filename} ({size})",
                        "Ignored DCC send from {nick}: {filename} ({size})"
                    );
                    print(st, id, "", LineKind::Ctcp, text);
                    return;
                }
                OfferAction::Ask => ask = true,
                OfferAction::Accept => {}
            }
        }
        let tid = st.next_id();
        let path = st.config.dcc.download_dir.join(&filename);
        if let Some(i) = requested {
            st.queue[i].status = QueueStatus::Transferring(tid);
            st.queue[i].filename = filename.clone();
        }
        let mode = if offer.passive() {
            tr!(" (DCC passif)", " (passive DCC)")
        } else {
            String::new()
        };
        print(
            st,
            id,
            "",
            LineKind::Ctcp,
            tr!(
                "Envoi DCC de {nick}{mode} : {filename} ({size})",
                "DCC send from {nick}{mode}: {filename} ({size})"
            ),
        );
        st.transfers.push(Transfer {
            id: tid,
            server: id,
            nick: nick.to_owned(),
            offer,
            path,
            offset: 0,
            position: Default::default(),
            status: TransferStatus::AskAccept,
            started: None,
            ended: None,
            queue_item: requested.map(|i| st.queue[i].id),
            resume_sent: None,
            announced: None,
            complete: false,
        });
        if ask {
            st.attention = true;
        } else {
            self.accept(st, tid, None);
        }
    }

    /// Accepte un envoi : gère le fichier existant (reprise, écrasement, renommage).
    fn accept(&mut self, st: &mut State, tid: TransferId, choice: Option<ExistsAction>) {
        let action = choice.unwrap_or(st.config.dcc.if_exists);
        let Some(t) = st.transfer_mut(tid) else { return };
        if let Some(dir) = t.path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            let dir = dir.display();
            let error = tr!("dossier {dir} : {e}", "folder {dir}: {e}");
            return self.end_transfer(st, tid, Err(error));
        }
        let existing = std::fs::metadata(&t.path).map(|m| m.len()).ok();
        match (existing, action) {
            (None, _) | (Some(_), ExistsAction::Overwrite) => {}
            (Some(_), ExistsAction::Ask) => {
                t.status = TransferStatus::AskExists;
                st.attention = true;
                return;
            }
            (Some(_), ExistsAction::Rename) => t.path = dcc::unique_path(&t.path),
            (Some(mut len), ExistsAction::Resume) => {
                // Fichier déjà complet : reprise sur le dernier octet (renvoyé à l'identique),
                // pour que l'émetteur clôture son envoi ; sans réponse il le laisserait en attente
                // et refuserait les demandes suivantes, et une reprise à la fin même est refusée.
                if let Some(size) = t.offer.size.filter(|&size| size > 0 && len >= size) {
                    t.complete = true;
                    len = size - 1;
                }
                if len > 0 {
                    t.offset = len;
                    t.status = TransferStatus::Resuming;
                    t.resume_sent = Some(Instant::now());
                    let o = &t.offer;
                    let port = if o.passive() { 0 } else { o.port };
                    let token = o.token.as_ref().map(|t| format!(" {t}")).unwrap_or_default();
                    let line = format!(
                        "PRIVMSG {} :\x01DCC RESUME {} {port} {len}{token}\x01",
                        t.nick,
                        dcc::quote(&o.filename)
                    );
                    let server = t.server;
                    self.send(server, line);
                    return;
                }
            }
        }
        self.start_transfer(st, tid, 0);
    }

    fn start_transfer(&mut self, st: &mut State, tid: TransferId, offset: u64) {
        let config = st.config.dcc.clone();
        let Some(t) = st.transfer_mut(tid) else { return };
        t.offset = offset;
        t.position.store(offset, Ordering::Relaxed);
        let mut passive_port = None;
        let endpoint = if t.offer.passive() {
            let listener = match dcc::listen(&config) {
                Ok(listener) => listener,
                Err(e) => return self.end_transfer(st, tid, Err(e)),
            };
            passive_port = listener.local_addr().map(|a| a.port()).ok();
            t.status = TransferStatus::Listening;
            Endpoint::Listen(listener)
        } else {
            match t.offer.ip {
                Some(ip) => {
                    t.status = TransferStatus::Connecting;
                    Endpoint::Connect(SocketAddr::new(ip, t.offer.port))
                }
                None => {
                    return self.end_transfer(
                        st,
                        tid,
                        Err(tr!("adresse de l'émetteur invalide", "invalid sender address")),
                    );
                }
            }
        };
        let job = Job {
            endpoint,
            path: t.path.clone(),
            offset,
            size: t.offer.size,
            timeout: dcc::idle_timeout(&config),
            position: t.position.clone(),
        };
        let tx = self.tx.clone();
        let task = tokio::spawn(async move {
            let started = tx.clone();
            let result = dcc::receive(job, move || {
                let _ = started.send(CoreMsg::Transfer(tid, TransferEvent::Started));
            })
            .await;
            let _ = tx.send(CoreMsg::Transfer(tid, TransferEvent::Ended(result)));
        });
        self.tasks.insert(tid, task.abort_handle());
        let Some(port) = passive_port else { return };
        if config.upnp {
            // L'offre ne part qu'une fois le port ouvert sur la box (ou l'UPnP en échec).
            let text = tr!(
                "UPnP : ouverture du port {port} sur la box…",
                "UPnP: opening port {port} on the router…"
            );
            print_transfer(st, tid, LineKind::Info, text);
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let result = upnp::open_dcc(port).await;
                let _ = tx.send(CoreMsg::Upnp(tid, port, result));
            });
        } else {
            self.passive_offer(st, tid, port, None);
        }
    }

    /// Offre d'un DCC passif : l'émetteur est invité à se connecter à notre adresse publique
    /// (choisie dans les options, sinon donnée par la box, sinon vue du serveur) sur `port`.
    fn passive_offer(&mut self, st: &mut State, tid: TransferId, port: u16, upnp_ip: Option<IpAddr>) {
        let Some(t) = st.transfers.iter().find(|t| t.id == tid) else {
            return;
        };
        if t.status != TransferStatus::Listening {
            return;
        }
        let (server, nick) = (t.server, t.nick.clone());
        let configured = st.config.dcc.external_ip.trim().parse().ok();
        let upnp_ip = upnp_ip.filter(IpAddr::is_ipv4);
        let Some(ip) = configured.or(upnp_ip).or_else(|| self.dcc_ip(st, server)) else {
            return self.end_transfer(
                st,
                tid,
                Err(tr!(
                    "adresse IPv4 publique inconnue (Options → DCC)",
                    "unknown public IPv4 address (Options → DCC)"
                )),
            );
        };
        let o = &t.offer;
        let line = format!(
            "PRIVMSG {nick} :\x01DCC SEND {} {} {port} {} {}\x01",
            dcc::quote(&o.filename),
            dcc::format_ip(ip),
            o.size.unwrap_or(0),
            o.token.as_deref().unwrap_or("")
        );
        self.send(server, line);
        if let Some(t) = st.transfer_mut(tid) {
            t.announced = Some(SocketAddr::new(ip, port));
        }
        let mut info = tr!(
            "DCC passif : {nick} doit se connecter à {ip}:{port}",
            "Passive DCC: {nick} must connect to {ip}:{port}"
        );
        let local = matches!(ip, IpAddr::V4(v4) if v4.is_private());
        if local {
            info.push_str(&tr!(
                " (adresse locale : indiquez l'adresse publique dans Options → DCC)",
                " (local address: enter the public address in Options → DCC)"
            ));
        }
        let kind = if local { LineKind::Error } else { LineKind::Info };
        print_transfer(st, tid, kind, info);
    }

    /// Adresse publique (DCC passif) d'après notre hôte vu par le serveur : un nom résolu par DNS
    /// (impossible pour un hôte masqué : l'adresse locale servira alors), ou une adresse IP, dont
    /// le nom est alors cherché par DNS inverse pour l'affichage.
    fn find_public_ip(&self, id: ServerId, host: &str) {
        if let Ok(ip) = host.parse::<IpAddr>() {
            // Une adresse IPv6 ne sert pas : les DCC sont en IPv4.
            if ip.is_ipv4() {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let name = dns_lookup::lookup_addr(&ip).unwrap_or_else(|_| ip.to_string());
                    let _ = tx.send(CoreMsg::PublicIp(id, name, ip));
                });
            }
        } else if !host.is_empty() {
            let (tx, host) = (self.tx.clone(), host.to_owned());
            tokio::spawn(async move {
                let ip = tokio::net::lookup_host((host.as_str(), 0))
                    .await
                    .ok()
                    .and_then(|mut addrs| addrs.find(SocketAddr::is_ipv4));
                if let Some(addr) = ip {
                    let _ = tx.send(CoreMsg::PublicIp(id, host, addr.ip()));
                }
            });
        }
    }

    /// Adresse IPv4 annoncée pour un DCC passif, faute de mieux : vue du serveur, sinon locale.
    fn dcc_ip(&self, st: &State, id: ServerId) -> Option<IpAddr> {
        let server = st.server(id)?;
        server.public_ip.or(server.local_ip).filter(IpAddr::is_ipv4)
    }

    fn on_transfer(&mut self, st: &mut State, tid: TransferId, event: TransferEvent) {
        match event {
            TransferEvent::Started => {
                if let Some(t) = st.transfer_mut(tid)
                    && !t.status.finished()
                {
                    t.status = TransferStatus::Receiving;
                    t.started = Some(Instant::now());
                    sound::notify(&st.config.sounds, Event::Started);
                }
            }
            // Sans effet si le transfert est déjà terminé (annulé…).
            TransferEvent::Ended(result) => self.end_transfer(st, tid, result),
        }
    }

    /// Fin d'un transfert, une seule fois : message, son, suite de la file. La tâche de réception
    /// encore en cours (DCC passif sans adresse à annoncer…) est arrêtée : elle ne le terminera
    /// pas une seconde fois à l'expiration de son délai.
    fn end_transfer(&mut self, st: &mut State, tid: TransferId, result: Result<(), String>) {
        let retry = st.config.xdcc.retries;
        let delay = Duration::from_secs(st.config.xdcc.retry_delay_secs);
        let Some(t) = st.transfer_mut(tid) else { return };
        if t.status.finished() {
            return;
        }
        if let Some(task) = self.tasks.remove(&tid) {
            task.abort();
        }
        t.ended = Some(Instant::now());
        let (name, nick) = (dcc::file_name(&t.path), t.nick.clone());
        let item = t.queue_item;
        // Fichier déjà complet sur le disque : rien de nouveau. Le son de fin accompagne celui de
        // début (une connexion a bien été ouverte pour le dernier octet), sinon il n'y en a pas.
        let already_complete = result.is_ok() && t.complete;
        let silent = already_complete && t.started.is_none();
        let text = match &result {
            Ok(()) if already_complete => {
                t.status = TransferStatus::Done;
                tr!(
                    "Réception DCC de {name} depuis {nick} : déjà téléchargé",
                    "DCC receive of {name} from {nick}: already downloaded"
                )
            }
            Ok(()) => {
                t.status = TransferStatus::Done;
                // Durée et débit moyen, comme « 00:00:09, 556 Kio/s ».
                let stats = t
                    .started
                    .map(|s| {
                        let secs = s.elapsed().as_secs_f64();
                        let bytes = t.position.load(Ordering::Relaxed) - t.offset;
                        let speed = human_size((bytes as f64 / secs.max(0.001)) as u64);
                        let secs = secs as u64;
                        format!(
                            " ({:02}:{:02}:{:02}, {speed}/s)",
                            secs / 3600,
                            secs / 60 % 60,
                            secs % 60
                        )
                    })
                    .unwrap_or_default();
                tr!(
                    "Réception DCC de {name} depuis {nick} terminée{stats}",
                    "DCC receive of {name} from {nick} completed{stats}"
                )
            }
            Err(e) => {
                t.status = TransferStatus::Failed(e.clone());
                tr!(
                    "Réception DCC de {name} depuis {nick} échouée ({e})",
                    "DCC receive of {name} from {nick} failed ({e})"
                )
            }
        };
        let event = if result.is_ok() {
            Event::Completed
        } else {
            Event::Failed
        };
        print_transfer(st, tid, LineKind::Ctcp, text);
        if !silent {
            sound::notify(&st.config.sounds, event);
        }
        // Un pack annulé entre-temps le reste.
        if let Some(item) = item.and_then(|i| st.queue_item_mut(i)).filter(|i| !i.status.finished()) {
            match result {
                Ok(()) => item.status = QueueStatus::Done,
                Err(e) => {
                    item.attempts += 1;
                    if item.attempts <= retry {
                        item.status = QueueStatus::Waiting;
                        item.retry_at = Some(Instant::now() + delay);
                    } else {
                        item.status = QueueStatus::Failed(e);
                    }
                }
            }
        }
        self.schedule(st);
    }

    fn transfer_action(&mut self, st: &mut State, tid: TransferId, action: TransferAction) {
        let Some(t) = st.transfers.iter().find(|t| t.id == tid) else {
            return;
        };
        let status = t.status.clone();
        match action {
            TransferAction::Accept if status == TransferStatus::AskAccept => self.accept(st, tid, None),
            TransferAction::Exists(choice) if status == TransferStatus::AskExists => self.accept(st, tid, Some(choice)),
            TransferAction::Cancel if !status.finished() => {
                if let Some(task) = self.tasks.remove(&tid) {
                    task.abort();
                }
                let t = st.transfer_mut(tid).expect("transfert");
                t.status = TransferStatus::Cancelled;
                t.ended = Some(Instant::now());
                if let Some(item) = t.queue_item.and_then(|i| st.queue_item_mut(i)) {
                    item.status = QueueStatus::Cancelled;
                }
                self.schedule(st);
            }
            TransferAction::Remove if status.finished() => st.transfers.retain(|t| t.id != tid),
            _ => {}
        }
    }

    // ---------------------------------------------------------------- XDCC

    fn xdcc_command(&mut self, st: &mut State, id: ServerId, buffer: &str, args: &str) {
        let words: Vec<&str> = args.split_whitespace().collect();
        match words.as_slice() {
            [verb, bot] if verb.eq_ignore_ascii_case("list") => self.message(st, id, buffer, bot, "XDCC LIST"),
            [verb] if verb.eq_ignore_ascii_case("cancel") => {
                let items: Vec<QueueId> = st
                    .queue
                    .iter()
                    .filter(|q| q.server == id && !q.status.finished())
                    .map(|q| q.id)
                    .collect();
                for item in items {
                    self.queue_action(st, item, QueueAction::Cancel);
                }
            }
            [verb, bot, packs @ ..]
                if !packs.is_empty() && ["send", "get", "batch"].contains(&&*verb.to_lowercase()) =>
            {
                let packs = parse_packs(&packs.join(" "));
                self.enqueue(st, id, buffer, bot, &packs);
            }
            [bot, packs @ ..] if !packs.is_empty() => {
                let packs = parse_packs(&packs.join(" "));
                self.enqueue(st, id, buffer, bot, &packs);
            }
            _ => print(
                st,
                id,
                buffer,
                LineKind::Info,
                tr!(
                    "Usage : /xdcc bot #pack[,#pack|#début-#fin…] | /xdcc list bot | /xdcc cancel",
                    "Usage: /xdcc bot #pack[,#pack|#start-#end…] | /xdcc list bot | /xdcc cancel"
                ),
            ),
        }
    }

    /// Packs ajoutés à la file depuis la fenêtre `window`.
    fn enqueue(&mut self, st: &mut State, id: ServerId, window: &str, bot: &str, packs: &[String]) {
        if packs.is_empty() {
            print(
                st,
                id,
                window,
                LineKind::Info,
                tr!("Aucun numéro de pack valide", "No valid pack number"),
            );
            return;
        }
        for pack in packs {
            let item = QueueItem {
                id: st.next_id(),
                server: id,
                bot: bot.to_owned(),
                pack: pack.clone(),
                status: QueueStatus::Waiting,
                since: Instant::now(),
                retry_at: None,
                attempts: 0,
                asked: false,
                filename: String::new(),
                window: window.to_owned(),
            };
            st.queue.push(item);
        }
        let list = packs.join(", ");
        let text = tr!(
            "XDCC : {list} de {bot} ajouté(s) à la file",
            "XDCC: {list} from {bot} added to the queue"
        );
        print(st, id, window, LineKind::Info, text);
        self.schedule(st);
    }

    /// Demande les packs en attente : un seul à la fois par bot (le suivant
    /// quand il est terminé), tous les bots en même temps ; c'est le bot qui gère sa propre file.
    fn schedule(&mut self, st: &mut State) {
        let now = Instant::now();
        for i in 0..st.queue.len() {
            let item = &st.queue[i];
            if item.status != QueueStatus::Waiting || item.retry_at.is_some_and(|t| t > now) {
                continue;
            }
            let ready = self
                .sessions
                .get(&item.server)
                .and_then(|s| s.ready_at)
                .is_some_and(|t| t <= now);
            if !ready || !self.connected(st, item.server) {
                continue;
            }
            let busy = st
                .queue
                .iter()
                .any(|q| q.server == item.server && irc::eq(&q.bot, &item.bot) && q.status.active());
            if busy {
                continue;
            }
            let (server, bot, pack) = (item.server, item.bot.clone(), item.pack.clone());
            self.send(server, format!("PRIVMSG {bot} :XDCC SEND {pack}"));
            let item = &mut st.queue[i];
            item.status = QueueStatus::Requested;
            item.since = now;
            item.retry_at = None;
            item.asked = true;
            // La ligne envoyée s'affiche dans la fenêtre de la demande.
            let window = item.window.clone();
            print(
                st,
                server,
                &window,
                LineKind::Own,
                format!("-> *{bot}* XDCC SEND {pack}"),
            );
        }
    }

    /// Demandes restées sans réponse du bot, et files du bot sans nouvelles.
    fn check_requests(&mut self, st: &mut State) {
        let config = st.config.xdcc.clone();
        let limit = Duration::from_secs(config.request_timeout_secs);
        let mut silent: Vec<(ServerId, String)> = Vec::new();
        for item in &mut st.queue {
            match item.status {
                QueueStatus::Requested
                    if config.request_timeout_secs > 0
                        && item.since.elapsed() > limit
                        && !silent.iter().any(|(s, b)| *s == item.server && irc::eq(b, &item.bot)) =>
                {
                    silent.push((item.server, item.bot.clone()));
                }
                // Le pack est redemandé (le bot répond en général qu'il est déjà dans sa file,
                // avec la position).
                QueueStatus::BotQueue(_) if item.since.elapsed() > BOT_QUEUE_SILENCE => {
                    item.status = QueueStatus::Waiting;
                }
                _ => {}
            }
        }
        for (id, bot) in silent {
            self.bot_unreachable(st, id, &bot, &tr!("ne répond pas", "is not responding"), true);
        }
    }

    /// Notice d'un bot : il a répondu (pack invalide, file d'attente du bot, bot plein…).
    fn on_bot_notice(&mut self, st: &mut State, id: ServerId, nick: &str, text: &str) {
        let reply = bot_reply(&crate::format::strip(text));
        let Some(item) = st
            .queue
            .iter_mut()
            .find(|q| q.server == id && irc::eq(&q.bot, nick) && q.status.awaiting_bot())
        else {
            return;
        };
        item.since = Instant::now();
        match reply {
            BotReply::Invalid(reason) => item.status = QueueStatus::Failed(reason),
            // Nouvelle demande dans 60 à 120 secondes.
            BotReply::Full => {
                let jitter = jiff::Timestamp::now().subsec_millisecond().unsigned_abs() % 61;
                item.status = QueueStatus::Waiting;
                item.retry_at = Some(Instant::now() + Duration::from_secs(60 + u64::from(jitter)));
            }
            BotReply::Queued(position) => item.status = QueueStatus::BotQueue(position),
            BotReply::Other => return,
        }
        self.schedule(st);
    }

    /// Bot injoignable (parti, absent du réseau, muet) : ses demandes en cours seront refaites plus
    /// tard ; si `attempt`, cela compte comme une tentative (abandon au-delà du nombre choisi).
    fn bot_unreachable(&mut self, st: &mut State, id: ServerId, nick: &str, reason: &str, attempt: bool) {
        let config = st.config.xdcc.clone();
        let mut affected = None;
        for item in st
            .queue
            .iter_mut()
            .filter(|q| q.server == id && irc::eq(&q.bot, nick) && q.status.awaiting_bot())
        {
            affected = Some(item.window.clone());
            item.attempts += u32::from(attempt);
            if item.attempts <= config.retries {
                item.status = QueueStatus::Waiting;
                item.retry_at = Some(Instant::now() + Duration::from_secs(config.retry_delay_secs));
            } else {
                item.status = QueueStatus::Failed(format!("{nick} {reason}"));
            }
        }
        if let Some(window) = affected {
            print(
                st,
                id,
                &window,
                LineKind::Error,
                tr!("XDCC : {nick} {reason}", "XDCC: {nick} {reason}"),
            );
            sound::notify(&st.config.sounds, Event::BotOffline);
        }
    }

    fn queue_action(&mut self, st: &mut State, qid: QueueId, action: QueueAction) {
        let Some(index) = st.queue.iter().position(|q| q.id == qid) else {
            return;
        };
        let item = &st.queue[index];
        let (server, bot, status) = (item.server, item.bot.clone(), item.status.clone());
        match action {
            QueueAction::Cancel | QueueAction::Remove if !status.finished() => {
                match status {
                    // Retire aussi la demande de la file d'attente du bot.
                    QueueStatus::Requested | QueueStatus::BotQueue(_) => {
                        self.send(server, format!("PRIVMSG {bot} :XDCC REMOVE"));
                    }
                    QueueStatus::Transferring(tid) => self.transfer_action(st, tid, TransferAction::Cancel),
                    _ => {}
                }
                if let Some(item) = st.queue_item_mut(qid) {
                    item.status = QueueStatus::Cancelled;
                }
                if matches!(action, QueueAction::Remove) {
                    st.queue.retain(|q| q.id != qid);
                }
                self.schedule(st);
            }
            QueueAction::Remove => st.queue.retain(|q| q.id != qid),
            QueueAction::Retry if status.finished() => {
                let item = &mut st.queue[index];
                item.status = QueueStatus::Waiting;
                item.attempts = 0;
                item.retry_at = None;
                self.schedule(st);
            }
            QueueAction::Up if index > 0 => st.queue.swap(index, index - 1),
            QueueAction::Down if index + 1 < st.queue.len() => st.queue.swap(index, index + 1),
            _ => {}
        }
    }
}

// -------------------------------------------------------------------- utilitaires

/// Aide de `/help` et du menu Aide → Commandes.
pub fn help() -> String {
    tr!(
        "\
Commandes :
  /server [-m] hôte[:port|:+port_tls]   connexion (-m : nouvelle fenêtre de serveur)
  /join #canal [clé]   /part [#canal] [raison]   /query pseudo [message]   /close   /clear
  /msg cible message   /notice cible message   /me action   /nick pseudo   /quit [message]
  /topic [#canal] [sujet]   /mode …   /kick [#canal] pseudo [raison]   /op /deop /voice /devoice pseudos
  /whois pseudo   /ctcp pseudo commande   /ping /version /time pseudo   /away [message]   /invite pseudo [#canal]
  /ns /cs /ms … message   (NickServ, ChanServ, MemoServ…)   /raw ligne   /reconnect   /disconnect
  /xdcc bot #1,#3,#5-#8   file de téléchargement (aussi : /msg bot xdcc send #1)
  /xdcc list bot   /xdcc cancel   /dcc : fenêtre des transferts
Fenêtres : Ctrl+Tab ou Ctrl+Page suiv. suivante, Ctrl+Shift+Tab ou Ctrl+Page préc. précédente,
  Alt+← Alt+→ visitée avant / après, Ctrl+W fermer ;
  F1 aide, Alt+O options, Échap ou Alt+F4 fermer la boîte de dialogue.
Défilement : Page préc. Page suiv. une page, Ctrl+↑ Ctrl+↓ une ligne, Ctrl+L trait des lignes lues,
  Ctrl+Début Ctrl+Fin tout en haut / tout en bas (saisie vide).
Saisie : Ctrl+Entrée envoyer comme du texte, Tab complétion des pseudos, ↑ ↓ historique,
  Ctrl+K couleur, Ctrl+B gras, Ctrl+U souligné, Ctrl+I italique, Ctrl+O normal.",
        "\
Commands:
  /server [-m] host[:port|:+tls_port]   connect (-m: new server window)
  /join #channel [key]   /part [#channel] [reason]   /query nick [message]   /close   /clear
  /msg target message   /notice target message   /me action   /nick nick   /quit [message]
  /topic [#channel] [topic]   /mode …   /kick [#channel] nick [reason]   /op /deop /voice /devoice nicks
  /whois nick   /ctcp nick command   /ping /version /time nick   /away [message]   /invite nick [#channel]
  /ns /cs /ms … message   (NickServ, ChanServ, MemoServ…)   /raw line   /reconnect   /disconnect
  /xdcc bot #1,#3,#5-#8   download queue (also: /msg bot xdcc send #1)
  /xdcc list bot   /xdcc cancel   /dcc: transfers window
Windows: Ctrl+Tab or Ctrl+Page Down next, Ctrl+Shift+Tab or Ctrl+Page Up previous,
  Alt+← Alt+→ back / forward, Ctrl+W close;
  F1 help, Alt+O options, Escape or Alt+F4 close the dialog.
Scrolling: Page Up Page Down one page, Ctrl+↑ Ctrl+↓ one line, Ctrl+L read marker,
  Ctrl+Home Ctrl+End top / bottom (empty input).
Input: Ctrl+Enter send as plain text, Tab nick completion, ↑ ↓ history,
  Ctrl+K color, Ctrl+B bold, Ctrl+U underline, Ctrl+I italic, Ctrl+O plain."
    )
}

fn favorite_matches(fav: &crate::config::Favorite, server: &Server) -> bool {
    let network = !fav.network.is_empty()
        && (server
            .network
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(&fav.network))
            || server.label.eq_ignore_ascii_case(&fav.network));
    network || (!fav.server.host.is_empty() && fav.server.host.eq_ignore_ascii_case(&server.addr.host))
}

fn join_line(channel: &str, key: &str) -> String {
    if key.is_empty() {
        format!("JOIN {channel}")
    } else {
        format!("JOIN {channel} {key}")
    }
}

fn add_user(server: &mut Server, channel: &str, nick: &str, prefixes: &str) {
    let Some(buffer) = server.buffer_mut(channel) else {
        return;
    };
    match buffer.user_index(nick) {
        Some(i) => buffer.users[i].prefixes = prefixes.to_owned(),
        None => buffer.users.push(User {
            nick: nick.to_owned(),
            prefixes: prefixes.to_owned(),
        }),
    }
    sort_users(server);
}

fn sort_users(server: &mut Server) {
    let ranks: Vec<char> = server.prefixes.iter().map(|&(_, p)| p).collect();
    let rank = |u: &User| {
        u.prefixes
            .chars()
            .next()
            .and_then(|p| ranks.iter().position(|&r| r == p))
            .unwrap_or(usize::MAX)
    };
    for buffer in server.buffers.iter_mut().filter(|b| b.kind == BufKind::Channel) {
        buffer.users.sort_by_cached_key(|u| (rank(u), irc::lower(&u.nick)));
    }
}

/// Le texte contient notre pseudo (mot entier) ou un mot surveillé.
fn highlights(st: &State, me: &str, text: &str) -> bool {
    let text = crate::format::strip(text).to_lowercase();
    let word_in = |word: &str| {
        let word = word.to_lowercase();
        !word.is_empty()
            && text.match_indices(&word).any(|(i, _)| {
                let before = text[..i].chars().next_back();
                let after = text[i + word.len()..].chars().next();
                before.is_none_or(|c| !c.is_alphanumeric()) && after.is_none_or(|c| !c.is_alphanumeric())
            })
    };
    word_in(me) || st.config.connection.highlight_words.iter().any(|w| word_in(w.trim()))
}

/// Masque le mot de passe des commandes de services (`identify`, `register`…).
fn mask_password(text: &str) -> String {
    let mut words = text.splitn(2, ' ');
    let first = words.next().unwrap_or("");
    let secret = ["identify", "id", "register", "ghost", "recover", "release", "regain"];
    match words.next() {
        Some(_) if secret.contains(&first.to_lowercase().as_str()) => format!("{first} ****"),
        _ => text.to_owned(),
    }
}

/// Texte d'une réponse numérique, sans notre pseudo (premier paramètre). Les nombres
/// intermédiaires sont omis quand le texte final les reprend déjà (« 167 239 Current local users
/// 167, max 239 » devient « Current local users 167, max 239 »).
fn numeric_text(params: &[String]) -> String {
    let rest = params.get(1..).unwrap_or_default();
    let repeated = |p: &String, last: &str| p.parse::<u64>().is_ok() && last.contains(p.as_str());
    match rest.split_last() {
        Some((last, middle)) if !middle.is_empty() && middle.iter().all(|p| repeated(p, last)) => last.clone(),
        _ => rest.join(" "),
    }
}

/// Réponse à un /whois, mise en forme (`None` pour les autres réponses numériques).
fn whois_text(command: &str, params: &[String]) -> Option<String> {
    let p = |i: usize| params.get(i).map(String::as_str).unwrap_or("");
    let nick = p(1);
    Some(match command {
        "311" | "314" => {
            let (user, host, real) = (p(2), p(3), p(5));
            tr!("{nick} est {user}@{host} * {real}", "{nick} is {user}@{host} * {real}")
        }
        "312" => {
            let (server, info) = (p(2), p(3));
            tr!("{nick} utilise {server} {info}", "{nick} using {server} {info}")
        }
        "319" => {
            let channels = p(2);
            tr!("{nick} est sur {channels}", "{nick} on {channels}")
        }
        "330" => {
            let account = p(2);
            tr!(
                "{nick} est identifié comme {account}",
                "{nick} is logged in as {account}"
            )
        }
        "317" => {
            let idle = idle_time(p(2).parse().unwrap_or(0));
            match p(3).parse().ok().and_then(format_date) {
                Some(date) => tr!(
                    "{nick} est inactif depuis {idle}, connecté {date}",
                    "{nick} has been idle {idle}, connected {date}"
                ),
                None => tr!("{nick} est inactif depuis {idle}", "{nick} has been idle {idle}"),
            }
        }
        // Absent, identifié, opérateur, connexion chiffrée, « is actually »… : le texte du serveur.
        "301" | "307" | "310" | "313" | "318" | "320" | "335" | "338" | "369" | "378" | "379" | "671" | "276" => {
            let text = params.get(2..).unwrap_or_default().join(" ");
            format!("{nick} {text}")
        }
        _ => return None,
    })
}

/// Durée d'inactivité, comme « 1 h 02 min 03 s ».
fn idle_time(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min {:02} s", secs / 60, secs % 60),
        _ => format!("{} h {:02} min {:02} s", secs / 3600, secs / 60 % 60, secs % 60),
    }
}

/// Hôte de « pseudo!user@hôte » à la fin du message de bienvenue, s'il y est.
fn welcome_host(text: &str) -> Option<&str> {
    let last = text.split_whitespace().next_back()?;
    last.contains('!')
        .then(|| last.split_once('@'))
        .flatten()
        .map(|(_, host)| host)
}

/// Sans nouvelles du bot pendant ce temps alors qu'on est dans sa file : le pack est redemandé.
const BOT_QUEUE_SILENCE: Duration = Duration::from_secs(3600);

/// Réponse d'un bot à une demande de pack.
#[derive(Debug, PartialEq)]
enum BotReply {
    Invalid(String),
    /// Plus de place, même dans sa file : réessayer plus tard.
    Full,
    /// Dans la file du bot : position annoncée (ou vide).
    Queued(String),
    Other,
}

/// Messages des bots XDCC courants (sans codes de mise en forme).
fn bot_reply(text: &str) -> BotReply {
    let lower = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    if has(&["invalid pack", "pack doesn't exist", "no such pack", "denied"]) {
        BotReply::Invalid(text.trim_start_matches(['*', ' ']).to_owned())
    } else if has(&["try again later", "queue is full", "is full"]) && !has(&["added you"]) {
        BotReply::Full
    } else if has(&["queue", "already requested"]) {
        let position = lower.find("position").map_or("", |i| {
            let rest = &text[i..];
            rest[..rest.find(['.', ',', '(']).unwrap_or(rest.len())].trim_end()
        });
        BotReply::Queued(position.to_owned())
    } else {
        BotReply::Other
    }
}

/// Packs d'une demande `xdcc send|get|batch …` tapée dans un /msg.
fn xdcc_request(text: &str) -> Option<Vec<String>> {
    let mut words = text.split_whitespace();
    if !words.next()?.eq_ignore_ascii_case("xdcc") {
        return None;
    }
    let verb = words.next()?.to_ascii_lowercase();
    if !["send", "get", "batch"].contains(&verb.as_str()) {
        return None;
    }
    let packs = parse_packs(&words.collect::<Vec<_>>().join(" "));
    (!packs.is_empty()).then_some(packs)
}

/// `#1,#3 5-7` → `#1 #3 #5 #6 #7` (1000 packs au plus).
pub fn parse_packs(text: &str) -> Vec<String> {
    let mut packs = Vec::new();
    for part in text.split([',', ' ']).filter(|p| !p.is_empty()) {
        let number = |s: &str| s.trim().trim_start_matches('#').parse::<u32>().ok();
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Some(a), Some(b)) = (number(a), number(b))
                    && a <= b
                {
                    packs.extend((a..=b).take(1000).map(|n| format!("#{n}")));
                }
            }
            None => packs.extend(number(part).map(|n| format!("#{n}"))),
        }
    }
    packs.truncate(1000);
    packs
}

pub fn human_size(bytes: u64) -> String {
    let french = crate::i18n::french();
    let units = if french {
        ["o", "Kio", "Mio", "Gio", "Tio"]
    } else {
        ["B", "KiB", "MiB", "GiB", "TiB"]
    };
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} {}", units[0]);
    }
    let text = format!("{value:.1} {}", units[unit]);
    if french { text.replace('.', ",") } else { text }
}

fn format_date(unix: i64) -> Option<String> {
    let time = jiff::Timestamp::from_second(unix)
        .ok()?
        .to_zoned(jiff::tz::TimeZone::system());
    let format = if crate::i18n::french() {
        "le %d/%m/%Y à %H:%M"
    } else {
        "on %m/%d/%Y at %-I:%M %p"
    };
    Some(time.strftime(format).to_string())
}

fn timestamp(format: &str) -> String {
    use std::fmt::Write;
    let mut text = String::new();
    if format.is_empty() || write!(text, "{}", jiff::Zoned::now().strftime(format)).is_err() {
        return String::new();
    }
    text
}

/// Ajoute une ligne à un tampon (fenêtre d'état si `buffer` est vide ou n'existe pas).
pub fn print(st: &mut State, id: ServerId, buffer: &str, kind: LineKind, text: impl Into<String>) {
    let activity = match kind {
        LineKind::Normal | LineKind::Action | LineKind::Notice => Activity::Message,
        LineKind::Own | LineKind::Separator => Activity::None,
        _ => Activity::Event,
    };
    add_line(st, id, buffer, Line::new(String::new(), kind, text.into()), activity);
}

/// Réponse du serveur dans la fenêtre d'état, avec les autres de son groupe ; `last` : elle
/// termine le groupe (séparateur après elle).
fn print_server(st: &mut State, id: ServerId, group: ServerGroup, last: bool, text: String) {
    let mut line = Line::new(String::new(), LineKind::Server, text);
    line.group = Some(group);
    add_line(st, id, "", line, Activity::Event);
    if last {
        print(st, id, "", LineKind::Separator, "-");
    }
}

/// Réponse du serveur à une commande (/whois, /who, /list, /names…) dans la fenêtre active ;
/// dans la fenêtre d'état, les lignes d'une même réponse restent ensemble, avec un séparateur
/// après la dernière (« End of … »).
fn print_reply(st: &mut State, id: ServerId, code: &str, text: String) {
    // Fins de liste : STATS, WHO, WHOIS, LIST, invitations, exceptions, LINKS, NAMES, bannis,
    // WHOWAS, INFO, USERS.
    const ENDS: [&str; 12] = [
        "219", "315", "318", "323", "347", "349", "365", "366", "368", "369", "374", "394",
    ];
    let window = active_window(st, id);
    let mut line = Line::new(String::new(), LineKind::Server, text);
    line.group = Some(ServerGroup::Reply);
    add_line(st, id, &window, line, Activity::Event);
    if ENDS.contains(&code) && window.is_empty() {
        print(st, id, "", LineKind::Separator, "-");
    }
}

fn print_message(st: &mut State, id: ServerId, buffer: &str, kind: LineKind, text: String, highlight: bool) {
    let activity = if highlight {
        Activity::Highlight
    } else {
        Activity::Message
    };
    if highlight {
        st.attention = true;
    }
    let sounds = &st.config.sounds;
    let shown = st.active.as_ref().is_some_and(|(s, b)| *s == id && irc::eq(b, buffer));
    let beep = if shown {
        sounds.beep_scrolled && st.scrolled_back
    } else if st.server(id).is_some_and(|s| s.is_channel(buffer)) {
        sounds.beep_channel
    } else {
        sounds.beep_query
    };
    if sounds.enabled && beep {
        sound::beep();
    }
    let mut line = Line::new(String::new(), kind, text);
    line.highlight = highlight;
    add_line(st, id, buffer, line, activity);
}

/// Canal où se trouve aussi `nick`, pour y afficher ses notices privées : la fenêtre active si
/// c'en est un, sinon le premier ; aucun s'il ne partage aucun canal avec nous.
fn common_channel(st: &State, id: ServerId, nick: &str) -> Option<String> {
    let server = st.server(id)?;
    let shared = |key: &str| {
        server
            .buffer(key)
            .is_some_and(|b| b.kind == BufKind::Channel && b.joined && b.user_index(nick).is_some())
    };
    let active = active_window(st, id);
    if shared(&active) {
        return Some(active);
    }
    server.buffers.iter().find(|b| shared(&b.key)).map(|b| b.key.clone())
}

/// Adresse publique trouvée, affichée dans la fenêtre d'état (« Hôte local : … »).
fn set_public_ip(st: &mut State, id: ServerId, host: &str, ip: IpAddr) {
    let Some(server) = st.server_mut(id) else { return };
    server.public_ip = Some(ip);
    let text = if host == ip.to_string() {
        tr!("Hôte local : {ip}", "Local host: {ip}")
    } else {
        tr!("Hôte local : {host} ({ip})", "Local host: {host} ({ip})")
    };
    print(st, id, "", LineKind::Info2, text);
}

/// Écrit un message d'un transfert dans la fenêtre d'état de son serveur.
fn print_transfer(st: &mut State, tid: TransferId, kind: LineKind, text: String) {
    if let Some(server) = st.transfers.iter().find(|t| t.id == tid).map(|t| t.server) {
        print(st, server, "", kind, text);
    }
}

/// Fenêtre affichée si elle appartient à ce serveur, sinon sa fenêtre d'état.
fn active_window(st: &State, id: ServerId) -> String {
    match &st.active {
        Some((server, buffer)) if *server == id => buffer.clone(),
        _ => String::new(),
    }
}

/// Ajoute une ligne dans la fenêtre active si elle appartient à ce serveur, sinon dans l'état.
fn print_active(st: &mut State, id: ServerId, kind: LineKind, text: String) {
    let buffer = active_window(st, id);
    print(st, id, &buffer, kind, text);
}

/// Ajoute `line` (horodatée ici) à un tampon. Dans la fenêtre d'état, chaque
/// évènement est encadré de lignes « - » ; les réponses d'un même groupe du serveur restent
/// ensemble.
fn add_line(st: &mut State, id: ServerId, buffer: &str, mut line: Line, activity: Activity) {
    let display = &st.config.display;
    if !display.show_joins_parts && matches!(line.kind, LineKind::Join | LineKind::Part | LineKind::Quit) {
        return;
    }
    if line.kind != LineKind::Separator {
        line.time = timestamp(&display.timestamp);
    }
    let limit = display.scrollback;
    let Some(server) = st.server_mut(id) else { return };
    let target = if server.buffer(buffer).is_some() { buffer } else { "" };
    let Some(b) = server.buffer_mut(target) else { return };
    let separator = || Line::new(String::new(), LineKind::Separator, "-".into());
    if !target.is_empty() {
        b.push(line, limit, activity);
        return;
    }
    let previous = b.lines.back().map(|l| (l.kind, l.group));
    let (kind, group) = (line.kind, line.group);
    let same_group = group.is_some() && previous.is_some_and(|(_, g)| g == group);
    let after_separator = previous.is_none_or(|(k, _)| k == LineKind::Separator);
    if kind == LineKind::Separator && after_separator {
        return;
    }
    if kind != LineKind::Separator && !after_separator && !same_group {
        b.push(separator(), limit, Activity::None);
    }
    b.push(line, limit, activity);
    if kind != LineKind::Separator && group.is_none() {
        b.push(separator(), limit, Activity::None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un transfert ne se termine qu'une fois (un seul message, un seul son) : la fin signalée
    /// ensuite par sa tâche de réception ne change rien.
    #[test]
    fn transfer_ends_once() {
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(State::new(Default::default())));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut core = Core::new(state.clone(), egui::Context::default(), tx);
        let mut st = lock(&state);
        st.transfers.push(Transfer {
            id: 1,
            server: 1,
            nick: "Bot".into(),
            offer: dcc::parse_send("f.bin 0 0 10 7").unwrap(),
            path: "f.bin".into(),
            offset: 0,
            position: Default::default(),
            status: TransferStatus::Listening,
            started: None,
            ended: None,
            queue_item: None,
            resume_sent: None,
            announced: None,
            complete: false,
        });
        core.end_transfer(&mut st, 1, Err("adresse IPv4 publique inconnue".into()));
        let ended = st.transfers[0].ended;
        core.on_transfer(&mut st, 1, TransferEvent::Ended(Err("délai dépassé".into())));
        assert!(st.transfers[0].status == TransferStatus::Failed("adresse IPv4 publique inconnue".into()));
        assert_eq!(st.transfers[0].ended, ended);
    }

    /// Le correspondant d'une conversation privée change de pseudo : elle est renommée, et
    /// l'interface en est avertie (elle la garde où elle était).
    #[test]
    fn query_renamed() {
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(State::new(Default::default())));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut core = Core::new(state.clone(), egui::Context::default(), tx);
        let mut st = lock(&state);
        let addr = ServerAddr::new("irc.example.net", 6697, true);
        st.servers.push(Server::new(1, "Test".into(), None, addr, "moi".into()));
        st.servers[0].open_buffer("Bob");
        core.on_nick(&mut st, 1, "Bob", "Robert", false);
        assert!(st.servers[0].buffer("robert").is_some_and(|b| b.name == "Robert"));
        assert_eq!(
            st.renamed,
            [(Focus::Buffer(1, "bob".into()), Focus::Buffer(1, "robert".into()))]
        );
    }

    /// Sans connexion, toute commande qui en a besoin le dit, /quit compris.
    #[test]
    fn not_connected() {
        let state: Shared = std::sync::Arc::new(std::sync::Mutex::new(State::new(Default::default())));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut core = Core::new(state.clone(), egui::Context::default(), tx);
        let mut st = lock(&state);
        let addr = ServerAddr::new("irc.example.net", 6697, true);
        st.servers.push(Server::new(1, "Test".into(), None, addr, "moi".into()));
        for line in ["quit", "disconnect bye", "whois Nick", "raw PING x"] {
            core.command(&mut st, 1, "", line);
        }
        let lines: Vec<&str> = st.servers[0].buffers[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            lines,
            [
                "* /quit : non connecté au serveur",
                "-",
                "* /disconnect : non connecté au serveur",
                "-",
                "* /whois : non connecté au serveur",
                "-",
                "* /raw : non connecté au serveur",
                "-",
            ]
        );
    }

    #[test]
    fn packs() {
        assert_eq!(parse_packs("#1,#3 5-7"), ["#1", "#3", "#5", "#6", "#7"]);
        assert_eq!(parse_packs("#2-#3"), ["#2", "#3"]);
        assert!(parse_packs("abc 5-2").is_empty());
        assert_eq!(parse_packs("1-100000").len(), 1000);
    }

    #[test]
    fn xdcc_messages() {
        assert_eq!(xdcc_request("xdcc send #12"), Some(vec!["#12".into()]));
        assert_eq!(xdcc_request("XDCC batch 1-2"), Some(vec!["#1".into(), "#2".into()]));
        assert_eq!(xdcc_request("xdcc list"), None);
        assert_eq!(xdcc_request("hello"), None);
    }

    /// Fenêtre d'état : chaque évènement entre deux « - », les groupes de réponses ensemble.
    #[test]
    fn status_separators() {
        let mut st = State::new(Default::default());
        let addr = ServerAddr::new("irc.example.net", 6697, true);
        st.servers.push(Server::new(1, "Test".into(), None, addr, "moi".into()));
        print(&mut st, 1, "", LineKind::Info, "Connexion");
        print_server(&mut st, 1, ServerGroup::Welcome, false, "Welcome".into());
        print_server(&mut st, 1, ServerGroup::Welcome, false, "Your host".into());
        print_server(&mut st, 1, ServerGroup::Stats, false, "There are".into());
        print_server(&mut st, 1, ServerGroup::Stats, true, "I have".into());
        print_server(&mut st, 1, ServerGroup::Counts, true, "Global".into());
        print(&mut st, 1, "", LineKind::Notice, "-NickServ- a");
        print(&mut st, 1, "", LineKind::Notice, "-NickServ- b");
        // Réponse à une commande : ses lignes ensemble, un séparateur après « End of … ».
        print_reply(&mut st, 1, "311", "Nick est user@host".into());
        print_reply(&mut st, 1, "319", "Nick est sur #canal".into());
        print_reply(&mut st, 1, "318", "Nick End of /WHOIS list.".into());
        let lines: Vec<&str> = st.servers[0].buffers[0].lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            lines,
            [
                "* Connexion",
                "-",
                "Welcome",
                "Your host",
                "-",
                "There are",
                "I have",
                "-",
                "Global",
                "-",
                "-NickServ- a",
                "-",
                "-NickServ- b",
                "-",
                "Nick est user@host",
                "Nick est sur #canal",
                "Nick End of /WHOIS list.",
                "-",
            ]
        );
    }

    #[test]
    fn whois() {
        let params = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let whois = |code, p: &[&str]| whois_text(code, &params(p)).unwrap();
        assert_eq!(
            whois("311", &["moi", "Nick", "~user", "host.fr", "*", "Real Name"]),
            "Nick est ~user@host.fr * Real Name"
        );
        assert_eq!(whois("319", &["moi", "Nick", "@#un #deux"]), "Nick est sur @#un #deux");
        assert_eq!(
            whois("312", &["moi", "Nick", "irc.example.net", "Info"]),
            "Nick utilise irc.example.net Info"
        );
        assert_eq!(
            whois("330", &["moi", "Nick", "Compte", "is logged in as"]),
            "Nick est identifié comme Compte"
        );
        assert!(
            whois("317", &["moi", "Nick", "63", "1773583500", "seconds idle, signon time"])
                .starts_with("Nick est inactif depuis 1 min 03 s, connecté le ")
        );
        assert_eq!(
            whois("338", &["moi", "Nick", "is actually ~user@host.fr [1.2.3.4]"]),
            "Nick is actually ~user@host.fr [1.2.3.4]"
        );
        assert_eq!(
            whois("318", &["moi", "Nick", "End of /WHOIS list."]),
            "Nick End of /WHOIS list."
        );
        assert!(whois_text("372", &params(&["moi", "- MOTD"])).is_none());
    }

    #[test]
    fn numerics() {
        let params = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let local = params(&["moi", "167", "239", "Current local users 167, max 239"]);
        assert_eq!(numeric_text(&local), "Current local users 167, max 239");
        let operators = params(&["moi", "17", "operator(s) online"]);
        assert_eq!(numeric_text(&operators), "17 operator(s) online");
        assert_eq!(numeric_text(&params(&["moi", "Welcome"])), "Welcome");
    }

    #[test]
    fn welcome() {
        assert_eq!(
            welcome_host("Welcome to the Example IRC Network moi!~moi@client.example.net"),
            Some("client.example.net")
        );
        assert_eq!(welcome_host("Welcome to the network, moi"), None);
    }

    #[test]
    fn bot_replies() {
        let queued = |p: &str| BotReply::Queued(p.into());
        assert_eq!(
            bot_reply(
                "** All Slots Full, Added you to the main queue for pack 5 (\"a.mkv\") in position 2. To Remove…"
            ),
            queued("position 2")
        );
        assert_eq!(
            bot_reply("Queued 1h20m for \"a.mkv\", in position 1 of 3. 2h left"),
            queued("position 1 of 3")
        );
        assert_eq!(bot_reply("** You already requested that pack"), queued(""));
        assert_eq!(
            bot_reply("** All Slots Full, Main queue of size 20 is Full, Try Again Later"),
            BotReply::Full
        );
        assert_eq!(
            bot_reply("** Invalid Pack Number, Try Again"),
            BotReply::Invalid("Invalid Pack Number, Try Again".into())
        );
        assert_eq!(bot_reply("** Sending you pack #5 (\"a.mkv\")"), BotReply::Other);
    }

    #[test]
    fn masking() {
        assert_eq!(mask_password("identify secret"), "identify ****");
        assert_eq!(mask_password("info nick"), "info nick");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 o");
        assert_eq!(human_size(1536), "1,5 Kio");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3,0 Gio");
    }

    #[test]
    fn highlight_words() {
        let st = State::new(Default::default());
        assert!(highlights(&st, "Bob", "hello bob!"));
        assert!(!highlights(&st, "Bob", "bobby"));
        assert!(highlights(&st, "Bob", "\x02bob\x02: hi"));
    }
}
