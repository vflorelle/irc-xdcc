//! Serveur ident (RFC 1413, port 113) : à la connexion, le serveur IRC
//! demande « qui utilise ce port ? » ; sans réponse, il préfixe le nom d'utilisateur d'un `~`
//! (« No ident response »). Derrière une box, le port 113 doit lui être redirigé : par UPnP si
//! c'est activé, sinon à la main.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::AbortHandle;

use crate::state::{Shared, lock};
use crate::upnp;

const PORT: u16 = 113;

/// Écoute le port 113 (sans erreur s'il est pris, par un autre client IRC par exemple) ; avec
/// `upnp`, la box le redirige vers ce PC tant que le serveur tourne.
pub fn start(state: Shared, upnp: bool) -> AbortHandle {
    tokio::spawn(async move {
        let Ok(listener) = TcpListener::bind(("0.0.0.0", PORT)).await else {
            return;
        };
        let serve = async {
            while let Ok((stream, _)) = listener.accept().await {
                let user = lock(&state).config.identity.user().to_owned();
                tokio::spawn(answer(stream, user));
            }
        };
        let redirect = async {
            loop {
                let _ = upnp::open_ident(PORT).await;
                // Renouvelée avant la fin du bail.
                tokio::time::sleep(upnp::IDENT_LEASE - Duration::from_secs(300)).await;
            }
        };
        if upnp {
            tokio::select! {
                () = serve => {}
                () = redirect => {}
            }
        } else {
            serve.await;
        }
    })
    .abort_handle()
}

async fn answer(stream: TcpStream, user: String) {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let mut reader = BufReader::new(read).take(100);
    if tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .is_ok_and(|r| r.is_ok())
    {
        let _ = write.write_all(reply(&line, &user).as_bytes()).await;
    }
}

/// Réponse à une requête « port-serveur , port-client ».
fn reply(query: &str, user: &str) -> String {
    let ports: Vec<Option<u16>> = query.trim().split(',').map(|p| p.trim().parse().ok()).collect();
    match ports.as_slice() {
        [Some(a), Some(b)] => format!("{a} , {b} : USERID : UNIX : {user}\r\n"),
        _ => format!("{} : ERROR : INVALID-PORT\r\n", query.trim()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies() {
        assert_eq!(reply("6193, 23\r\n", "moi"), "6193 , 23 : USERID : UNIX : moi\r\n");
        assert_eq!(reply("abc\r\n", "moi"), "abc : ERROR : INVALID-PORT\r\n");
    }
}
