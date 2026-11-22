//! Boîte de dialogue Options (identité, connexion, serveurs, favoris, DCC, XDCC, sons, affichage).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Color32, ComboBox, DragValue, FontId, Grid, Id, RichText, ScrollArea, TextEdit};

use crate::config::{
    Config, Display, ExistsAction, Favorite, FontVariant, IgnoreMode, Language, Network, OfferAction, ServerAddr,
    Sounds, Theme,
};
use crate::fonts::{self, SYMBOL_DOWN, SYMBOL_PLAY, SYMBOL_REMOVE, SYMBOL_UP, symbol_button};
use crate::sound;

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Identity,
    Connection,
    Servers,
    Favorites,
    Dcc,
    Xdcc,
    Sounds,
    Display,
}

pub const TABS: [Tab; 8] = [
    Tab::Identity,
    Tab::Connection,
    Tab::Servers,
    Tab::Favorites,
    Tab::Dcc,
    Tab::Xdcc,
    Tab::Sounds,
    Tab::Display,
];

impl Tab {
    fn label(self) -> String {
        match self {
            Tab::Identity => tr!("Identité", "Identity"),
            Tab::Connection => tr!("Connexion", "Connection"),
            Tab::Servers => tr!("Serveurs", "Servers"),
            Tab::Favorites => tr!("Favoris", "Favorites"),
            Tab::Dcc => "DCC".into(),
            Tab::Xdcc => "XDCC".into(),
            Tab::Sounds => tr!("Sons", "Sounds"),
            Tab::Display => tr!("Affichage", "Display"),
        }
    }
}

/// Identifiant de la fenêtre des options (Échap, Alt+F4 la ferment).
pub const WINDOW_ID: &str = "options";

pub struct Options {
    draft: Config,
    /// Affichage en vigueur : l'aperçu montre la police installée, à sa taille.
    applied: Display,
    tab: Tab,
    network: usize,
    /// Favoris sur un serveur hors de la liste du réseau : texte « hôte:+port » en cours de saisie.
    custom_servers: HashMap<usize, String>,
}

/// Déplacement ou suppression d'un élément de liste, demandé par ses petits boutons.
enum Edit {
    Swap(usize, usize),
    Remove(usize),
}

impl Edit {
    fn apply<T>(self, list: &mut Vec<T>) {
        match self {
            Edit::Swap(a, b) => list.swap(a, b),
            Edit::Remove(i) => {
                list.remove(i);
            }
        }
    }
}

/// Boutons monter, descendre et supprimer de l'élément `i` d'une liste de `len` éléments.
fn row_buttons(ui: &mut egui::Ui, i: usize, len: usize, edit: &mut Option<Edit>) {
    ui.horizontal(|ui| {
        if ui
            .add_enabled_ui(i > 0, |ui| symbol_button(ui, SYMBOL_UP, &tr!("Monter", "Move up")))
            .inner
        {
            *edit = Some(Edit::Swap(i, i - 1));
        }
        if ui
            .add_enabled_ui(i + 1 < len, |ui| {
                symbol_button(ui, SYMBOL_DOWN, &tr!("Descendre", "Move down"))
            })
            .inner
        {
            *edit = Some(Edit::Swap(i, i + 1));
        }
        if symbol_button(ui, SYMBOL_REMOVE, &tr!("Supprimer", "Remove")) {
            *edit = Some(Edit::Remove(i));
        }
    });
}

/// Configuration à appliquer, et fermeture de la boîte de dialogue.
pub struct Outcome {
    pub config: Option<Config>,
    pub close: bool,
}

impl Options {
    pub fn new(config: &Config, tab: Tab) -> Self {
        Options {
            draft: config.clone(),
            applied: config.display.clone(),
            tab,
            network: 0,
            custom_servers: HashMap::new(),
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Outcome {
        let mut open = true;
        let mut outcome = Outcome {
            config: None,
            close: false,
        };
        egui::Window::new("Options")
            .id(Id::unique(WINDOW_ID))
            .collapsible(false)
            .default_size([900.0, 600.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for tab in TABS {
                        ui.selectable_value(&mut self.tab, tab, tab.label());
                    }
                });
                ui.separator();
                ScrollArea::vertical()
                    .max_height((ui.available_height() - 36.0).max(100.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| match self.tab {
                        Tab::Identity => self.identity(ui),
                        Tab::Connection => self.connection(ui),
                        Tab::Servers => self.servers(ui),
                        Tab::Favorites => self.favorites(ui),
                        Tab::Dcc => self.dcc(ui),
                        Tab::Xdcc => self.xdcc(ui),
                        Tab::Sounds => self.sounds(ui),
                        Tab::Display => self.display(ui),
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        outcome = Outcome {
                            config: Some(cleaned(&self.draft)),
                            close: true,
                        };
                    }
                    if ui.button(tr!("Annuler", "Cancel")).clicked() {
                        outcome.close = true;
                    }
                    if ui.button(tr!("Appliquer", "Apply")).clicked() {
                        outcome.config = Some(cleaned(&self.draft));
                    }
                });
            });
        if let Some(config) = &outcome.config {
            self.applied = config.display.clone();
        }
        outcome.close |= !open;
        outcome
    }

