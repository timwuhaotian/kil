//! The session server, `kil --server` (DESIGN.md §14): owns every session's PTY and child,
//! so they outlive the app that shows them.

use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions, Permissions},
    io::{self, ErrorKind},
    os::{
        fd::AsFd,
        unix::{
            fs::{OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use polling::{Event, Events, PollMode, Poller};

use crate::{
    child,
    events::SessionEvent,
    reaper,
    session::Session,
    spawn, sys,
    wire::{self, Frame, PROTOCOL},
};

/// With no session and no client for this long, the server exits.
const IDLE_EXIT: Duration = Duration::from_secs(5);
/// How often the accept loop looks at whether it is idle.
const IDLE_CHECK: Duration = Duration::from_secs(1);
/// Natural exit after `StopAll` before the reaper escalates, as on quit (DESIGN.md §4.6).
const STOP_GRACE: Duration = Duration::from_millis(500);
/// `sun_path` holds 104 bytes on macOS, the NUL included.
const SOCKET_PATH_MAX: usize = 103;

/// The socket of the server that keeps its files in `dir`; under `$TMPDIR` if `dir` is too
/// long for a socket path.
#[must_use]
pub fn socket_path(dir: &Path) -> PathBuf {
    let path = dir.join("server.sock");
    if path.as_os_str().len() <= SOCKET_PATH_MAX {
        return path;
    }
    std::env::temp_dir()
        .join(format!("kil-{}", sys::effective_uid()))
        .join("server.sock")
}

/// Starts `exe --server <dir>` from the app, detached from its session and process group,
/// with the hygiene of a shell (DESIGN.md §4.2). Returns once the server has daemonized.
///
/// # Errors
/// If the server can't be started.
pub fn launch(exe: &Path, dir: &Path) -> io::Result<()> {
    private_dir(dir)?;
    let log = File::create(dir.join("server.log"))?;
    let mut command = Command::new(exe);
    command
        .arg("--server")
        .arg(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    sys::install_child_setup(&mut command, spawn::fd_sweep_limit(), false);
    let status = command.spawn()?.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "session server failed to start: {status}"
        )))
    }
}

/// In `kil --server`, before anything else: the launching process exits here, so the
/// server is never a child of the app and leaves no zombie behind.
///
/// # Errors
/// If `fork` fails.
pub fn daemonize() -> io::Result<()> {
    if !sys::fork_single_threaded()? {
        std::process::exit(0);
    }
    Ok(())
}

/// Serves until idle (DESIGN.md §14.3). Returns at once if another server holds `dir`.
///
/// # Errors
/// If the socket can't be set up.
pub fn serve(dir: &Path) -> io::Result<()> {
    let Some(listening) = listen(dir)? else {
        log::info!("another session server owns {}", dir.display());
        return Ok(());
    };
    listening.run();
    Ok(())
}

/// Starts a server on a thread of this process, for tests and for when no separate server
/// can be started; its sessions then end with this process. Returns once it listens.
///
/// # Errors
/// If the socket can't be set up, or another server owns `dir`.
pub fn serve_in_thread(dir: &Path) -> io::Result<()> {
    let listening = listen(dir)?.ok_or_else(|| {
        io::Error::new(
            ErrorKind::AddrInUse,
            "another session server owns the directory",
        )
    })?;
    thread::Builder::new()
        .name("kil-server".into())
        .spawn(move || listening.run())?;
    Ok(())
}

fn private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, Permissions::from_mode(0o700))
}

struct Listening {
    listener: UnixListener,
    socket: PathBuf,
    /// Held for the server's life: a second server would steal the socket.
    _lock: File,
}

fn listen(dir: &Path) -> io::Result<Option<Listening>> {
    private_dir(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.join("server.lock"))?;
    if !sys::try_lock(lock.as_fd())? {
        return Ok(None);
    }
    let socket = socket_path(dir);
    if let Some(parent) = socket.parent() {
        private_dir(parent)?;
    }
    // Ours now that we hold the lock: whatever is there is a dead server's.
    match fs::remove_file(&socket) {
        Err(err) if err.kind() != ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, Permissions::from_mode(0o600))?;
    Ok(Some(Listening {
        listener,
        socket,
        _lock: lock,
    }))
}

/// Sessions by id. A generation tells a session apart from a later one with the same id.
#[derive(Default)]
struct Registry {
    sessions: Mutex<HashMap<String, (u64, Session)>>,
    generation: AtomicU64,
    /// Connections being served that are not (yet) a session's.
    clients: AtomicUsize,
}

impl Registry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (u64, Session)>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn is_idle(&self) -> bool {
        self.clients.load(Ordering::Acquire) == 0 && self.lock().is_empty()
    }
}

