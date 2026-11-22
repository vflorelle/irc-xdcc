//! Connexion à un serveur IRC (TCP ou TLS) : lecture des lignes et envoi avec anti-flood.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::AbortHandle;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

use crate::config::ServerAddr;
use crate::core::CoreMsg;
use crate::state::ServerId;

pub enum NetEvent {
    Connected { local_ip: IpAddr },
    Line(String),
    Closed(String),
}

/// Connexion en cours : envoi des lignes et arrêt.
pub struct Connection {
    out: UnboundedSender<String>,
    task: AbortHandle,
}

impl Connection {
    pub fn send(&self, line: String) {
        let _ = self.out.send(line);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// `generation` distingue cette connexion des précédentes du même serveur : un évènement encore en
/// file d'une connexion abandonnée (reconnexion) est ainsi ignoré.
pub fn connect(id: ServerId, generation: u64, addr: ServerAddr, core: UnboundedSender<CoreMsg>) -> Connection {
    let (out, out_rx) = unbounded_channel();
    let task = tokio::spawn(async move {
        let reason = match run((id, generation), &addr, &core, out_rx).await {
            Ok(()) => tr!("connexion fermée par le serveur", "connection closed by the server"),
            Err(e) => e,
        };
        let _ = core.send(CoreMsg::Net(id, generation, NetEvent::Closed(reason)));
    });
    Connection {
        out,
        task: task.abort_handle(),
    }
}

/// Connexion en IPv4 de préférence, IPv6 sinon : l'adresse vue par le serveur sert aux DCC
/// passifs, que les émetteurs ne font qu'en IPv4.
async fn connect_ipv4_first(addr: &ServerAddr) -> Result<TcpStream, String> {
    let mut addrs: Vec<SocketAddr> = tokio::net::lookup_host((addr.host.as_str(), addr.port))
        .await
        .map_err(|e| tr!("adresse introuvable : {e}", "address not found: {e}"))?
        .collect();
    addrs.sort_by_key(SocketAddr::is_ipv6);
    let mut error = tr!("adresse introuvable", "address not found");
    for address in addrs {
        match TcpStream::connect(address).await {
            Ok(tcp) => return Ok(tcp),
            Err(e) => error = tr!("connexion impossible : {e}", "unable to connect: {e}"),
        }
    }
    Err(error)
}

async fn run(
    id: (ServerId, u64),
    addr: &ServerAddr,
    core: &UnboundedSender<CoreMsg>,
    out_rx: UnboundedReceiver<String>,
) -> Result<(), String> {
    let tcp = timeout(Duration::from_secs(30), connect_ipv4_first(addr))
        .await
        .map_err(|_| tr!("délai de connexion dépassé", "connection timed out"))??;
    let _ = tcp.set_nodelay(true);
    let local_ip = tcp.local_addr().map_err(|e| e.to_string())?.ip();
    if !addr.tls {
        let _ = core.send(CoreMsg::Net(id.0, id.1, NetEvent::Connected { local_ip }));
        return io(id, tcp, core, out_rx).await;
    }
    let name = ServerName::try_from(addr.host.clone())
        .map_err(|e| tr!("nom de serveur invalide : {e}", "invalid server name: {e}"))?;
    let connector = TlsConnector::from(Arc::new(tls_config(addr.accept_invalid_cert)));
    let tls = timeout(Duration::from_secs(30), connector.connect(name, tcp))
        .await
        .map_err(|_| tr!("délai de négociation TLS dépassé", "TLS handshake timed out"))?
        .map_err(|e| tr!("TLS : {e}", "TLS: {e}"))?;
    let _ = core.send(CoreMsg::Net(id.0, id.1, NetEvent::Connected { local_ip }));
    io(id, tls, core, out_rx).await
}

async fn io<S: AsyncRead + AsyncWrite + Send + 'static>(
    id: (ServerId, u64),
    stream: S,
    core: &UnboundedSender<CoreMsg>,
    out_rx: UnboundedReceiver<String>,
) -> Result<(), String> {
    let (reader, writer) = tokio::io::split(stream);
    let writer = tokio::spawn(write_lines(writer, out_rx)).abort_handle();
    let result = read_lines(id, reader, core).await;
    writer.abort();
    result
}

async fn read_lines(
    id: (ServerId, u64),
    reader: impl AsyncRead + Unpin,
    core: &UnboundedSender<CoreMsg>,
) -> Result<(), String> {
    let mut reader = BufReader::new(reader);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // Aucun serveur n'envoie de ligne aussi longue : une ligne anormale est tronquée.
        let n = (&mut reader)
            .take(64 * 1024)
            .read_until(b'\n', &mut buf)
            .await
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        let line = crate::irc::decode(&buf);
        let line = line.trim_end_matches(['\r', '\n']);
        if !line.is_empty() {
            let _ = core.send(CoreMsg::Net(id.0, id.1, NetEvent::Line(line.to_owned())));
        }
    }
}

/// Anti-flood : 5 lignes d'affilée, puis une toutes les 0,7 s (les serveurs déconnectent sinon).
async fn write_lines(mut writer: impl AsyncWrite + Unpin, mut out_rx: UnboundedReceiver<String>) {
    const BURST: u32 = 5;
    const INTERVAL: Duration = Duration::from_millis(700);
    let mut credit = BURST;
    let mut last = Instant::now();
    while let Some(line) = out_rx.recv().await {
        let earned = (last.elapsed().as_millis() / INTERVAL.as_millis()) as u32;
        if earned > 0 {
            credit = (credit + earned).min(BURST);
            last = Instant::now();
        }
        if credit == 0 {
            sleep_until(last + INTERVAL).await;
            last = Instant::now();
            credit = 1;
        }
        credit -= 1;
        let mut line = line;
        if line.len() > 510 {
            let cut = (0..=510).rev().find(|&i| line.is_char_boundary(i)).unwrap_or(0);
            line.truncate(cut);
        }
        let mut bytes = line.into_bytes();
        bytes.extend_from_slice(b"\r\n");
        if writer.write_all(&bytes).await.is_err() {
            return;
        }
    }
}

fn tls_config(accept_invalid: bool) -> ClientConfig {
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("versions TLS par défaut");
    if accept_invalid {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
            .with_no_client_auth()
    } else {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        builder.with_root_certificates(roots).with_no_client_auth()
    }
}

/// Accepte n'importe quel certificat (option par serveur) ; les signatures restent vérifiées.
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, tokio_rustls::rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, tokio_rustls::rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Négociation TLS avec un vrai serveur (réseau requis) : `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn tls_handshake() {
        let (tx, mut rx) = unbounded_channel();
        // Premier serveur de la liste proposée au premier lancement.
        let addr = crate::config::Config::first_run().networks[0].servers[0].clone();
        let _connection = connect(1, 1, addr, tx);
        let event = timeout(Duration::from_secs(30), rx.recv()).await.unwrap().unwrap();
        match event {
            CoreMsg::Net(1, _, NetEvent::Connected { .. }) => {}
            CoreMsg::Net(_, _, NetEvent::Closed(reason)) => panic!("{reason}"),
            _ => panic!("évènement inattendu"),
        }
    }
}
