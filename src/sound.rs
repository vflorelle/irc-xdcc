//! Sons des évènements (transferts, bot injoignable, déconnexion) et bip des messages.
//!
//! Sous Windows : un fichier audio (WAV, MP3…) au choix, sinon un son du système ; le bip est le
//! son « Bip par défaut ». Sous Linux : joués par `paplay` ou `aplay`, sons du système pris dans
//! le thème freedesktop. Les sons sont joués l'un après l'autre, jamais par-dessus.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::config::{Sound, Sounds};

#[derive(Clone, Copy)]
pub enum Event {
    Started,
    Completed,
    Failed,
    BotOffline,
    Disconnected,
}

pub const EVENTS: [Event; 5] = [
    Event::Started,
    Event::Completed,
    Event::Failed,
    Event::BotOffline,
    Event::Disconnected,
];

impl Event {
    /// Nom dans les options.
    pub fn label(self) -> String {
        match self {
            Event::Started => tr!("Début d'un transfert", "Transfer started"),
            Event::Completed => tr!("Transfert terminé", "Transfer completed"),
            Event::Failed => tr!("Transfert échoué", "Transfer failed"),
            Event::BotOffline => tr!(
                "Bot injoignable (absent, ne répond pas)",
                "Bot unreachable (gone, not answering)"
            ),
            Event::Disconnected => tr!("Déconnecté du serveur", "Disconnected from the server"),
        }
    }
}

impl Sounds {
    pub fn get(&self, event: Event) -> &Sound {
        match event {
            Event::Started => &self.started,
            Event::Completed => &self.completed,
            Event::Failed => &self.failed,
            Event::BotOffline => &self.bot_offline,
            Event::Disconnected => &self.disconnected,
        }
    }

    pub fn get_mut(&mut self, event: Event) -> &mut Sound {
        match event {
            Event::Started => &mut self.started,
            Event::Completed => &mut self.completed,
            Event::Failed => &mut self.failed,
            Event::BotOffline => &mut self.bot_offline,
            Event::Disconnected => &mut self.disconnected,
        }
    }
}

/// Joue le son de l'évènement s'il est activé (lui et les sons en général).
pub fn notify(sounds: &Sounds, event: Event) {
    let sound = sounds.get(event);
    if sounds.enabled && sound.enabled {
        play(&sound.file, event);
    }
}

/// Bip d'un message, au plus un par seconde.
pub fn beep() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
        return;
    }
    *last = Some(Instant::now());
    enqueue(Request::Beep);
}

/// Joue le fichier (vide : son du système), après les sons déjà en attente.
pub fn play(file: &Path, event: Event) {
    enqueue(Request::Event(file.to_path_buf(), event));
}

enum Request {
    /// Fichier audio (vide : son du système de l'évènement).
    Event(PathBuf, Event),
    Beep,
}

/// Les sons sont joués l'un après l'autre par un seul fil : deux sons rapprochés (début et fin
/// d'un petit transfert…) ne se superposent pas.
fn enqueue(request: Request) {
    static QUEUE: OnceLock<Sender<Request>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || rx.into_iter().for_each(output));
        tx
    });
    let _ = queue.send(request);
}

#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// Son de Windows (alias du Panneau de configuration), joué jusqu'au bout.
#[cfg(windows)]
fn system_sound(alias: &str) {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_SYNC};
    use windows::core::PCWSTR;
    // SAFETY : chaîne terminée par un zéro, valable pendant l'appel.
    unsafe {
        let _ = PlaySoundW(PCWSTR(wide(alias).as_ptr()), None, SND_ALIAS | SND_SYNC);
    }
}

/// Joue un son jusqu'au bout.
#[cfg(windows)]
fn output(request: Request) {
    use windows::Win32::Media::Multimedia::mciSendStringW;
    use windows::core::PCWSTR;

    let (file, event) = match request {
        Request::Beep => return system_sound("SystemDefault"),
        Request::Event(file, event) => (file, event),
    };
    if file.as_os_str().is_empty() {
        system_sound(match event {
            Event::Started | Event::Completed => "SystemAsterisk",
            Event::BotOffline => "SystemExclamation",
            Event::Failed | Event::Disconnected => "SystemHand",
        });
        return;
    }
    // MCI lit le WAV, le MP3, le WMA…
    // SAFETY : commande terminée par un zéro, sans tampon de retour ni fenêtre.
    let mci = |command: String| unsafe { mciSendStringW(PCWSTR(wide(&command).as_ptr()), None, None) };
    if mci(format!("open \"{}\" type mpegvideo alias irc-xdcc", file.display())) == 0 {
        mci("play irc-xdcc wait".into());
        mci("close irc-xdcc".into());
    }
}

/// Joue un son jusqu'au bout ; sans fichier, celui du thème de sons du bureau (freedesktop).
#[cfg(not(windows))]
fn output(request: Request) {
    let system = |name: &str| PathBuf::from(format!("/usr/share/sounds/freedesktop/stereo/{name}.oga"));
    let file = match request {
        Request::Beep => system("bell"),
        Request::Event(file, _) if !file.as_os_str().is_empty() => file,
        Request::Event(_, event) => system(match event {
            Event::Started => "message",
            Event::Completed => "complete",
            Event::BotOffline => "dialog-warning",
            Event::Failed => "dialog-error",
            Event::Disconnected => "network-connectivity-lost",
        }),
    };
    for player in ["paplay", "aplay"] {
        if std::process::Command::new(player)
            .arg(&file)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
        {
            return;
        }
    }
}
