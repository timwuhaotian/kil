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
//! - **Public** (`--public`): the bridge runs the user's own Cloudflare
//!   Tunnel (`cloudflared`, remotely-managed on their account and domain,
//!   outbound-only, so no router setup) to a loopback WebSocket shim, and
//!   the pinned TLS byte stream rides inside WebSocket binary frames. The
//!   QR carries the tunnel's stable hostname and works from anywhere — and
//!   also carries the LAN address, which the phone tries first at home —
//!   and the tunnel closes when the bridge stops. Cloudflare relays
//!   ciphertext only: the TLS session still ends at this bridge, pinned by
//!   the QR fingerprint.
//!
//! The [`Bridge`] type runs the same machinery inside the Kil app, so the
//! Settings window can show the QR without a separate command line.

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdTcpListener, UdpSocket},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use qrcode::render::unicode;
use ring::{
    digest::{SHA1_FOR_LEGACY_USE_ONLY, SHA256, digest},
    rand::{SecureRandom, SystemRandom},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, copy, split},
    net::{TcpListener, TcpStream, UnixStream},
    process::{Child, Command},
    sync::{mpsc, watch},
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
    /// A binary that can run `--server <dir>` if no session server is running.
    pub launcher: Option<PathBuf>,
    /// Expose the bridge through the user's own Cloudflare Tunnel so the QR
    /// works from anywhere (requires `cloudflared`; no router setup).
    pub public: bool,
    /// The public hostname of the user's own Cloudflare Tunnel (e.g.
    /// `kil.example.com`); it goes in the QR. Required when `public`.
    pub tunnel_hostname: Option<String>,
    /// The token of the user's own remotely-managed Cloudflare Tunnel,
    /// handed to `cloudflared` via its environment. Required when `public`.
    pub tunnel_token: Option<String>,
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

/// A fresh self-signed identity for the tunnel: the SHA-256 fingerprint of
/// the certificate the phone pins (it goes in the QR), and the acceptor that
/// serves it. A new one per bridge start is what makes **New QR** a re-pair.
fn tls_identity() -> Result<(String, TlsAcceptor)> {
    let certified = rcgen::generate_simple_self_signed(vec!["kil".into()])
        .context("generate a self-signed certificate")?;
    let fingerprint = fingerprint(certified.cert.der());
    let key = PrivateKeyDer::Pkcs8(certified.key_pair.serialize_der().into());
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certified.cert.der().clone()], key)
        .context("build the TLS server config")?;
    Ok((fingerprint, TlsAcceptor::from(Arc::new(tls))))
}

/// The one-time pairing secret, the token issued after the first use, and a
/// per-IP failure ledger that throttles pairing-code brute force. The code
/// never expires: it is good until the bridge stops, and the first phone to
/// spend it holds the token from then on.
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

/// The user's tunnel for a public bridge — its hostname and token, trimmed —
/// or `None` for a LAN-only bridge. Public mode rides that tunnel, so both
/// halves are required; a blank string is no tunnel at all.
fn public_tunnel(config: &Config) -> Result<Option<(String, String)>> {
    if !config.public {
        return Ok(None);
    }
    let hostname = config
        .tunnel_hostname
        .as_deref()
        .map(str::trim)
        .filter(|hostname| !hostname.is_empty());
    let token = config
        .tunnel_token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let (Some(hostname), Some(token)) = (hostname, token) else {
        bail!(
            "Public internet needs your Cloudflare Tunnel: set tunnel_hostname and tunnel_token in config.toml"
        );
    };
    Ok(Some((hostname.to_owned(), token.to_owned())))
}