    fn identity(&mut self, ui: &mut egui::Ui) {
        let identity = &mut self.draft.identity;
        Grid::new("identité")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(tr!("Pseudo", "Nickname"));
                ui.text_edit_singleline(&mut identity.nick);
                ui.end_row();
                ui.label(tr!("Pseudo de secours", "Alternate nickname"));
                ui.text_edit_singleline(&mut identity.alt_nick).on_hover_text(tr!(
                    "Utilisé si le pseudo est déjà pris ; ensuite : pseudo_, pseudo__…",
                    "Used if the nickname is already taken; then: nick_, nick__…"
                ));
                ui.end_row();
                ui.label(tr!("Nom d'utilisateur (ident)", "Username (ident)"));
                ui.text_edit_singleline(&mut identity.username);
                ui.end_row();
                ui.label(tr!("Nom réel", "Real name"));
                ui.text_edit_singleline(&mut identity.realname);
                ui.end_row();
                ui.label(tr!("Message de départ", "Quit message"));
                ui.add(TextEdit::singleline(&mut identity.quit_message).hint_text(tr!("aucun", "none")));
                ui.end_row();
                ui.label(tr!("Réponse à VERSION", "VERSION reply"));
                ui.horizontal(|ui| {
                    ui.add(
                        TextEdit::singleline(&mut identity.version_reply).hint_text(tr!("aucune réponse", "no reply")),
                    )
                    .on_hover_text(tr!(
                        "Logiciel annoncé à qui le demande (/ctcp pseudo VERSION). Vide : rien \
                             n'est révélé.",
                        "Software reported to whoever asks (/ctcp nick VERSION). Empty: nothing \
                             is revealed."
                    ));
                    if ui.small_button(tr!("Ce logiciel", "This software")).clicked() {
                        identity.version_reply = crate::core::VERSION.into();
                    }
                });
                ui.end_row();
            });
        ui.add_space(4.0);
        ui.label(tr!(
            "Discrétion : le nom d'utilisateur, le nom réel et le message de départ sont visibles de \
             tous ; l'heure donnée à qui la demande (/ctcp pseudo TIME) est en UTC, sans votre fuseau \
             horaire.",
            "Privacy: the username, real name and quit message are visible to everyone; the time \
             given to whoever asks (/ctcp nick TIME) is in UTC, without your time zone."
        ));
        if identity.nick.trim().is_empty() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                tr!(
                    "Choisissez un pseudo avant de vous connecter.",
                    "Choose a nickname before connecting."
                ),
            );
        }
        ui.add_space(8.0);
        ui.label(tr!(
            "Chaque réseau peut avoir son propre pseudo (onglet Serveurs).",
            "Each network can have its own nickname (Servers tab)."
        ));
    }

    fn connection(&mut self, ui: &mut egui::Ui) {
        let connection = &mut self.draft.connection;
        ui.checkbox(
            &mut connection.reconnect,
            tr!(
                "Se reconnecter automatiquement après une déconnexion",
                "Reconnect automatically after a disconnection"
            ),
        );
        ui.horizontal(|ui| {
            ui.label(tr!("Délai avant reconnexion", "Reconnect delay"));
            ui.add(
                DragValue::new(&mut connection.reconnect_delay_secs)
                    .range(1..=3600)
                    .suffix(" s"),
            );
        });
        ui.checkbox(
            &mut connection.rejoin_channels,
            tr!(
                "Rejoindre les canaux ouverts après une reconnexion",
                "Rejoin open channels after reconnecting"
            ),
        );
        ui.checkbox(
            &mut connection.identd,
            tr!("Serveur ident (port 113)", "Ident server (port 113)"),
        )
        .on_hover_text(tr!(
            "Sans réponse ident, le serveur IRC préfixe votre nom d'utilisateur d'un ~ \
             (« No ident response »). Derrière une box, le port 113 doit être redirigé vers ce \
             PC : par UPnP (Options → DCC) ou à la main ; sans cela le ~ reste, ce qui est sans \
             conséquence.",
            "Without an ident response, the IRC server prefixes your username with a ~ (\"No \
             ident response\"). Behind a router, port 113 must be forwarded to this PC: by UPnP \
             (Options → DCC) or manually; otherwise the ~ stays, which is harmless."
        ));
        ui.checkbox(
            &mut connection.invisible,
            tr!("Mode invisible (+i)", "Invisible mode (+i)"),
        )
        .on_hover_text(tr!(
            "Activé à chaque connexion : vous n'apparaissez pas dans /who ni dans la liste des \
             utilisateurs pour qui ne partage aucun canal avec vous.",
            "Set on every connection: you don't appear in /who or in the user list for anyone \
             who shares no channel with you."
        ));
        ui.add_space(8.0);
        ui.label(tr!(
            "Mots mis en évidence (en plus du pseudo), un par ligne :",
            "Highlight words (in addition to the nickname), one per line:"
        ));
        list_edit(ui, &mut connection.highlight_words, 4);
    }

    fn servers(&mut self, ui: &mut egui::Ui) {
        let networks = &mut self.draft.networks;
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(190.0);
                for (i, net) in networks.iter().enumerate() {
                    let name = if net.name.is_empty() {
                        tr!("(sans nom)", "(unnamed)")
                    } else {
                        net.name.clone()
                    };
                    if ui.selectable_label(self.network == i, name).clicked() {
                        self.network = i;
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button(tr!("Ajouter", "Add")).clicked() {
                        networks.push(Network {
                            name: tr!("Nouveau réseau", "New network"),
                            servers: vec![ServerAddr::new("irc.example.net", 6697, true)],
                            ..Default::default()
                        });
                        self.network = networks.len() - 1;
                    }
                    let (i, len) = (self.network, networks.len());
                    let mut edit = None;
                    if i < len {
                        row_buttons(ui, i, len, &mut edit);
                    }
                    match edit {
                        Some(Edit::Swap(a, b)) => {
                            networks.swap(a, b);
                            self.network = b;
                        }
                        Some(Edit::Remove(i)) => {
                            networks.remove(i);
                            self.network = i.saturating_sub(1);
                        }
                        None => {}
                    }
                });
                ui.label(tr!("Ordre du menu Serveurs.", "Order of the Servers menu."));
            });
            ui.separator();
            ui.vertical(|ui| match networks.get_mut(self.network) {
                Some(net) => network(ui, net),
                None => {
                    ui.label(tr!("Ajoutez un réseau.", "Add a network."));
                }
            });
        });
    }

    fn favorites(&mut self, ui: &mut egui::Ui) {
        ui.label(tr!(
            "Serveur « automatique » : ceux du réseau, à tour de rôle ; ou un serveur précis (port TLS \
             particulier…). « Auto » : rejoint à chaque connexion à ce réseau, y compris celle lancée en \
             cliquant sur un autre favori. L'ordre est celui du menu Favoris.",
            "\"Automatic\" server: the network's servers, in turn; or a specific server (particular TLS \
             port…). \"Auto\": joined on every connection to this network, including one started by \
             clicking another favorite. The order is that of the Favorites menu."
        ));
        ui.add_space(4.0);
        let networks = self.draft.networks.clone();
        let favorites = &mut self.draft.favorites;
        let len = favorites.len();
        let mut edit = None;
        Grid::new("favoris").striped(true).num_columns(6).show(ui, |ui| {
            for title in [
                tr!("Canal", "Channel"),
                tr!("Clé", "Key"),
                tr!("Réseau", "Network"),
                tr!("Serveur", "Server"),
                "Auto".into(),
                String::new(),
            ] {
                ui.label(RichText::new(title).strong());
            }
            ui.end_row();
            for (i, fav) in favorites.iter_mut().enumerate() {
                field(ui, 220.0, TextEdit::singleline(&mut fav.channel));
                field(
                    ui,
                    100.0,
                    TextEdit::singleline(&mut fav.key).hint_text(tr!("aucune", "none")),
                );
                let before = fav.network.clone();
                ComboBox::from_id_salt(("réseau", i))
                    .selected_text(if fav.network.is_empty() {
                        tr!("(aucun)", "(none)")
                    } else {
                        fav.network.clone()
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut fav.network, String::new(), tr!("(aucun)", "(none)"));
                        for net in &networks {
                            ui.selectable_value(&mut fav.network, net.name.clone(), &net.name);
                        }
                    });
                if fav.network != before {
                    fav.server = ServerAddr::default();
                    self.custom_servers.remove(&i);
                }
                let servers = networks
                    .iter()
                    .find(|n| n.name == fav.network)
                    .map(|n| n.servers.as_slice())
                    .unwrap_or_default();
                favorite_server(ui, i, fav, servers, &mut self.custom_servers);
                ui.checkbox(&mut fav.auto_join, "");
                row_buttons(ui, i, len, &mut edit);
                ui.end_row();
            }
        });
        if let Some(edit) = edit {
            edit.apply(favorites);
            // Les saisies en cours sont rattachées aux positions : elles ne suivent pas.
            self.custom_servers.clear();
        }
        if ui.button(tr!("Ajouter un favori", "Add a favorite")).clicked() {
            favorites.push(Favorite {
                channel: "#".into(),
                network: networks.first().map(|n| n.name.clone()).unwrap_or_default(),
                auto_join: true,
                ..Default::default()
            });
        }
    }

    fn dcc(&mut self, ui: &mut egui::Ui) {
        let dcc = &mut self.draft.dcc;
        ui.horizontal(|ui| {
            ui.label(tr!("Dossier de téléchargement", "Download folder"));
            let mut dir = dcc.download_dir.display().to_string();
            let edit =
                TextEdit::singleline(&mut dir).hint_text(tr!(r"ex. \\NAS\partage\XDCC", r"e.g. \\NAS\share\XDCC"));
            if field(ui, WIDE, edit)
                .on_hover_text(tr!(
                    r"Dossier local, lecteur réseau (Z:\…) ou chemin réseau (\\serveur\partage\…)",
                    r"Local folder, network drive (Z:\…) or network path (\\server\share\…)"
                ))
                .changed()
            {
                dcc.download_dir = dir.into();
            }
            if ui.button(tr!("Parcourir…", "Browse…")).clicked()
                && let Some(dir) = rfd::FileDialog::new().set_directory(&dcc.download_dir).pick_folder()
            {
                dcc.download_dir = dir;
            }
        });
        ui.add_space(6.0);
        ui.label(tr!(
            "Envoi non sollicité (les packs XDCC demandés sont toujours acceptés) :",
            "Unsolicited sends (requested XDCC packs are always accepted):"
        ));
        ui.radio_value(
            &mut dcc.on_offer,
            OfferAction::Ask,
            tr!("Afficher la demande", "Show the request"),
        );
        ui.radio_value(
            &mut dcc.on_offer,
            OfferAction::Accept,
            tr!("Accepter automatiquement", "Accept automatically"),
        );
        ui.radio_value(
            &mut dcc.on_offer,
            OfferAction::Ignore,
            tr!("Tout ignorer", "Ignore all"),
        );
        ui.horizontal(|ui| {
            ui.label(tr!("Si le fichier existe déjà", "If the file exists"));
            const CHOICES: [ExistsAction; 4] = [
                ExistsAction::Ask,
                ExistsAction::Resume,
                ExistsAction::Overwrite,
                ExistsAction::Rename,
            ];
            let label = |action| match action {
                ExistsAction::Ask => tr!("Demander", "Ask"),
                ExistsAction::Resume => tr!("Reprendre", "Resume"),
                ExistsAction::Overwrite => tr!("Écraser", "Overwrite"),
                ExistsAction::Rename => tr!("Renommer", "Rename"),
            };
            ComboBox::from_id_salt("existant")
                .selected_text(label(dcc.if_exists))
                .show_ui(ui, |ui| {
                    for action in CHOICES {
                        ui.selectable_value(&mut dcc.if_exists, action, label(action));
                    }
                });
        })
        .response
        .on_hover_text(tr!(
            "Aussi pour les packs XDCC : « Reprendre » continue un fichier interrompu",
            "Also for XDCC packs: \"Resume\" continues an interrupted file"
        ));
        ui.add_space(6.0);
        ui.label(tr!(
            "Liste d'ignorés (envois non sollicités) :",
            "Ignore list (unsolicited sends):"
        ));
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut dcc.ignore_mode,
                IgnoreMode::Disabled,
                tr!("Désactivée", "Disabled"),
            );
            ui.radio_value(
                &mut dcc.ignore_mode,
                IgnoreMode::IgnoreListed,
                tr!("Refuser ces fichiers", "Reject these files"),
            );
            ui.radio_value(
                &mut dcc.ignore_mode,
                IgnoreMode::AcceptOnlyListed,
                tr!("N'accepter que ces fichiers", "Accept only these files"),
            );
        });
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.label(tr!("Fichiers (jokers * et ?)", "Files (wildcards * and ?)"));
                list_edit(ui, &mut dcc.ignore_files, 6);
            });
            ui.vertical(|ui| {
                ui.label(tr!("Pseudos refusés", "Rejected nicknames"));
                list_edit(ui, &mut dcc.ignore_nicks, 6);
            });
        });
        ui.add_space(6.0);
        ui.label(tr!(
            "DCC passif (l'émetteur ne peut pas recevoir de connexion : c'est vous qui écoutez) :",
            "Passive DCC (the sender cannot accept connections: you listen instead):"
        ));
        Grid::new("dcc").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label(tr!("Ports d'écoute", "Listening ports"));
            ui.horizontal(|ui| {
                ui.add(DragValue::new(&mut dcc.port_first).range(1..=65535));
                ui.label(tr!("à", "to"));
                ui.add(DragValue::new(&mut dcc.port_last).range(1..=65535));
            });
            ui.end_row();
            ui.label(tr!("Adresse IP publique", "Public IP address"));
            ui.add(
                TextEdit::singleline(&mut dcc.external_ip)
                    .hint_text(tr!("automatique (box ou serveur)", "automatic (router or server)")),
            );
            ui.end_row();
            ui.label("");
            ui.checkbox(
                &mut dcc.upnp,
                tr!(
                    "UPnP : la box ouvre d'elle-même les ports",
                    "UPnP: the router opens the ports by itself"
                ),
            )
            .on_hover_text(tr!(
                "Ports des DCC passifs et du serveur ident ; la box donne aussi l'adresse publique. \
                 UPnP doit être activé sur la box ; sinon, les ports doivent y être redirigés à la main.",
                "Ports for passive DCC and the ident server; the router also provides the public \
                 address. UPnP must be enabled on the router; otherwise, the ports must be forwarded \
                 manually."
            ));
            ui.end_row();
            ui.label(tr!("Délai sans données", "Idle timeout"));
            ui.add(DragValue::new(&mut dcc.timeout_secs).range(10..=3600).suffix(" s"));
            ui.end_row();
        });
        ui.label(tr!(
            "Sans UPnP, ces ports doivent être redirigés vers ce PC sur la box / le routeur. Le pare-feu \
             de Windows doit aussi autoriser ce logiciel (question posée au premier transfert).",
            "Without UPnP, these ports must be forwarded to this PC on the router. The Windows \
             firewall must also allow this software (you are asked on the first transfer)."
        ));
    }

    fn xdcc(&mut self, ui: &mut egui::Ui) {
        let xdcc = &mut self.draft.xdcc;
        ui.label(tr!(
            "Les packs (/xdcc bot #1,#2…, ou /msg bot xdcc send #1) sont demandés un par un à chaque bot (le suivant quand le précédent est terminé), à \
             tous les bots en même temps. C'est le bot qui gère sa file d'attente : la position \
             qu'il annonce s'affiche dans Transferts ; s'il est plein, le pack est redemandé une à \
             deux minutes plus tard ; sans nouvelles de sa file pendant une heure, il est redemandé.",
            "Packs (/xdcc bot #1,#2…, or /msg bot xdcc send #1) are requested one at a time from each \
             bot (the next one when the previous one is done), from all bots at once. The bot manages \
             its own queue: the position it reports is shown in Transfers; if it is full, the pack is \
             requested again one to two minutes later; with no news from its queue for an hour, it is \
             requested again."
        ));
        ui.add_space(4.0);
        Grid::new("xdcc").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label(tr!("Nouvelles tentatives après échec", "Retries after failure"));
            ui.add(DragValue::new(&mut xdcc.retries).range(0..=20));
            ui.end_row();
            ui.label(tr!("Délai entre deux tentatives", "Delay between retries"));
            ui.add(DragValue::new(&mut xdcc.retry_delay_secs).range(1..=3600).suffix(" s"));
            ui.end_row();
            ui.label(tr!(
                "Abandon si le bot ne répond pas",
                "Give up if the bot doesn't reply"
            ));
            ui.add(
                DragValue::new(&mut xdcc.request_timeout_secs)
                    .range(0..=86400)
                    .suffix(" s"),
            )
            .on_hover_text(tr!("0 : attendre indéfiniment", "0: wait forever"));
            ui.end_row();
        });
        ui.label(tr!(
            "Une tentative reprend le fichier là où il s'était arrêté.",
            "A retry resumes the file where it left off."
        ));
    }

    fn sounds(&mut self, ui: &mut egui::Ui) {
        let sounds = &mut self.draft.sounds;
        ui.checkbox(&mut sounds.enabled, tr!("Activer les sons", "Enable sounds"));
        ui.add_enabled_ui(sounds.enabled, |ui| {
            ui.add_space(4.0);
            ui.label(tr!("Bip à chaque message :", "Beep on every message:"));
            ui.horizontal(|ui| {
                ui.checkbox(&mut sounds.beep_channel, tr!("Canal", "Channel"))
                    .on_hover_text(tr!(
                        "Dans un canal qui n'est pas la fenêtre active",
                        "In a channel that isn't the active window"
                    ));
                ui.checkbox(
                    &mut sounds.beep_query,
                    tr!("Conversation privée", "Private conversation"),
                )
                .on_hover_text(tr!(
                    "Dans une conversation privée qui n'est pas la fenêtre active",
                    "In a private conversation that isn't the active window"
                ));
                ui.checkbox(&mut sounds.beep_scrolled, tr!("Historique remonté", "Scrolled back"))
                    .on_hover_text(tr!(
                        "Dans la fenêtre active, quand on lit plus haut dans l'historique",
                        "In the active window, when reading further up in the history"
                    ));
            });
            ui.add_space(8.0);
            ui.label(tr!(
                "Évènements : fichier WAV, MP3, WMA… ; sans fichier, le son correspondant du système. \
                 Les sons sont joués l'un après l'autre, jamais par-dessus.",
                "Events: WAV, MP3, WMA file…; without a file, the matching system sound. Sounds are \
                 played one after another, never on top of each other."
            ));
            ui.add_space(4.0);
            Self::sound_events(ui, sounds);
        });
    }

    /// Son de chaque évènement : activé, fichier, écoute.
    fn sound_events(ui: &mut egui::Ui, sounds: &mut Sounds) {
        Grid::new("sons")
            .striped(true)
            .num_columns(3)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                for event in sound::EVENTS {
                    let sound = sounds.get_mut(event);
                    ui.checkbox(&mut sound.enabled, event.label());
                    let mut file = sound.file.display().to_string();
                    let edit = TextEdit::singleline(&mut file).hint_text(tr!("son du système", "system sound"));
                    if field(ui, WIDE, edit).changed() {
                        sound.file = file.into();
                    }
                    ui.horizontal(|ui| {
                        if ui.button(tr!("Parcourir…", "Browse…")).clicked()
                            && let Some(file) = rfd::FileDialog::new()
                                .add_filter(tr!("Sons", "Sounds"), &["wav", "mp3", "wma", "mid"])
                                .pick_file()
                        {
                            sound.file = file;
                        }
                        if symbol_button(ui, SYMBOL_PLAY, &tr!("Écouter", "Play")) {
                            sound::play(&clean_path(&sound.file), event);
                        }
                    });
                    ui.end_row();
                }
            });
    }

    fn display(&mut self, ui: &mut egui::Ui) {
        let display = &mut self.draft.display;
        ui.horizontal(|ui| {
            ui.label(tr!("Langue", "Language"));
            const LANGUAGES: [Language; 3] = [Language::Auto, Language::English, Language::French];
            let name = |language| match language {
                Language::Auto => tr!(
                    "Automatique (celle du système, sinon anglais)",
                    "Automatic (system language, otherwise English)"
                ),
                // Chaque langue dans sa propre langue, avec sa région (en-US, fr-FR).
                Language::English => "English (United States)".to_owned(),
                Language::French => "Français (France)".to_owned(),
            };
            ComboBox::from_id_salt("langue")
                .selected_text(name(display.language))
                .show_ui(ui, |ui| {
                    for language in LANGUAGES {
                        ui.selectable_value(&mut display.language, language, name(language));
                    }
                });
        });
        ui.add_space(6.0);
        ui.label(tr!("Police :", "Font:"));
        ui.radio_value(
            &mut display.font,
            FontVariant::Standard,
            tr!(
                "Fixedsys Excelsior standard : <= est une flèche (comme =>), =< est « inférieur ou égal »",
                "Fixedsys Excelsior standard: <= is an arrow (like =>), =< is \"less than or equal\""
            ),
        );
        ui.radio_value(
            &mut display.font,
            FontVariant::Alt,
            tr!(
                "Fixedsys Excelsior ALT : <= est « inférieur ou égal » (pour le code)",
                "Fixedsys Excelsior ALT: <= is \"less than or equal\" (for code)"
            ),
        );
        ui.radio_value(
            &mut display.font,
            FontVariant::Custom,
            tr!("Autre police (fichier .ttf, .otf)", "Other font (.ttf, .otf file)"),
        )
        .on_hover_text(tr!(
            "La Fixedsys de Windows est une police bitmap (.fon) qu'on ne peut pas charger ici : \
             Fixedsys Excelsior en est la copie.",
            "Windows' Fixedsys is a bitmap font (.fon) that can't be loaded here: Fixedsys \
             Excelsior is a copy of it."
        ));
        if display.font == FontVariant::Custom {
            ui.horizontal(|ui| {
                ui.add_space(24.0);
                let mut file = display.custom_font.display().to_string();
                if field(ui, WIDE, TextEdit::singleline(&mut file)).changed() {
                    display.custom_font = file.into();
                }
                if ui.button(tr!("Parcourir…", "Browse…")).clicked()
                    && let Some(file) = rfd::FileDialog::new()
                        .add_filter(tr!("Polices", "Fonts"), &["ttf", "otf", "ttc"])
                        .pick_file()
                {
                    display.custom_font = file;
                }
                ui.add(
                    DragValue::new(&mut display.custom_font_size)
                        .range(6.0..=72.0)
                        .speed(0.5)
                        .suffix(" pt"),
                );
            });
            let installed = fonts::windows_fonts();
            if !installed.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.label(tr!("Polices de Windows :", "Windows fonts:"));
                    for (name, path) in installed {
                        if ui.small_button(name).clicked() {
                            display.custom_font = path;
                        }
                    }
                });
            }
        }
        // La police choisie n'est installée qu'avec OK ou Appliquer : l'aperçu montre celle en
        // vigueur, à sa propre taille (Fixedsys à la taille d'une autre police serait déformée).
        let size = fonts::size(&self.applied, ui.ctx().pixels_per_point());
        ui.label(
            RichText::new(tr!(
                "Aperçu (police en vigueur, changée par OK ou Appliquer) : <= => >= =< != -> :: é ç €",
                "Preview (font in use, changed by OK or Apply): <= => >= =< != -> :: é ç €"
            ))
            .font(FontId::monospace(size)),
        );
        ui.add_space(6.0);
        Grid::new("affichage")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                if display.font != FontVariant::Custom {
                    ui.label(tr!("Taille", "Size"));
                    ComboBox::from_id_salt("taille")
                        .selected_text(match display.font_scale {
                            0 => tr!("Automatique", "Automatic"),
                            n => format!("{} px", 16 * u32::from(n)),
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut display.font_scale, 0, tr!("Automatique", "Automatic"))
                                .on_hover_text(tr!(
                                    "16 px nets × l'échelle de l'écran arrondie",
                                    "Sharp 16 px × the screen scale, rounded"
                                ));
                            for n in 1..=3 {
                                ui.selectable_value(&mut display.font_scale, n, format!("{} px", 16 * u32::from(n)));
                            }
                        });
                    ui.end_row();
                }
                ui.label(tr!("Thème", "Theme"));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut display.theme, Theme::Light, tr!("Clair", "Light"));
                    ui.radio_value(&mut display.theme, Theme::Dark, tr!("Sombre", "Dark"));
                });
                ui.end_row();
                ui.label(tr!("Horodatage", "Timestamp"));
                ui.add(TextEdit::singleline(&mut display.timestamp).hint_text(tr!("vide : aucun", "empty: none")))
                    .on_hover_text(tr!(
                        "Format strftime : %H heures, %M minutes, %S secondes…",
                        "strftime format: %H hours, %M minutes, %S seconds…"
                    ));
                ui.end_row();
                ui.label(tr!("Historique par fenêtre", "Scrollback per window"));
                ui.add(
                    DragValue::new(&mut display.scrollback)
                        .range(100..=100_000)
                        .suffix(tr!(" lignes", " lines")),
                );
                ui.end_row();
            });
        ui.checkbox(
            &mut display.show_joins_parts,
            tr!("Afficher les arrivées et départs", "Show joins and parts"),
        );
        ui.checkbox(
            &mut display.strip_colors,
            tr!("Ignorer les couleurs du texte", "Strip text colors"),
        );
    }
}

