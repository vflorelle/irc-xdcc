//! Interface : arborescence des fenêtres, discussions, liste des pseudos, saisie, menus.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::text::{CCursor, CCursorRange, LayoutJob};
use eframe::egui::text_edit::TextEditState;
use eframe::egui::{
    self, Color32, CursorIcon, FontId, Galley, Id, Key, Label, Modifiers, RichText, ScrollArea, Sense, TextEdit,
    TextFormat, UserAttentionType, ViewportBuilder, ViewportCommand, ViewportId, vec2,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{Config, Favorite, FontVariant, ServerAddr, Theme};
use crate::core::{Command, Core, CoreMsg};
use crate::format::{self, Link};
use crate::options::{Options, Tab};
use crate::state::{Activity, BufKind, Buffer, ConnStatus, Focus, LineKind, Shared, State, lock};
use crate::{fonts, transfers};

pub const APP_NAME: &str = "irc-xdcc";

/// Couleurs d'un thème.
struct Palette {
    background: Color32,
    text: Color32,
    timestamp: Color32,
    action: Color32,
    notice: Color32,
    ctcp: Color32,
    event: Color32,
    quit: Color32,
    info: Color32,
    error: Color32,
    highlight: Color32,
    activity_event: Color32,
    activity_message: Color32,
    activity_highlight: Color32,
    disabled: Color32,
    /// Trait qui sépare les lignes lues des nouvelles.
    marker: Color32,
}

fn palette(theme: Theme) -> Palette {
    let c = format::irc_color;
    match theme {
        // Couleurs IRC classiques.
        Theme::Light => Palette {
            background: Color32::WHITE,
            text: c(1),
            timestamp: c(14),
            action: c(6),
            notice: c(5),
            ctcp: c(4),
            event: c(3),
            quit: c(2),
            info: c(2),
            error: c(4),
            highlight: c(4),
            activity_event: c(12),
            activity_message: c(4),
            activity_highlight: c(13),
            disabled: c(14),
            marker: Color32::from_gray(0xc0),
        },
        Theme::Dark => Palette {
            background: Color32::from_rgb(0x1e, 0x1e, 0x1e),
            text: c(15),
            timestamp: c(14),
            action: Color32::from_rgb(0xc7, 0x7d, 0xff),
            notice: Color32::from_rgb(0xe0, 0xa0, 0x60),
            ctcp: Color32::from_rgb(0xff, 0x60, 0x60),
            event: Color32::from_rgb(0x4e, 0xc9, 0x4e),
            quit: Color32::from_rgb(0x6b, 0x8c, 0xff),
            info: Color32::from_rgb(0x7f, 0xa7, 0xff),
            error: Color32::from_rgb(0xff, 0x55, 0x55),
            highlight: Color32::from_rgb(0xff, 0x70, 0x70),
            activity_event: Color32::from_rgb(0x6f, 0xa8, 0xff),
            activity_message: Color32::from_rgb(0xff, 0x60, 0x60),
            activity_highlight: Color32::from_rgb(0xff, 0x66, 0xff),
            disabled: c(14),
            marker: Color32::from_gray(0x5a),
        },
    }
}

impl Palette {
    fn line(&self, kind: LineKind) -> Color32 {
        match kind {
            LineKind::Normal | LineKind::Own | LineKind::Server | LineKind::Separator => self.text,
            LineKind::Action => self.action,
            LineKind::Notice => self.notice,
            LineKind::Ctcp => self.ctcp,
            LineKind::Join | LineKind::Part | LineKind::Mode | LineKind::Topic | LineKind::Info2 => self.event,
            LineKind::Quit => self.quit,
            LineKind::Info => self.info,
            LineKind::Error => self.error,
        }
    }

    fn activity(&self, activity: Activity) -> Option<Color32> {
        match activity {
            Activity::None => None,
            Activity::Event => Some(self.activity_event),
            Activity::Message => Some(self.activity_message),
            Activity::Highlight => Some(self.activity_highlight),
        }
    }
}

/// Ligne mise en forme, gardée d'une image à l'autre (les longs historiques restent fluides).
/// La mise en page elle-même (`Galley`) est redemandée à chaque image pour les lignes visibles :
/// gardée, elle pointerait vers des glyphes effacés quand egui vide sa texture de glyphes pleine
/// (lettres blanches au hasard).
struct Row {
    id: u64,
    job: LayoutJob,
    /// Copie où seul le texte en gras est visible, dessinée un pixel plus à droite.
    bold: Option<LayoutJob>,
    height: f32,
    links: Vec<(Range<usize>, Link)>,
}

/// Ce qui oblige à refaire la mise en page.
#[derive(Clone, PartialEq)]
struct Layout {
    width: u32,
    font_size: u32,
    pixels_per_point: u32,
    theme: Theme,
    colors: bool,
    chantypes: String,
}

/// Position dans les lignes : identifiant de la ligne, caractère.
type TextPos = (u64, usize);

#[derive(Default)]
struct LineCache {
    layout: Option<Layout>,
    rows: VecDeque<Row>,
    /// Sélection pendant que le bouton de la souris est enfoncé : du point de départ au pointeur.
    /// Au relâchement, le texte est copié et la sélection disparaît.
    selection: Option<(TextPos, TextPos)>,
}

#[derive(Default)]
struct Input {
    text: String,
    history: Vec<String>,
    history_pos: Option<usize>,
    completion: Option<Completion>,
}

/// Complétion des pseudos avec Tab (appuis successifs : candidat suivant).
struct Completion {
    start: usize,
    prefix: String,
    next: usize,
    /// Texte après la dernière complétion (s'il a changé, on repart de zéro).
    result: String,
}

pub struct App {
    state: Shared,
    core: UnboundedSender<CoreMsg>,
    selected: Focus,
    /// Fenêtre affichée à l'image précédente : en changer allonge l'historique.
    shown: Focus,
    /// Fenêtres lues à l'image précédente (voir `displayed`).
    reading: Vec<Focus>,
    /// Fenêtre ouverte à part qui a le focus de Windows.
    focused_detached: Option<(Focus, ViewportId)>,
    /// Fenêtres visitées, dans l'ordre (Alt+← / Alt+→), et position dans cette liste.
    history: Vec<Focus>,
    history_pos: usize,
    /// Changement de fenêtre par Alt+← / Alt+→ : l'historique n'est pas modifié.
    navigating: bool,
    inputs: HashMap<Focus, Input>,
    /// Lignes mises en page, par vue (fenêtre principale ou séparée) : une fenêtre ouverte à part
    /// peut aussi être affichée dans la fenêtre principale, à une autre largeur.
    caches: HashMap<(ViewportId, Focus), LineCache>,
    /// Fenêtres ouvertes à part, avec leur fenêtre native (qui la garde si la fenêtre est
    /// renommée).
    detached: Vec<(Focus, ViewportId)>,
    options: Option<Options>,
    quick_connect: Option<ServerAddr>,
    /// Fenêtre d'aide (Aide → Commandes, /help).
    help: bool,
    /// Boîtes de dialogue ouvertes, celle du dessus en dernier (Échap, Alt+F4).
    open_dialogs: Vec<Dialog>,
    /// Police installée (variante, fichier de la police personnalisée).
    font: Option<(FontVariant, PathBuf)>,
    theme: Option<Theme>,
    title: String,
    error: Option<String>,
    /// Favori ajouté depuis un menu contextuel (appliqué après le parcours des serveurs).
    favorite_to_add: Option<Favorite>,
    transfers: transfers::Speeds,
    _runtime: tokio::runtime::Runtime,
}

impl App {
    /// `loaded` : la configuration lue au démarrage (voir `Config::load`).
    pub fn new(cc: &eframe::CreationContext, loaded: (Config, bool, Option<String>)) -> Self {
        let (config, first_run, error) = loaded;
        // Libellés non sélectionnables : seuls le texte des discussions (sélection à la souris,
        // copiée au relâchement) et le sujet du canal le sont.
        cc.egui_ctx
            .all_styles_mut(|style| style.interaction.selectable_labels = false);
        cc.egui_ctx.add_font_provider(Arc::new(fonts::SystemFonts::new()));
        let state: Shared = Arc::new(std::sync::Mutex::new(State::new(config.clone())));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime tokio");
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        runtime.spawn(Core::new(state.clone(), cc.egui_ctx.clone(), tx.clone()).run(rx));
        App {
            state,
            core: tx,
            selected: Focus::Transfers,
            shown: Focus::Transfers,
            reading: Vec::new(),
            focused_detached: None,
            history: vec![Focus::Transfers],
            history_pos: 0,
            navigating: false,
            inputs: HashMap::new(),
            caches: HashMap::new(),
            detached: Vec::new(),
            // Premier lancement : l'identité d'abord.
            options: first_run.then(|| Options::new(&config, Tab::Identity)),
            quick_connect: None,
            help: false,
            open_dialogs: Vec::new(),
            font: None,
            theme: None,
            title: String::new(),
            error,
            favorite_to_add: None,
            transfers: Default::default(),
            _runtime: runtime,
        }
    }

    fn send(&self, command: Command) {
        let _ = self.core.send(CoreMsg::Ui(command));
    }

    /// Police, taille et thème selon la configuration (et l'échelle de l'écran).
    fn apply_style(&mut self, ctx: &egui::Context, display: &crate::config::Display) {
        let font = (display.font, display.custom_font.clone());
        if self.font.as_ref() != Some(&font) {
            if let Err(e) = fonts::install(ctx, display) {
                self.error = Some(e);
            }
            self.font = Some(font);
            // Les lignes déjà mises en page l'ont été avec l'ancienne police.
            self.caches.clear();
        }
        if self.theme != Some(display.theme) {
            ctx.set_theme(match display.theme {
                Theme::Light => egui::Theme::Light,
                Theme::Dark => egui::Theme::Dark,
            });
            self.theme = Some(display.theme);
        }
    }

    fn font_id(ctx: &egui::Context, config: &Config) -> FontId {
        FontId::monospace(fonts::size(&config.display, ctx.pixels_per_point()))
    }

    /// Fenêtre quittée (ou fenêtre séparée qui perd le focus) : ce qui y était affiché est lu,
    /// trait de séparation au retour. Retrouvée sans rien de nouveau : pas de trait, même si des
    /// lignes arrivent ensuite pendant qu'on la regarde.
    fn mark_read(&mut self, st: &mut State) {
        let reading: Vec<Focus> = std::iter::once(self.selected.clone())
            .chain(self.focused_detached.as_ref().map(|(f, _)| f.clone()))
            .collect();
        for focus in self.reading.iter().filter(|f| !reading.contains(f)) {
            if let Some(buffer) = buffer_mut(st, focus) {
                buffer.read_marker = buffer.lines.back().map(|l| l.id);
            }
        }
        for focus in reading.iter().filter(|f| !self.reading.contains(f)) {
            if let Some(buffer) = buffer_mut(st, focus)
                && buffer.read_marker == buffer.lines.back().map(|l| l.id)
            {
                buffer.read_marker = None;
            }
        }
        self.reading = reading;
    }

    /// Fenêtre renommée (correspondant d'une conversation privée qui change de pseudo) : elle
    /// garde sa place (fenêtre principale, fenêtre séparée, historique) et la saisie en cours.
    fn rename(&mut self, old: &Focus, new: &Focus) {
        let rename = |focus: &mut Focus| {
            if focus == old {
                *focus = new.clone();
            }
        };
        rename(&mut self.selected);
        rename(&mut self.shown);
        self.reading.iter_mut().for_each(rename);
        self.history.iter_mut().for_each(rename);
        self.detached.iter_mut().for_each(|(focus, _)| rename(focus));
        if let Some((focus, _)) = &mut self.focused_detached {
            rename(focus);
        }
        if let Some(input) = self.inputs.remove(old) {
            self.inputs.insert(new.clone(), input);
        }
    }

    /// Fenêtre lue : celle de la fenêtre principale, ou celle ouverte à part qui a le focus. Ce
    /// qui y arrive est lu : sans couleur d'activité dans l'arborescence, ni trait.
    fn displayed(&self, focus: &Focus) -> bool {
        self.selected == *focus || self.focused_detached.as_ref().is_some_and(|(f, _)| f == focus)
    }

    /// Fenêtre active, et la vue qui la montre : celle ouverte à part qui a le focus, sinon celle
    /// de la fenêtre principale. Les notices et réponses y sont écrites, son défilement compte
    /// pour les bips.
    fn active(&self) -> (ViewportId, &Focus) {
        match &self.focused_detached {
            Some((focus, viewport)) => (*viewport, focus),
            None => (ViewportId::ROOT, &self.selected),
        }
    }

    /// Ordre d'affichage des fenêtres (arborescence, Ctrl+Page préc./suiv.).
    fn windows(st: &State) -> Vec<Focus> {
        let mut list = Vec::new();
        for server in &st.servers {
            list.push(Focus::Buffer(server.id, String::new()));
            list.extend(
                sorted_buffers(server)
                    .into_iter()
                    .map(|b| Focus::Buffer(server.id, b.key.clone())),
            );
        }
        list.push(Focus::Transfers);
        list
    }

    fn exists(st: &State, focus: &Focus) -> bool {
        match focus {
            Focus::Transfers => true,
            Focus::Buffer(id, key) => st.server(*id).is_some_and(|s| s.buffer(key).is_some()),
        }
    }
}

/// Fenêtre native d'une fenêtre ouverte à part, choisie à son ouverture.
fn viewport_id(focus: &Focus) -> ViewportId {
    ViewportId::from_hash_of(("fenêtre", focus))
}

/// Tampon d'une fenêtre de discussion (aucun pour celle des transferts).
fn buffer_mut<'a>(st: &'a mut State, focus: &Focus) -> Option<&'a mut Buffer> {
    let Focus::Buffer(id, key) = focus else { return None };
    st.server_mut(*id)?.buffer_mut(key)
}