/// An embeddable bridge: the same TLS tunnel as `kil --remote`, running on a
/// background thread so the Kil app can show the QR in its Settings window.
pub struct Bridge {
    auth: Arc<Mutex<Auth>>,
    shutdown: watch::Sender<bool>,
    qr: Arc<Mutex<Option<QrPayload>>>,
    error: Arc<Mutex<Option<String>>>,
    /// Whether the session server understands a phone's declaration, probed
    /// once when the bridge starts: a server that answers it also answers the
    /// app's presence queries, so this is whether a phone is marked at all.
    tags_remote: Arc<AtomicBool>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Bridge {
    /// Starts the bridge on a background thread. The QR payload becomes
    /// available through [`Bridge::qr`] once the listener and (in `--public`
    /// mode) the tunnel are ready.
    ///
    /// # Errors
    /// If public mode lacks the user's Cloudflare Tunnel, the TLS identity
    /// cannot be generated, or the port cannot be bound.
    pub fn start(config: &Config) -> Result<Arc<Self>> {
        let tunnel_config = public_tunnel(config)?;

        // If a provider is already installed (GPUI's HTTP client installs one), keep it.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let (fingerprint, acceptor) = tls_identity()?;
        let code = random_code()?;
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
        let tags_remote = Arc::new(AtomicBool::new(false));
        let socket_path = kil_term::server::socket_path(&config.dir);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let config = config.clone();
        let thread_auth = Arc::clone(&auth);
        let qr_ready = Arc::clone(&qr);
        let error_slot = Arc::clone(&error);
        let thread_tags = Arc::clone(&tags_remote);
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
                    // Probed once, not once per phone connection: the answer is
                    // the server's, and the app's presence poll needs it too.
                    let tags = server_tags_remote(&socket_path).await;
                    thread_tags.store(tags, Ordering::Release);

                    let tls_port = listener.local_addr()?.port();
                    let mut tunnel = match tunnel_config.as_ref() {
                        Some((_, token)) => Some(PublicTunnel::start(tls_port, token).await?),
                        None => None,
                    };
                    let lan_host = match config.host.as_deref() {
                        Some(host) => host.to_owned(),
                        None => lan_ip()?.to_string(),
                    };
                    let (host, port, url) = if let Some((hostname, _)) = tunnel_config.as_ref() {
                        let host = hostname.clone();
                        let url = format!(
                            "kil://{host}:443?code={code}&fp=sha256:{fingerprint}&v=1&tun=ws&lan={lan_host}:{tls_port}"
                        );
                        (host, 443, url)
                    } else {
                        let url = format!(
                            "kil://{lan_host}:{tls_port}?code={code}&fp=sha256:{fingerprint}&v=1"
                        );
                        (lan_host, tls_port, url)
                    };
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
                        Relay {
                            socket_path,
                            tags_remote: tags,
                        },
                        thread_auth,
                        shutdown_rx,
                    )
                    .await;
                    if let Some(tunnel) = tunnel.as_mut() {
                        tunnel.stop().await;
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
            tags_remote,
            thread: Mutex::new(Some(thread)),
        }))
    }

    /// Stops the bridge: signals the server loop, joins the background thread,
    /// and closes the tunnel if one was created.
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

    /// Whether the session server reports a phone's presence at all. The
    /// `Remote` declaration it understands and the `Watchers` query the app
    /// polls arrived in the same Kil, so one probe answers both: `false` is a
    /// server from an older Kil, which marks no pane until it is replaced.
    pub fn reports_presence(&self) -> bool {
        self.tags_remote.load(Ordering::Acquire)
    }

    /// A fatal setup or runtime error, if any, for the UI to show.
    pub fn error(&self) -> Option<String> {
        self.error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// What every relayed connection needs about Kil's session server: its
/// socket, and whether it understands a phone's declaration (probed once when
/// the bridge starts, since a server that answers it also answers the app's
/// presence queries).
#[derive(Clone)]
struct Relay {
    socket_path: PathBuf,
    tags_remote: bool,
}

/// Runs the bridge as a command line: prints the QR, waits for Ctrl-C, stops.
///
/// # Errors
/// If the bridge cannot start or the QR payload never becomes ready.
pub fn run(config: &Config) -> Result<()> {
    let bridge = Bridge::start(config)?;
    // Public mode waits on cloudflared registering the tunnel (30s budget
    // there; normally a matter of seconds).
    let timeout = Duration::from_secs(if config.public { 45 } else { 15 });
    let qr = wait_for_qr(&bridge, timeout)?;
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

/// The fixed loopback port of the WebSocket shim: the ingress rule in the
/// Cloudflare dashboard points at `http://127.0.0.1:48242`, so it cannot be
/// random.
const SHIM_PORT: u16 = 48242;

/// The user's own Cloudflare Tunnel to the bridge: `cloudflared` dials out
/// with their tunnel's token, so no router or NAT setup is involved, and
/// the ingress rule on their account forwards the phone's `wss://`
/// connection to the loopback WebSocket shim below. The pinned TLS stream
/// rides inside the WebSocket frames, so Cloudflare relays ciphertext only.
struct PublicTunnel {
    child: Child,
}

impl PublicTunnel {
    async fn start(tls_port: u16, token: &str) -> Result<Self> {
        let shim = TcpListener::bind((Ipv4Addr::LOCALHOST, SHIM_PORT))
            .await
            .with_context(|| format!("bind the WebSocket shim on 127.0.0.1:{SHIM_PORT}"))?;
        tokio::spawn(ws_shim_loop(shim, tls_port));

        let Some(cloudflared) = find_cloudflared() else {
            bail!("Public internet needs cloudflared (brew install cloudflared)");
        };
        // The token travels only in the environment, never on the command
        // line where other processes could read it.
        let mut child = Command::new(cloudflared)
            .args(["tunnel", "--no-autoupdate", "run"])
            .env("TUNNEL_TOKEN", token)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("start cloudflared")?;
        wait_for_tunnel_ready(&mut child).await?;
        Ok(Self { child })
    }

    async fn stop(&mut self) {
        if let Err(err) = self.child.kill().await {
            log::warn!("cloudflared: failed to stop the tunnel: {err}");
        }
        let _ = self.child.wait().await;
    }
}

/// `cloudflared` on the `PATH` or where Homebrew puts it — a GUI launch
/// often has neither `/opt/homebrew/bin` nor `/usr/local/bin` on its `PATH`.
fn find_cloudflared() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("cloudflared");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    [
        "/opt/homebrew/bin/cloudflared",
        "/usr/local/bin/cloudflared",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|candidate| candidate.is_file())
}

/// Drains `cloudflared`'s output until it registers a tunnel connection,
/// then keeps draining in the background so the pipe never blocks the child.
async fn wait_for_tunnel_ready(child: &mut Child) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<String>(128);
    if let Some(stdout) = child.stdout.take() {
        forward_lines(stdout, tx.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        forward_lines(stderr, tx);
    }
    let mut recent: Vec<String> = Vec::new();
    let found = tokio::time::timeout(
        Duration::from_secs(30),
        read_until_registered(&mut rx, &mut recent),
    )
    .await;
    match found {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            let _ = child.kill().await;
            return Err(err);
        }
        Err(_) => {
            let _ = child.kill().await;
            bail!(
                "cloudflared did not register a tunnel connection in 30s; its last output:\n{}",
                recent.join("\n")
            );
        }
    }
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            log::debug!("cloudflared: {line}");
        }
    });
    Ok(())
}