fn network(ui: &mut egui::Ui, net: &mut Network) {
    Grid::new("réseau").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(tr!("Nom", "Name"));
        field(ui, WIDE, TextEdit::singleline(&mut net.name));
        ui.end_row();
        ui.label("");
        ui.checkbox(
            &mut net.connect_on_startup,
            tr!("Se connecter au démarrage", "Connect on startup"),
        );
        ui.end_row();
        ui.label(tr!("Pseudo", "Nickname"));
        ui.add(TextEdit::singleline(&mut net.nick).hint_text(tr!("identité générale", "general identity")));
        ui.end_row();
        ui.label(tr!("Pseudo de secours", "Alternate nickname"));
        ui.add(TextEdit::singleline(&mut net.alt_nick).hint_text(tr!("identité générale", "general identity")));
        ui.end_row();
        ui.label(tr!("Mot de passe du serveur", "Server password"));
        ui.add(TextEdit::singleline(&mut net.password).password(true));
        ui.end_row();
        ui.label(tr!("Compte SASL", "SASL account"));
        ui.add(TextEdit::singleline(&mut net.sasl_account).hint_text(tr!("vide : pas de SASL", "empty: no SASL")));
        ui.end_row();
        ui.label(tr!("Mot de passe SASL", "SASL password"));
        ui.add(TextEdit::singleline(&mut net.sasl_password).password(true));
        ui.end_row();
    });
    ui.add_space(6.0);
    ui.label(tr!(
        "Serveurs (essayés dans cet ordre, à tour de rôle) :",
        "Servers (tried in this order, in turn):"
    ));
    let mut edit = None;
    let len = net.servers.len();
    Grid::new("serveurs").striped(true).num_columns(5).show(ui, |ui| {
        for (i, server) in net.servers.iter_mut().enumerate() {
            field(
                ui,
                WIDE,
                TextEdit::singleline(&mut server.host).hint_text(tr!("irc.exemple.net", "irc.example.net")),
            );
            ui.add(DragValue::new(&mut server.port).range(1..=65535));
            ui.checkbox(&mut server.tls, "TLS");
            ui.checkbox(
                &mut server.accept_invalid_cert,
                tr!("Certificat non vérifié", "Unverified certificate"),
            );
            row_buttons(ui, i, len, &mut edit);
            ui.end_row();
        }
    });
    if let Some(edit) = edit {
        edit.apply(&mut net.servers);
    }
    if ui.button(tr!("Ajouter un serveur", "Add a server")).clicked() {
        net.servers.push(ServerAddr::new("", 6697, true));
    }
    ui.add_space(6.0);
    ui.label(tr!(
        "Commandes à la connexion, une par ligne (ex. /ns identify motdepasse) :",
        "Commands on connect, one per line (e.g. /ns identify password):"
    ));
    list_edit(ui, &mut net.perform, 4);
    ui.horizontal(|ui| {
        ui.label(tr!(
            "Attendre avant d'entrer dans les canaux",
            "Wait before joining channels"
        ));
        ui.add(DragValue::new(&mut net.join_delay_secs).range(0..=120).suffix(" s"))
            .on_hover_text(tr!(
                "Le temps que l'identification (NickServ) soit prise en compte",
                "Time for identification (NickServ) to take effect"
            ));
    });
}