/// Canaux (par ordre alphabétique) puis conversations privées.
fn sorted_buffers(server: &crate::state::Server) -> Vec<&Buffer> {
    let mut buffers: Vec<&Buffer> = server.buffers.iter().skip(1).collect();
    buffers.sort_by_key(|b| (b.kind != BufKind::Channel, b.key.clone()));
    buffers
}

fn window_title(st: &State, focus: &Focus) -> String {
    match focus {
        Focus::Transfers => tr!("Transferts", "Transfers"),
        Focus::Buffer(id, key) => match st.server(*id) {
            Some(server) if key.is_empty() => format!("{} — {}", server.label, server.nick),
            Some(server) => {
                let name = server.buffer(key).map(|b| b.name.as_str()).unwrap_or(key);
                format!("{name} — {} ({})", server.label, server.nick)
            }
            None => String::new(),
        },
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let state = self.state.clone();
        let mut st = lock(&state);
        let display = st.config.display.clone();
        self.apply_style(&ctx, &display);

        for (old, new) in std::mem::take(&mut st.renamed) {
            self.rename(&old, &new);
        }
        if let Some(focus) = st.focus.take() {
            self.selected = focus;
        }
        if std::mem::take(&mut st.help) {
            self.help = true;
        }
        self.detached.retain(|(d, _)| Self::exists(&st, d));
        self.prune_history(&st);
        // Fenêtre affichée fermée : la précédente visitée, sinon celle de son serveur.
        if !Self::exists(&st, &self.selected) {
            let previous = self
                .history
                .get(self.history_pos)
                .filter(|f| **f != self.selected)
                .cloned();
            self.navigating = previous.is_some();
            self.selected = previous.unwrap_or_else(|| match &self.selected {
                Focus::Buffer(id, _) if st.server(*id).is_some() => Focus::Buffer(*id, String::new()),
                _ => st
                    .servers
                    .first()
                    .map(|s| Focus::Buffer(s.id, String::new()))
                    .unwrap_or(Focus::Transfers),
            });
        }
        self.mark_read(&mut st);
        if self.shown != self.selected {
            // Fenêtre choisie autrement que par Alt+← / Alt+→ : la suite de l'historique est
            // remplacée, comme dans un navigateur.
            if !std::mem::take(&mut self.navigating) {
                self.history.truncate(self.history_pos + 1);
                self.history.push(self.selected.clone());
                if self.history.len() > HISTORY_LEN {
                    self.history.remove(0);
                }
                self.history_pos = self.history.len() - 1;
            }
            self.shown = self.selected.clone();
        }
        let detached = &self.detached;
        self.caches.retain(|(viewport, focus), _| {
            Self::exists(&st, focus) && (*viewport == ViewportId::ROOT || detached.iter().any(|(_, v)| v == viewport))
        });
        // Clignotement si aucune fenêtre du logiciel (principale ou séparée) n'a le focus.
        if std::mem::take(&mut st.attention)
            && self.focused_detached.is_none()
            && !ctx.input(|i| i.viewport().focused.unwrap_or(true))
        {
            ctx.send_viewport_cmd(ViewportCommand::RequestUserAttention(UserAttentionType::Informational));
        }
        self.dialog_keys(&ctx, &st);
        self.switch_keys(&ctx, &st);

        let title = format!("{APP_NAME} — {}", window_title(&st, &self.selected));
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &mut st));
        let pal = palette(display.theme);
        egui::Panel::left("tree")
            .resizable(true)
            .default_size(180.0)
            .show(ui, |ui| self.tree(ui, &mut st, &pal));
        let selected = self.selected.clone();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(ui.style())
                    .fill(pal.background)
                    .inner_margin(0.0),
            )
            .show(ui, |ui| self.window(ui, &mut st, &selected, &pal));

        let mut focused_detached = None;
        for (focus, viewport) in self.detached.clone() {
            let title = format!("{APP_NAME} — {}", window_title(&st, &focus));
            let builder = ViewportBuilder::default()
                .with_title(title)
                .with_inner_size([800.0, 550.0]);
            let mut close = false;
            ctx.show_viewport_immediate(viewport, builder, |ui, _| {
                if ui.input(|i| i.viewport().focused == Some(true)) {
                    focused_detached = Some((focus.clone(), viewport));
                }
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::central_panel(ui.style())
                            .fill(pal.background)
                            .inner_margin(0.0),
                    )
                    .show(ui, |ui| self.window(ui, &mut st, &focus, &pal));
                // Fermée par sa croix ou Ctrl+W : la fenêtre (canal…) reste dans l'arborescence.
                close = ui.input_mut(|i| i.viewport().close_requested() || consume_exact(i, Modifiers::CTRL, Key::W));
            });
            if close {
                self.detached.retain(|(d, _)| *d != focus);
            }
        }
        self.focused_detached = focused_detached.filter(|f| self.detached.contains(f));

        self.dialogs(&ctx, &mut st);
        crate::transfers::prompts(&ctx, &st, &self.core);

        st.active = match self.active().1 {
            Focus::Buffer(id, key) => Some((*id, key.clone())),
            Focus::Transfers => None,
        };
        // Progression des transferts en cours.
        if st.transfers.iter().any(|t| !t.status.finished()) {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let connected = lock(&self.state)
            .servers
            .iter()
            .any(|s| s.status == ConnStatus::Connected);
        self.send(Command::Quit);
        if connected {
            // Laisse partir les QUIT.
            std::thread::sleep(Duration::from_millis(400));
        }
    }
}