async fn read_until_registered(
    rx: &mut mpsc::Receiver<String>,
    recent: &mut Vec<String>,
) -> Result<()> {
    while let Some(line) = rx.recv().await {
        log::debug!("cloudflared: {line}");
        if tunnel_registered(&line) {
            return Ok(());
        }
        recent.push(line);
        if recent.len() > 20 {
            recent.remove(0);
        }
    }
    bail!(
        "cloudflared exited before registering a tunnel connection; its last output:\n{}",
        recent.join("\n")
    )
}

fn forward_lines<R>(reader: R, tx: mpsc::Sender<String>)
where
    R: AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).await.is_err() {
                break;
            }
        }
    });
}

/// Whether a line of `cloudflared` output says a tunnel connection is up.
fn tunnel_registered(line: &str) -> bool {
    line.contains("Registered tunnel connection")
}

/// The WebSocket shim the tunnel points at: each upgrade dials the bridge's
/// own TLS listener on loopback and pumps bytes between WebSocket binary
/// payloads and the TCP stream, so the pairing lines and wire frames inside
/// the TLS session never know the tunnel exists.
///
/// Tunneled connections reach the TLS listener from 127.0.0.1, so the per-IP
/// pairing throttle buckets them all together. That is acceptable: failed
/// pairings are still delayed, the bucket just is not per-phone.
async fn ws_shim_loop(listener: TcpListener, tls_port: u16) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            if let Err(err) = handle_ws_connection(stream, tls_port).await {
                log::info!("tunneled connection ended: {err:#}");
            }
        });
    }
}