/// Serveur d'un favori : automatique (ceux du réseau), l'un des serveurs du réseau, ou un autre
/// saisi comme pour `/server` (`hôte:+port` : TLS).
fn favorite_server(
    ui: &mut egui::Ui,
    i: usize,
    fav: &mut Favorite,
    servers: &[ServerAddr],
    custom: &mut HashMap<usize, String>,
) {
    let is_other = |fav: &Favorite, custom: &HashMap<usize, String>| {
        let listed = fav.server.host.is_empty() || servers.contains(&fav.server);
        custom.contains_key(&i) || !listed || servers.is_empty()
    };
    let other = is_other(fav, custom);
    ui.horizontal(|ui| {
        if !servers.is_empty() {
            let selected = if other {
                tr!("Autre…", "Other…")
            } else if fav.server.host.is_empty() {
                tr!("Automatique", "Automatic")
            } else {
                fav.server.to_string()
            };
            ComboBox::from_id_salt(("serveur", i))
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(!other && fav.server.host.is_empty(), tr!("Automatique", "Automatic"))
                        .clicked()
                    {
                        fav.server = ServerAddr::default();
                        custom.remove(&i);
                    }
                    for server in servers {
                        if ui
                            .selectable_label(!other && fav.server == *server, server.to_string())
                            .clicked()
                        {
                            fav.server = server.clone();
                            custom.remove(&i);
                        }
                    }
                    if ui.selectable_label(other, tr!("Autre…", "Other…")).clicked() && !other {
                        custom.insert(i, String::new());
                    }
                });
        }
        // Après le choix dans la liste : « Automatique » ou un serveur referme le champ « Autre ».
        if is_other(fav, custom) {
            let text = custom.entry(i).or_insert_with(|| {
                if fav.server.host.is_empty() {
                    String::new()
                } else {
                    fav.server.to_string()
                }
            });
            let invalid = !text.is_empty() && ServerAddr::parse(text).is_none();
            let mut edit = TextEdit::singleline(text).hint_text(tr!("hôte:+port (+ : TLS)", "host:+port (+: TLS)"));
            if invalid {
                edit = edit.text_color(Color32::RED);
            }
            if field(ui, WIDE - 60.0, edit).changed() {
                fav.server = ServerAddr::parse(text).unwrap_or_default();
            }
        }
    });
}

