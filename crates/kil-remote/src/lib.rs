//! The network bridge for Agent on Edge: `kil --remote` (or the standalone
//! `kil-remote` binary).
//!
//! Kil's session server deliberately listens only on a Unix socket trusted by
//! peer UID. This bridge wraps that socket in a TLS tunnel so an iPhone can
//! drive the same panes over the LAN:
//!
//! 1. The Mac prints a QR code carrying the address, a one-time pairing code
//!    and the SHA-256 fingerprint of a fresh self-signed certificate.
//! 2. Agent on Edge scans it, proves the code over TLS, and receives a session
//!    token it can reuse for its other connections.
//! 3. The bridge then does nothing but pump bytes between the TLS stream and
//!    the session server's Unix socket. It never parses terminal output, so
//!    the server's wire protocol and its `peer_uid` trust check are untouched.

use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener as StdTcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use qrcode::render::unicode;
use ring::{
    digest::{SHA256, digest},
    rand::{SecureRandom, SystemRandom},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, copy, split},
    net::{TcpListener, TcpStream, UnixStream},
};
use tokio_rustls::TlsAcceptor;

/// How a caller wants the bridge to behave.
pub struct Config {
    /// The session server's directory (`kil --server <dir>` keeps its socket here).
    pub dir: PathBuf,
    /// Port to listen on; `0` picks a free port and prints it in the QR.
    pub port: u16,
    /// Address to put in the QR, overriding the detected LAN IP.
    pub host: Option<String>,
    /// How long a printed pairing code stays valid.
    pub ttl: Duration,
    /// A binary that can run `--server <dir>` if no session server is running.
    pub launcher: Option<PathBuf>,
}

/// The one-time pairing secret and the token issued after the first use.
#[derive(Default)]
struct Auth {
    code: String,
    token: Option<String>,
}

/// Runs the bridge until the process is stopped.
///
/// # Errors
/// If the TLS identity cannot be generated, the port cannot be bound, Kil's
/// session server cannot be reached, or the runtime fails to start.
pub fn run(config: &Config) -> Result<()> {
    // If a provider is already installed (GPUI's HTTP client installs one), keep it.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let certified = rcgen::generate_simple_self_signed(vec!["kil".into()])
        .context("generate a self-signed certificate")?;
    let cert = certified.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(certified.key_pair.serialize_der().into());
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .context("build the TLS server config")?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));

    let code = random_code()?;
    let fingerprint = fingerprint(&cert);
    let host = match config.host.as_deref() {
        Some(host) => host.to_owned(),
        None => lan_ip().context("could not detect a LAN address; pass --host <ip>")?,
    };

    let listener = StdTcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port)))
        .with_context(|| format!("bind port {}", config.port))?;
    let port = listener.local_addr()?.port();
    let payload = format!("kil://{host}:{port}?code={code}&fp=sha256:{fingerprint}&v=1");
    print_qr(&payload, &code, &fingerprint, &config.dir);

    let socket_path = kil_term::server::socket_path(&config.dir);
    listener.set_nonblocking(true)?;
    let auth = Arc::new(Mutex::new(Auth { code, token: None }));

    let runtime = tokio::runtime::Runtime::new().context("start the tokio runtime")?;
    runtime.block_on(async {
        let listener = TcpListener::from_std(listener).context("register the TCP listener")?;
        ensure_server(&socket_path, config.launcher.as_deref(), &config.dir).await?;
        keep_alive(&socket_path).await?;
        serve(listener, acceptor, socket_path, auth, config.ttl).await
    })
}

/// Makes sure Kil's session server is up: the Kil app normally runs it, and
/// `kil --remote` can start it itself (the standalone binary cannot).
async fn ensure_server(socket_path: &Path, launcher: Option<&Path>, dir: &Path) -> Result<()> {
    if UnixStream::connect(socket_path).await.is_ok() {
        log::info!(
            "session server already running at {}",
            socket_path.display()
        );
        return Ok(());
    }
    log::info!("session server not running; starting it");
    match launcher {
        Some(exe) => kil_term::server::launch(exe, dir).context("start Kil's session server"),
        None => bail!(
            "Kil's session server is not running ({}); open Kil first",
            socket_path.display()
        ),
    }
}

