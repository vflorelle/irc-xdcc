//! UPnP : la box ouvre d'elle-même les ports qui doivent recevoir des connexions (DCC passif,
//! serveur ident), sans redirection à configurer, et donne l'adresse publique.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use igd_next::aio::Gateway;
use igd_next::aio::tokio::{Tokio, search_gateway};
use igd_next::{AddPortError, PortMappingProtocol, SearchOptions};

/// Durée d'une redirection pour un DCC passif : le temps que l'émetteur se connecte (la
/// connexion établie reste ouverte au-delà).
const DCC_LEASE: Duration = Duration::from_secs(600);
/// Durée de la redirection du serveur ident, renouvelée tant qu'il tourne.
pub const IDENT_LEASE: Duration = Duration::from_secs(3600);
/// Délai maximal d'une opération (recherche de la box comprise).
const TIMEOUT: Duration = Duration::from_secs(10);
const TCP: PortMappingProtocol = PortMappingProtocol::TCP;

/// Redirige le port d'un DCC passif vers ce PC ; renvoie l'adresse publique vue par la box, si
/// elle la donne.
pub async fn open_dcc(port: u16) -> Result<Option<IpAddr>, String> {
    guarded(async move {
        let (gateway, local) = gateway(port).await?;
        match gateway
            .add_port(TCP, port, local, DCC_LEASE.as_secs() as u32, "DCC")
            .await
        {
            Ok(()) => {}
            // Box sans bail limité : redirection permanente, retirée après le même délai.
            Err(AddPortError::OnlyPermanentLeasesSupported) => {
                gateway.add_port(TCP, port, local, 0, "DCC").await.map_err(refused)?;
                let gateway = gateway.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(DCC_LEASE).await;
                    let _ = gateway.remove_port(TCP, port).await;
                });
            }
            Err(e) => return Err(refused(e)),
        }
        Ok(gateway.get_external_ip().await.ok())
    })
    .await
}

/// Redirige le port `port` du serveur ident vers ce PC pour `IDENT_LEASE` (pas de redirection
/// permanente : elle resterait sur la box après la fermeture du logiciel).
pub async fn open_ident(port: u16) -> Result<(), String> {
    guarded(async move {
        let (gateway, local) = gateway(port).await?;
        let lease = IDENT_LEASE.as_secs() as u32;
        gateway
            .add_port(TCP, port, local, lease, "ident")
            .await
            .map_err(refused)
    })
    .await
}

/// La box, et l'adresse de ce PC sur son réseau (avec `port`).
async fn gateway(port: u16) -> Result<(Gateway<Tokio>, SocketAddr), String> {
    // Recherche depuis l'interface de la route par défaut (192.0.2.1 est une adresse de
    // documentation : seule la table de routage est consultée) : sinon Windows peut l'envoyer
    // sur une carte virtuelle (WSL, Hyper-V, VPN…), où aucune box ne répond.
    let mut options = SearchOptions::default();
    options.bind_addr = SocketAddr::new(local_ip(SocketAddr::from(([192, 0, 2, 1], 9)))?, 0);
    options.timeout = Some(Duration::from_secs(5));
    let gateway = search_gateway(options)
        .await
        .map_err(|e| tr!("box UPnP introuvable ({e})", "UPnP router not found ({e})"))?;
    let ip = local_ip(gateway.addr)?;
    Ok((gateway, SocketAddr::new(ip, port)))
}

/// Adresse de ce PC sur la route vers `destination` (aucun paquet n'est envoyé).
fn local_ip(destination: SocketAddr) -> Result<IpAddr, String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    socket.connect(destination).map_err(|e| e.to_string())?;
    Ok(socket.local_addr().map_err(|e| e.to_string())?.ip())
}

fn refused(e: impl std::fmt::Display) -> String {
    tr!(
        "redirection refusée par la box ({e})",
        "port forwarding refused by the router ({e})"
    )
}

/// Opération dans sa propre tâche, avec un délai maximal : une box muette ou une erreur imprévue
/// ne bloque jamais l'appelant.
async fn guarded<T: Send + 'static>(
    operation: impl Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    match tokio::time::timeout(TIMEOUT, tokio::spawn(operation)).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(tr!("erreur UPnP imprévue", "unexpected UPnP error")),
        Err(_) => Err(tr!("la box ne répond pas", "the router is not responding")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Une erreur imprévue (ou une box muette) donne une réponse quand même : l'offre du DCC
    /// passif part toujours.
    #[tokio::test]
    async fn never_blocks() {
        let failed: Result<(), String> = guarded(async { panic!("erreur") }).await;
        assert_eq!(failed, Err("erreur UPnP imprévue".into()));
    }
}
