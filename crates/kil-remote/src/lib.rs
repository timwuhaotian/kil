//! The network bridge for Agent on Edge: `kil --remote` (or the standalone
//! `kil-remote` binary).
//!
//! Kil's session server deliberately listens only on a Unix socket trusted by
//! peer UID. This bridge wraps that socket in a TLS tunnel so an iPhone can
//! drive the same panes:
//!
//! 1. The Mac prints a QR code carrying the address, a one-time pairing code
//!    and the SHA-256 fingerprint of a fresh self-signed certificate.
//! 2. Agent on Edge scans it, proves the code over TLS, and receives a session
//!    token it can reuse for its other connections.
//! 3. The bridge then does nothing but pump bytes between the TLS stream and
//!    the session server's Unix socket. It never parses terminal output, so
//!    the server's wire protocol and its `peer_uid` trust check are untouched.
//!
//! Two network modes share this design:
//!
//! - **LAN** (default): the Mac listens on a TCP port and the QR carries the
//!   Mac's LAN address.
//! - **Public** (`--public`): the bridge asks the user's own router (`UPnP` IGD)
//!   for a port mapping to the Mac's public static IP, prints a QR that works
//!   from anywhere, and removes the mapping when it stops. No cloud service is
//!   involved — the router is the user's own device.
//!
//! The [`Bridge`] type runs the same machinery inside the Kil app, so the
//! Settings window can show the QR without a separate command line.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdTcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use igd_next::{PortMappingProtocol, SearchOptions, aio};
use qrcode::render::unicode;
use ring::{
    digest::{SHA256, digest},
    rand::{SecureRandom, SystemRandom},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, copy, split},
    net::{TcpListener, TcpStream, UnixStream},
    sync::watch,
};
use tokio_rustls::TlsAcceptor;

/// How a caller wants the bridge to behave.
#[derive(Clone)]
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
    /// Expose the bridge through the router's `UPnP` port mapping so the QR
    /// works from the public internet (requires a public IP on the WAN side).
    pub public: bool,
}

/// What the phone needs to pair, ready for a QR code or a settings panel.
#[derive(Clone, Debug)]
pub struct QrPayload {
    pub host: String,
    pub port: u16,
    pub code: String,
    pub fingerprint: String,
    pub url: String,
}

/// The one-time pairing secret, the token issued after the first use, and a
/// per-IP failure ledger that throttles pairing-code brute force.
struct Auth {
    code: String,
    token: Option<String>,
    failures: HashMap<IpAddr, (u32, std::time::Instant)>,
}

impl Auth {
    /// Records a failed pairing from `ip`. Returns `true` once the IP has
    /// exceeded the per-minute budget and should be throttled hard. Call with
    /// the [`Auth`] mutex already held.
    fn note_failure(&mut self, ip: IpAddr) -> bool {
        if self.failures.len() >= 4096 {
            self.failures.clear();
        }
        let now = std::time::Instant::now();
        let (count, since) = self.failures.entry(ip).or_insert((0, now));
        if now.duration_since(*since) > Duration::from_secs(60) {
            *count = 0;
            *since = now;
        }
        *count += 1;
        *count > 5
    }
}