impl App {
    /// Raccourcis des boîtes de dialogue : aide (F1), options (Alt+O) ; Échap et Alt+F4 ferment
    /// celle du dessus (Alt+F4 ne ferme le logiciel que si aucune n'est ouverte).
    fn dialog_keys(&mut self, ctx: &egui::Context, st: &State) {
        // Ordre des boîtes ouvertes : une nouvelle passe au premier plan, comme une boîte
        // cliquée (egui la met alors au-dessus des autres).
        let open: Vec<Dialog> = Dialog::ALL.into_iter().filter(|d| self.is_open(*d)).collect();
        self.open_dialogs.retain(|d| open.contains(d));
        let mut opened = false;
        for dialog in open {
            if !self.open_dialogs.contains(&dialog) {
                self.open_dialogs.push(dialog);
                ctx.move_to_top(dialog.layer());
                opened = true;
            }
        }
        // egui ne la met au-dessus qu'à la fin de l'image : en attendant, l'ancienne y est encore.
        if !opened
            && let Some(top) = ctx.top_layer_id()
            && let Some(pos) = self.open_dialogs.iter().position(|d| d.layer() == top)
        {
            let dialog = self.open_dialogs.remove(pos);
            self.open_dialogs.push(dialog);
        }
        // Sans boîte de dialogue, Échap garde son rôle habituel ; il ferme d'abord une liste
        // déroulante ou un menu ouverts.
        let dialog = !self.open_dialogs.is_empty();
        let menu = egui::Popup::is_any_open(ctx);
        let (help, options, escape, alt_f4) = ctx.input_mut(|i| {
            (
                consume_exact(i, Modifiers::NONE, Key::F1),
                consume_exact(i, Modifiers::ALT, Key::O),
                dialog && !menu && consume_exact(i, Modifiers::NONE, Key::Escape),
                i.viewport().close_requested() && i.modifiers.alt,
            )
        });
        if help {
            self.help = true;
        }
        if options && self.options.is_none() {
            self.options = Some(Options::new(&st.config, Tab::Identity));
        }
        if (escape || alt_f4)
            && let Some(top) = self.open_dialogs.pop()
        {
            match top {
                Dialog::Options => self.options = None,
                Dialog::Help => self.help = false,
                Dialog::QuickConnect => self.quick_connect = None,
                Dialog::Error => self.error = None,
            }
            if alt_f4 {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            }
        }
    }

    fn is_open(&self, dialog: Dialog) -> bool {
        match dialog {
            Dialog::Options => self.options.is_some(),
            Dialog::Help => self.help,
            Dialog::QuickConnect => self.quick_connect.is_some(),
            Dialog::Error => self.error.is_some(),
        }
    }

    /// Raccourcis de fenêtre : suivante / précédente dans l'arborescence (Ctrl+Page, Ctrl+Tab),
    /// visitée avant / après (Alt+← / Alt+→), fermeture (Ctrl+W).
    fn switch_keys(&mut self, ctx: &egui::Context, st: &State) {
        let (next, previous, back, forward, close) = ctx.input_mut(|i| {
            (
                consume_exact(i, Modifiers::CTRL, Key::PageDown) || consume_exact(i, Modifiers::CTRL, Key::Tab),
                consume_exact(i, Modifiers::CTRL, Key::PageUp)
                    || consume_exact(i, Modifiers::CTRL | Modifiers::SHIFT, Key::Tab),
                consume_exact(i, Modifiers::ALT, Key::ArrowLeft),
                consume_exact(i, Modifiers::ALT, Key::ArrowRight),
                consume_exact(i, Modifiers::CTRL, Key::W),
            )
        });
        if (back || forward)
            && let Some(focus) = self.history_step(if back { -1 } else { 1 })
        {
            self.navigating = true;
            self.selected = focus;
        }
        if close {
            self.close(st, &self.selected.clone());
        }
        if !next && !previous {
            return;
        }
        let list = Self::windows(st);
        let Some(pos) = list.iter().position(|f| *f == self.selected) else {
            return;
        };
        let len = list.len();
        let index = if next { (pos + 1) % len } else { (pos + len - 1) % len };
        self.selected = list[index].clone();
    }

    /// Retire de l'historique les fenêtres fermées, et les doublons qui se retrouvent alors côte à
    /// côte ; la position suit (sur la fenêtre précédente si celle où elle était a disparu).
    fn prune_history(&mut self, st: &State) {
        let current = self.history_pos;
        let mut kept: Vec<Focus> = Vec::with_capacity(self.history.len());
        for (i, focus) in self.history.drain(..).enumerate() {
            if Self::exists(st, &focus) && kept.last() != Some(&focus) {
                kept.push(focus);
            }
            if i == current {
                self.history_pos = kept.len().saturating_sub(1);
            }
        }
        self.history = kept;
    }

    /// Fenêtre précédente (`-1`) ou suivante (`1`) dans l'ordre de visite, en sautant la fenêtre
    /// affichée ; aucune au bout de la liste.
    fn history_step(&mut self, step: isize) -> Option<Focus> {
        let mut pos = self.history_pos;
        loop {
            pos = pos.checked_add_signed(step)?;
            let focus = self.history.get(pos)?;
            if *focus != self.selected {
                self.history_pos = pos;
                return Some(focus.clone());
            }
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui, st: &mut State) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button(tr!("Fichier", "File"), |ui| {
                if ui.add(egui::Button::new("Options…").shortcut_text("Alt+O")).clicked() {
                    self.options = Some(Options::new(&st.config, Tab::Identity));
                }
                ui.separator();
                if ui.button(tr!("Quitter", "Quit")).clicked() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                }
            });
            ui.menu_button(tr!("Serveurs", "Servers"), |ui| {
                for net in &st.config.networks {
                    if ui.button(&net.name).clicked() {
                        self.send(Command::ConnectNetwork(net.name.clone()));
                    }
                }
                if !st.config.networks.is_empty() {
                    ui.separator();
                }
                if ui.button(tr!("Autre serveur…", "Other server…")).clicked() {
                    self.quick_connect = Some(ServerAddr::new("", 6697, true));
                }
                if ui.button(tr!("Gérer les serveurs…", "Manage servers…")).clicked() {
                    self.options = Some(Options::new(&st.config, Tab::Servers));
                }
            });
            ui.menu_button(tr!("Favoris", "Favorites"), |ui| {
                let current = match &self.selected {
                    Focus::Buffer(id, key) => st
                        .server(*id)
                        .and_then(|s| s.buffer(key).filter(|b| b.kind == BufKind::Channel).map(|b| (s, b))),
                    Focus::Transfers => None,
                };
                if let Some((server, buffer)) = current {
                    let name = &buffer.name;
                    if ui
                        .button(tr!("Ajouter {name} aux favoris", "Add {name} to favorites"))
                        .clicked()
                    {
                        let favorite = new_favorite(server, &buffer.name);
                        if let Err(e) = add_favorite(st, favorite) {
                            self.error = Some(tr!("Enregistrement impossible : {e}", "Could not save: {e}"));
                        }
                    }
                    ui.separator();
                }
                for (i, fav) in st.config.favorites.iter().enumerate() {
                    let label = if fav.network.is_empty() {
                        format!("{} ({})", fav.channel, fav.server)
                    } else {
                        format!("{} — {}", fav.channel, fav.network)
                    };
                    if ui.button(label).clicked() {
                        self.send(Command::JoinFavorite(i));
                    }
                }
                if !st.config.favorites.is_empty() {
                    ui.separator();
                }
                if ui.button(tr!("Organiser…", "Organize…")).clicked() {
                    self.options = Some(Options::new(&st.config, Tab::Favorites));
                }
            });
            ui.menu_button(tr!("Fenêtre", "Window"), |ui| {
                if ui.button(tr!("Transferts", "Transfers")).clicked() {
                    self.selected = Focus::Transfers;
                }
                self.detach_button(ui, &self.selected.clone());
                if ui.button(tr!("Fermer", "Close")).clicked() {
                    self.close(st, &self.selected.clone());
                }
            });
            ui.menu_button(tr!("Aide", "Help"), |ui| {
                if ui
                    .add(egui::Button::new(tr!("Commandes", "Commands")).shortcut_text("F1"))
                    .clicked()
                {
                    self.help = true;
                }
                ui.label(format!("{APP_NAME} {}", env!("CARGO_PKG_VERSION")));
            });
        });
    }

    /// Action de menu : ouvrir la fenêtre à part, ou, si elle l'est déjà, mettre au premier plan
    /// la fenêtre séparée.
    fn detach_button(&mut self, ui: &mut egui::Ui, focus: &Focus) {
        let viewport = self.detached.iter().find(|(f, _)| f == focus).map(|(_, v)| *v);
        let label = if viewport.is_some() {
            tr!("Afficher la fenêtre séparée", "Show the separate window")
        } else {
            tr!("Ouvrir dans une fenêtre séparée", "Open in a separate window")
        };
        if ui.button(label).clicked() {
            match viewport {
                Some(viewport) => ui.ctx().send_viewport_cmd_to(viewport, ViewportCommand::Focus),
                None => self.detached.push((focus.clone(), viewport_id(focus))),
            }
        }
    }

    /// Ferme une fenêtre (celle d'un serveur avec ses canaux) ; si c'est celle affichée, la
    /// précédente dans l'arborescence prend sa place (la suivante s'il n'y en a pas avant).
    fn close(&mut self, st: &State, focus: &Focus) {
        if let Focus::Buffer(id, key) = focus
            && *focus == self.selected
        {
            let closed =
                |f: &Focus| f == focus || key.is_empty() && matches!(f, Focus::Buffer(other, _) if other == id);
            let list = Self::windows(st);
            if let Some(pos) = list.iter().position(|f| f == focus) {
                let before = list[..pos].iter().rev().find(|f| !closed(f));
                let after = list[pos..].iter().find(|f| !closed(f));
                if let Some(target) = before.or(after) {
                    self.selected = target.clone();
                }
            }
        }
        match focus {
            Focus::Buffer(id, key) => self.send(Command::CloseBuffer(*id, key.clone())),
            Focus::Transfers => self.detached.retain(|(d, _)| d != focus),
        }
    }

    fn tree(&mut self, ui: &mut egui::Ui, st: &mut State, pal: &Palette) {
        let font = Self::font_id(ui.ctx(), &st.config);
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            let mut clicked = None;
            for server in &st.servers {
                let status = match server.status {
                    ConnStatus::Connected => String::new(),
                    ConnStatus::Connecting | ConnStatus::Registering => tr!(" (connexion…)", " (connecting…)"),
                    ConnStatus::Disconnected => tr!(" (déconnecté)", " (disconnected)"),
                    ConnStatus::Waiting(_) => tr!(" (reconnexion…)", " (reconnecting…)"),
                };
                let focus = Focus::Buffer(server.id, String::new());
                let mut text = RichText::new(format!("{}{status}", server.label)).font(font.clone());
                if server.status != ConnStatus::Connected {
                    text = text.color(pal.disabled);
                } else if !self.displayed(&focus)
                    && let Some(color) = pal.activity(server.buffers[0].activity)
                {
                    text = text.color(color);
                }
                let response = ui.selectable_label(self.selected == focus, text);
                if response.clicked() {
                    clicked = Some(focus.clone());
                }
                response.context_menu(|ui| {
                    if server.status == ConnStatus::Disconnected {
                        if ui.button(tr!("Se connecter", "Connect")).clicked() {
                            self.send(Command::Reconnect(server.id));
                        }
                    } else {
                        if ui.button(tr!("Se reconnecter", "Reconnect")).clicked() {
                            self.send(Command::Reconnect(server.id));
                        }
                        if ui.button(tr!("Se déconnecter", "Disconnect")).clicked() {
                            self.send(Command::Disconnect(server.id));
                        }
                    }
                    self.detach_button(ui, &focus);
                    if ui.button(tr!("Fermer", "Close")).clicked() {
                        self.close(st, &focus);
                    }
                });
                for buffer in sorted_buffers(server) {
                    let focus = Focus::Buffer(server.id, buffer.key.clone());
                    let mut text = RichText::new(&buffer.name).font(font.clone());
                    if buffer.kind == BufKind::Channel && !buffer.joined {
                        text = text.color(pal.disabled);
                    } else if !self.displayed(&focus)
                        && let Some(color) = pal.activity(buffer.activity)
                    {
                        text = text.color(color);
                    }
                    let response = ui
                        .horizontal(|ui| {
                            ui.add_space(14.0);
                            ui.selectable_label(self.selected == focus, text)
                        })
                        .inner;
                    if response.clicked() {
                        clicked = Some(focus.clone());
                    }
                    response.context_menu(|ui| {
                        if buffer.kind == BufKind::Channel {
                            if buffer.joined {
                                if ui.button(tr!("Quitter le canal", "Part channel")).clicked() {
                                    self.send(Command::Input {
                                        server: server.id,
                                        buffer: buffer.key.clone(),
                                        text: format!("/part {}", buffer.name),
                                    });
                                }
                            } else if ui.button(tr!("Rejoindre", "Join")).clicked() {
                                self.send(Command::Input {
                                    server: server.id,
                                    buffer: buffer.key.clone(),
                                    text: format!("/join {}", buffer.name),
                                });
                            }
                            if ui.button(tr!("Ajouter aux favoris", "Add to favorites")).clicked() {
                                self.favorite_to_add = Some(new_favorite(server, &buffer.name));
                            }
                        }
                        self.detach_button(ui, &focus);
                        if ui.button(tr!("Fermer", "Close")).clicked() {
                            self.close(st, &focus);
                        }
                    });
                }
            }
            ui.separator();
            let active = st.transfers.iter().filter(|t| !t.status.finished()).count();
            let waiting = st.queue.iter().filter(|q| !q.status.finished()).count();
            let mut label = tr!("Transferts", "Transfers");
            if active + waiting > 0 {
                label = tr!(
                    "Transferts ({active} en cours, {waiting} en file)",
                    "Transfers ({active} active, {waiting} queued)"
                );
            }
            let response = ui.selectable_label(self.selected == Focus::Transfers, label);
            if response.clicked() {
                clicked = Some(Focus::Transfers);
            }
            response.context_menu(|ui| {
                self.detach_button(ui, &Focus::Transfers);
            });
            if let Some(focus) = clicked {
                self.selected = focus;
            }
        });
        if let Some(favorite) = self.favorite_to_add.take()
            && let Err(e) = add_favorite(st, favorite)
        {
            self.error = Some(tr!("Enregistrement impossible : {e}", "Could not save: {e}"));
        }
    }
}

