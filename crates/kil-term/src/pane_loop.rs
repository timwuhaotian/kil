//! A session's I/O thread (DESIGN.md §3, §14.4): reads the PTY into the terminal, writes
//! input, watches the child's exit, answers the program's queries, and forwards output to
//! the client attached through the session server.
//!
//! Not alacritty's `EventLoop`: that reads before it takes the terminal lock, so an attach
//! landing between a read and its parse would lose or repeat those bytes. Here one thread
//! reads, parses and forwards, and handles attaches, so the hand-off is exact.

use std::{
    borrow::Cow,
    collections::VecDeque,
    io::{self, ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use alacritty_terminal::{
    event::{Event as TermEvent, EventListener, WindowSize},
    sync::FairMutex,
    term::Term,
    vte::ansi::{Processor, StdSyncHandler},
};
use polling::{Event, Events, PollMode, Poller};

use crate::{
    events::{EventProxy, SessionEvent},
    pty::{CHILD_EVENT_TOKEN, Pty, READ_WRITE_TOKEN},
    session::GridSize,
    snapshot::{Palette, Rgba},
    state,
    wire::{self, Frame, Inbox, OUTPUT_CHUNK},
};

/// Poller key of the attached client.
const CLIENT_TOKEN: usize = 2;
/// Bytes parsed per terminal lock, so the UI's snapshots are not starved.
const MAX_LOCKED_READ: usize = 64 << 10;
/// Bytes read before the loop goes back to the poller, so input and commands keep flowing.
const MAX_READ: usize = 1 << 20;
/// With this much output waiting for the client, the PTY is not read: the program waits,
/// as it would for any terminal that falls behind (DESIGN.md §14.4).
const CLIENT_HIGH_WATER: usize = 1 << 20;
/// A client that takes nothing for this long while output waits is dropped, so a stopped
/// or hung app never stalls the program for good. It reattaches with the full state.
const CLIENT_STALL: Duration = Duration::from_secs(2);
/// A full-screen program attached to is made to redraw: shrunk by a row, then this much
/// later restored (DESIGN.md §14.5).
const REDRAW_NUDGE: Duration = Duration::from_millis(30);
/// Time to send a closing client what is queued for it, e.g. the exit status.
const FLUSH_ON_EXIT: Duration = Duration::from_millis(200);

pub(crate) enum Command {
    Input(Cow<'static, [u8]>),
    Resize(WindowSize),
    /// Hand this connection the session: its state, then its output.
    Attach(UnixStream),
    /// Answer color queries from this palette.
    Palette(Palette),
    Shutdown,
}

/// Commands to a pane loop, from any thread.
#[derive(Clone)]
pub(crate) struct LoopSender {
    tx: Sender<Command>,
    poller: Arc<Poller>,
}

impl LoopSender {
    pub(crate) fn send(&self, command: Command) {
        if self.tx.send(command).is_ok() {
            let _ = self.poller.notify();
        }
    }

    pub(crate) fn input(&self, bytes: Cow<'static, [u8]>) {
        self.send(Command::Input(bytes));
    }
}

/// The poller and the command channel, made before the terminal so its replies can reach
/// the loop from the first parse.
pub(crate) struct Channel {
    sender: LoopSender,
    rx: Receiver<Command>,
}

impl Channel {
    pub(crate) fn new() -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let poller = Arc::new(Poller::new()?);
        Ok(Self {
            sender: LoopSender { tx, poller },
            rx,
        })
    }

    pub(crate) fn sender(&self) -> LoopSender {
        self.sender.clone()
    }
}

/// Starts the loop. It ends on [`Command::Shutdown`], a client's `Close`, or the child's
/// exit, and drops the PTY itself (hangup, then the reaper), so nothing ever joins it.
pub(crate) fn spawn(
    channel: Channel,
    term: Arc<FairMutex<Term<EventProxy>>>,
    events: EventProxy,
    pty: Pty,
    palette: Palette,
) -> io::Result<()> {
    let pane_loop = PaneLoop {
        poller: Arc::clone(&channel.sender.poller),
        rx: channel.rx,
        pty,
        term,
        events,
        parser: Processor::new(),
        notifications: crate::notify::Scanner::default(),
        writes: VecDeque::new(),
        writing: 0,
        interest: Event::readable(READ_WRITE_TOKEN),
        client: None,
        nudge: None,
        palette,
    };
    thread::Builder::new()
        .name("kil-pane".into())
        .spawn(move || pane_loop.run())
        .map(drop)
}

struct PaneLoop {
    poller: Arc<Poller>,
    rx: Receiver<Command>,
    pty: Pty,
    term: Arc<FairMutex<Term<EventProxy>>>,
    events: EventProxy,
    parser: Processor<StdSyncHandler>,
    /// Picks OSC 9 and 777 notifications out of the output, which the parser drops.
    notifications: crate::notify::Scanner,
    /// Input for the program; `writing` bytes of the front one are already written.
    writes: VecDeque<Cow<'static, [u8]>>,
    writing: usize,
    /// What the master is registered for.
    interest: Event,
    client: Option<Client>,
    /// When to restore the size shrunk to make a full-screen program redraw.
    nudge: Option<(Instant, WindowSize)>,
    /// What color queries are answered from, after the program's own overrides.
    palette: Palette,
}

/// What ended the loop.
enum Stop {
    Exited(std::process::ExitStatus),
    Closed,
}

impl PaneLoop {
    fn run(mut self) {
        if let Err(err) = self
            .pty
            .register(&self.poller, Event::readable(READ_WRITE_TOKEN))
        {
            log::error!("pane loop registration failed: {err}");
            return;
        }
        let mut events = Events::new();
        let mut buf = vec![0_u8; MAX_LOCKED_READ];
        let stop = loop {
            events.clear();
            if let Err(err) = self.poller.wait(&mut events, self.timeout())
                && err.kind() != ErrorKind::Interrupted
            {
                log::error!("pane loop poll failed: {err}");
                break Stop::Closed;
            }
            self.timers();
            if !self.commands() {
                break Stop::Closed;
            }
            let mut exited = None;
            let mut closed = false;
            for event in events.iter() {
                match event.key {
                    CHILD_EVENT_TOKEN => exited = self.pty.next_child_event().or(exited),
                    READ_WRITE_TOKEN => {
                        if event.readable
                            && let Err(err) = self.read(&mut buf)
                        {
                            log::error!("PTY read failed: {err}");
                            closed = true;
                        }
                        if event.writable
                            && let Err(err) = self.write()
                        {
                            log::error!("PTY write failed: {err}");
                            closed = true;
                        }
                    }
                    CLIENT_TOKEN => closed |= self.serve_client(event),
                    _ => {}
                }
            }
            if let Some(status) = exited {
                break Stop::Exited(status);
            }
            if closed {
                break Stop::Closed;
            }
            self.update_interest();
        };
        if let Stop::Exited(status) = stop {
            self.events.emit(SessionEvent::ChildExited(status));
            // The program's last words, written just before it exited (F13).
            let _ = self.read(&mut buf);
            self.term.lock().exit();
            self.events.send_event(TermEvent::Wakeup);
            if let Some(client) = &mut self.client {
                client.queue(&Frame::Exited(status));
            }
        }
        self.detach(true);
        let _ = self.pty.deregister(&self.poller);
        // Dropping the PTY hangs the session up and hands a live child to the reaper.
    }

    fn timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let sync = self.parser.sync_timeout().sync_timeout();
        let nudge = self.nudge.map(|(at, _)| at);
        let stall = self
            .client
            .as_ref()
            .filter(|client| client.pending() > 0)
            .map(|client| client.progress + CLIENT_STALL);
        [sync, nudge, stall]
            .into_iter()
            .flatten()
            .min()
            .map(|at| at.saturating_duration_since(now))
    }

    fn timers(&mut self) {
        let now = Instant::now();
        if self
            .parser
            .sync_timeout()
            .sync_timeout()
            .is_some_and(|at| at <= now)
        {
            self.parser.stop_sync(&mut *self.term.lock());
            self.events.send_event(TermEvent::Wakeup);
        }
        if let Some((at, size)) = self.nudge
            && at <= now
        {
            self.nudge = None;
            self.pty.on_resize(size);
        }
    }

    /// Drains the command channel; `false` on shutdown.
    fn commands(&mut self) -> bool {
        loop {
            match self.rx.try_recv() {
                Ok(Command::Input(bytes)) => self.writes.push_back(bytes),
                Ok(Command::Resize(size)) => self.pty.on_resize(size),
                Ok(Command::Palette(palette)) => self.palette = palette,
                Ok(Command::Attach(stream)) => {
                    if let Err(err) = self.attach(stream) {
                        log::warn!("attach failed: {err}");
                    }
                }
                Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => return false,
                Err(TryRecvError::Empty) => return true,
            }
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<()> {
        let mut total = 0;
        let mut synced = 0;
        while total < MAX_READ
            && self
                .client
                .as_ref()
                .is_none_or(|client| client.pending() < CLIENT_HIGH_WATER)
        {
            let read = match self.pty.master().read(buf) {
                // macOS reports a hung-up slave as EOF; the child's exit follows.
                Ok(0) => break,
                Ok(read) => read,
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => return Err(err),
            };
            let bytes = &buf[..read];
            {
                let mut term = self.term.lock();
                self.parser.advance(&mut *term, bytes);
            }
            for notification in self.notifications.scan(bytes) {
                self.events.emit(SessionEvent::Notify(notification));
            }
            synced = self.parser.sync_bytes_count();
            if let Some(client) = &mut self.client {
                client.queue_output(bytes);
            }
            total += read;
        }
        self.answer_colors();
        if total > 0 && synced < total {
            self.events.send_event(TermEvent::Wakeup);
        }
        Ok(())
    }

    /// OSC 4/10/11 queries from the parse just done: the program's own overrides first,
    /// then the palette of the app's theme.
    fn answer_colors(&mut self) {
        let queries = self.events.take_color_queries();
        if queries.is_empty() {
            return;
        }
        let term = self.term.lock();
        for query in queries {
            let color = term.colors()[query.index]
                .map_or_else(|| self.palette.slot(query.index), Rgba::from);
            self.writes
                .push_back((query.reply)(color.into()).into_bytes().into());
        }
    }

    fn write(&mut self) -> io::Result<()> {
        while let Some(front) = self.writes.front() {
            match self.pty.master().write(&front[self.writing..]) {
                Ok(0) => break,
                Ok(written) => {
                    self.writing += written;
                    if self.writing == front.len() {
                        self.writes.pop_front();
                        self.writing = 0;
                    }
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    fn update_interest(&mut self) {
        let stalled = self.client.as_ref().is_some_and(|client| {
            client.pending() > 0 && client.progress.elapsed() >= CLIENT_STALL
        });
        if stalled {
            log::warn!("client took nothing for {CLIENT_STALL:?}; detaching it");
            self.detach(false);
        } else if let Some(client) = &mut self.client
            && let Err(err) = client.watch(&self.poller)
        {
            log::warn!("client watch failed: {err}");
            self.detach(false);
        }
        let mut interest = Event::readable(READ_WRITE_TOKEN);
        interest.readable = self
            .client
            .as_ref()
            .is_none_or(|client| client.pending() < CLIENT_HIGH_WATER);
        interest.writable = !self.writes.is_empty();
        if (interest.readable, interest.writable)
            != (self.interest.readable, self.interest.writable)
        {
            self.interest = interest;
            if let Err(err) = self.pty.reregister(&self.poller, interest) {
                log::error!("PTY reregister failed: {err}");
            }
        }
    }

    fn attach(&mut self, stream: UnixStream) -> io::Result<()> {
        self.detach(false);
        stream.set_nonblocking(true)?;
        let (bytes, alt) = {
            let mut term = self.term.lock();
            let title = self.events.title();
            let alt = term
                .mode()
                .contains(alacritty_terminal::term::TermMode::ALT_SCREEN);
            (state::render(&mut term, title.as_deref()), alt)
        };
        let mut client = Client::new(stream);
        client.queue_output(&bytes);
        client.register(&self.poller)?;
        self.client = Some(client);
        if alt {
            self.nudge_redraw();
        }
        Ok(())
    }

    /// Makes a full-screen program redraw, which also sets its scroll region again in both
    /// terminals: alacritty doesn't expose the region, so the state stream can't carry it.
    fn nudge_redraw(&mut self) {
        let size = self.events.size();
        if size.rows < 2 {
            return;
        }
        let shrunk = GridSize {
            rows: size.rows - 1,
            ..size
        };
        self.pty.on_resize(shrunk.into());
        self.nudge = Some((Instant::now() + REDRAW_NUDGE, size.into()));
    }

    /// Reads the client's frames and writes what is queued for it; `true` if it asked to
    /// close the session.
    fn serve_client(&mut self, event: Event) -> bool {
        let Some(client) = &mut self.client else {
            return false;
        };
        let mut frames = Vec::new();
        let alive = (!event.readable || client.receive(&mut frames))
            && (!event.writable || client.flush().is_ok());
        let mut close = false;
        for frame in frames {
            match frame {
                Frame::Input(bytes) => self.writes.push_back(bytes.into()),
                Frame::Resize(size) => self.resize(size),
                Frame::Palette(palette) => self.palette = palette,
                Frame::Close => close = true,
                other => log::warn!("unexpected frame from client: {other:?}"),
            }
        }
        if !alive {
            self.detach(false);
        }
        close
    }

    fn resize(&mut self, size: GridSize) {
        let size = size.clamped();
        self.events.set_size(size);
        self.term.lock().resize(size);
        self.events.mark_dirty();
        self.nudge = None;
        self.pty.on_resize(size.into());
    }

    /// Drops the client, after a last try at sending what is queued for it if `flush`.
    fn detach(&mut self, flush: bool) {
        let Some(mut client) = self.client.take() else {
            return;
        };
        let _ = self.poller.delete(&client.stream);
        if flush {
            let _ = client.stream.set_nonblocking(false);
            let _ = client.stream.set_write_timeout(Some(FLUSH_ON_EXIT));
            let _ = client.flush();
        }
    }
}

/// The connection of the attached client.
struct Client {
    stream: UnixStream,
    inbox: Inbox,
    /// When it last took output, or had none waiting.
    progress: Instant,
    out: Vec<u8>,
    sent: usize,
    registered: bool,
    writable: bool,
}

impl Client {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            inbox: Inbox::default(),
            progress: Instant::now(),
            out: Vec::new(),
            sent: 0,
            registered: false,
            writable: false,
        }
    }

    fn pending(&self) -> usize {
        self.out.len() - self.sent
    }

    fn queue(&mut self, frame: &Frame) {
        self.waiting();
        frame.encode(&mut self.out);
    }

    /// Output starts waiting now, if none was: the stall clock runs from here.
    fn waiting(&mut self) {
        if self.pending() == 0 {
            self.progress = Instant::now();
        }
    }

    fn queue_output(&mut self, bytes: &[u8]) {
        self.waiting();
        for chunk in bytes.chunks(OUTPUT_CHUNK) {
            wire::encode_output(&mut self.out, chunk);
        }
    }

    #[expect(
        unsafe_code,
        reason = "polling requires a source to outlive its registration: the stream lives in \
                  `self`, and `PaneLoop::detach` deletes it before dropping it"
    )]
    fn register(&mut self, poller: &Poller) -> io::Result<()> {
        let mut interest = Event::readable(CLIENT_TOKEN);
        interest.writable = true;
        // SAFETY: see the method's `expect`.
        unsafe { poller.add_with_mode(&self.stream, interest, PollMode::Level)? };
        self.registered = true;
        self.writable = true;
        Ok(())
    }

    /// Watches for writability only while something is queued.
    fn watch(&mut self, poller: &Poller) -> io::Result<()> {
        let wants = self.pending() > 0;
        if self.registered && wants != self.writable {
            let mut interest = Event::readable(CLIENT_TOKEN);
            interest.writable = wants;
            poller.modify_with_mode(&self.stream, interest, PollMode::Level)?;
            self.writable = wants;
        }
        Ok(())
    }

    /// Appends the frames that arrived; `false` once the client is gone.
    fn receive(&mut self, frames: &mut Vec<Frame>) -> bool {
        let open = matches!(self.inbox.fill(&mut self.stream), Ok(true));
        loop {
            match self.inbox.next() {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => return open,
                Err(err) => {
                    log::warn!("bad frame from client: {err}");
                    return false;
                }
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        while self.sent < self.out.len() {
            match self.stream.write(&self.out[self.sent..]) {
                Ok(0) => return Err(ErrorKind::WriteZero.into()),
                Ok(written) => {
                    self.sent += written;
                    self.progress = Instant::now();
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                Err(err) => return Err(err),
            }
        }
        if self.sent == self.out.len() {
            self.out.clear();
            self.sent = 0;
        }
        Ok(())
    }
}
