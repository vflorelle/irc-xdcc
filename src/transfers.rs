//! Fenêtre des transferts (file XDCC, réceptions DCC) et demandes de confirmation.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Instant;

use eframe::egui::{self, Id, ProgressBar, RichText, ScrollArea};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::ExistsAction;
use crate::core::{Command, CoreMsg, QueueAction, TransferAction, human_size};
use crate::state::{QueueStatus, State, Transfer, TransferId, TransferStatus};
use crate::{dcc, fonts};

/// Débit mesuré de chaque transfert (moyenne glissante).
#[derive(Default)]
pub struct Speeds(HashMap<TransferId, Speed>);

struct Speed {
    at: Instant,
    position: u64,
    rate: f64,
}

impl Speeds {
    fn rate(&mut self, t: &Transfer) -> f64 {
        let position = t.position.load(Ordering::Relaxed);
        let now = Instant::now();
        let speed = self.0.entry(t.id).or_insert(Speed {
            at: now,
            position,
            rate: 0.0,
        });
        let elapsed = now.duration_since(speed.at).as_secs_f64();
        if elapsed >= 1.0 {
            let instant = position.saturating_sub(speed.position) as f64 / elapsed;
            speed.rate = if speed.rate == 0.0 {
                instant
            } else {
                speed.rate * 0.6 + instant * 0.4
            };
            speed.at = now;
            speed.position = position;
        }
        speed.rate
    }
}

fn send(core: &UnboundedSender<CoreMsg>, command: Command) {
    let _ = core.send(CoreMsg::Ui(command));
}

fn duration(secs: f64) -> String {
    let secs = secs as u64;
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min {:02} s", secs / 60, secs % 60),
        _ => format!("{} h {:02} min", secs / 3600, secs % 3600 / 60),
    }
}

pub fn ui(ui: &mut egui::Ui, st: &State, speeds: &mut Speeds, core: &UnboundedSender<CoreMsg>) {
    speeds.0.retain(|id, _| st.transfers.iter().any(|t| t.id == *id));
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.horizontal(|ui| {
            let finished =
                st.queue.iter().any(|q| q.status.finished()) || st.transfers.iter().any(|t| t.status.finished());
            if ui
                .add_enabled(
                    finished,
                    egui::Button::new(tr!("Retirer les terminés", "Remove finished")),
                )
                .on_hover_text(tr!(
                    "Packs et réceptions terminés, échoués ou annulés",
                    "Packs and receives that are done, failed or cancelled"
                ))
                .clicked()
            {
                send(core, Command::ClearFinished);
            }
            let dir = &st.config.dcc.download_dir;
            if ui
                .button(tr!("Ouvrir le dossier de téléchargement", "Open download folder"))
                .on_hover_text(dir.display().to_string())
                .clicked()
            {
                // Pas encore de téléchargement : le dossier n'existe peut-être pas.
                let _ = std::fs::create_dir_all(dir);
                open_dir(dir);
            }
        });
        ui.add_space(8.0);
        ui.heading(tr!("File d'attente XDCC", "XDCC queue"));
        if st.queue.is_empty() {
            ui.label(tr!(
                "Vide. Ajoutez des packs avec /xdcc bot #1 (plusieurs : #1,#3,#5-#8) ou /msg bot xdcc send \
                 #1. Toutes les commandes : Aide → Commandes.",
                "Empty. Add packs with /xdcc bot #1 (several: #1,#3,#5-#8) or /msg bot xdcc send #1. All \
                 commands: Help → Commands."
            ));
        } else {
            queue(ui, st, core);
        }
        ui.add_space(12.0);
        ui.heading(tr!("Réceptions DCC", "DCC downloads"));
        if st.transfers.is_empty() {
            ui.label(tr!("Aucune.", "None."));
        } else {
            transfers(ui, st, speeds, core);
        }
    });
}