impl App {
    /// Contenu d'une fenêtre : discussion (sujet, lignes, pseudos, saisie) ou transferts.
    fn window(&mut self, ui: &mut egui::Ui, st: &mut State, focus: &Focus, pal: &Palette) {
        let Focus::Buffer(id, key) = focus else {
            egui::Frame::new()
                .inner_margin(8.0)
                .show(ui, |ui| transfers::ui(ui, st, &mut self.transfers, &self.core));
            return;
        };
        let font = Self::font_id(ui.ctx(), &st.config);
        // Lu avant la saisie, qui traiterait sinon aussi ces touches.
        let typing = self.inputs.get(focus).is_some_and(|entry| !entry.text.is_empty());
        let scroll = ui.input_mut(|input| scroll_keys(input, typing));
        // Fenêtre lue : son activité aussi. Une fenêtre séparée sans le focus la garde.
        if self.displayed(focus)
            && let Some(buffer) = buffer_mut(st, focus)
        {
            buffer.activity = Activity::None;
        }
        let Some(server) = st.server(*id) else { return };
        let Some(buffer) = server.buffer(key) else { return };
        let colors = !st.config.display.strip_colors;
        if buffer.kind == BufKind::Channel {
            egui::Panel::top("sujet").show(ui, |ui| {
                let mut job = LayoutJob::default();
                let base = TextFormat {
                    font_id: font.clone(),
                    color: pal.text,
                    ..Default::default()
                };
                format::append(&mut job, &buffer.topic, &base, colors, ui.visuals().panel_fill);
                job.wrap.max_width = ui.available_width();
                job.wrap.max_rows = 2;
                ui.add(Label::new(job).selectable(true));
            });
        }
        egui::Panel::bottom("saisie").show(ui, |ui| self.input(ui, focus, *id, buffer, &font));
        if buffer.kind == BufKind::Channel {
            egui::Panel::right("pseudos")
                .resizable(true)
                .default_size(160.0)
                .show(ui, |ui| self.nicklist(ui, *id, buffer, &font, pal));
        }
        let layout = Layout {
            width: 0,
            font_size: font.size.to_bits(),
            pixels_per_point: ui.ctx().pixels_per_point().to_bits(),
            theme: st.config.display.theme,
            colors,
            chantypes: server.chantypes.clone(),
        };
        let scrolled_back = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(pal.background).inner_margin(4.0))
            .show(ui, |ui| self.lines(ui, focus, *id, buffer, layout, &font, pal, scroll))
            .inner;
        // Vue de la fenêtre active (la même fenêtre peut être affichée deux fois).
        if self.active() == (ui.ctx().viewport_id(), focus) {
            st.scrolled_back = scrolled_back;
        }
    }

    /// Lignes de la discussion ; vrai si l'historique est remonté (le bas n'est pas affiché).
    #[allow(clippy::too_many_arguments)]
    fn lines(
        &mut self,
        ui: &mut egui::Ui,
        focus: &Focus,
        server: crate::state::ServerId,
        buffer: &Buffer,
        mut layout: Layout,
        font: &FontId,
        pal: &Palette,
        scroll: Option<Scroll>,
    ) -> bool {
        let width = (ui.available_width() - ui.spacing().scroll.bar_width - 4.0).max(50.0);
        layout.width = width as u32;
        let cache = self.caches.entry((ui.ctx().viewport_id(), focus.clone())).or_default();
        if cache.layout.as_ref() != Some(&layout) {
            cache.rows.clear();
            cache.layout = Some(layout.clone());
        }
        // Lignes supprimées (historique limité, /clear), puis nouvelles lignes.
        let first = buffer.lines.front().map(|l| l.id);
        while cache.rows.front().is_some_and(|r| first.is_none_or(|f| r.id < f)) {
            cache.rows.pop_front();
        }
        let last = cache.rows.back().map(|r| r.id);
        let start = buffer.lines.partition_point(|l| last.is_some_and(|x| l.id <= x));
        for line in buffer.lines.range(start..) {
            cache
                .rows
                .push_back(layout_row(ui.ctx(), line, &layout, width, font, pal));
        }

        let mut clicked = None;
        // Trait sous la dernière ligne lue, s'il y en a de nouvelles, dans un petit espace.
        let marker = buffer
            .read_marker
            .filter(|m| cache.rows.back().is_some_and(|r| r.id != *m));
        // Position propre à chaque fenêtre : on la retrouve en y revenant (tout en bas, la vue
        // suit les nouvelles lignes).
        let output = ScrollArea::vertical()
            .id_salt(focus)
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show_viewport(ui, |ui, viewport| {
                let total: f32 = cache.rows.iter().map(|r| span(r, marker)).sum();
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), total), Sense::click_and_drag());
                let origin = rect.min;
                let galley = |row: &Row| ui.fonts_mut(|f| f.layout_job(row.job.clone()));

                // Défilement au clavier (un décalage positif remonte).
                if let Some(scroll) = scroll {
                    let line = ui.fonts_mut(|f| f.row_height(font));
                    let delta = match scroll {
                        Scroll::Pages(n) => Some(n * (viewport.height() - line).max(line)),
                        Scroll::Lines(n) => Some(n * line),
                        Scroll::Top => Some(total),
                        Scroll::Bottom => Some(-total),
                        // Le trait en haut de la vue, sous deux lignes déjà lues.
                        Scroll::Marker => marker.map(|id| {
                            let below: f32 = cache
                                .rows
                                .iter()
                                .take_while(|r| r.id != id)
                                .map(|r| span(r, marker))
                                .sum();
                            viewport.min.y - (below - line).max(0.0)
                        }),
                    };
                    if let Some(delta) = delta {
                        ui.scroll_with_delta(vec2(0.0, delta));
                    }
                }

                // Sélection à la souris : le texte passe en inversé et est copié au relâchement.
                if response.drag_started()
                    && let Some(start) = ui.input(|i| i.pointer.press_origin())
                {
                    let at = text_pos(&cache.rows, marker, start - origin, galley);
                    cache.selection = Some((at, at));
                }
                if response.dragged()
                    && let Some((_, head)) = &mut cache.selection
                    && let Some(pos) = response.interact_pointer_pos()
                {
                    *head = text_pos(&cache.rows, marker, pos - origin, galley);
                    // Pointeur au-dessus ou au-dessous de la vue : défilement.
                    let clip = ui.clip_rect();
                    let outside = if pos.y < clip.top() {
                        pos.y - clip.top()
                    } else {
                        (pos.y - clip.bottom()).max(0.0)
                    };
                    if outside != 0.0 {
                        ui.scroll_with_delta(vec2(0.0, -outside.clamp(-40.0, 40.0)));
                        ui.ctx().request_repaint();
                    }
                }
                if response.drag_stopped()
                    && let Some((a, b)) = cache.selection.take()
                {
                    let text = selected_text(&cache.rows, a.min(b), a.max(b));
                    if !text.is_empty() {
                        ui.ctx().copy_text(text);
                    }
                }
                let selection = cache.selection.map(|(a, b)| (a.min(b), a.max(b)));

                let painter = ui.painter();
                let mut y = 0.0;
                for row in &cache.rows {
                    if y > viewport.max.y {
                        break;
                    }
                    if y + row.height >= viewport.min.y {
                        let pos = origin + vec2(0.0, y);
                        let galley = galley(row);
                        // Gras : le texte redessiné un pixel (physique) plus à droite.
                        let bold_at = pos + vec2(1.0 / ui.ctx().pixels_per_point(), 0.0);
                        let layout = |job: &LayoutJob| ui.fonts_mut(|f| f.layout_job(job.clone()));
                        painter.galley(pos, galley.clone(), pal.text);
                        if let Some(job) = &row.bold {
                            painter.galley(bold_at, layout(job), pal.text);
                        }
                        if let Some(range) = selection.and_then(|s| row_selection(row, s)) {
                            // Gras en inversé : sa copie avec la couleur du fond, sauf le texte
                            // masqué (transparent), qui doit le rester.
                            let bold = row.bold.as_ref().map(|job| {
                                let mut job = job.clone();
                                for section in &mut job.sections {
                                    if section.format.color != Color32::TRANSPARENT {
                                        section.format.color = pal.background;
                                    }
                                }
                                layout(&job)
                            });
                            for r in selection_rects(&galley, range) {
                                let r = r.translate(pos.to_vec2());
                                painter.rect_filled(r, 0.0, pal.text);
                                let painter = painter.with_clip_rect(r.intersect(painter.clip_rect()));
                                painter.galley_with_override_text_color(pos, galley.clone(), pal.background);
                                if let Some(bold) = &bold {
                                    painter.galley(bold_at, bold.clone(), pal.background);
                                }
                            }
                        }
                        if cache.selection.is_none()
                            && let Some(hover) = response.hover_pos()
                            && (y..y + row.height).contains(&(hover.y - origin.y))
                            && let Some(link) = link_at(row, &galley, hover - pos)
                        {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            if response.clicked() {
                                clicked = Some(link);
                            }
                        }
                        if marker == Some(row.id) {
                            // Centré entre le texte des deux lignes : le blanc au-dessus des
                            // majuscules (~3/16 de la hauteur) dépasse celui sous les jambages
                            // (~1/16), d'où le décalage vers le bas.
                            let y = pos.y + row.height + (MARKER_GAP + font.size / 8.0) / 2.0;
                            painter.hline(rect.x_range(), y, egui::Stroke::new(1.5, pal.marker));
                        }
                    }
                    y += span(row, marker);
                }
            });
        match clicked {
            Some(Link::Url(url)) => open_url(&url),
            Some(Link::Channel(channel)) => self.send(Command::Input {
                server,
                buffer: buffer.key.clone(),
                text: format!("/join {channel}"),
            }),
            None => {}
        }
        output.state.clamped_offset().y + output.inner_rect.height() < output.content_size.y - 2.0
    }

    fn nicklist(
        &mut self,
        ui: &mut egui::Ui,
        server: crate::state::ServerId,
        buffer: &Buffer,
        font: &FontId,
        pal: &Palette,
    ) {
        let count = buffer.users.len();
        let plural = if crate::i18n::french() { count > 1 } else { count != 1 };
        let s = if plural { "s" } else { "" };
        ui.label(tr!("{count} utilisateur{s}", "{count} user{s}"));
        let row_height = ui.ctx().fonts_mut(|f| f.row_height(font));
        let command = |text: String| Command::Input {
            server,
            buffer: buffer.key.clone(),
            text,
        };
        let mut commands = Vec::new();
        ScrollArea::vertical()
            .auto_shrink(false)
            .show_rows(ui, row_height, buffer.users.len(), |ui, range| {
                for user in &buffer.users[range] {
                    let prefix = user.prefixes.chars().next().unwrap_or(' ');
                    let text = RichText::new(format!("{prefix}{}", user.nick))
                        .font(font.clone())
                        .color(pal.text);
                    let response = ui.add(Label::new(text).selectable(false).sense(Sense::click()));
                    let nick = &user.nick;
                    if response.double_clicked() {
                        commands.push(command(format!("/query {nick}")));
                    }
                    response.context_menu(|ui| {
                        let items = [
                            (tr!("Conversation privée", "Private chat"), format!("/query {nick}")),
                            ("Whois".into(), format!("/whois {nick}")),
                            ("Version".into(), format!("/version {nick}")),
                            (tr!("Liste XDCC", "XDCC list"), format!("/msg {nick} xdcc list")),
                            (String::new(), String::new()),
                            ("Op".into(), format!("/op {nick}")),
                            ("Deop".into(), format!("/deop {nick}")),
                            ("Voice".into(), format!("/voice {nick}")),
                            ("Devoice".into(), format!("/devoice {nick}")),
                            (tr!("Expulser", "Kick"), format!("/kick {nick}")),
                        ];
                        for (label, text) in items {
                            if label.is_empty() {
                                ui.separator();
                            } else if ui.button(label).clicked() {
                                commands.push(command(text));
                            }
                        }
                    });
                }
            });
        for command in commands {
            self.send(command);
        }
    }

    fn input(
        &mut self,
        ui: &mut egui::Ui,
        focus: &Focus,
        server: crate::state::ServerId,
        buffer: &Buffer,
        font: &FontId,
    ) {
        let ctx = ui.ctx().clone();
        // Une par vue : le texte en cours est commun, le curseur propre à chacune.
        let id = Id::unique(("saisie", ctx.viewport_id(), focus));
        let entry = self.inputs.entry(focus.clone()).or_default();
        let (mut insert, mut complete, mut up, mut down, mut literal) = (None, false, false, false, false);
        if ctx.memory(|m| m.has_focus(id)) {
            ctx.input_mut(|i| {
                // Codes de mise en forme IRC.
                for (key, code) in [
                    (Key::K, '\x03'),
                    (Key::B, '\x02'),
                    (Key::U, '\x1f'),
                    (Key::I, '\x1d'),
                    (Key::O, '\x0f'),
                ] {
                    if consume_exact(i, Modifiers::CTRL, key) {
                        insert = Some(code);
                    }
                }
                complete = consume_exact(i, Modifiers::NONE, Key::Tab);
                up = consume_exact(i, Modifiers::NONE, Key::ArrowUp);
                down = consume_exact(i, Modifiers::NONE, Key::ArrowDown);
                // Ctrl+Entrée : envoyé comme texte, même s'il commence par « / ».
                literal = consume_exact(i, Modifiers::CTRL, Key::Enter);
            });
        }
        if let Some(code) = insert {
            let mut state = TextEditState::load(&ctx, id).unwrap_or_default();
            let chars = entry.text.chars().count();
            let index = state
                .cursor
                .char_range()
                .map(|r| r.primary.index.0.min(chars))
                .unwrap_or(chars);
            let byte = entry
                .text
                .char_indices()
                .nth(index)
                .map(|(b, _)| b)
                .unwrap_or(entry.text.len());
            entry.text.insert(byte, code);
            state
                .cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(index + 1))));
            state.store(&ctx, id);
        }
        if up || down {
            let len = entry.history.len();
            entry.history_pos = match (entry.history_pos, up) {
                (None, true) if len > 0 => Some(len - 1),
                (Some(p), true) => Some(p.saturating_sub(1)),
                (Some(p), false) if p + 1 < len => Some(p + 1),
                _ => None,
            };
            entry.text = entry.history_pos.map(|p| entry.history[p].clone()).unwrap_or_default();
            cursor_to_end(&ctx, id, &entry.text);
        }
        if complete {
            complete_nick(entry, buffer);
            cursor_to_end(&ctx, id, &entry.text);
        }
        let response = ui.add(
            TextEdit::singleline(&mut entry.text)
                .id(id)
                .font(font.clone())
                .desired_width(f32::INFINITY)
                .lock_focus(true),
        );
        if literal || response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            let text = std::mem::take(&mut entry.text);
            if !text.trim().is_empty() {
                // « //texte » : envoyé tel quel (« /texte ») au lieu d'être une commande.
                let sent = if literal && text.starts_with('/') {
                    format!("/{text}")
                } else {
                    text.clone()
                };
                if entry.history.last() != Some(&text) {
                    entry.history.push(text);
                    if entry.history.len() > 100 {
                        entry.history.remove(0);
                    }
                }
                entry.history_pos = None;
                let _ = self.core.send(CoreMsg::Ui(Command::Input {
                    server,
                    buffer: buffer.key.clone(),
                    text: sent,
                }));
            }
            response.request_focus();
        }
        // La saisie reprend le focus dès que rien d'autre ne l'a.
        if ctx.memory(|m| m.focused().is_none()) {
            response.request_focus();
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context, st: &mut State) {
        if let Some(options) = &mut self.options {
            let outcome = options.show(ctx);
            if let Some(config) = outcome.config {
                st.config = config;
                crate::i18n::set(st.config.display.language);
                if let Err(e) = st.config.save() {
                    self.error = Some(tr!("Enregistrement impossible : {e}", "Could not save: {e}"));
                }
                self.send(Command::ConfigChanged);
            }
            if outcome.close {
                self.options = None;
            }
        }

        egui::Window::new(tr!("Commandes", "Commands"))
            .id(Dialog::Help.id())
            .collapsible(false)
            .open(&mut self.help)
            .show(ctx, |ui| {
                ScrollArea::both().show(ui, |ui| ui.monospace(crate::core::help()));
            });

        if let Some(addr) = &mut self.quick_connect {
            let (mut open, mut connect) = (true, false);
            egui::Window::new(tr!("Autre serveur", "Other server"))
                .id(Dialog::QuickConnect.id())
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    egui::Grid::new("serveur").num_columns(2).show(ui, |ui| {
                        ui.label(tr!("Hôte", "Host"));
                        let host = ui.text_edit_singleline(&mut addr.host);
                        if host.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            connect = true;
                        }
                        ui.end_row();
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut addr.port).range(1..=65535));
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(&mut addr.tls, "TLS (SSL)");
                        ui.end_row();
                        ui.label("");
                        ui.checkbox(
                            &mut addr.accept_invalid_cert,
                            tr!("Accepter un certificat non valide", "Accept invalid certificate"),
                        );
                        ui.end_row();
                    });
                    connect |= ui.button(tr!("Se connecter", "Connect")).clicked();
                });
            if connect && !addr.host.trim().is_empty() {
                let mut addr = addr.clone();
                addr.host = addr.host.trim().to_owned();
                self.send(Command::Connect(addr));
                open = false;
            }
            if !open {
                self.quick_connect = None;
            }
        }

        if let Some(error) = &self.error {
            let mut open = true;
            egui::Window::new(tr!("Erreur", "Error"))
                .id(Dialog::Error.id())
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(error);
                });
            if !open {
                self.error = None;
            }
        }
    }
}