/// An embeddable bridge: the same TLS tunnel as `kil --remote`, running on a
/// background thread so the Kil app can show the QR in its Settings window.
pub struct Bridge {
    auth: Arc<Mutex<Auth>>,
    shutdown: watch::Sender<bool>,
    qr: Arc<Mutex<Option<QrPayload>>>,
    error: Arc<Mutex<Option<String>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Bridge {
    /// Starts the bridge on a background thread. The QR payload becomes
    /// available through [`Bridge::qr`] once the listener and (in `--public`
    /// mode) the router's port mapping are ready.
    ///
    /// # Errors
    /// If the TLS identity cannot be generated or the port cannot be bound.
    pub fn start(config: &Config) -> Result<Arc<Self>> {
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
        let listener = StdTcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port)))
            .with_context(|| format!("bind port {}", config.port))?;
        listener.set_nonblocking(true)?;

        let auth = Arc::new(Mutex::new(Auth {
            code: code.clone(),
            token: None,
            failures: HashMap::new(),
        }));
        let qr = Arc::new(Mutex::new(None));
        let error = Arc::new(Mutex::new(None));
        let socket_path = kil_term::server::socket_path(&config.dir);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let config = config.clone();
        let thread_auth = Arc::clone(&auth);
        let qr_ready = Arc::clone(&qr);
        let error_slot = Arc::clone(&error);
        let thread = std::thread::Builder::new()
            .name("kil-remote".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Runtime::new() {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        *error_slot.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(format!("{err:#}"));
                        return;
                    }
                };
                let result = runtime.block_on(async {
                    let listener =
                        TcpListener::from_std(listener).context("register the TCP listener")?;
                    ensure_server(&socket_path, config.launcher.as_deref(), &config.dir).await?;
                    keep_alive(&socket_path).await?;

                    let public = if config.public {
                        Some(PublicMapping::create(listener.local_addr()?).await?)
                    } else {
                        None
                    };
                    let (host, port) = if let Some(p) = public.as_ref() {
                        (p.external_ip.to_string(), p.external_port)
                    } else {
                        let host = match config.host.as_deref() {
                            Some(host) => host.to_owned(),
                            None => lan_ip()?.to_string(),
                        };
                        (host, listener.local_addr()?.port())
                    };
                    let url =
                        format!("kil://{host}:{port}?code={code}&fp=sha256:{fingerprint}&v=1");
                    *qr_ready.lock().unwrap_or_else(PoisonError::into_inner) = Some(QrPayload {
                        host,
                        port,
                        code,
                        fingerprint,
                        url,
                    });

                    let result = serve(
                        listener,
                        acceptor,
                        socket_path,
                        thread_auth,
                        config.ttl,
                        shutdown_rx,
                    )
                    .await;
                    if let Some(public) = public.as_ref() {
                        public.remove().await;
                    }
                    result
                });
                if let Err(err) = result {
                    *error_slot.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(format!("{err:#}"));
                }
            })?;

        Ok(Arc::new(Self {
            auth,
            shutdown: shutdown_tx,
            qr,
            error,
            thread: Mutex::new(Some(thread)),
        }))
    }

    /// Stops the bridge: signals the server loop, joins the background thread,
    /// and removes the router port mapping if one was created.
    pub fn stop(&self) {
        let _ = self.shutdown.send(true);
        if let Some(handle) = self
            .thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let _ = handle.join();
        }
    }

    /// The QR payload once the bridge is ready; `None` while it is still
    /// setting up (or after it stopped).
    pub fn qr(&self) -> Option<QrPayload> {
        self.qr
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Whether an iPhone has already spent the one-time pairing code.
    pub fn paired(&self) -> bool {
        self.auth
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .token
            .is_some()
    }

    /// A fatal setup or runtime error, if any, for the UI to show.
    pub fn error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Runs the bridge as a command line: prints the QR, waits for Ctrl-C, stops.
///
/// # Errors
/// If the bridge cannot start or the QR payload never becomes ready.
pub fn run(config: &Config) -> Result<()> {
    let bridge = Bridge::start(config)?;
    let qr = wait_for_qr(&bridge, Duration::from_secs(15))?;
    print_qr(
        &qr.url,
        &qr.code,
        &qr.fingerprint,
        &config.dir,
        config.public,
    );

    let runtime = tokio::runtime::Runtime::new().context("start the tokio runtime")?;
    runtime.block_on(tokio::signal::ctrl_c())?;
    bridge.stop();
    Ok(())
}

fn wait_for_qr(bridge: &Bridge, timeout: Duration) -> Result<QrPayload> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(qr) = bridge.qr() {
            return Ok(qr);
        }
        if let Some(err) = bridge.error() {
            bail!("{err}");
        }
        if std::time::Instant::now() >= deadline {
            bail!("the bridge did not become ready in time");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A `UPnP` port mapping set up on the user's own router. No third-party or
/// cloud service is involved: the router is the user's device, and the bridge
/// only ever forwards TLS ciphertext through it.
struct PublicMapping {
    gateway: aio::Gateway<aio::tokio::Tokio>,
    external_ip: IpAddr,
    external_port: u16,
}

impl PublicMapping {
    async fn create(listener_local: SocketAddr) -> Result<Self> {
        let gateway = aio::tokio::search_gateway(SearchOptions::default())
            .await
            .context("UPnP: no router found — is UPnP enabled on the router?")?;
        let external_ip = gateway
            .get_external_ip()
            .await
            .context("UPnP: could not read the router's external IP")?;
        let lan = lan_ip().context("UPnP: could not determine this Mac's LAN address")?;
        let local_addr = SocketAddr::new(lan, listener_local.port());
        let external_port = gateway
            .add_any_port(PortMappingProtocol::TCP, local_addr, 0, "kil --remote")
            .await
            .context("UPnP: could not add the port mapping")?;
        Ok(Self {
            gateway,
            external_ip,
            external_port,
        })
    }

    async fn remove(&self) {
        if let Err(err) = self
            .gateway
            .remove_port(PortMappingProtocol::TCP, self.external_port)
            .await
        {
            log::warn!("UPnP: failed to remove the port mapping: {err}");
        }
    }
}

/// Makes sure Kil's session server is up: the Kil app normally runs it, and
/// `kil --remote` can start it itself (the standalone binary cannot). The
/// server daemonizes, so after launching we wait for its socket to appear.
async fn ensure_server(socket_path: &Path, launcher: Option<&Path>, dir: &Path) -> Result<()> {
    if UnixStream::connect(socket_path).await.is_ok() {
        log::info!(
            "session server already running at {}",
            socket_path.display()
        );
        return Ok(());
    }
    log::info!("session server not running; starting it");
    let Some(exe) = launcher else {
        bail!(
            "Kil's session server is not running ({}); open Kil first",
            socket_path.display()
        )
    };
    kil_term::server::launch(exe, dir).context("start Kil's session server")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if UnixStream::connect(socket_path).await.is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "Kil's session server did not start ({}); open Kil first",
                socket_path.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
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
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let started = std::time::Instant::now();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (tcp, peer) = accepted?;
                let acceptor = acceptor.clone();
                let auth = Arc::clone(&auth);
                let socket_path = socket_path.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(acceptor, tcp, peer, &socket_path, auth, ttl, started).await {
                        log::info!("remote connection from {peer} ended: {err:#}");
                    }
                });
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow_and_update() {
                    log::info!("stopping (shutdown)");
                    return Ok(());
                }
            }
        }
    }
}

