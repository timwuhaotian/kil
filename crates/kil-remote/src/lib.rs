//! The network bridge for Agent on Edge: `kil --remote` (or the standalone
//! `kil-remote` binary).
//!
//! Kil's session server deliberately listens only on a Unix socket trusted by
//! peer UID. This bridge wraps that socket in a TLS tunnel so an iPhone can
//! drive the same panes:
//!
//! 1. The Mac prints a QR code carrying the address, a one-time pairing code
//!    and the SHA-256 fingerprint of a persistent self-signed certificate
//!    (stored owner-only under the session dir; only a pairing reset rotates it).
//! 2. Agent on Edge scans it, proves the code over TLS, and receives a device
//!    token. The Mac keeps only a verifier — SHA-256 of the token bytes — so
//!    later connections prove the token by challenge-response (`KIL3`) without
//!    the token ever being stored on the Mac or re-sent after first pairing.
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
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, copy, split},
    net::{TcpListener, TcpStream, UnixStream},
    process::{Child, Command},
    sync::{Semaphore, mpsc, watch},
};
use tokio_rustls::TlsAcceptor;

/// The user-facing default port for the bridge. A remembered phone dials the
/// port from its QR, so the Mac must keep listening on the same one across
/// restarts; `0` (a fresh free port) remains available to library callers/tests.
pub const DEFAULT_PORT: u16 = 48231;

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
    /// Discard the persisted certificate, pairing code and device-token
    /// verifier on start and generate all fresh, revoking the old phone.
    pub reset_pairing: bool,
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

/// Bounds on unauthenticated work: a successful bridge connection can type as
/// the Mac user, so connection count, auth serialization and read timeouts are
/// security-critical, not niceties.
const MAX_CONNECTIONS: usize = 16;
const MAX_SHIM_CONNECTIONS: usize = 16;
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_AUTH_LINE: usize = 128;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const FAILURE_BUDGET: u32 = 5;
const MAX_FAILURE_IPS: usize = 4096;

const CERT_FILE: &str = "bridge-cert.der";
const KEY_FILE: &str = "bridge-key.der";
const PAIRING_FILE: &str = "bridge-pairing.txt";

/// The pairing state persisted under `Config.dir`: the certificate the phone
/// pins, its key, the unspent pairing code, and — once paired — the device
/// token's verifier only, never the token itself. All files are owner-only.
struct PairingState {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    fingerprint: String,
    fingerprint_bytes: [u8; 32],
    code: String,
    verifier: Option<[u8; 32]>,
}

fn acceptor_for(cert_der: &[u8], key_der: &[u8]) -> Result<TlsAcceptor> {
    // If a provider is already installed (GPUI's HTTP client installs one), keep it.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let key = PrivateKeyDer::Pkcs8(key_der.to_vec().into());
    let tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(cert_der.to_vec())], key)
        .context("build the TLS server config")?;
    Ok(TlsAcceptor::from(Arc::new(tls)))
}

/// Loads the persisted pairing state, or generates and persists a fresh one
/// on first use or an explicit reset. Existing but invalid state is an error:
/// only `reset` may revoke paired devices or rotate the pinned identity.
fn load_or_create_state(dir: &Path, reset: bool) -> Result<(PairingState, TlsAcceptor)> {
    if !reset && let Some(loaded) = load_state(dir)? {
        let acceptor = acceptor_for(&loaded.cert_der, &loaded.key_der)
            .context("persisted bridge identity is unusable; explicitly reset pairing")?;
        return Ok((loaded, acceptor));
    }
    let certified = rcgen::generate_simple_self_signed(vec!["kil".into()])
        .context("generate a self-signed certificate")?;
    let fp = fingerprint(certified.cert.der());
    let fingerprint_bytes = *digest(&SHA256, certified.cert.der().as_ref())
        .as_ref()
        .first_chunk::<32>()
        .context("SHA-256 digest is 32 bytes")?;
    let state = PairingState {
        cert_der: certified.cert.der().as_ref().to_vec(),
        key_der: certified.key_pair.serialize_der(),
        fingerprint: fp,
        fingerprint_bytes,
        code: random_hex32()?,
        verifier: None,
    };
    save_state(dir, &state).context("persist the bridge pairing state")?;
    let acceptor = acceptor_for(&state.cert_der, &state.key_der)?;
    Ok((state, acceptor))
}