fn layout_row(
    ctx: &egui::Context,
    line: &crate::state::Line,
    layout: &Layout,
    width: f32,
    font: &FontId,
    pal: &Palette,
) -> Row {
    let color = if line.highlight {
        pal.highlight
    } else {
        pal.line(line.kind)
    };
    let base = TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    if !line.time.is_empty() {
        let time = TextFormat {
            color: pal.timestamp,
            ..base.clone()
        };
        job.append(&line.time, 0.0, time);
        job.append(" ", 0.0, base.clone());
    }
    let start = job.text.len();
    let bold = format::append(&mut job, &line.text, &base, layout.colors, pal.background);
    let links: Vec<(Range<usize>, Link)> = format::links(&job.text[start..], &layout.chantypes)
        .into_iter()
        .map(|(r, link)| (r.start + start..r.end + start, link))
        .collect();
    let ranges: Vec<Range<usize>> = links.iter().map(|(r, _)| r.clone()).collect();
    format::underline(&mut job, &ranges);
    let height = ctx.fonts_mut(|f| f.layout_job(job.clone())).size().y;
    Row {
        id: line.id,
        bold: format::bold_job(&job, &bold),
        job,
        height,
        links,
    }
}

/// Ouvre une adresse web dans le navigateur par défaut, comme un lien cliqué ailleurs dans
/// Windows (le navigateur déjà ouvert reçoit un nouvel onglet et passe au premier plan).
fn open_url(url: &str) {
    #[cfg(windows)]
    crate::explorer::open_url(url);
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Lien à la position donnée (relative à la ligne).
fn link_at(row: &Row, galley: &Galley, pos: egui::Vec2) -> Option<Link> {
    if row.links.is_empty() {
        return None;
    }
    let cursor = galley.cursor_from_pos(pos);
    let byte = row.job.text.char_indices().nth(cursor.index.0).map(|(b, _)| b)?;
    row.links
        .iter()
        .find(|(r, _)| r.contains(&byte))
        .map(|(_, l)| l.clone())
}

/// Caractère à la position donnée (relative au haut des lignes) ; au-dessus ou au-dessous de
/// toutes les lignes : leur début ou leur fin.
fn text_pos(
    rows: &VecDeque<Row>,
    marker: Option<u64>,
    pos: egui::Vec2,
    galley: impl Fn(&Row) -> Arc<Galley>,
) -> TextPos {
    let mut y = 0.0;
    for row in rows {
        if pos.y < y {
            return (row.id, 0);
        }
        if pos.y < y + row.height {
            return (row.id, galley(row).cursor_from_pos(pos - vec2(0.0, y)).index.0);
        }
        y += span(row, marker);
    }
    rows.back().map_or((0, 0), |row| (row.id, row.job.text.chars().count()))
}

/// Défilement demandé au clavier : pages ou lignes (positif : vers le haut), tout en haut, tout
/// en bas, jusqu'au trait des lignes lues.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Scroll {
    Pages(f32),
    Lines(f32),
    Top,
    Bottom,
    Marker,
}