impl Listening {
    #[expect(
        unsafe_code,
        reason = "polling requires a source to outlive its registration: the listener lives in \
                  `self` and is deleted before `run` returns"
    )]
    fn run(self) {
        let registry = Arc::new(Registry::default());
        let poller = match Poller::new() {
            Ok(poller) => poller,
            Err(err) => {
                log::error!("session server: kqueue unavailable: {err}");
                return;
            }
        };
        if let Err(err) = self.listener.set_nonblocking(true).and_then(|()| {
            // SAFETY: see the method's `expect`.
            // Level-triggered: `add`'s default one-shot would stop watching after the first
            // connection, leaving later ones to the idle check's 1 s timeout.
            unsafe { poller.add_with_mode(&self.listener, Event::readable(0), PollMode::Level) }
        }) {
            log::error!("session server: cannot watch the socket: {err}");
            return;
        }
        let mut events = Events::new();
        let mut idle_since = None;
        loop {
            events.clear();
            if let Err(err) = poller.wait(&mut events, Some(IDLE_CHECK))
                && err.kind() != ErrorKind::Interrupted
            {
                log::error!("session server: poll failed: {err}");
                break;
            }
            loop {
                match self.listener.accept() {
                    Ok((stream, _)) => accept(stream, &registry),
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == ErrorKind::Interrupted => {}
                    Err(err) => {
                        log::warn!("session server: accept failed: {err}");
                        break;
                    }
                }
            }
            if registry.is_idle() {
                let since = *idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= IDLE_EXIT {
                    break;
                }
            } else {
                idle_since = None;
            }
        }
        let _ = poller.delete(&self.listener);
        let _ = fs::remove_file(&self.socket);
        log::info!("session server: idle, exiting");
    }
}

fn accept(stream: UnixStream, registry: &Arc<Registry>) {
    // macOS hands out accepted sockets with the listener's O_NONBLOCK.
    if let Err(err) = stream.set_nonblocking(false) {
        log::warn!("session server: cannot use a connection: {err}");
        return;
    }
    match sys::peer_uid(stream.as_fd()) {
        Ok(uid) if uid == sys::effective_uid() => {}
        result => {
            log::warn!("session server: refused a connection from {result:?}");
            return;
        }
    }
    registry.clients.fetch_add(1, Ordering::AcqRel);
    let registry = Arc::clone(registry);
    let spawned = thread::Builder::new()
        .name("kil-server-client".into())
        .spawn(move || {
            if let Err(err) = serve_client(stream, &registry) {
                log::debug!("session server: client ended: {err}");
            }
            registry.clients.fetch_sub(1, Ordering::AcqRel);
        });
    if let Err(err) = spawned {
        log::error!("session server: no thread for a client: {err}");
    }
}

/// Serves requests until the connection closes or becomes a session's.
fn serve_client(mut stream: UnixStream, registry: &Arc<Registry>) -> io::Result<()> {
    let Some(Frame::Hello { protocol }) = wire::receive(&mut stream)? else {
        return Ok(());
    };
    wire::send(&mut stream, &Frame::Hello { protocol: PROTOCOL })?;
    if protocol != PROTOCOL {
        return Ok(());
    }
    loop {
        let Some(frame) = wire::receive(&mut stream)? else {
            return Ok(());
        };
        match frame {
            Frame::List => {
                let ids = registry.lock().keys().cloned().collect();
                wire::send(&mut stream, &Frame::Sessions(ids))?;
            }
            Frame::StopAll => {
                stop_all(registry);
                wire::send(&mut stream, &Frame::Stopped)?;
            }
            Frame::Spawn { id, config } => {
                let generation = registry.generation.fetch_add(1, Ordering::AcqRel);
                let weak = Arc::downgrade(registry);
                let name = id.clone();
                let ended = move |event| {
                    if let (SessionEvent::Ended, Some(registry)) = (event, weak.upgrade()) {
                        let mut sessions = registry.lock();
                        if sessions.get(&name).is_some_and(|(g, _)| *g == generation) {
                            sessions.remove(&name);
                        }
                    }
                };
                let mut sessions = registry.lock();
                if sessions.contains_key(&id) {
                    drop(sessions);
                    wire::send(&mut stream, &Frame::Failed(format!("{id} already runs")))?;
                    continue;
                }
                match Session::spawn(&config, ended) {
                    Ok(session) => {
                        wire::send(&mut stream, &Frame::Attached(session.identity()))?;
                        session.attach(stream);
                        sessions.insert(id, (generation, session));
                        return Ok(());
                    }
                    Err(err) => {
                        drop(sessions);
                        let message = match std::error::Error::source(&err) {
                            Some(source) => format!("{err}: {source}"),
                            None => err.to_string(),
                        };
                        wire::send(&mut stream, &Frame::Failed(message))?;
                    }
                }
            }
            Frame::Attach { id } => {
                let sessions = registry.lock();
                if let Some((_, session)) = sessions.get(&id) {
                    wire::send(&mut stream, &Frame::Attached(session.identity()))?;
                    session.attach(stream);
                    return Ok(());
                }
                drop(sessions);
                wire::send(&mut stream, &Frame::Missing)?;
            }
            other => {
                log::warn!("session server: unexpected request {other:?}");
                return Ok(());
            }
        }
    }
}

/// Hangs every session up, then gives their programs a moment before the reaper's SIGKILL.
fn stop_all(registry: &Registry) {
    let sessions: Vec<_> = registry.lock().drain().collect();
    drop(sessions);
    if !child::wait_for_children(STOP_GRACE) {
        reaper::expedite_reaping();
        if !child::wait_for_children(STOP_GRACE) {
            log::warn!(
                "{} session children still running after StopAll",
                child::live_children()
            );
        }
    }
}