fn queue(ui: &mut egui::Ui, st: &State, core: &UnboundedSender<CoreMsg>) {
    egui::Grid::new("file").striped(true).num_columns(5).show(ui, |ui| {
        for title in [
            "Bot".into(),
            "Pack".into(),
            tr!("Réseau", "Network"),
            tr!("Fichier", "File"),
            tr!("État", "Status"),
        ] {
            ui.label(RichText::new(title).strong());
        }
        ui.end_row();
        for item in &st.queue {
            let server = st.server(item.server).map(|s| s.label.as_str()).unwrap_or("?");
            ui.label(&item.bot);
            ui.label(&item.pack);
            ui.label(server);
            ui.label(&item.filename);
            let status = match &item.status {
                QueueStatus::Waiting => match item.retry_at.map(|t| t.saturating_duration_since(Instant::now())) {
                    Some(left) if !left.is_zero() => {
                        let left = duration(left.as_secs_f64());
                        tr!(
                            "En attente (nouvelle demande dans {left})",
                            "Waiting (new request in {left})"
                        )
                    }
                    _ => tr!("En attente", "Waiting"),
                },
                QueueStatus::Requested => {
                    let ago = duration(item.since.elapsed().as_secs_f64());
                    tr!("Demandé il y a {ago}", "Requested {ago} ago")
                }
                QueueStatus::BotQueue(position) if position.is_empty() => {
                    tr!("Dans la file du bot", "In the bot's queue")
                }
                QueueStatus::BotQueue(position) => {
                    tr!("Dans la file du bot ({position})", "In the bot's queue ({position})")
                }
                QueueStatus::Transferring(_) => tr!("Téléchargement", "Downloading"),
                QueueStatus::Done => tr!("Terminé", "Done"),
                QueueStatus::Failed(e) => tr!("Échec : {e}", "Failed: {e}"),
                QueueStatus::Cancelled => tr!("Annulé", "Cancelled"),
            };
            ui.horizontal(|ui| {
                ui.label(status);
                let action = |a| Command::Queue(item.id, a);
                if item.status.finished() {
                    if ui.small_button(tr!("Relancer", "Retry")).clicked() {
                        send(core, action(QueueAction::Retry));
                    }
                } else {
                    if fonts::symbol_button(ui, fonts::SYMBOL_UP, &tr!("Monter", "Move up")) {
                        send(core, action(QueueAction::Up));
                    }
                    if fonts::symbol_button(ui, fonts::SYMBOL_DOWN, &tr!("Descendre", "Move down")) {
                        send(core, action(QueueAction::Down));
                    }
                    if ui.small_button(tr!("Annuler", "Cancel")).clicked() {
                        send(core, action(QueueAction::Cancel));
                    }
                }
                if ui.small_button(tr!("Retirer", "Remove")).clicked() {
                    send(core, action(QueueAction::Remove));
                }
            });
            ui.end_row();
        }
    });
}

fn transfers(ui: &mut egui::Ui, st: &State, speeds: &mut Speeds, core: &UnboundedSender<CoreMsg>) {
    egui::Grid::new("transferts")
        .striped(true)
        .num_columns(6)
        .show(ui, |ui| {
            for title in [
                tr!("Fichier", "File"),
                tr!("De", "From"),
                tr!("Taille", "Size"),
                tr!("Progression", "Progress"),
                tr!("Durée", "Duration"),
                tr!("État", "Status"),
            ] {
                ui.label(RichText::new(title).strong());
            }
            ui.end_row();
            for t in &st.transfers {
                let position = t.position.load(Ordering::Relaxed);
                ui.label(dcc::file_name(&t.path))
                    .on_hover_text(t.path.display().to_string());
                ui.label(&t.nick);
                ui.label(t.offer.size.map(human_size).unwrap_or_else(|| "?".into()));
                let fraction = t.offer.size.filter(|&s| s > 0).map(|s| position as f32 / s as f32);
                let mut text = human_size(position);
                if t.status == TransferStatus::Receiving {
                    let rate = speeds.rate(t);
                    text = format!("{text} — {}/s", human_size(rate as u64));
                    if let Some(size) = t.offer.size
                        && rate > 0.0
                    {
                        text = format!("{text} — {}", duration(size.saturating_sub(position) as f64 / rate));
                    }
                } else if t.status == TransferStatus::Done
                    && let (Some(start), Some(end)) = (t.started, t.ended)
                {
                    let secs = end.duration_since(start).as_secs_f64().max(0.001);
                    let rate = position.saturating_sub(t.offset) as f64 / secs;
                    let rate = human_size(rate as u64);
                    text = tr!("{text} — {rate}/s en moyenne", "{text} — {rate}/s average");
                }
                let bar = ProgressBar::new(fraction.unwrap_or(0.0))
                    .desired_width(260.0)
                    .text(text);
                ui.add(bar);
                // Temps de téléchargement : écoulé, ou total une fois terminé.
                let elapsed = t
                    .started
                    .map(|start| t.ended.unwrap_or_else(Instant::now).duration_since(start));
                ui.label(elapsed.map(|d| duration(d.as_secs_f64())).unwrap_or_default());
                let status = match &t.status {
                    TransferStatus::AskAccept => tr!("À accepter", "Awaiting acceptance"),
                    TransferStatus::AskExists => tr!("Fichier existant", "File exists"),
                    TransferStatus::Resuming => tr!("Reprise demandée", "Resume requested"),
                    TransferStatus::Connecting => tr!("Connexion…", "Connecting…"),
                    TransferStatus::Listening => match t.announced {
                        Some(addr) => tr!(
                            "Attente de l'émetteur sur {addr} (passif)",
                            "Waiting for the sender on {addr} (passive)"
                        ),
                        None => tr!(
                            "Ouverture du port sur la box (UPnP)…",
                            "Opening the port on the router (UPnP)…"
                        ),
                    },
                    TransferStatus::Receiving => tr!("Réception", "Receiving"),
                    TransferStatus::Done => tr!("Terminé", "Done"),
                    TransferStatus::Failed(e) => match t.announced {
                        Some(addr) => tr!(
                            "Échec : {e} (adresse annoncée : {addr})",
                            "Failed: {e} (announced address: {addr})"
                        ),
                        None => tr!("Échec : {e}", "Failed: {e}"),
                    },
                    TransferStatus::Cancelled => tr!("Annulé", "Cancelled"),
                };
                ui.horizontal(|ui| {
                    ui.label(status);
                    let action = |a| Command::Transfer(t.id, a);
                    if t.status.finished() {
                        if ui.small_button(tr!("Retirer", "Remove")).clicked() {
                            send(core, action(TransferAction::Remove));
                        }
                    } else if ui.small_button(tr!("Annuler", "Cancel")).clicked() {
                        send(core, action(TransferAction::Cancel));
                    }
                    if t.status == TransferStatus::Done
                        && ui
                            .small_button(tr!("Ouvrir", "Open"))
                            .on_hover_text(tr!(
                                "Ouvrir avec le logiciel associé",
                                "Open with the associated program"
                            ))
                            .clicked()
                    {
                        open_file(&t.path);
                    }
                    if ui
                        .small_button(tr!("Dossier", "Folder"))
                        .on_hover_text(tr!("Afficher dans le dossier", "Show in folder"))
                        .clicked()
                    {
                        open_folder(&t.path);
                    }
                });
                ui.end_row();
            }
        });
}