/// Page préc. / Page suiv. : une page ; Ctrl+↑ / Ctrl+↓ : une ligne ; Ctrl+L : jusqu'au trait des
/// lignes lues ; Ctrl+Début / Ctrl+Fin : tout en haut / tout en bas (la vue suit alors de nouveau
/// les nouvelles lignes), seulement si la saisie est vide (`typing` : elle ne l'est pas) : ils y
/// déplacent sinon le curseur.
fn scroll_keys(input: &mut egui::InputState, typing: bool) -> Option<Scroll> {
    [
        (Modifiers::NONE, Key::PageUp, Scroll::Pages(1.0)),
        (Modifiers::NONE, Key::PageDown, Scroll::Pages(-1.0)),
        (Modifiers::CTRL, Key::ArrowUp, Scroll::Lines(1.0)),
        (Modifiers::CTRL, Key::ArrowDown, Scroll::Lines(-1.0)),
        (Modifiers::CTRL, Key::L, Scroll::Marker),
        (Modifiers::CTRL, Key::Home, Scroll::Top),
        (Modifiers::CTRL, Key::End, Scroll::Bottom),
    ]
    .into_iter()
    .filter(|(_, _, scroll)| !typing || !matches!(scroll, Scroll::Top | Scroll::Bottom))
    .find(|(modifiers, key, _)| consume_exact(input, *modifiers, *key))
    .map(|(.., scroll)| scroll)
}

/// Consomme un raccourci seulement s'il est pressé avec exactement ces touches de modification :
/// egui ignore sinon Shift et Alt en plus, et Ctrl+Shift+Fin (sélection jusqu'à la fin) serait
/// pris pour Ctrl+Fin.
fn consume_exact(input: &mut egui::InputState, modifiers: Modifiers, key: Key) -> bool {
    let exact = input.events.iter().any(|event| {
        matches!(event, egui::Event::Key { key: k, pressed: true, modifiers: m, .. }
            if *k == key && m.shift == modifiers.shift && m.alt == modifiers.alt)
    });
    exact && input.consume_key(modifiers, key)
}

/// Espace où passe le trait des lignes lues (assez pour rester loin d'un mot souligné).
const MARKER_GAP: f32 = 14.0;

/// Boîtes de dialogue fermées par Échap et Alt+F4.
#[derive(Clone, Copy, PartialEq)]
enum Dialog {
    Options,
    Help,
    QuickConnect,
    Error,
}

impl Dialog {
    const ALL: [Dialog; 4] = [Dialog::Options, Dialog::Help, Dialog::QuickConnect, Dialog::Error];

    fn id(self) -> Id {
        match self {
            Dialog::Options => Id::unique(crate::options::WINDOW_ID),
            Dialog::Help => Id::unique("aide"),
            Dialog::QuickConnect => Id::unique("autre serveur"),
            Dialog::Error => Id::unique("erreur"),
        }
    }

    fn layer(self) -> egui::LayerId {
        egui::LayerId::new(egui::Order::Middle, self.id())
    }
}

/// Fenêtres retenues pour Alt+← / Alt+→.
const HISTORY_LEN: usize = 100;

/// Hauteur occupée par une ligne : la sienne, plus l'espace du trait s'il passe dessous.
fn span(row: &Row, marker: Option<u64>) -> f32 {
    row.height + if marker == Some(row.id) { MARKER_GAP } else { 0.0 }
}

/// Caractères sélectionnés d'une ligne, pour la sélection `(début, fin)`.
fn row_selection(row: &Row, (start, end): (TextPos, TextPos)) -> Option<Range<usize>> {
    if row.id < start.0 || row.id > end.0 {
        return None;
    }
    let from = if row.id == start.0 { start.1 } else { 0 };
    let to = if row.id == end.0 {
        end.1
    } else {
        row.job.text.chars().count()
    };
    (from < to).then_some(from..to)
}

/// Texte sélectionné, une ligne par ligne d'historique.
fn selected_text(rows: &VecDeque<Row>, start: TextPos, end: TextPos) -> String {
    let newline = if cfg!(windows) { "\r\n" } else { "\n" };
    let mut parts = Vec::new();
    for row in rows {
        if let Some(range) = row_selection(row, (start, end)) {
            parts.push(
                row.job
                    .text
                    .chars()
                    .skip(range.start)
                    .take(range.len())
                    .collect::<String>(),
            );
        } else if row.id > start.0 && row.id < end.0 {
            parts.push(String::new());
        }
    }
    parts.join(newline)
}

/// Rectangles (relatifs à la ligne) couverts par les caractères `range`, une ligne à l'écran
/// pouvant être coupée en plusieurs.
fn selection_rects(galley: &Galley, range: Range<usize>) -> Vec<egui::Rect> {
    let start = galley.pos_from_cursor(CCursor::new(range.start));
    let end = galley.pos_from_cursor(CCursor::new(range.end));
    galley
        .rows
        .iter()
        .filter_map(|placed| {
            let r = placed.rect();
            if r.max.y <= start.center().y || r.min.y > end.center().y {
                return None;
            }
            let left = if start.center().y >= r.min.y {
                start.min.x
            } else {
                r.min.x
            };
            let right = if end.center().y < r.max.y { end.min.x } else { r.max.x };
            (right > left).then(|| egui::Rect::from_min_max(egui::pos2(left, r.min.y), egui::pos2(right, r.max.y)))
        })
        .collect()
}