async fn handle_connection(
    acceptor: TlsAcceptor,
    tcp: TcpStream,
    peer: SocketAddr,
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
        // Public exposure makes the pairing code brute-forceable in theory;
        // each failed attempt costs two seconds, and an IP that fails more
        // than five times a minute is throttled to thirty seconds per try.
        let throttled = {
            let mut guard = auth.lock().unwrap_or_else(PoisonError::into_inner);
            guard.note_failure(peer.ip())
        };
        tokio::time::sleep(if throttled {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(2)
        })
        .await;
        reply(&mut reader, "ERR rejected").await?;
        return Ok(());
    };

    let mut unix = UnixStream::connect(socket_path)
        .await
        .context("connect to Kil's session server")?;
    // A supporting server tags the connection a phone's from this declaration, so its
    // input reads as driving; an older one never sees it, and the relay then behaves
    // exactly as before.
    if server_tags_remote(socket_path).await {
        unix.write_all(REMOTE_DECLARATION)
            .await
            .context("declare the relay a phone's")?;
    }
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

/// The wire protocol's `Remote` frame (kind 19, empty): a u32 length of 1, the kind, no
/// payload. Sent on a relay connection before the phone's own bytes.
const REMOTE_DECLARATION: &[u8] = &[1, 0, 0, 0, 19];