/// The persisted state, strictly parsed. No files is first use; a partial,
/// unreadable or malformed state needs an explicit reset, never silent repair.
fn load_state(dir: &Path) -> Result<Option<PairingState>> {
    let cert_path = dir.join(CERT_FILE);
    let key_path = dir.join(KEY_FILE);
    let pairing_path = dir.join(PAIRING_FILE);
    if !cert_path.exists() && !key_path.exists() && !pairing_path.exists() {
        return Ok(None);
    }
    let cert_der = std::fs::read(&cert_path).context("read the persisted bridge certificate")?;
    let key_der = std::fs::read(&key_path).context("read the persisted bridge private key")?;
    let text =
        std::fs::read_to_string(&pairing_path).context("read the persisted pairing state")?;
    let state = parse_state(cert_der, key_der, &text)
        .context("persisted pairing state is invalid; explicitly reset pairing")?;
    // Files we wrote are 0600; tighten any that were loosened since.
    for path in [&cert_path, &key_path, &pairing_path] {
        set_owner_only(path).context("secure the persisted bridge files")?;
    }
    Ok(Some(state))
}

fn parse_state(cert_der: Vec<u8>, key_der: Vec<u8>, text: &str) -> Option<PairingState> {
    let mut fingerprint = None;
    let mut code = None;
    let mut verifier: Option<Option<[u8; 32]>> = None;
    let mut seen_version = false;
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        match key {
            "v" => {
                if value != "1" || seen_version {
                    return None;
                }
                seen_version = true;
            }
            "fingerprint" => {
                if fingerprint.is_some() {
                    return None;
                }
                fingerprint = Some(value.to_owned());
            }
            "code" => {
                if code.is_some() {
                    return None;
                }
                code = Some(value.to_owned());
            }
            "verifier" => {
                if verifier.is_some() {
                    return None;
                }
                verifier = Some(if value.is_empty() {
                    None
                } else {
                    Some(decode_hex32(value)?)
                });
            }
            _ => return None,
        }
    }
    let fingerprint = fingerprint?;
    let code = code?;
    let verifier = verifier?;
    if !seen_version || decode_hex32(&code).is_none() {
        return None;
    }
    let digest_bytes = digest(&SHA256, &cert_der);
    let fingerprint_bytes = *digest_bytes.as_ref().first_chunk::<32>()?;
    if fingerprint != to_hex(&fingerprint_bytes) || cert_der.is_empty() || key_der.is_empty() {
        return None;
    }
    Some(PairingState {
        cert_der,
        key_der,
        fingerprint,
        fingerprint_bytes,
        code,
        verifier,
    })
}

fn set_owner_only(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Writes `bytes` owner-only via a temp file renamed over `path`.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        set_owner_only(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    if let Err(err) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(err.into());
    }
    set_owner_only(path)?;
    Ok(())
}

fn pairing_text(state: &PairingState) -> String {
    let verifier = state
        .verifier
        .as_ref()
        .map_or_else(String::new, |v| to_hex(v));
    format!(
        "v=1\nfingerprint={}\ncode={}\nverifier={verifier}\n",
        state.fingerprint, state.code
    )
}

fn save_state(dir: &Path, state: &PairingState) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    write_private(&dir.join(CERT_FILE), &state.cert_der)?;
    write_private(&dir.join(KEY_FILE), &state.key_der)?;
    save_pairing_file(dir, state)
}

fn save_pairing_file(dir: &Path, state: &PairingState) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    write_private(&dir.join(PAIRING_FILE), pairing_text(state).as_bytes())
}