/// Answers the HTTP request: a WebSocket upgrade gets a 101 (and `true`),
/// anything else a plain health-check response.
async fn ws_upgrade(stream: &mut TcpStream) -> Result<bool> {
    let headers = read_http_headers(stream).await?;
    let Some(key) = header_value(&headers, "sec-websocket-key") else {
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\nConnection: close\r\n\r\nkil bridge",
            )
            .await?;
        return Ok(false);
    };
    let accept = ws_accept(&key);
    stream
        .write_all(
            format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await?;
    Ok(true)
}

async fn handle_ws_connection(mut stream: TcpStream, tls_port: u16) -> Result<()> {
    if !ws_upgrade(&mut stream).await? {
        return Ok(());
    }

    let tcp = TcpStream::connect((Ipv4Addr::LOCALHOST, tls_port))
        .await
        .context("dial the bridge's TLS listener")?;
    let (mut ws_read, mut ws_write) = stream.into_split();
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    // One writer task owns the WebSocket side: TCP bytes become binary
    // frames, and pongs/close from the frame loop go through the same queue.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if ws_write.write_all(&frame).await.is_err() {
                break;
            }
        }
    });
    let pump_tx = out_tx.clone();
    let pump = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 << 10];
        loop {
            match tcp_read.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    log::debug!("ws shim: tcp read ended");
                    // The TLS side ended: close the WebSocket too, so the
                    // phone does not hang on a dead tunnel.
                    let _ = pump_tx.send(encode_ws_frame(8, &[])).await;
                    break;
                }
                Ok(n) => {
                    if pump_tx.send(encode_ws_frame(2, &buf[..n])).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // The payloads are one byte stream: continuation and binary (and,
    // defensively, text) payloads just append to the TCP side — there is no
    // message to reassemble.
    loop {
        let (opcode, payload) = match read_ws_frame(&mut ws_read).await {
            Ok(frame) => frame,
            Err(err) => {
                log::debug!("ws shim: frame read ended: {err:#}");
                break;
            }
        };
        if (0..=2).contains(&opcode) {
            if tcp_write.write_all(&payload).await.is_err() {
                break;
            }
        } else if opcode == 8 {
            let _ = out_tx.send(encode_ws_frame(8, &payload)).await;
            break;
        } else if opcode == 9 && out_tx.send(encode_ws_frame(10, &payload)).await.is_err() {
            break;
        }
    }
    pump.abort();
    writer.abort();
    Ok(())
}