/// Whether the session server understands the `Remote` declaration: a probe connection
/// says hello, declares itself remote, and asks for the sessions. A server from before
/// the declaration hangs up on the unknown frame instead of answering, which reads as
/// no support.
async fn server_tags_remote(socket_path: &Path) -> bool {
    tokio::time::timeout(Duration::from_secs(2), probe_remote(socket_path))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false)
}

async fn probe_remote(socket_path: &Path) -> Result<bool> {
    let mut stream = UnixStream::connect(socket_path).await?;
    // Hello { protocol: 2 }.
    stream.write_all(&[5, 0, 0, 0, 1, 2, 0, 0, 0]).await?;
    if read_kind(&mut stream).await? != 1 {
        return Ok(false);
    }
    stream.write_all(REMOTE_DECLARATION).await?;
    // List; answered with Sessions (kind 9) only if the declaration didn't hang up.
    stream.write_all(&[1, 0, 0, 0, 2]).await?;
    Ok(read_kind(&mut stream).await? == 9)
}

/// The kind byte of the next frame, discarding its payload.
async fn read_kind(stream: &mut UnixStream) -> Result<u8> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > (16 << 20) {
        bail!("bad frame length");
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await?;
    Ok(body[0])
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
fn print_qr(payload: &str, code: &str, fingerprint: &str, dir: &Path, public: bool) {
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
    if public {
        println!(
            "  public: this bridge is reachable from the internet through your router's UPnP mapping;"
        );
        println!("          the mapping is removed when this process stops.");
    }
    println!("  The code is valid once and expires shortly after printing.");
    println!();
}

/// The LAN address the iPhone should dial; the UDP trick picks the interface
/// used by the default route without sending a packet.
fn lan_ip() -> Result<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect("8.8.8.8:80")?;
    Ok(socket.local_addr()?.ip())
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
            failures: HashMap::new(),
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

    #[test]
    fn failure_throttling_kicks_in_after_the_budget() {
        let auth = auth_with("ABCDEF", None);
        let ip: IpAddr = "203.0.113.7"
            .parse()
            .unwrap_or_else(|_| Ipv4Addr::LOCALHOST.into());
        let mut guard = auth.lock().unwrap_or_else(PoisonError::into_inner);
        for _ in 0..5 {
            assert!(!guard.note_failure(ip), "first five failures are budgeted");
        }
        assert!(guard.note_failure(ip), "the sixth failure is throttled");
    }

    /// A scripted session server on a socket of its own: `script` answers the probe's
    /// frames, then the connection hangs up.
    fn fake_server(name: &str, script: Vec<Vec<u8>>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kil-probe-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            for reply in script {
                let mut len = [0u8; 4];
                if stream.read_exact(&mut len).await.is_err() {
                    return;
                }
                let len = u32::from_le_bytes(len) as usize;
                let mut body = vec![0u8; len];
                if stream.read_exact(&mut body).await.is_err() {
                    return;
                }
                if stream.write_all(&reply).await.is_err() {
                    return;
                }
            }
        });
        socket
    }

    #[tokio::test]
    async fn the_probe_sees_a_server_that_tags_remote() {
        // Hello answered, the declaration swallowed, List answered with Sessions.
        let socket = fake_server(
            "new",
            vec![
                vec![5, 0, 0, 0, 1, 2, 0, 0, 0],
                vec![],
                vec![5, 0, 0, 0, 9, 0, 0, 0, 0],
            ],
        );
        assert!(server_tags_remote(&socket).await);
    }

    #[tokio::test]
    async fn the_probe_falls_back_on_a_server_from_before_remote() {
        // Hello answered, then a hangup on the declaration: no declaration is sent.
        let socket = fake_server("old", vec![vec![5, 0, 0, 0, 1, 2, 0, 0, 0]]);
        assert!(!server_tags_remote(&socket).await);
    }
}