fn cursor_to_end(ctx: &egui::Context, id: Id, text: &str) {
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    let end = CCursor::new(text.chars().count());
    state.cursor.set_char_range(Some(CCursorRange::one(end)));
    state.store(ctx, id);
}

/// Complète le dernier mot avec un pseudo du canal (ou le nom de la fenêtre).
fn complete_nick(entry: &mut Input, buffer: &Buffer) {
    if entry.completion.as_ref().is_none_or(|c| c.result != entry.text) {
        let start = entry.text.rfind(' ').map(|i| i + 1).unwrap_or(0);
        entry.completion = Some(Completion {
            start,
            prefix: entry.text[start..].to_owned(),
            next: 0,
            result: String::new(),
        });
    }
    let completion = entry.completion.as_mut().expect("complétion");
    let prefix = crate::irc::lower(&completion.prefix);
    let mut candidates: Vec<&str> = buffer
        .users
        .iter()
        .map(|u| u.nick.as_str())
        .filter(|n| crate::irc::lower(n).starts_with(&prefix))
        .collect();
    if buffer.kind != BufKind::Status && crate::irc::lower(&buffer.name).starts_with(&prefix) {
        candidates.push(&buffer.name);
    }
    if candidates.is_empty() {
        return;
    }
    let nick = candidates[completion.next % candidates.len()];
    completion.next += 1;
    let suffix = if completion.start == 0 { ": " } else { " " };
    entry.text = format!("{}{nick}{suffix}", &entry.text[..completion.start]);
    completion.result = entry.text.clone();
}

fn new_favorite(server: &crate::state::Server, channel: &str) -> Favorite {
    // Réseau configuré : ses serveurs à tour de rôle ; sinon, ce serveur-là.
    let addr = if server.network.is_some() {
        ServerAddr::default()
    } else {
        server.addr.clone()
    };
    Favorite {
        channel: channel.to_owned(),
        key: String::new(),
        network: server.network.clone().unwrap_or_else(|| server.label.clone()),
        server: addr,
        auto_join: true,
    }
}

