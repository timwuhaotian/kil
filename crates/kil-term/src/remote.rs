//! The app's side of the session server (DESIGN.md §14): connecting to it, spawning and
//! attaching sessions, and the loop that feeds each session's mirror terminal.

use std::{
    io::{self, ErrorKind, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use alacritty_terminal::{
    event::{Event as TermEvent, EventListener},
    sync::FairMutex,
    term::Term,
    vte::ansi::{Processor, StdSyncHandler},
};
use polling::{Event, Events, PollMode, Poller};

use crate::{
    error::SpawnError,
    events::{EventProxy, Role, SessionEvent},
    server,
    session::{GridSize, Session, SessionConfig, term_config},
    snapshot::Palette,
    wire::{self, Frame, Inbox, MIN_PROTOCOL, PROTOCOL},
};

/// How long a request to the server may take before it counts as gone.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a just-launched server may take to start listening.
const START_TIMEOUT: Duration = Duration::from_secs(3);
/// Time to send what is queued (e.g. a `Close`) when the session is dropped.
const FLUSH_ON_DROP: Duration = Duration::from_millis(200);

/// A connection to the session server.
pub struct Client {
    dir: PathBuf,
    control: Mutex<UnixStream>,
    /// What the server and this client both speak.
    protocol: u32,
}

impl Client {
    /// Connects to the server that keeps its socket in `dir`.
    ///
    /// # Errors
    /// If no server answers there, or it speaks another protocol.
    pub fn connect(dir: &Path) -> io::Result<Self> {
        let (control, server) = handshake(dir, PROTOCOL)?;
        let (control, protocol) = if server >= PROTOCOL {
            (control, PROTOCOL)
        } else if server >= MIN_PROTOCOL {
            // An older server hangs up on a newer hello: speak its protocol instead.
            (handshake(dir, server)?.0, server)
        } else {
            return Err(io::Error::new(
                ErrorKind::Unsupported,
                format!("the session server speaks protocol {server}, older than {MIN_PROTOCOL}"),
            ));
        };
        control.set_read_timeout(Some(REQUEST_TIMEOUT))?;
        Ok(Self {
            dir: dir.to_owned(),
            control: Mutex::new(control),
            protocol,
        })
    }

    /// A new connection to the server, past the handshake.
    fn connection(&self) -> io::Result<UnixStream> {
        handshake(&self.dir, self.protocol).map(|(stream, _)| stream)
    }

    /// Connects, first starting `exe --server <dir>` if no server answers.
    ///
    /// # Errors
    /// If the server can't be started or doesn't start listening in time.
    pub fn start(dir: &Path, exe: &Path) -> io::Result<Self> {
        if let Ok(client) = Self::connect(dir) {
            return Ok(client);
        }
        server::launch(exe, dir)?;
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            match Self::connect(dir) {
                Ok(client) => return Ok(client),
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                Err(err) => return Err(err),
            }
        }
    }

    /// Ids of the sessions the server runs.
    ///
    /// # Errors
    /// If the server is gone.
    pub fn sessions(&self) -> io::Result<Vec<String>> {
        match self.request(&Frame::List)? {
            Frame::Sessions(ids) => Ok(ids),
            other => Err(unexpected(&other)),
        }
    }

    /// Ids of the sessions another client watches (a phone attached to them).
    ///
    /// Asked on its own connection: a server older than the `Watchers` frame hangs up
    /// on it, which reads as no presence info rather than disturbing the control
    /// connection.
    ///
    /// # Errors
    /// If the server is gone.
    pub fn watchers(&self) -> io::Result<Vec<String>> {
        let mut stream = self.connection()?;
        wire::send(&mut stream, &Frame::Watchers)?;
        match wire::receive(&mut stream)? {
            Some(Frame::Watching(ids)) => Ok(ids),
            None => Ok(Vec::new()),
            Some(other) => Err(unexpected(&other)),
        }
    }

    /// Ids of the sessions a phone recently typed in (it is driving, not just watching).
    ///
    /// Asked on its own connection: a server older than the `Controllers` frame hangs up
    /// on it, which reads as no presence info rather than disturbing the control
    /// connection.
    ///
    /// # Errors
    /// If the server is gone.
    pub fn controlling(&self) -> io::Result<Vec<String>> {
        let mut stream = self.connection()?;
        wire::send(&mut stream, &Frame::Controllers)?;
        match wire::receive(&mut stream)? {
            Some(Frame::Controlling(ids)) => Ok(ids),
            None => Ok(Vec::new()),
            Some(other) => Err(unexpected(&other)),
        }
    }

    /// Ends every session and waits until they are reaped (or killed).
    ///
    /// # Errors
    /// If the server is gone.
    pub fn stop_all(&self) -> io::Result<()> {
        match self.request(&Frame::StopAll)? {
            Frame::Stopped => Ok(()),
            other => Err(unexpected(&other)),
        }
    }

    fn request(&self, frame: &Frame) -> io::Result<Frame> {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        wire::send(&mut *control, frame)?;
        wire::receive(&mut *control)?.ok_or_else(|| ErrorKind::UnexpectedEof.into())
    }

    /// Starts a session named `id` in the server, and mirrors it here.
    ///
    /// # Errors
    /// If the server is gone or the session can't be started.
    pub fn spawn(
        &self,
        id: &str,
        config: &SessionConfig,
        sink: impl Fn(SessionEvent) + Send + Sync + 'static,
    ) -> Result<Session, SpawnError> {
        let config = stamped(config);
        let mut stream = self.connection().map_err(SpawnError::Server)?;
        let request = Frame::Spawn {
            id: id.to_owned(),
            config: config.clone(),
        };
        wire::send(&mut stream, &request).map_err(SpawnError::Server)?;
        match wire::receive(&mut stream).map_err(SpawnError::Server)? {
            Some(Frame::Attached(identity)) => self
                .mirror(
                    stream,
                    id,
                    identity,
                    config.scrollback_lines,
                    Box::new(sink),
                )
                .map_err(SpawnError::Server),
            Some(Frame::Failed(message)) => Err(SpawnError::Refused(message)),
            other => Err(SpawnError::Server(match other {
                Some(frame) => unexpected(&frame),
                None => ErrorKind::UnexpectedEof.into(),
            })),
        }
    }

    /// Mirrors session `id`, which keeps running in the server; `None` if it is gone.
    ///
    /// # Errors
    /// If the server is gone.
    pub fn attach(
        &self,
        id: &str,
        scrollback_lines: usize,
        sink: impl Fn(SessionEvent) + Send + Sync + 'static,
    ) -> io::Result<Option<Session>> {
        let mut stream = self.connection()?;
        match request_attach(&mut stream, id)? {
            Some(identity) => self
                .mirror(stream, id, identity, scrollback_lines, Box::new(sink))
                .map(Some),
            None => Ok(None),
        }
    }
}

/// Asks for session `id` on a fresh connection: its identity, with the state stream to
/// follow, or `None` if the server has no such session.
fn request_attach(stream: &mut UnixStream, id: &str) -> io::Result<Option<wire::Identity>> {
    wire::send(stream, &Frame::Attach { id: id.to_owned() })?;
    match wire::receive(stream)? {
        Some(Frame::Attached(identity)) => Ok(Some(identity)),
        Some(Frame::Missing) => Ok(None),
        Some(other) => Err(unexpected(&other)),
        None => Err(ErrorKind::UnexpectedEof.into()),
    }
}

/// `config` with this client's own name and version in its env. The server stamps
/// `TERM_PROGRAM{,_VERSION}` from the binary that started it, and a server outlives app
/// upgrades (DESIGN.md §14.3), so its stamp can claim an old Kil long after the app moved
/// on — and a program deciding from that version whether Kil shows OSC 9 notifications
/// itself (killi from 0.0.11) falls back to `osascript`, whose notifications open Script
/// Editor when clicked. What a pane can do is set by the app mirroring it, which is this
/// client: the scanner that picks notifications out of the output runs in the mirror feed.
/// Env in the config is applied after the server's own stamp, so this wins on old servers
/// too, without a protocol change.
fn stamped(config: &SessionConfig) -> SessionConfig {
    let mut config = config.clone();
    for (name, value) in [
        ("TERM_PROGRAM", "kil"),
        ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION")),
    ] {
        config
            .env
            .retain(|(key, _)| key.as_os_str() != std::ffi::OsStr::new(name));
        config.env.push((name.into(), value.into()));
    }
    config
}

fn unexpected(frame: &Frame) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidData,
        format!("unexpected reply from the session server: {frame:?}"),
    )
}