/// Holds one idle connection to the session server so it does not hit its
/// idle timeout and exit between the QR being printed and a phone connecting.
/// The server treats it as a client; it sends only a `Hello` frame.
async fn keep_alive(socket_path: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path).await?;
    log::info!("keep-alive connected to {}", socket_path.display());
    // Hello { protocol: 2 } — the wire protocol's very first frame: a u32
    // length of 5 (one kind byte + four protocol bytes), kind 1, payload.
    stream.write_all(&[5, 0, 0, 0, 1, 2, 0, 0, 0]).await?;
    tokio::spawn(async move {
        let mut buf = [0u8; 1024];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    Ok(())
}

async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    socket_path: PathBuf,
    auth: Arc<Mutex<Auth>>,
    ttl: Duration,
) -> Result<()> {
    let started = std::time::Instant::now();
    loop {
        let (tcp, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let auth = Arc::clone(&auth);
        let socket_path = socket_path.clone();
        tokio::spawn(async move {
            if let Err(err) =
                handle_connection(acceptor, tcp, &socket_path, auth, ttl, started).await
            {
                log::info!("remote connection from {peer} ended: {err:#}");
            }
        });
    }
}

async fn handle_connection(
    acceptor: TlsAcceptor,
    tcp: TcpStream,
    socket_path: &Path,
    auth: Arc<Mutex<Auth>>,
    ttl: Duration,
    started: std::time::Instant,
) -> Result<()> {
    let tls = acceptor.accept(tcp).await.context("TLS handshake")?;
    let mut reader = BufReader::new(tls);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .context("read the pairing line")?;

    let line = line.trim();

    // The pairing code expires; an issued token does not, so a phone that is
    // already paired can keep opening control/attach connections for as long
    // as the bridge runs.
    if line.starts_with("KIL1 ") && started.elapsed() > ttl {
        reply(&mut reader, "ERR expired").await?;
        return Ok(());
    }

    let Some(token) = authenticate(line, &auth) else {
        reply(&mut reader, "ERR rejected").await?;
        return Ok(());
    };

    let unix = UnixStream::connect(socket_path)
        .await
        .context("connect to Kil's session server")?;
    reply(&mut reader, &format!("OK {token}")).await?;

    // The client only starts the wire protocol after reading `OK`, so nothing
    // is left inside the buffered reader. From here on it is a pure byte pump.
    let tls = reader.into_inner();
    let (mut a, mut b) = split(tls);
    let (mut c, mut d) = split(unix);
    let mut t1 = tokio::spawn(async move {
        let _ = copy(&mut a, &mut d).await;
    });
    let mut t2 = tokio::spawn(async move {
        let _ = copy(&mut c, &mut b).await;
    });
    tokio::select! {
        _ = &mut t1 => t2.abort(),
        _ = &mut t2 => t1.abort(),
    }
    Ok(())
}

async fn reply(
    reader: &mut BufReader<tokio_rustls::server::TlsStream<TcpStream>>,
    text: &str,
) -> Result<()> {
    let writer = reader.get_mut();
    writer.write_all(text.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn authenticate(line: &str, auth: &Mutex<Auth>) -> Option<String> {
    let mut guard = auth.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(code) = line.strip_prefix("KIL1 ") {
        if guard.token.is_some() {
            return None; // The one-time code is already spent.
        }
        if constant_time_eq(code.as_bytes(), guard.code.as_bytes()) {
            let token = random_token().ok()?;
            guard.token = Some(token.clone());
            return Some(token);
        }
        return None;
    }
    if let Some(token) = line.strip_prefix("KIL2 ") {
        let expected = guard.token.as_ref()?;
        return constant_time_eq(token.as_bytes(), expected.as_bytes()).then(|| expected.clone());
    }
    None
}

/// Compares two byte slices without an early exit on the first difference.
/// `verify_slices_are_equal` is deprecated in ring, and this is all we need.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Prints the QR and the human-readable pairing details.
fn print_qr(payload: &str, code: &str, fingerprint: &str, dir: &Path) {
    let image = match qrcode::QrCode::new(payload.as_bytes()) {
        Ok(qr) => qr
            .render::<unicode::Dense1x2>()
            .quiet_zone(true)
            .module_dimensions(1, 1)
            .build(),
        Err(err) => format!("(QR failed to render: {err})"),
    };
    println!();
    println!("{image}");
    println!();
    println!("Agent on Edge — direct connection");
    println!("  pairing code:  {code}");
    println!("  fingerprint:  sha256:{fingerprint}");
    println!("  session dir:   {}", dir.display());
    println!("  paste URL:     {payload}");
    println!("  The code is valid once and expires shortly after printing.");
    println!();
}

/// The LAN address the iPhone should dial; the UDP trick picks the interface
/// used by the default route without sending a packet.
fn lan_ip() -> Result<String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect("8.8.8.8:80")?;
    Ok(socket.local_addr()?.ip().to_string())
}

const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

fn random_code() -> Result<String> {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 6];
    rng.fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("system RNG failed"))?;
    Ok(bytes
        .iter()
        .map(|b| CODE_ALPHABET[usize::from(*b) % CODE_ALPHABET.len()] as char)
        .collect())
}

fn random_token() -> Result<String> {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 16];
    rng.fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("system RNG failed"))?;
    Ok(to_hex(&bytes))
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[usize::from(b >> 4)] as char);
        s.push(HEX[usize::from(b & 0x0f)] as char);
    }
    s
}

fn fingerprint(cert: &CertificateDer<'_>) -> String {
    to_hex(digest(&SHA256, cert.as_ref()).as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth_with(code: &str, token: Option<&str>) -> Mutex<Auth> {
        Mutex::new(Auth {
            code: code.to_owned(),
            token: token.map(str::to_owned),
        })
    }

    #[test]
    fn the_first_connection_spends_the_code_and_gets_a_token() {
        let auth = auth_with("ABCDEF", None);
        let token = authenticate("KIL1 ABCDEF", &auth).expect("the code should work");
        assert_eq!(token.len(), 32);
        // A second phone with the same QR is rejected.
        assert!(authenticate("KIL1 ABCDEF", &auth).is_none());
        // The issued token reconnects.
        assert_eq!(
            authenticate(&format!("KIL2 {token}"), &auth).as_deref(),
            Some(token.as_str())
        );
    }

    #[test]
    fn a_wrong_code_is_rejected_and_leaves_no_token() {
        let auth = auth_with("ABCDEF", None);
        assert!(authenticate("KIL1 WRONG", &auth).is_none());
        assert!(authenticate("KIL2 anything", &auth).is_none());
    }

    #[test]
    fn a_bogus_line_is_rejected() {
        let auth = auth_with("ABCDEF", None);
        assert!(authenticate("HELLO", &auth).is_none());
    }

    #[test]
    fn the_code_alphabet_skips_ambiguous_characters() {
        assert!(!CODE_ALPHABET.contains(&b'0'));
        assert!(!CODE_ALPHABET.contains(&b'1'));
        assert!(!CODE_ALPHABET.contains(&b'I'));
        assert!(!CODE_ALPHABET.contains(&b'O'));
    }
}