/// HTTP headers up to the blank line, read a byte at a time so nothing past
/// the headers is swallowed before the frame loop takes over.
async fn read_http_headers(stream: &mut TcpStream) -> Result<String> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await?;
        bytes.push(byte[0]);
        if bytes.len() > (16 << 10) {
            bail!("HTTP headers too large");
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn header_value(headers: &str, name: &str) -> Option<String> {
    headers.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

/// The `Sec-WebSocket-Accept` answer for a client's key (RFC 6455 §1.3):
/// SHA-1 of the key and the protocol's fixed GUID, base64-encoded.
fn ws_accept(key: &str) -> String {
    let mut input = key.to_owned();
    input.push_str("258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(digest(&SHA1_FOR_LEGACY_USE_ONLY, input.as_bytes()).as_ref())
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        for shift in [18, 12, 6, 0] {
            out.push(BASE64[usize::try_from((triple >> shift) & 0x3f).unwrap_or(0)] as char);
        }
        for _ in chunk.len()..3 {
            out.pop();
        }
        for _ in chunk.len()..3 {
            out.push('=');
        }
    }
    out
}

/// One client frame, unmasked: its opcode and payload. Client frames are
/// masked; a single frame is capped at 16 MiB.
async fn read_ws_frame<R>(reader: &mut R) -> Result<(u8, Vec<u8>)>
where
    R: AsyncRead + Unpin,
{
    let mut head = [0u8; 2];
    reader.read_exact(&mut head).await?;
    let opcode = head[0] & 0x0f;
    let masked = head[1] & 0x80 != 0;
    let len: u64 = match head[1] & 0x7f {
        126 => {
            let mut ext = [0u8; 2];
            reader.read_exact(&mut ext).await?;
            u64::from(u16::from_be_bytes(ext))
        }
        127 => {
            let mut ext = [0u8; 8];
            reader.read_exact(&mut ext).await?;
            u64::from_be_bytes(ext)
        }
        len => u64::from(len),
    };
    if len > (16 << 20) {
        bail!("WebSocket frame too large");
    }
    let mask = if masked {
        let mut mask = [0u8; 4];
        reader.read_exact(&mut mask).await?;
        Some(mask)
    } else {
        None
    };
    let len = usize::try_from(len).map_err(|_| anyhow::anyhow!("WebSocket frame too large"))?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    if let Some(mask) = mask {
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
    }
    Ok((opcode, payload))
}

/// One unmasked server frame (FIN set) carrying `payload`.
fn encode_ws_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | opcode);
    match payload.len() {
        0..=125 => frame.push(u8::try_from(payload.len()).unwrap_or(125)),
        126..=65_535 => {
            frame.push(126);
            let len = u16::try_from(payload.len()).unwrap_or(u16::MAX);
            frame.extend_from_slice(&len.to_be_bytes());
        }
        _ => {
            frame.push(127);
            let len = u64::try_from(payload.len()).unwrap_or(u64::MAX);
            frame.extend_from_slice(&len.to_be_bytes());
        }
    }
    frame.extend_from_slice(payload);
    frame
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
    relay: Relay,
    auth: Arc<Mutex<Auth>>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (tcp, peer) = accepted?;
                let acceptor = acceptor.clone();
                let auth = Arc::clone(&auth);
                let relay = relay.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(acceptor, tcp, peer, relay, auth).await {
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
    relay: Relay,
    auth: Arc<Mutex<Auth>>,
) -> Result<()> {
    let tls = acceptor.accept(tcp).await.context("TLS handshake")?;
    let mut reader = BufReader::new(tls);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .context("read the pairing line")?;

    let line = line.trim();

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

    let mut unix = UnixStream::connect(&relay.socket_path)
        .await
        .context("connect to Kil's session server")?;
    // A supporting server tags the connection a phone's from this declaration, so its
    // input reads as driving; an older one never sees it, and the relay then behaves
    // exactly as before.
    if relay.tags_remote {
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
///
/// Asked once when the bridge starts: a server from the same generation answers the
/// app's `Watchers` query too, so this one probe decides whether a phone is reported
/// on the panes at all.
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
            "  public: this bridge is reachable from anywhere through your own Cloudflare Tunnel, on your"
        );
        println!("          account and domain — outbound-only,");
        println!("          no router setup; the tunnel closes when the bridge stops.");
        println!(
            "          Traffic stays end-to-end encrypted to Kil; Cloudflare relays ciphertext only."
        );
    }
    println!("  The code is spent by the first phone and stays good until this bridge stops.");
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

    #[test]
    fn the_accept_key_matches_the_rfc6455_vector() {
        assert_eq!(
            ws_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    /// A client frame as the phone sends it: FIN set, masked with 1,2,3,4.
    fn masked_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [1u8, 2, 3, 4];
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            0..=125 => frame.push(0x80 | u8::try_from(payload.len()).unwrap_or(125)),
            126..=65_535 => {
                frame.push(0xfe);
                let len = u16::try_from(payload.len()).unwrap_or(u16::MAX);
                frame.extend_from_slice(&len.to_be_bytes());
            }
            _ => {
                frame.push(0xff);
                let len = u64::try_from(payload.len()).unwrap_or(u64::MAX);
                frame.extend_from_slice(&len.to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(i, byte)| byte ^ mask[i % 4]),
        );
        frame
    }

    #[tokio::test]
    async fn masked_client_frames_decode_at_every_length() {
        for payload in [vec![7u8; 3], vec![7u8; 200], vec![7u8; 70_000]] {
            let frame = masked_frame(2, &payload);
            let mut reader: &[u8] = &frame;
            let (opcode, decoded) = read_ws_frame(&mut reader).await.unwrap();
            assert_eq!(opcode, 2);
            assert_eq!(decoded, payload);
        }
    }

    #[tokio::test]
    async fn server_frames_encode_unmasked_and_round_trip() {
        for payload in [vec![9u8; 3], vec![9u8; 200], vec![9u8; 70_000]] {
            let frame = encode_ws_frame(2, &payload);
            assert_eq!(frame[0], 0x82);
            assert_eq!(frame[1] & 0x80, 0, "server frames are unmasked");
            let mut reader: &[u8] = &frame;
            let (opcode, decoded) = read_ws_frame(&mut reader).await.unwrap();
            assert_eq!(opcode, 2);
            assert_eq!(decoded, payload);
        }
        assert_eq!(encode_ws_frame(2, &[0u8; 200])[1], 126);
        assert_eq!(encode_ws_frame(2, &vec![0u8; 70_000])[1], 127);
    }

    #[test]
    fn only_the_registration_line_says_the_tunnel_is_up() {
        assert!(tunnel_registered(
            "2026-01-01T00:00:00Z INF Registered tunnel connection connIndex=0 location=sjc"
        ));
        assert!(!tunnel_registered(
            "2026-01-01T00:00:00Z INF Starting tunnel tunnelID=abc"
        ));
        assert!(!tunnel_registered(
            "2026-01-01T00:00:00Z INF Connection 0 registered connIndex=0"
        ));
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

    /// A server from before the `Remote` declaration, on a socket of its own: it answers
    /// hello, then hangs up on the frame it doesn't know — as a real one from before
    /// 0.0.32 does, both to the declaration and to the app's `Watchers`.
    fn server_from_before_presence(dir: &Path) -> PathBuf {
        use std::io::{Read as _, Write as _};

        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let socket = dir.join("server.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut stream = stream;
                    // Hello { protocol: 2 }, then nothing: the next frame is unknown.
                    let _ = stream.write_all(&[5, 0, 0, 0, 1, 2, 0, 0, 0]);
                    let mut buf = [0u8; 64];
                    let _ = stream.read(&mut buf);
                });
            }
        });
        socket
    }

    fn bridge_on(dir: &Path) -> Arc<Bridge> {
        Bridge::start(&Config {
            dir: dir.to_owned(),
            port: 0,
            host: None,
            launcher: None,
            public: false,
            tunnel_hostname: None,
            tunnel_token: None,
        })
        .expect("the bridge starts")
    }

    #[tokio::test]
    async fn a_current_server_reports_a_phone_to_the_bridge() {
        let dir = std::env::temp_dir().join(format!("kil-bridge-new-{}", std::process::id()));
        kil_term::server::serve_in_thread(&dir).unwrap();
        let bridge = bridge_on(&dir);
        wait_for_qr(&bridge, Duration::from_secs(15)).unwrap();
        assert!(
            bridge.reports_presence(),
            "a server that answers the declaration reports the phone"
        );
        bridge.stop();
    }

    #[tokio::test]
    async fn a_server_from_before_presence_reports_no_phone_to_the_bridge() {
        let dir = std::env::temp_dir().join(format!("kil-bridge-old-{}", std::process::id()));
        server_from_before_presence(&dir);
        let bridge = bridge_on(&dir);
        wait_for_qr(&bridge, Duration::from_secs(15)).unwrap();
        assert!(
            !bridge.reports_presence(),
            "a server that hangs up on the declaration reports no phone"
        );
        bridge.stop();
    }

    #[tokio::test]
    async fn the_probe_falls_back_on_a_server_from_before_remote() {
        // Hello answered, then a hangup on the declaration: no declaration is sent.
        let socket = fake_server("old", vec![vec![5, 0, 0, 0, 1, 2, 0, 0, 0]]);
        assert!(!server_tags_remote(&socket).await);
    }
}