/// The unspent pairing code, the device-token verifier once paired, and the
/// failure ledger throttling brute force. The code never expires on a clock:
/// it is good until spent or the pairing is reset, and it persists across
/// restarts so an unscanned QR still works.
struct Auth {
    dir: PathBuf,
    fingerprint: String,
    fingerprint_bytes: [u8; 32],
    code: String,
    verifier: Option<[u8; 32]>,
    failures: HashMap<IpAddr, (u32, std::time::Instant)>,
    global_failures: (u32, std::time::Instant),
}

impl Auth {
    fn from_state(dir: &Path, state: &PairingState) -> Self {
        Self {
            dir: dir.to_owned(),
            fingerprint: state.fingerprint.clone(),
            fingerprint_bytes: state.fingerprint_bytes,
            code: state.code.clone(),
            verifier: state.verifier,
            failures: HashMap::new(),
            global_failures: (0, std::time::Instant::now()),
        }
    }

    /// Records a failed attempt from `ip` at `now` and returns how long the
    /// caller must wait (holding the auth gate) before answering. The global
    /// ledger matters because tunnel peers all arrive as 127.0.0.1; flooding
    /// the per-IP map never resets it — a full map keeps only live-window
    /// entries and simply stops learning new IPs. Call with the mutex held.
    fn failure_delay(&mut self, ip: IpAddr, now: std::time::Instant) -> Duration {
        let (global, since) = &mut self.global_failures;
        if now.duration_since(*since) > FAILURE_WINDOW {
            *global = 0;
            *since = now;
        }
        *global += 1;
        let global = *global;
        if self.failures.len() >= MAX_FAILURE_IPS && !self.failures.contains_key(&ip) {
            self.failures
                .retain(|_, (_, at)| now.duration_since(*at) <= FAILURE_WINDOW);
        }
        let per_ip = if self.failures.len() < MAX_FAILURE_IPS || self.failures.contains_key(&ip) {
            let (count, since) = self.failures.entry(ip).or_insert((0, now));
            if now.duration_since(*since) > FAILURE_WINDOW {
                *count = 0;
                *since = now;
            }
            *count += 1;
            *count
        } else {
            0
        };
        if global > FAILURE_BUDGET || per_ip > FAILURE_BUDGET {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(2)
        }
    }