/// A new connection saying hello in `protocol`; returns the server's protocol too.
fn handshake(dir: &Path, protocol: u32) -> io::Result<(UnixStream, u32)> {
    let mut stream = UnixStream::connect(server::socket_path(dir))?;
    stream.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    wire::send(&mut stream, &Frame::Hello { protocol })?;
    match wire::receive(&mut stream)? {
        Some(Frame::Hello { protocol: server }) => Ok((stream, server)),
        _ => Err(ErrorKind::ConnectionAborted.into()),
    }
}

type Sink = Box<dyn Fn(SessionEvent) + Send + Sync>;

impl Client {
    fn mirror(
        &self,
        stream: UnixStream,
        id: &str,
        identity: wire::Identity,
        scrollback_lines: usize,
        sink: Sink,
    ) -> io::Result<Session> {
        let rejoin = Rejoin {
            dir: self.dir.clone(),
            protocol: self.protocol,
            id: id.to_owned(),
            identity,
            scrollback_lines,
            size: None,
            palette: None,
            exited: false,
            last: None,
        };
        mirror(stream, identity, rejoin, sink, self.protocol >= 2)
    }
}

fn mirror(
    stream: UnixStream,
    identity: wire::Identity,
    rejoin: Rejoin,
    sink: Sink,
    palettes: bool,
) -> io::Result<Session> {
    let size = identity.size.clamped();
    let events = EventProxy::new(sink, size, Role::Mirror);
    let term = Arc::new(FairMutex::new(Term::new(
        term_config(rejoin.scrollback_lines),
        &size,
        events.clone(),
    )));
    stream.set_read_timeout(None)?;
    stream.set_nonblocking(true)?;
    let (tx, rx) = mpsc::channel();
    let poller = Arc::new(Poller::new()?);
    let remote = Remote {
        tx: Some(tx),
        poller: Arc::clone(&poller),
        palettes,
    };
    let feed = Feed {
        stream,
        poller,
        rx,
        term: Arc::clone(&term),
        events: events.clone(),
        parser: Processor::new(),
        notifications: crate::notify::Scanner::default(),
        inbox: Inbox::default(),
        out: Vec::new(),
        writable: false,
        rejoin,
    };
    thread::Builder::new()
        .name("kil-mirror".into())
        .spawn(move || feed.run())?;
    Session::mirror(identity, term, events, remote)
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "invalid pid from the server"))
}