/// Largeur des champs de saisie principaux (hôtes, chemins, listes).
const WIDE: f32 = 320.0;

/// Champ de saisie de largeur fixe. `desired_width` ne suffit pas dans une grille : une colonne
/// (sauf la dernière) n'y offre que sa largeur de l'image précédente, au départ 40 points, et le
/// champ en resterait là.
fn field(ui: &mut egui::Ui, width: f32, edit: TextEdit) -> egui::Response {
    ui.add_sized([width, ui.spacing().interact_size.y], edit.desired_width(width))
}

/// Liste modifiable sous forme de texte, un élément par ligne.
fn list_edit(ui: &mut egui::Ui, list: &mut Vec<String>, rows: usize) {
    let mut text = list.join("\n");
    let edit = TextEdit::multiline(&mut text)
        .desired_rows(rows)
        .desired_width(WIDE + 80.0);
    if ui.add(edit).changed() {
        *list = text.split('\n').map(String::from).collect();
    }
}

/// Chemin sans espaces autour ni guillemets (« Copier en tant que chemin » de l'Explorateur).
fn clean_path(path: &Path) -> PathBuf {
    path.to_string_lossy().trim().trim_matches('"').trim().into()
}

/// Configuration sans lignes vides ni espaces superflus.
fn cleaned(config: &Config) -> Config {
    let mut config = config.clone();
    let clean = |list: &mut Vec<String>| {
        *list = list
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
    };
    clean(&mut config.connection.highlight_words);
    clean(&mut config.dcc.ignore_files);
    clean(&mut config.dcc.ignore_nicks);
    for net in &mut config.networks {
        clean(&mut net.perform);
        for server in &mut net.servers {
            server.host = server.host.trim().to_owned();
        }
        net.servers.retain(|s| !s.host.is_empty());
    }
    for fav in &mut config.favorites {
        fav.channel = fav.channel.trim().to_owned();
        fav.server.host = fav.server.host.trim().to_owned();
    }
    config.favorites.retain(|f| f.channel.len() > 1);
    config.dcc.download_dir = clean_path(&config.dcc.download_dir);
    config.display.custom_font = clean_path(&config.display.custom_font);
    for event in sound::EVENTS {
        let sound = config.sounds.get_mut(event);
        sound.file = clean_path(&sound.file);
    }
    if config.dcc.port_last < config.dcc.port_first {
        config.dcc.port_last = config.dcc.port_first;
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dans une grille, un champ garde sa largeur (et pas les 40 points d'une colonne vide).
    #[test]
    fn grid_field_width() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        let mut width = 0.0;
        for _ in 0..3 {
            ctx.run_ui(Default::default(), |ui| {
                Grid::new("grille").num_columns(2).show(ui, |ui| {
                    width = field(ui, 200.0, TextEdit::singleline(&mut text)).rect.width();
                    ui.label("fin");
                    ui.end_row();
                });
            })
            .textures_delta
            .clear();
        }
        assert!(width >= 200.0, "{width}");
    }
}