/// Envois non sollicités et fichiers déjà présents : l'utilisateur décide.
pub fn prompts(ctx: &egui::Context, st: &State, core: &UnboundedSender<CoreMsg>) {
    for t in &st.transfers {
        if !matches!(t.status, TransferStatus::AskAccept | TransferStatus::AskExists) {
            continue;
        }
        let size = t
            .offer
            .size
            .map(human_size)
            .unwrap_or_else(|| tr!("taille inconnue", "unknown size"));
        let name = dcc::file_name(&t.path);
        let nick = &t.nick;
        let action = |a| Command::Transfer(t.id, a);
        egui::Window::new(tr!("Réception DCC — {nick}", "Incoming DCC — {nick}"))
            .id(Id::unique(("dcc", t.id)))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                if t.status == TransferStatus::AskAccept {
                    ui.label(tr!(
                        "{nick} veut vous envoyer {name} ({size}).",
                        "{nick} wants to send you {name} ({size})."
                    ));
                    if t.offer.passive() {
                        ui.label(tr!(
                            "DCC passif : l'émetteur se connectera à vous.",
                            "Passive DCC: the sender will connect to you."
                        ));
                    }
                    ui.horizontal(|ui| {
                        if ui.button(tr!("Accepter", "Accept")).clicked() {
                            send(core, action(TransferAction::Accept));
                        }
                        if ui.button(tr!("Refuser", "Decline")).clicked() {
                            send(core, action(TransferAction::Cancel));
                        }
                    });
                } else {
                    let existing = std::fs::metadata(&t.path)
                        .map(|m| human_size(m.len()))
                        .unwrap_or_default();
                    ui.label(tr!(
                        "{name} existe déjà ({existing} sur {size}).",
                        "{name} already exists ({existing} of {size})."
                    ));
                    ui.horizontal(|ui| {
                        for (label, choice) in [
                            (tr!("Reprendre", "Resume"), ExistsAction::Resume),
                            (tr!("Écraser", "Overwrite"), ExistsAction::Overwrite),
                            (tr!("Renommer", "Rename"), ExistsAction::Rename),
                        ] {
                            if ui.button(label).clicked() {
                                send(core, action(TransferAction::Exists(choice)));
                            }
                        }
                        if ui.button(tr!("Annuler", "Cancel")).clicked() {
                            send(core, action(TransferAction::Cancel));
                        }
                    });
                }
            });
    }
}

/// Ouvre le dossier du fichier (sélectionné dans l'Explorateur sous Windows).
fn open_folder(path: &Path) {
    #[cfg(windows)]
    if path.exists() {
        crate::explorer::reveal(path);
        return;
    }
    open_dir(path.parent().unwrap_or(path));
}

/// Ouvre un fichier avec le logiciel associé à son type.
fn open_file(path: &Path) {
    #[cfg(windows)]
    crate::explorer::open_file(path);
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Ouvre un dossier dans l'Explorateur, en réutilisant l'onglet qui le montre déjà (le
/// gestionnaire de fichiers sous Linux).
fn open_dir(dir: &Path) {
    #[cfg(windows)]
    crate::explorer::open(dir);
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}