/// Frames to the server for one session, from any thread. Dropping it ends the feed and
/// closes the connection, which only detaches the session.
pub(crate) struct Remote {
    tx: Option<Sender<Frame>>,
    poller: Arc<Poller>,
    /// The server takes `Palette` frames (protocol 2).
    palettes: bool,
}

impl Remote {
    pub(crate) fn send(&self, frame: Frame) {
        if self.tx.as_ref().is_some_and(|tx| tx.send(frame).is_ok()) {
            let _ = self.poller.notify();
        }
    }

    /// A protocol-1 server would drop the connection on a frame it doesn't know.
    pub(crate) fn send_palette(&self, palette: &Palette) {
        if self.palettes {
            self.send(Frame::Palette(palette.clone()));
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // Closed before the wakeup, so the feed is sure to see it.
        drop(self.tx.take());
        let _ = self.poller.notify();
    }
}

/// Feeds a mirror terminal from its connection, and sends what the app writes.
struct Feed {
    stream: UnixStream,
    poller: Arc<Poller>,
    rx: Receiver<Frame>,
    term: Arc<FairMutex<Term<EventProxy>>>,
    events: EventProxy,
    parser: Processor<StdSyncHandler>,
    notifications: crate::notify::Scanner,
    inbox: Inbox,
    out: Vec<u8>,
    writable: bool,
    rejoin: Rejoin,
}

/// What a mirror needs to attach again when the server drops its connection while the
/// session runs on: it fell behind or stalled (DESIGN.md §14.4), or a server from before
/// 0.0.31, which holds one client per session, handed the session to a phone.
struct Rejoin {
    dir: PathBuf,
    protocol: u32,
    id: String,
    /// The session as first attached: one of the same id in another process is not it.
    identity: wire::Identity,
    scrollback_lines: usize,
    /// What the app last asked for, asked again on the new connection.
    size: Option<GridSize>,
    palette: Option<Palette>,
    /// The server reported the program's exit: the session is over, not dropped.
    exited: bool,
    /// When the mirror last attached again.
    last: Option<Instant>,
}

/// A drop this soon after the last rejoin is not chased: a server from before 0.0.31 and a
/// phone would only take the session back and forth.
const REJOIN_BACKOFF: Duration = Duration::from_secs(5);

/// Who ended a connection.
enum End {
    /// The app let go of the session, or the feed itself failed: nothing to rejoin.
    App,
    /// The server closed or broke it.
    Server,
}

const STREAM_TOKEN: usize = 0;

impl Feed {
    #[expect(
        unsafe_code,
        reason = "polling requires a source to outlive its registration: each stream lives in \
                  `self` and is deleted from the poller before it is replaced or `run` returns"
    )]
    fn run(mut self) {
        loop {
            // SAFETY: see the method's `expect`.
            if let Err(err) = unsafe {
                self.poller.add_with_mode(
                    &self.stream,
                    Event::readable(STREAM_TOKEN),
                    PollMode::Level,
                )
            } {
                log::error!("mirror registration failed: {err}");
                break;
            }
            self.writable = false;
            if !self.out.is_empty() {
                // What a rejoin queued goes out without waiting for the server to speak.
                let _ = self.poller.notify();
            }
            let end = self.serve();
            let _ = self.poller.delete(&self.stream);
            if !matches!(end, End::Server) || !self.rejoin() {
                break;
            }
        }
        self.events.emit(SessionEvent::Ended);
    }

    /// Carries frames both ways until the connection ends.
    fn serve(&mut self) -> End {
        let mut events = Events::new();
        loop {
            let timeout = self
                .parser
                .sync_timeout()
                .sync_timeout()
                .map(|at| at.saturating_duration_since(Instant::now()));
            events.clear();
            if let Err(err) = self.poller.wait(&mut events, timeout)
                && err.kind() != ErrorKind::Interrupted
            {
                log::error!("mirror poll failed: {err}");
                return End::App;
            }
            if self
                .parser
                .sync_timeout()
                .sync_timeout()
                .is_some_and(|at| at <= Instant::now())
            {
                self.parser.stop_sync(&mut *self.term.lock());
                self.events.send_event(TermEvent::Wakeup);
            }
            if !self.outgoing() {
                self.flush_before_closing();
                return End::App;
            }
            let readable = events.iter().any(|event| event.readable);
            if readable && !self.incoming() {
                return End::Server;
            }
            if self.flush().is_err() {
                return End::Server;
            }
            let wants = !self.out.is_empty();
            if wants != self.writable {
                self.writable = wants;
                let mut interest = Event::readable(STREAM_TOKEN);
                interest.writable = wants;
                let _ = self
                    .poller
                    .modify_with_mode(&self.stream, interest, PollMode::Level);
            }
        }
    }

    /// Attaches again after the server dropped the connection, from the session's whole
    /// state and at the grid and palette the app last asked for, so the window keeps its
    /// pane instead of showing it ended. `false` if the session is over or gone, the server
    /// is, or the last rejoin was too recent ([`REJOIN_BACKOFF`]).
    fn rejoin(&mut self) -> bool {
        let rejoin = &mut self.rejoin;
        if rejoin.exited || rejoin.last.is_some_and(|at| at.elapsed() < REJOIN_BACKOFF) {
            return false;
        }
        rejoin.last = Some(Instant::now());
        let attached = handshake(&rejoin.dir, rejoin.protocol).and_then(|(mut stream, _)| {
            let identity = request_attach(&mut stream, &rejoin.id)?;
            stream.set_read_timeout(None)?;
            stream.set_nonblocking(true)?;
            Ok(identity.map(|identity| (stream, identity)))
        });
        let (stream, identity) = match attached {
            Ok(Some(found)) => found,
            Ok(None) => return false,
            Err(err) => {
                log::warn!("session {}: attaching again failed: {err}", rejoin.id);
                return false;
            }
        };
        let first = rejoin.identity;
        if (identity.pid, identity.start_sec, identity.start_usec)
            != (first.pid, first.start_sec, first.start_usec)
        {
            return false;
        }
        log::info!(
            "session {}: the server dropped this client; attached again",
            rejoin.id
        );
        self.stream = stream;
        self.inbox = Inbox::default();
        // Whatever was half sent belongs to the old connection.
        self.out.clear();
        self.parser = Processor::new();
        self.notifications = crate::notify::Scanner::default();
        // The state stream that follows is laid out for the server's grid: a blank terminal
        // of that size takes it, then the app's own grid, as on a first attach.
        let size = identity.size.clamped();
        *self.term.lock() = Term::new(
            term_config(self.rejoin.scrollback_lines),
            &size,
            self.events.clone(),
        );
        self.events.set_size(size);
        if let Some(grid) = self.rejoin.size.filter(|grid| *grid != size) {
            self.term.lock().resize(grid);
            self.events.set_size(grid);
            Frame::Resize(grid).encode(&mut self.out);
        }
        if let Some(palette) = &self.rejoin.palette {
            Frame::Palette(palette.clone()).encode(&mut self.out);
        }
        true
    }

    /// Queues what the app sent; `false` once the session handle is gone.
    fn outgoing(&mut self) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(frame) => {
                    match &frame {
                        Frame::Resize(size) => self.rejoin.size = Some(*size),
                        Frame::Palette(palette) => self.rejoin.palette = Some(palette.clone()),
                        _ => {}
                    }
                    frame.encode(&mut self.out);
                }
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
    }

    /// Reads and applies the server's frames; `false` once the connection closed.
    fn incoming(&mut self) -> bool {
        let open = match self.inbox.fill(&mut self.stream) {
            Ok(open) => open,
            Err(err) => {
                log::warn!("session server connection failed: {err}");
                false
            }
        };
        let mut parsed = 0;
        loop {
            let frame = match self.inbox.next_raw() {
                Ok(Some((wire::OUTPUT, bytes))) => {
                    self.parser.advance(&mut *self.term.lock(), bytes);
                    for notification in self.notifications.scan(bytes) {
                        self.events.emit(SessionEvent::Notify(notification));
                    }
                    parsed += bytes.len();
                    continue;
                }
                Ok(Some((kind, payload))) => Frame::decode(kind, payload),
                Ok(None) => break,
                Err(err) => Err(err),
            };
            match frame {
                Ok(Frame::Exited(status)) => {
                    self.rejoin.exited = true;
                    self.events.emit(SessionEvent::ChildExited(status));
                }
                Ok(other) => log::warn!("unexpected frame from the server: {other:?}"),
                Err(err) => {
                    log::warn!("bad frame from the server: {err}");
                    return false;
                }
            }
        }
        if parsed > self.parser.sync_bytes_count() {
            self.events.send_event(TermEvent::Wakeup);
        }
        open
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut sent = 0;
        while sent < self.out.len() {
            match self.stream.write(&self.out[sent..]) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(written) => sent += written,
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => return Err(err),
            }
        }
        self.out.drain(..sent);
        Ok(())
    }

    fn flush_before_closing(&mut self) {
        if self.out.is_empty() {
            return;
        }
        let _ = self.stream.set_nonblocking(false);
        let _ = self.stream.set_write_timeout(Some(FLUSH_ON_DROP));
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::GridSize;

    #[test]
    fn a_spawned_pane_is_stamped_with_the_clients_version_not_the_servers() {
        let mut config = SessionConfig::new(GridSize {
            rows: 24,
            cols: 80,
            cell_width: 8,
            cell_height: 16,
        });
        // As if an older layer already put a stale stamp in: the client's replaces it.
        config.env = vec![
            ("TERM_PROGRAM_VERSION".into(), "0.0.8".into()),
            ("OTHER".into(), "kept".into()),
        ];
        let config = stamped(&config);
        let get = |name: &str| {
            config
                .env
                .iter()
                .filter(|(key, _)| key.as_os_str() == std::ffi::OsStr::new(name))
                .map(|(_, value)| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(get("TERM_PROGRAM"), ["kil"]);
        assert_eq!(get("TERM_PROGRAM_VERSION"), [env!("CARGO_PKG_VERSION")]);
        assert_eq!(get("OTHER"), ["kept"]);
    }
}