    fn note_success(&mut self, ip: IpAddr) {
        self.failures.remove(&ip);
        self.global_failures = (0, std::time::Instant::now());
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

        let (state, acceptor) = load_or_create_state(&config.dir, config.reset_pairing)?;
        let fingerprint = state.fingerprint.clone();
        let code = state.code.clone();
        let listener = StdTcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, config.port)))
            .with_context(|| format!("bind port {}", config.port))?;
        listener.set_nonblocking(true)?;

        let auth = Arc::new(Mutex::new(Auth::from_state(&config.dir, &state)));
        let qr = Arc::new(Mutex::new(None));
        let error = Arc::new(Mutex::new(None));
        let tags_remote = Arc::new(AtomicBool::new(false));
        let socket_path = kil_term::server::socket_path(&config.dir);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let mut cancel = shutdown_rx.clone();

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
                    let tls_port = listener.local_addr()?.port();
                    // Setup can take a while (a server to start, a tunnel that takes up to
                    // 30 s to register), and `stop` waits for this thread: it gives way to
                    // the shutdown signal, dropping what it started (`cloudflared` is
                    // killed on drop).
                    let setup = prepare(
                        &socket_path,
                        &config,
                        tunnel_config.as_ref().map(|(_, token)| token.as_str()),
                        tls_port,
                        &thread_tags,
                    );
                    let Some((tags, mut tunnel)) = until_stopped(&mut cancel, setup).await? else {
                        log::info!("stopping (shutdown during setup)");
                        return Ok(());
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

    /// Whether a phone has already spent the pairing code — true immediately
    /// after a restart too, since the verifier is persisted.
    pub fn paired(&self) -> bool {
        self.auth
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .verifier
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
    let slots = Arc::new(Semaphore::new(MAX_SHIM_CONNECTIONS));
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
            drop(stream);
            continue;
        };
        tokio::spawn(async move {
            let _permit = permit;
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
    let upgraded = tokio::time::timeout(AUTH_TIMEOUT, ws_upgrade(&mut stream))
        .await
        .context("WebSocket upgrade timed out")??;
    if !upgraded {
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
async fn read_http_headers<R>(stream: &mut R) -> Result<String>
where
    R: AsyncRead + Unpin,
{
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

/// What the bridge needs before it accepts a phone: the session server up, whether it tags
/// remote clients (probed once, not once per phone connection: the app's presence poll needs
/// the answer too), and the tunnel, if one is configured.
async fn prepare(
    socket_path: &Path,
    config: &Config,
    tunnel_token: Option<&str>,
    tls_port: u16,
    tags_remote: &AtomicBool,
) -> Result<(bool, Option<PublicTunnel>)> {
    ensure_server(socket_path, config.launcher.as_deref(), &config.dir).await?;
    keep_alive(socket_path).await?;
    let tags = server_tags_remote(socket_path).await;
    tags_remote.store(tags, Ordering::Release);
    let tunnel = match tunnel_token {
        Some(token) => Some(PublicTunnel::start(tls_port, token).await?),
        None => None,
    };
    Ok((tags, tunnel))
}

/// Runs `work` unless shutdown is signalled first: `None` then, with `work` dropped.
async fn until_stopped<T>(
    stop: &mut watch::Receiver<bool>,
    work: impl std::future::Future<Output = Result<T>>,
) -> Result<Option<T>> {
    let stopped = async {
        // A closed channel means the bridge handle is gone: nobody is left to serve.
        while !*stop.borrow_and_update() && stop.changed().await.is_ok() {}
    };
    tokio::select! {
        done = work => done.map(Some),
        () = stopped => Ok(None),
    }
}

async fn serve(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    relay: Relay,
    auth: Arc<Mutex<Auth>>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let auth_gate = Arc::new(Semaphore::new(1));
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (tcp, peer) = accepted?;
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    drop(tcp);
                    continue;
                };
                let acceptor = acceptor.clone();
                let auth = Arc::clone(&auth);
                let relay = relay.clone();
                let gate = Arc::clone(&auth_gate);
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(err) = handle_connection(acceptor, tcp, peer, relay, auth, gate).await {
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

/// Waits out one failed attempt's penalty. Called with the auth gate held,
/// so the delay is a real attempt-rate limit, not a courtesy sleep in an
/// unbounded spawned task.
async fn fail_penalty(auth: &Mutex<Auth>, ip: IpAddr) {
    let delay = {
        let mut guard = auth.lock().unwrap_or_else(PoisonError::into_inner);
        guard.failure_delay(ip, std::time::Instant::now())
    };
    tokio::time::sleep(delay).await;
}

/// One auth line: at most [`MAX_AUTH_LINE`] bytes, newline-terminated,
/// within [`AUTH_TIMEOUT`]. `None` on EOF, oversize, timeout or error.
async fn read_auth_line<R>(reader: &mut R) -> Option<String>
where
    R: AsyncRead + Unpin,
{
    let read = async {
        let mut bytes = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match reader.read(&mut byte).await {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            if byte[0] == b'\n' {
                break;
            }
            bytes.push(byte[0]);
            if bytes.len() > MAX_AUTH_LINE {
                return None;
            }
        }
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        Some(String::from_utf8_lossy(&bytes).into_owned())
    };
    tokio::time::timeout(AUTH_TIMEOUT, read).await.ok()?
}

async fn handle_connection(
    acceptor: TlsAcceptor,
    tcp: TcpStream,
    peer: SocketAddr,
    relay: Relay,
    auth: Arc<Mutex<Auth>>,
    auth_gate: Arc<Semaphore>,
) -> Result<()> {
    // Serialize authentication (TLS handshake included) so no number of
    // sockets parallelizes guessing; a phone's quick success releases it.
    let Ok(Ok(auth_permit)) = tokio::time::timeout(AUTH_TIMEOUT, auth_gate.acquire_owned()).await
    else {
        return Ok(());
    };
    let deadline = tokio::time::Instant::now() + AUTH_TIMEOUT;
    let Ok(Ok(tls)) = tokio::time::timeout_at(deadline, acceptor.accept(tcp)).await else {
        fail_penalty(&auth, peer.ip()).await;
        return Ok(());
    };
    let mut reader = BufReader::new(tls);
    let outcome = tokio::time::timeout_at(deadline, authenticate(&mut reader, &auth))
        .await
        .ok()
        .flatten();
    let Some(ok_reply) = outcome else {
        fail_penalty(&auth, peer.ip()).await;
        let _ = reply(&mut reader, "ERR rejected").await;
        return Ok(());
    };
    {
        let mut guard = auth.lock().unwrap_or_else(PoisonError::into_inner);
        guard.note_success(peer.ip());
    }

    // Authentication is complete; deliver its reply before fallible relay setup.
    tokio::time::timeout(AUTH_TIMEOUT, reply(&mut reader, &ok_reply))
        .await
        .context("send the authentication reply")??;
    drop(auth_permit);

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

    // A client may pipeline its first wire frame with KIL3's RESP. Preserve
    // anything the line reader buffered instead of dropping it at into_inner.
    let pending = reader.buffer().to_vec();
    let tls = reader.into_inner();
    if !pending.is_empty() {
        unix.write_all(&pending)
            .await
            .context("forward pipelined wire bytes")?;
    }
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

async fn reply(reader: &mut TlsReader, text: &str) -> Result<()> {
    let writer = reader.get_mut();
    writer.write_all(text.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

type TlsReader = BufReader<tokio_rustls::server::TlsStream<TcpStream>>;

/// Runs one auth exchange over the TLS stream and returns the `OK …` reply
/// on success. `KIL1` spends the pairing code and issues the device token;
/// `KIL2` is the legacy bearer reconnect; `KIL3` proves the token by
/// HMAC challenge-response so it never crosses the wire again.
async fn authenticate(reader: &mut TlsReader, auth: &Mutex<Auth>) -> Option<String> {
    let line = read_auth_line(reader).await?;
    if let Some(code) = line.strip_prefix("KIL1 ") {
        return pair_with_code(code, auth).map(|token| format!("OK {token}"));
    }
    if let Some(token) = line.strip_prefix("KIL2 ") {
        // Legacy clients retain the OK payload as their session token.
        return check_bearer(token, auth).then(|| format!("OK {}", token.trim()));
    }
    if line == "KIL3" {
        let (verifier, fingerprint_bytes) = {
            let guard = auth.lock().unwrap_or_else(PoisonError::into_inner);
            (guard.verifier?, guard.fingerprint_bytes)
        };
        let nonce = random_bytes32().ok()?;
        reply(reader, &format!("CHAL {}", to_hex(&nonce)))
            .await
            .ok()?;
        let response = read_auth_line(reader).await?;
        let presented = decode_hex32(response.strip_prefix("RESP ")?)?;
        let expected = kil3_response(&verifier, &nonce, &fingerprint_bytes);
        return constant_time_eq(&presented, &expected).then(|| "OK device".to_owned());
    }
    None
}

/// `KIL1`: the code works once, ever, until a local reset. The new token's
/// verifier is persisted before the token is handed out; if that write
/// fails, the pairing does not happen.
fn pair_with_code(code: &str, auth: &Mutex<Auth>) -> Option<String> {
    let mut guard = auth.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.verifier.is_some() {
        return None;
    }
    if !constant_time_eq(code.as_bytes(), guard.code.as_bytes()) {
        return None;
    }
    let token_bytes = random_bytes32().ok()?;
    let verifier = verifier_of(&token_bytes);
    let snapshot = PairingState {
        cert_der: Vec::new(),
        key_der: Vec::new(),
        fingerprint: guard.fingerprint.clone(),
        fingerprint_bytes: guard.fingerprint_bytes,
        code: guard.code.clone(),
        verifier: Some(verifier),
    };
    save_pairing_file(&guard.dir, &snapshot).ok()?;
    guard.verifier = Some(verifier);
    Some(to_hex(&token_bytes))
}

/// `KIL2` legacy bearer: derive the verifier from the presented token and
/// compare in constant time. The bridge holds no token plaintext.
fn check_bearer(token: &str, auth: &Mutex<Auth>) -> bool {
    let guard = auth.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(expected) = guard.verifier else {
        return false;
    };
    decode_hex32(token).is_some_and(|bytes| constant_time_eq(&verifier_of(&bytes), &expected))
}

/// The verifier/key bytes for a device token: SHA-256 of its 32 raw bytes.
fn verifier_of(token_bytes: &[u8; 32]) -> [u8; 32] {
    let digest_bytes = digest(&SHA256, token_bytes);
    *digest_bytes
        .as_ref()
        .first_chunk::<32>()
        .unwrap_or(&[0u8; 32])
}

/// The `KIL3` signed message: domain, nonce and the pinned certificate
/// fingerprint, NUL-separated, so a response is bound to this bridge.
fn kil3_message(nonce: &[u8; 32], fingerprint_bytes: &[u8; 32]) -> Vec<u8> {
    let mut message = Vec::with_capacity(11 + 1 + 32 + 1 + 32);
    message.extend_from_slice(b"kil-auth-v3");
    message.push(0);
    message.extend_from_slice(nonce);
    message.push(0);
    message.extend_from_slice(fingerprint_bytes);
    message
}

fn kil3_response(verifier: &[u8; 32], nonce: &[u8; 32], fingerprint_bytes: &[u8; 32]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, verifier);
    let tag = hmac::sign(&key, &kil3_message(nonce, fingerprint_bytes));
    *tag.as_ref().first_chunk::<32>().unwrap_or(&[0u8; 32])
}

/// Exactly 64 hex chars → 32 bytes; anything else is no value.
fn decode_hex32(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
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
    println!(
        "  The code is spent by the first phone; the pairing persists across restarts until reset."
    );
    println!();
}

/// The LAN address the iPhone should dial; the UDP trick picks the interface
/// used by the default route without sending a packet.
fn lan_ip() -> Result<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.connect("8.8.8.8:80")?;
    Ok(socket.local_addr()?.ip())
}

fn random_bytes32() -> Result<[u8; 32]> {
    let rng = SystemRandom::new();
    let mut bytes = [0u8; 32];
    rng.fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("system RNG failed"))?;
    Ok(bytes)
}

/// A 256-bit secret as 64 lowercase hex chars (pairing codes, device tokens).
fn random_hex32() -> Result<String> {
    Ok(to_hex(&random_bytes32()?))
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

    const TEST_CODE: &str = "abababababababababababababababababababababababababababababababab";
    // Shared with the Dart KilConnection tests: same token/nonce/fingerprint.
    const VECTOR_TOKEN: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const VECTOR_NONCE: &str = "ffeeddccbbaa9988776655443322110000112233445566778899aabbccddeeff";
    const VECTOR_FP: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    const VECTOR_VERIFIER: &str =
        "630dcd2966c4336691125448bbb25b4ff412a49c732db2c8abc1b8581bd710dd";
    const VECTOR_HMAC: &str = "959d4759b29834e5ad283c5092754532c28650d6db81406f786965127c000822";

    fn auth_with(dir: &Path, code: &str, verifier: Option<[u8; 32]>) -> Mutex<Auth> {
        Mutex::new(Auth {
            dir: dir.to_owned(),
            fingerprint: VECTOR_FP.to_owned(),
            fingerprint_bytes: decode_hex32(VECTOR_FP).unwrap(),
            code: code.to_owned(),
            verifier,
            failures: HashMap::new(),
            global_failures: (0, std::time::Instant::now()),
        })
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kil-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn kil1_spends_the_code_persists_only_the_verifier_and_kil2_reconnects() {
        let dir = scratch_dir("kil1");
        let auth = auth_with(&dir, TEST_CODE, None);
        let token = pair_with_code(TEST_CODE, &auth).expect("the code should work");
        assert_eq!(token.len(), 64);
        assert!(decode_hex32(&token).is_some());
        // A second phone with the same QR is rejected, forever.
        assert!(pair_with_code(TEST_CODE, &auth).is_none());
        // The issued token reconnects via the legacy bearer path.
        assert!(check_bearer(&token, &auth));
        assert!(!check_bearer(TEST_CODE, &auth));
        // The verifier reached disk before the token was handed out, and the
        // token plaintext is nowhere in the persisted state.
        let persisted = std::fs::read_to_string(dir.join(PAIRING_FILE)).unwrap();
        assert!(persisted.contains(&to_hex(&verifier_of(&decode_hex32(&token).unwrap()))));
        assert!(!persisted.contains(&token), "no token plaintext on disk");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wrong_code_is_rejected_and_leaves_no_verifier() {
        let dir = scratch_dir("wrong");
        let auth = auth_with(&dir, TEST_CODE, None);
        assert!(pair_with_code("cdcd", &auth).is_none());
        assert!(!check_bearer(TEST_CODE, &auth));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kil3_matches_the_shared_known_answer_vector() {
        let token = decode_hex32(VECTOR_TOKEN).unwrap();
        let verifier = verifier_of(&token);
        assert_eq!(to_hex(&verifier), VECTOR_VERIFIER);
        let nonce = decode_hex32(VECTOR_NONCE).unwrap();
        let fp = decode_hex32(VECTOR_FP).unwrap();
        assert_eq!(to_hex(&kil3_response(&verifier, &nonce, &fp)), VECTOR_HMAC);
        // A response for another nonce or bridge does not verify.
        let other = kil3_response(&verifier, &[0u8; 32], &fp);
        assert!(!constant_time_eq(
            &other,
            &decode_hex32(VECTOR_HMAC).unwrap()
        ));
    }

    #[test]
    fn new_secrets_are_256_bit_lowercase_hex() {
        let secret = random_hex32().unwrap();
        assert_eq!(secret.len(), 64);
        assert!(
            secret
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn failure_delay_grows_after_the_budget_globally_and_per_ip() {
        let dir = scratch_dir("throttle");
        let auth = auth_with(&dir, TEST_CODE, None);
        let now = std::time::Instant::now();
        let mut guard = auth.lock().unwrap_or_else(PoisonError::into_inner);
        // Five failures from five different IPs still trip the global ledger:
        // tunnel peers collapse to 127.0.0.1, so per-IP alone is not enough.
        for octet in 0..5u8 {
            let ip = IpAddr::from(Ipv4Addr::new(203, 0, 113, octet));
            assert_eq!(guard.failure_delay(ip, now), Duration::from_secs(2));
        }
        let sixth = IpAddr::from(Ipv4Addr::new(198, 51, 100, 9));
        assert_eq!(guard.failure_delay(sixth, now), Duration::from_secs(30));
        // A new window resets the budget.
        let later = now + FAILURE_WINDOW + Duration::from_secs(1);
        assert_eq!(guard.failure_delay(sixth, later), Duration::from_secs(2));
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn auth_lines_are_bounded_and_need_a_newline() {
        let mut ok: &[u8] = b"KIL3\n";
        assert_eq!(read_auth_line(&mut ok).await.as_deref(), Some("KIL3"));
        let mut crlf: &[u8] = b"KIL3\r\n";
        assert_eq!(read_auth_line(&mut crlf).await.as_deref(), Some("KIL3"));
        let mut no_newline: &[u8] = b"KIL3";
        assert_eq!(read_auth_line(&mut no_newline).await, None);
        let long = vec![b'A'; MAX_AUTH_LINE + 2];
        let mut oversize: &[u8] = &long;
        assert_eq!(read_auth_line(&mut oversize).await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn auth_and_websocket_header_reads_time_out() {
        let (_auth_writer, mut auth_reader) = tokio::io::duplex(1);
        assert_eq!(read_auth_line(&mut auth_reader).await, None);

        let (_header_writer, mut header_reader) = tokio::io::duplex(1);
        let result =
            tokio::time::timeout(AUTH_TIMEOUT, read_http_headers(&mut header_reader)).await;
        assert!(result.is_err(), "incomplete HTTP headers hit the 5 s limit");
    }

    #[test]
    fn invalid_persisted_state_requires_an_explicit_reset() {
        let dir = scratch_dir("corrupt");
        let (first, _) = load_or_create_state(&dir, false).unwrap();
        std::fs::write(dir.join(PAIRING_FILE), b"v=1\ncode=nope\n").unwrap();
        assert!(load_or_create_state(&dir, false).is_err());
        // Reset is the only operation that repairs it, rotating the identity.
        let (second, _) = load_or_create_state(&dir, true).unwrap();
        assert_ne!(second.fingerprint, first.fingerprint);
        assert_eq!(second.code.len(), 64);
        let (third, _) = load_or_create_state(&dir, false).unwrap();
        assert_eq!(third.fingerprint, second.fingerprint);
        assert_eq!(third.code, second.code);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for name in [CERT_FILE, KEY_FILE, PAIRING_FILE] {
                let mode = std::fs::metadata(dir.join(name))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o600, "{name} is owner-only");
            }
        }
        // A key that does not belong to the certificate is never served.
        let other = scratch_dir("otherkey");
        load_or_create_state(&other, false).unwrap();
        std::fs::copy(other.join(KEY_FILE), dir.join(KEY_FILE)).unwrap();
        assert!(load_or_create_state(&dir, false).is_err());
        let _ = std::fs::remove_dir_all(&other);
        let _ = std::fs::remove_dir_all(&dir);
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
        bridge_on_reset(dir, false)
    }

    fn bridge_on_reset(dir: &Path, reset_pairing: bool) -> Arc<Bridge> {
        Bridge::start(&Config {
            dir: dir.to_owned(),
            port: 0,
            host: Some("127.0.0.1".to_owned()),
            launcher: None,
            public: false,
            tunnel_hostname: None,
            tunnel_token: None,
            reset_pairing,
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
    async fn setup_should_give_way_to_shutdown() {
        let (tx, mut rx) = watch::channel(false);
        let hung = std::future::pending::<Result<()>>();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(true).unwrap();
            tx
        });
        let started = std::time::Instant::now();
        assert!(until_stopped(&mut rx, hung).await.unwrap().is_none());
        assert!(started.elapsed() < Duration::from_secs(2));
        stopper.await.unwrap();
    }

    #[tokio::test]
    async fn setup_should_give_way_to_a_shutdown_sent_before_it_began() {
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        let hung = std::future::pending::<Result<()>>();
        assert!(until_stopped(&mut rx, hung).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn setup_should_finish_and_keep_its_error_when_nothing_stops_it() {
        let (_tx, mut rx) = watch::channel(false);
        assert_eq!(
            until_stopped(&mut rx, async { Ok(7) }).await.unwrap(),
            Some(7)
        );
        let failed = until_stopped(&mut rx, async { bail!("no tunnel") as Result<()> }).await;
        assert!(failed.is_err_and(|err| err.to_string() == "no tunnel"));
    }

    #[tokio::test]
    async fn the_probe_falls_back_on_a_server_from_before_remote() {
        // Hello answered, then a hangup on the declaration: no declaration is sent.
        let socket = fake_server("old", vec![vec![5, 0, 0, 0, 1, 2, 0, 0, 0]]);
        assert!(!server_tags_remote(&socket).await);
    }
}
