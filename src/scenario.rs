//! Scénario complet contre un faux serveur IRC et un faux bot XDCC : pseudo de secours, perform,
//! favoris rejoints automatiquement, file XDCC, DCC actif, DCC passif, reprise, fichier déjà
//! complet, bot absent (et envoi tardif), file d'attente du bot.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;

use crate::config::{Config, Favorite, Network, ServerAddr};
use crate::core::{Command, Core, CoreMsg};
use crate::state::{QueueStatus, Shared, State, TransferStatus, lock};

struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    /// Attend une ligne commençant par `prefix` (les autres sont ignorées).
    async fn expect(&mut self, prefix: &str) -> String {
        timeout(Duration::from_secs(10), async {
            loop {
                let mut line = String::new();
                assert!(self.reader.read_line(&mut line).await.unwrap() > 0, "connexion fermée");
                let line = line.trim_end().to_owned();
                if line.starts_with(prefix) {
                    return line;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("ligne attendue : {prefix}"))
    }

    async fn send(&mut self, line: &str) {
        self.writer.write_all(format!("{line}\r\n").as_bytes()).await.unwrap();
    }
}

/// Le bot écoute : DCC actif.
async fn active_offer(client: &mut Client, bot: &str, name: &str, data: &'static [u8]) -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let size = data.len();
    let offer = format!(":{bot}!b@h PRIVMSG tester_ :\x01DCC SEND {name} 2130706433 {port} {size}\x01");
    client.send(&offer).await;
    listener
}

async fn serve(listener: TcpListener, data: &'static [u8]) {
    let (mut socket, _) = timeout(Duration::from_secs(10), listener.accept())
        .await
        .unwrap()
        .unwrap();
    socket.write_all(data).await.unwrap();
    // Attend le dernier accusé de réception avant de fermer.
    let mut ack = [0; 4];
    while socket.read_exact(&mut ack).await.is_ok() && u32::from_be_bytes(ack) < data.len() as u32 {}
}

async fn wait_for(state: &Shared, what: &str, check: impl Fn(&State) -> bool) {
    for _ in 0..200 {
        if check(&lock(state)) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("délai dépassé : {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn irc_and_xdcc() {
    let dir = std::env::temp_dir().join(format!("irc-xdcc-scenario-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Début du 3ᵉ fichier déjà reçu : il sera repris.
    std::fs::write(dir.join("file3.bin"), b"01234").unwrap();

    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = Config::default();
    config.identity.nick = "tester".into();
    config.identity.alt_nick = "tester_".into();
    config.networks.push(Network {
        name: "Test".into(),
        servers: vec![ServerAddr::new("127.0.0.1", server.local_addr().unwrap().port(), false)],
        perform: vec!["/ns identify secret".into()],
        ..Default::default()
    });
    config.favorites.push(Favorite {
        channel: "#xdcc".into(),
        network: "Test".into(),
        auto_join: true,
        ..Default::default()
    });
    config.dcc.download_dir = dir.clone();
    config.dcc.external_ip = "127.0.0.1".into();
    config.dcc.port_first = 40000;
    config.dcc.port_last = 40100;
    config.dcc.upnp = false;
    config.connection.identd = false;

    let state: Shared = Arc::new(Mutex::new(State::new(config)));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(Core::new(state.clone(), egui::Context::default(), tx.clone()).run(rx));
    let ui = |command| tx.send(CoreMsg::Ui(command)).unwrap();
    ui(Command::ConnectNetwork("Test".into()));

    let (socket, _) = timeout(Duration::from_secs(5), server.accept()).await.unwrap().unwrap();
    let (reader, writer) = socket.into_split();
    let mut irc = Client {
        reader: BufReader::new(reader),
        writer,
    };

    // Pseudo pris : pseudo de secours.
    irc.expect("NICK tester").await;
    irc.expect("USER ").await;
    irc.send(":srv 433 * tester :Nickname is already in use").await;
    irc.expect("NICK tester_").await;
    irc.send(":srv 001 tester_ :Welcome").await;
    irc.send(":srv 005 tester_ PREFIX=(ov)@+ CHANTYPES=# NETWORK=TestNet :are supported")
        .await;
    irc.send(":srv 376 tester_ :End of MOTD").await;
    // Mode invisible, perform, puis favori rejoint automatiquement.
    irc.expect("MODE tester_ +i").await;
    // Fin du message du jour : séparateur « - », sans horodatage.
    wait_for(&state, "séparateur", |st| {
        st.servers[0].buffers[0]
            .lines
            .iter()
            .any(|l| l.kind == crate::state::LineKind::Separator && l.text == "-" && l.time.is_empty())
    })
    .await;
    assert_eq!(
        irc.expect("PRIVMSG NickServ").await,
        "PRIVMSG NickServ :identify secret"
    );
    irc.expect("JOIN #xdcc").await;
    irc.send(":tester_!u@h JOIN #xdcc").await;
    irc.send(":srv 353 tester_ = #xdcc :tester_ @Bot +voiced").await;
    irc.send(":srv 366 tester_ #xdcc :End of NAMES").await;
    wait_for(&state, "liste des pseudos", |st| {
        st.servers[0].buffer("#xdcc").is_some_and(|b| {
            b.users
                .iter()
                .map(|u| format!("{}{}", u.prefixes, u.nick))
                .collect::<Vec<_>>()
                == ["@Bot", "+voiced", "tester_"]
        })
    })
    .await;
    let id = lock(&state).servers[0].id;

    // Lignes malformées : pas de plantage ; /clear dans la fenêtre d'état (clé vide).
    irc.send(":srv 005").await;
    irc.send(":srv MODE").await;
    wait_for(&state, "ligne MODE", |st| {
        st.servers[0].buffers[0]
            .lines
            .iter()
            .any(|l| l.text.starts_with("* srv met le mode"))
    })
    .await;
    ui(Command::Input {
        server: id,
        buffer: String::new(),
        text: "/clear".into(),
    });
    wait_for(&state, "/clear", |st| st.servers[0].buffers[0].lines.is_empty()).await;

    // Trois packs : un seul demandé à la fois.
    ui(Command::Input {
        server: id,
        buffer: "#xdcc".into(),
        text: "/msg Bot xdcc send #1-#3".into(),
    });
    irc.expect("PRIVMSG Bot :XDCC SEND #1").await;
    let listener = active_offer(&mut irc, "Bot", "file1.bin", b"hello world").await;
    serve(listener, b"hello world").await;

    // Pack suivant : DCC passif (le bot donne un port 0 et un jeton, c'est nous qui écoutons).
    irc.expect("PRIVMSG Bot :XDCC SEND #2").await;
    irc.send(":Bot!b@h PRIVMSG tester_ :\x01DCC SEND \"file 2.bin\" 2130706433 0 7 42\x01")
        .await;
    let reply = irc.expect("PRIVMSG Bot :\x01DCC SEND").await;
    let args = reply
        .trim_start_matches("PRIVMSG Bot :\x01DCC SEND ")
        .trim_end_matches('\x01');
    let offer = crate::dcc::parse_send(args).unwrap();
    assert_eq!(
        (offer.filename.as_str(), offer.size, offer.token.as_deref()),
        ("file 2.bin", Some(7), Some("42"))
    );
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", offer.port)).await.unwrap();
    socket.write_all(b"passive").await.unwrap();
    let mut ack = [0; 4];
    socket.read_exact(&mut ack).await.unwrap();

    // Dernier pack : reprise du fichier partiel.
    irc.expect("PRIVMSG Bot :XDCC SEND #3").await;
    let listener = active_offer(&mut irc, "Bot", "file3.bin", b"0123456789").await;
    let port = listener.local_addr().unwrap().port();
    let resume = irc.expect("PRIVMSG Bot :\x01DCC RESUME").await;
    assert_eq!(resume, format!("PRIVMSG Bot :\x01DCC RESUME file3.bin {port} 5\x01"));
    irc.send(&format!(
        ":Bot!b@h PRIVMSG tester_ :\x01DCC ACCEPT file3.bin {port} 5\x01"
    ))
    .await;
    serve(listener, b"56789").await;

    wait_for(&state, "fin des transferts", |st| {
        st.queue.len() == 3 && st.queue.iter().all(|q| q.status == QueueStatus::Done)
    })
    .await;
    assert!(lock(&state).transfers.iter().all(|t| t.status == TransferStatus::Done));
    assert_eq!(std::fs::read(dir.join("file1.bin")).unwrap(), b"hello world");
    assert_eq!(std::fs::read(dir.join("file 2.bin")).unwrap(), b"passive");
    assert_eq!(std::fs::read(dir.join("file3.bin")).unwrap(), b"0123456789");
    let label = lock(&state).servers[0].label.clone();
    assert_eq!(label, "Test");
    // La demande s'affiche dans sa fenêtre ; les messages des transferts dans la fenêtre d'état ;
    // les notices du bot dans un canal où il se trouve.
    let shown = |window: &str, text: &str| {
        lock(&state).servers[0]
            .buffer(window)
            .is_some_and(|b| b.lines.iter().any(|l| l.text.starts_with(text)))
    };
    assert!(shown("#xdcc", "-> *Bot* XDCC SEND #1"));
    assert!(shown("", "* DCC passif : Bot doit se connecter à 127.0.0.1:"));
    assert!(shown("", "Réception DCC de file1.bin depuis Bot terminée (00:00:0"));
    irc.send(":Bot!b@h NOTICE tester_ :** Transfer Completed").await;
    wait_for(&state, "notice du bot", |st| {
        st.servers[0]
            .buffer("#xdcc")
            .is_some_and(|b| b.lines.iter().any(|l| l.text == "-Bot- ** Transfer Completed"))
    })
    .await;

    // Fichier déjà complet : reprise sur le dernier octet, renvoyé à l'identique, et l'envoi du
    // bot se termine normalement.
    ui(Command::Input {
        server: id,
        buffer: String::new(),
        text: "/xdcc Bot #3".into(),
    });
    irc.expect("PRIVMSG Bot :XDCC SEND #3").await;
    let listener = active_offer(&mut irc, "Bot", "file3.bin", b"0123456789").await;
    let port = listener.local_addr().unwrap().port();
    let resume = irc.expect("PRIVMSG Bot :\x01DCC RESUME").await;
    assert_eq!(resume, format!("PRIVMSG Bot :\x01DCC RESUME file3.bin {port} 9\x01"));
    irc.send(&format!(
        ":Bot!b@h PRIVMSG tester_ :\x01DCC ACCEPT file3.bin {port} 9\x01"
    ))
    .await;
    serve(listener, b"9").await;
    wait_for(&state, "déjà téléchargé", |st| {
        st.queue.last().is_some_and(|q| q.status == QueueStatus::Done)
    })
    .await;
    assert!(shown("", "Réception DCC de file3.bin depuis Bot : déjà téléchargé"));
    assert_eq!(std::fs::read(dir.join("file3.bin")).unwrap(), b"0123456789");

    // Bot absent du réseau : la demande sera refaite plus tard.
    ui(Command::Input {
        server: id,
        buffer: String::new(),
        text: "/xdcc Absent #9".into(),
    });
    irc.expect("PRIVMSG Absent :XDCC SEND #9").await;
    irc.send(":srv 401 tester_ Absent :No such nick/channel").await;
    wait_for(&state, "bot absent", |st| {
        st.queue
            .last()
            .is_some_and(|q| q.status == QueueStatus::Waiting && q.attempts == 1)
    })
    .await;
    // Il finit par l'envoyer : le pack est rattaché à la demande (accepté, pas redemandé).
    let listener = active_offer(&mut irc, "Absent", "file9.bin", b"tardif").await;
    serve(listener, b"tardif").await;
    wait_for(&state, "envoi tardif", |st| {
        st.queue.last().is_some_and(|q| q.status == QueueStatus::Done)
    })
    .await;
    assert!(shown("", "Réception DCC de file9.bin depuis Absent terminée"));

    // File d'attente du bot : la position annoncée est gardée, le pack n'est pas redemandé.
    ui(Command::Input {
        server: id,
        buffer: String::new(),
        text: "/xdcc Plein #4".into(),
    });
    irc.expect("PRIVMSG Plein :XDCC SEND #4").await;
    irc.send(":Plein!p@h NOTICE tester_ :** All Slots Full, Added you to the main queue for pack 4 in position 2.")
        .await;
    wait_for(&state, "file du bot", |st| {
        st.queue
            .last()
            .is_some_and(|q| q.status == QueueStatus::BotQueue("position 2".into()))
    })
    .await;

    // Discrétion : pas de réponse à VERSION, heure en UTC.
    irc.send(":Curieux!c@h PRIVMSG tester_ :\x01VERSION\x01").await;
    irc.send(":Curieux!c@h PRIVMSG tester_ :\x01TIME\x01").await;
    let reply = irc.expect("NOTICE Curieux :").await;
    assert!(
        reply.starts_with("NOTICE Curieux :\x01TIME ") && reply.ends_with(" UTC\x01"),
        "{reply}"
    );

    // Réseau ou favori déjà ouvert : rien n'est ouvert en double.
    ui(Command::ConnectNetwork("Test".into()));
    ui(Command::JoinFavorite(0));
    ui(Command::Input {
        server: id,
        buffer: String::new(),
        text: "/help".into(),
    });
    wait_for(&state, "commandes traitées", |st| st.help).await;
    assert_eq!(lock(&state).servers.len(), 1);

    // Favori « auto » quitté (ou jamais rejoint) : redemander le serveur le rejoint.
    irc.send(":tester_!u@h PART #xdcc").await;
    wait_for(&state, "#xdcc quitté", |st| {
        st.servers[0].buffer("#xdcc").is_some_and(|b| !b.joined)
    })
    .await;
    ui(Command::ConnectNetwork("Test".into()));
    irc.expect("JOIN #xdcc").await;
    irc.send(":tester_!u@h JOIN #xdcc").await;

    // Reconnexion manuelle : retour dans les canaux ouverts, même hors favoris.
    irc.send(":tester_!u@h JOIN #autre").await;
    wait_for(&state, "#autre", |st| {
        st.servers[0].buffer("#autre").is_some_and(|b| b.joined)
    })
    .await;
    ui(Command::Reconnect(id));
    irc.expect("QUIT").await;
    let (socket, _) = timeout(Duration::from_secs(5), server.accept()).await.unwrap().unwrap();
    let (reader, writer) = socket.into_split();
    let mut irc = Client {
        reader: BufReader::new(reader),
        writer,
    };
    irc.expect("NICK tester").await;
    irc.send(":srv 001 tester :Welcome").await;
    irc.send(":srv 422 tester :No MOTD").await;
    irc.expect("JOIN #autre").await;
    std::fs::remove_dir_all(&dir).unwrap();
}