fn add_favorite(st: &mut State, favorite: Favorite) -> Result<(), String> {
    let exists = st.config.favorites.iter().any(|f| {
        crate::irc::eq(&f.channel, &favorite.channel)
            && f.network.eq_ignore_ascii_case(&favorite.network)
            && f.server.host.eq_ignore_ascii_case(&favorite.server.host)
    });
    if !exists {
        st.config.favorites.push(favorite);
        return st.config.save();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;
    use std::time::Instant;

    use super::*;
    use crate::dcc::Offer;
    use crate::state::{Line, QueueItem, QueueStatus, Server, Transfer, TransferStatus, User};

    /// Sélection sur plusieurs lignes : caractères retenus, texte copié, zones inversées.
    #[test]
    fn selection() {
        let ctx = egui::Context::default();
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        let row = |id, text: &str| {
            let job = LayoutJob::single_section(text.into(), TextFormat::default());
            let height = ctx.fonts_mut(|f| f.layout_job(job.clone())).size().y;
            Row {
                id,
                job,
                bold: None,
                height,
                links: Vec::new(),
            }
        };
        let rows = VecDeque::from([row(1, "abc"), row(2, "defg"), row(3, "hi")]);
        assert_eq!(row_selection(&rows[0], ((1, 1), (2, 2))), Some(1..3));
        assert_eq!(row_selection(&rows[1], ((1, 1), (2, 2))), Some(0..2));
        assert_eq!(row_selection(&rows[2], ((1, 1), (2, 2))), None);
        let newline = if cfg!(windows) { "\r\n" } else { "\n" };
        assert_eq!(
            selected_text(&rows, (1, 1), (3, 1)),
            format!("bc{newline}defg{newline}h")
        );
        let galley = ctx.fonts_mut(|f| f.layout_job(rows[1].job.clone()));
        let rects = selection_rects(&galley, 1..3);
        assert_eq!(rects.len(), 1);
        assert!(rects[0].width() > 0.0 && rects[0].min.x > 0.0);
        // Sous la dernière ligne : fin du texte.
        let galley = |r: &Row| ctx.fonts_mut(|f| f.layout_job(r.job.clone()));
        let below = text_pos(&rows, None, vec2(0.0, 1000.0), galley);
        assert_eq!(below, (3, 2));
        // Dans l'espace du trait des lignes lues (sous la 1ʳᵉ) : début de la ligne suivante.
        let gap = rows[0].height + MARKER_GAP / 2.0;
        assert_eq!(text_pos(&rows, Some(1), vec2(0.0, gap), galley), (2, 0));
    }

    /// Touches de défilement ; Ctrl+Page (changement de fenêtre) n'en est pas une.
    #[test]
    fn scroll_shortcuts() {
        let ctx = egui::Context::default();
        let press = |key, modifiers: Modifiers, typing| {
            let event = egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            };
            let input = egui::RawInput {
                events: vec![event],
                ..Default::default()
            };
            let mut scroll = None;
            ctx.run_ui(input, |ui| scroll = ui.input_mut(|i| scroll_keys(i, typing)))
                .textures_delta
                .clear();
            scroll
        };
        let ctrl_shift = Modifiers {
            shift: true,
            ..Modifiers::CTRL
        };
        assert_eq!(press(Key::PageUp, Modifiers::NONE, false), Some(Scroll::Pages(1.0)));
        assert_eq!(press(Key::PageDown, Modifiers::NONE, true), Some(Scroll::Pages(-1.0)));
        assert_eq!(press(Key::ArrowUp, Modifiers::CTRL, true), Some(Scroll::Lines(1.0)));
        assert_eq!(press(Key::End, Modifiers::CTRL, false), Some(Scroll::Bottom));
        assert_eq!(press(Key::L, Modifiers::CTRL, true), Some(Scroll::Marker));
        // Texte en cours de saisie : Ctrl+Début / Ctrl+Fin y déplacent le curseur.
        assert_eq!(press(Key::End, Modifiers::CTRL, true), None);
        // Sélection (Shift), changement de fenêtre (Ctrl+Page), saisie (↑, Début) : pas de défilement.
        assert_eq!(press(Key::End, ctrl_shift, false), None);
        assert_eq!(press(Key::PageUp, Modifiers::SHIFT, false), None);
        assert_eq!(press(Key::PageUp, Modifiers::CTRL, false), None);
        assert_eq!(press(Key::ArrowUp, Modifiers::NONE, false), None);
        assert_eq!(press(Key::Home, Modifiers::NONE, false), None);
    }

    /// Ctrl+Tab / Ctrl+Page suivante, Ctrl+Shift+Tab / Ctrl+Page précédente : fenêtre suivante /
    /// précédente dans l'arborescence, celles ouvertes à part comprises.
    #[test]
    fn window_shortcuts() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, (Config::default(), false, None));
        let mut st = State::new(Config::default());
        let addr = ServerAddr::new("irc.example.net", 6697, true);
        let mut server = Server::new(7, "Test".into(), None, addr, "moi".into());
        server.open_buffer("#a");
        server.open_buffer("#b");
        st.servers.push(server);
        let press = |app: &mut App, key, modifiers| {
            let event = egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            };
            let input = egui::RawInput {
                events: vec![event],
                ..Default::default()
            };
            ctx.run_ui(input, |ui| app.switch_keys(ui.ctx(), &st))
                .textures_delta
                .clear();
            app.selected.clone()
        };
        // Arborescence : Test, #a, #b, puis les transferts.
        let (test, a, b) = (
            Focus::Buffer(7, String::new()),
            Focus::Buffer(7, "#a".into()),
            Focus::Buffer(7, "#b".into()),
        );
        app.selected = Focus::Transfers;
        assert_eq!(press(&mut app, Key::Tab, Modifiers::CTRL), test);
        assert_eq!(press(&mut app, Key::PageDown, Modifiers::CTRL), a);
        assert_eq!(press(&mut app, Key::Tab, Modifiers::CTRL | Modifiers::SHIFT), test);
        assert_eq!(press(&mut app, Key::PageUp, Modifiers::CTRL), Focus::Transfers);
        // #a ouverte à part : affichée aussi dans la fenêtre principale.
        app.detached.push((a.clone(), viewport_id(&a)));
        app.selected = test;
        assert_eq!(press(&mut app, Key::Tab, Modifiers::CTRL), a);
        assert_eq!(press(&mut app, Key::Tab, Modifiers::CTRL), b);
    }

    /// Conversation privée renommée (le correspondant change de pseudo) : elle reste affichée et
    /// ouverte à part, dans la même fenêtre native, avec sa saisie en cours et son historique.
    #[test]
    fn rename_keeps_window() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, (Config::default(), false, None));
        let (old, new) = (Focus::Buffer(7, "bob".into()), Focus::Buffer(7, "robert".into()));
        app.selected = old.clone();
        app.history = vec![Focus::Transfers, old.clone()];
        app.detached.push((old.clone(), viewport_id(&old)));
        app.inputs.entry(old.clone()).or_default().text = "salut".into();
        app.rename(&old, &new);
        assert_eq!(app.selected, new);
        assert_eq!(app.history, [Focus::Transfers, new.clone()]);
        assert!(app.detached == [(new.clone(), viewport_id(&old))]);
        assert_eq!(app.inputs[&new].text, "salut");
    }

    /// Trait des lignes lues : posé en quittant une fenêtre, y compris une fenêtre séparée qui
    /// perd le focus (elle n'est alors plus lue ni active) ; retiré en y revenant sans rien de
    /// nouveau.
    #[test]
    fn read_marker() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, (Config::default(), false, None));
        let mut st = State::new(Config::default());
        let addr = ServerAddr::new("irc.example.net", 6697, true);
        let mut server = Server::new(7, "Test".into(), None, addr, "moi".into());
        for key in ["#a", "#b"] {
            let buffer = server.open_buffer(key);
            buffer.push(
                Line::new(String::new(), LineKind::Normal, "x".into()),
                100,
                Activity::Message,
            );
        }
        st.servers.push(server);
        let (a, b) = (Focus::Buffer(7, "#a".into()), Focus::Buffer(7, "#b".into()));
        let marker = |st: &State, key| st.servers[0].buffer(key).unwrap().read_marker;
        let last = |st: &State, key| st.servers[0].buffer(key).unwrap().lines.back().map(|l| l.id);
        app.selected = a.clone();
        app.detached.push((b.clone(), viewport_id(&b)));
        app.focused_detached = Some((b.clone(), viewport_id(&b)));
        app.mark_read(&mut st);
        assert!(app.displayed(&b) && app.active() == (viewport_id(&b), &b));
        app.focused_detached = None;
        app.mark_read(&mut st);
        assert!(!app.displayed(&b) && app.active() == (ViewportId::ROOT, &a));
        assert_eq!(marker(&st, "#b"), last(&st, "#b"));
        app.focused_detached = Some((b.clone(), viewport_id(&b)));
        app.mark_read(&mut st);
        assert_eq!(marker(&st, "#b"), None);
        app.selected = b;
        app.mark_read(&mut st);
        assert_eq!(marker(&st, "#a"), last(&st, "#a"));
    }

    /// F1 : aide ; Alt+O : options ; Échap et Alt+F4 ferment la dernière ouverte, Alt+F4 ne
    /// fermant le logiciel qu'une fois toutes fermées.
    #[test]
    fn dialog_shortcuts() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, (Config::default(), false, None));
        let st = State::new(Config::default());
        // Vrai si la fermeture du logiciel a été annulée.
        let run = |app: &mut App, events: Vec<egui::Event>, close: bool| {
            let mut input = egui::RawInput {
                events,
                ..Default::default()
            };
            if close {
                input.viewports.entry(egui::ViewportId::ROOT).or_default().events = vec![egui::ViewportEvent::Close];
            }
            let mut output = ctx.run_ui(input, |ui| app.dialog_keys(ui.ctx(), &st));
            output.textures_delta.clear();
            output
                .viewport_output
                .remove(&egui::ViewportId::ROOT)
                .is_some_and(|v| v.commands.contains(&ViewportCommand::CancelClose))
        };
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        run(&mut app, vec![key(Key::F1, Modifiers::NONE)], false);
        run(&mut app, vec![key(Key::O, Modifiers::ALT)], false);
        assert!(app.help && app.options.is_some());
        run(&mut app, vec![key(Key::Escape, Modifiers::NONE)], false);
        assert!(app.help && app.options.is_none());
        let alt_f4 = || {
            vec![
                egui::Event::ModifiersChanged(Modifiers::ALT),
                key(Key::F4, Modifiers::ALT),
            ]
        };
        assert!(run(&mut app, alt_f4(), true));
        assert!(!app.help);
        assert!(!run(&mut app, alt_f4(), true));
    }

    /// Alt+← / Alt+→ : fenêtres visitées ; une fenêtre fermée quitte l'historique.
    #[test]
    fn window_history() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, (Config::default(), false, None));
        let mut st = State::new(Config::default());
        let addr = ServerAddr::new("irc.example.net", 6697, true);
        let mut server = Server::new(7, "Test".into(), None, addr, "moi".into());
        server.open_buffer("#a");
        server.open_buffer("#b");
        st.servers.push(server);
        let (a, b) = (Focus::Buffer(7, "#a".into()), Focus::Buffer(7, "#b".into()));
        let transfers = Focus::Transfers;
        app.history = vec![transfers.clone(), a.clone(), b.clone(), a.clone()];
        app.history_pos = 2;
        app.selected = b.clone();
        assert_eq!(app.history_step(-1), Some(a.clone()));
        app.selected = a.clone();
        assert_eq!(app.history_step(-1), Some(transfers.clone()));
        app.selected = transfers.clone();
        assert_eq!(app.history_step(-1), None);
        // #a fermée : retirée, et les deux visites de #b qui l'entouraient n'en font plus qu'une.
        app.history = vec![b.clone(), a.clone(), b.clone(), transfers.clone()];
        app.history_pos = 1;
        app.selected = a;
        st.servers[0].buffers.retain(|buffer| buffer.key != "#a");
        app.prune_history(&st);
        assert_eq!(app.history, [b.clone(), transfers]);
        assert_eq!(app.history_pos, 0);
        assert_eq!(app.history_step(1), Some(Focus::Transfers));
        app.selected = Focus::Transfers;
        assert_eq!(app.history_step(-1), Some(b));
    }

    /// Fermeture de la fenêtre affichée : la précédente dans l'arborescence prend sa place, en
    /// sautant les canaux d'un serveur fermé.
    #[test]
    fn close_selects_previous() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, (Config::default(), false, None));
        let mut st = State::new(Config::default());
        for id in [7, 8] {
            let addr = ServerAddr::new("irc.example.net", 6697, true);
            let mut server = Server::new(id, format!("Réseau {id}"), None, addr, "moi".into());
            server.open_buffer("#a");
            server.open_buffer("#b");
            st.servers.push(server);
        }
        let focus = |id, key: &str| Focus::Buffer(id, key.into());
        app.selected = focus(7, "#b");
        app.close(&st, &focus(7, "#b"));
        assert_eq!(app.selected, focus(7, "#a"));
        app.selected = focus(8, "");
        app.close(&st, &focus(8, ""));
        assert_eq!(app.selected, focus(7, "#b"));
        // Premier serveur : rien avant lui, la fenêtre suivante hors de ses canaux.
        app.selected = focus(7, "");
        app.close(&st, &focus(7, ""));
        assert_eq!(app.selected, focus(8, ""));
    }

    /// Toute l'interface, sans fenêtre : fenêtres, onglets d'options, transferts, fenêtre détachée.
    #[test]
    fn headless_ui() {
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let mut app = App::new(&cc, (Config::default(), false, None));
        let mut frame = eframe::Frame::_new_kittest();
        {
            let mut st = lock(&app.state);
            let mut server = Server::new(
                7,
                "Test".into(),
                None,
                ServerAddr::new("irc.test", 6697, true),
                "moi".into(),
            );
            server.status = ConnStatus::Connected;
            let channel = server.open_buffer("#chan");
            channel.joined = true;
            channel.topic = "\x0304Sujet\x03 en \x1fcouleur\x1f https://example.com".into();
            channel.users = vec![
                User {
                    nick: "Bot".into(),
                    prefixes: "@".into(),
                },
                User {
                    nick: "moi".into(),
                    prefixes: String::new(),
                },
            ];
            for i in 0..300 {
                let text = format!("<@Bot> ligne {i} \x02gras\x02 \x0303,01vert\x03 #autre www.example.org <= =>");
                channel.push(
                    Line::new("[12:00]".into(), LineKind::Normal, text),
                    5000,
                    Activity::Message,
                );
            }
            server.open_buffer("Bot").push(
                Line::new(String::new(), LineKind::Action, "Bot salue".into()),
                5000,
                Activity::Highlight,
            );
            st.servers.push(server);
            let offer = Offer {
                filename: "f.bin".into(),
                ip: None,
                port: 1,
                size: Some(100),
                token: None,
            };
            for (id, status) in [
                (1, TransferStatus::AskAccept),
                (2, TransferStatus::AskExists),
                (3, TransferStatus::Receiving),
                (4, TransferStatus::Done),
                (5, TransferStatus::Failed("erreur".into())),
            ] {
                st.transfers.push(Transfer {
                    id,
                    server: 7,
                    nick: "Bot".into(),
                    offer: offer.clone(),
                    path: std::env::temp_dir().join("f.bin"),
                    offset: 0,
                    position: Arc::new(AtomicU64::new(40)),
                    status,
                    started: Some(Instant::now()),
                    ended: Some(Instant::now()),
                    queue_item: None,
                    resume_sent: None,
                    announced: None,
                    complete: false,
                });
            }
            for (id, status) in [
                (10, QueueStatus::Waiting),
                (11, QueueStatus::Requested),
                (12, QueueStatus::Done),
            ] {
                st.queue.push(QueueItem {
                    id,
                    server: 7,
                    bot: "Bot".into(),
                    pack: format!("#{id}"),
                    status,
                    since: Instant::now(),
                    retry_at: None,
                    attempts: 0,
                    asked: false,
                    filename: String::new(),
                    window: "#chan".into(),
                });
            }
        }
        let mut run = |app: &mut App| {
            for _ in 0..2 {
                let mut output = ctx.run_ui(egui::RawInput::default(), |ui| eframe::App::ui(app, ui, &mut frame));
                output.textures_delta.clear();
            }
        };
        for focus in [
            Focus::Buffer(7, String::new()),
            Focus::Buffer(7, "#chan".into()),
            Focus::Buffer(7, "bot".into()),
            Focus::Transfers,
        ] {
            app.selected = focus;
            run(&mut app);
        }
        assert!(lock(&app.state).servers[0].buffer("#chan").unwrap().activity == Activity::None);
        {
            let config = &mut lock(&app.state).config;
            let first = config.networks.first().map(|n| n.name.clone()).unwrap_or_default();
            for (network, host) in [(first, ""), (String::new(), "irc.example.org")] {
                config.favorites.push(Favorite {
                    channel: "#fav".into(),
                    network,
                    server: ServerAddr::new(host, 6697, true),
                    ..Default::default()
                });
            }
            config.display.font = FontVariant::Custom;
        }
        for tab in crate::options::TABS {
            app.options = Some(Options::new(&lock(&app.state).config, tab));
            run(&mut app);
        }
        app.options = None;
        let chan = Focus::Buffer(7, "#chan".into());
        app.detached.push((chan.clone(), viewport_id(&chan)));
        app.quick_connect = Some(ServerAddr::new("", 6697, true));
        app.error = Some("test".into());
        run(&mut app);
        let cache = app
            .caches
            .iter()
            .find(|((_, f), _)| *f == chan)
            .map(|(_, c)| c)
            .expect("#chan");
        assert_eq!(cache.rows.len(), 300);
        assert!(!cache.rows[0].links.is_empty());
    }
}
