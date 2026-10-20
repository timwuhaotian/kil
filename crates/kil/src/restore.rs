//! The window's tabs, splits, screens and running jobs: saved while kil runs and when it
//! quits, reopened at the next launch however the last one ended (DESIGN.md §4.9).

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use gpui::{Bounds, Pixels, WindowBounds, point, px, size};
use kil_term::Transcript;
use serde::{Deserialize, Serialize};

use crate::pane_tree::Shape;

/// One pane: where its shell was, the job it was running, and its screen (`panes/<id>.ansi`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPane {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Typed into the new shell to run the job again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Index of the focused pane in leaf order.
    #[serde(default)]
    pub focused: usize,
    pub root: Shape<SavedPane>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowState {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

/// The window's frame; for maximized and full screen, the frame it returns to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub state: WindowState,
}

impl SavedWindow {
    pub fn of(bounds: WindowBounds) -> Self {
        let (state, frame) = match bounds {
            WindowBounds::Windowed(frame) => (WindowState::Windowed, frame),
            WindowBounds::Maximized(frame) => (WindowState::Maximized, frame),
            WindowBounds::Fullscreen(frame) => (WindowState::Fullscreen, frame),
        };
        Self {
            x: f32::from(frame.origin.x),
            y: f32::from(frame.origin.y),
            width: f32::from(frame.size.width),
            height: f32::from(frame.size.height),
            state,
        }
    }

    pub fn bounds(self) -> WindowBounds {
        let frame = Bounds::new(
            point(px(self.x), px(self.y)),
            size(px(self.width), px(self.height)),
        );
        match self.state {
            WindowState::Windowed => WindowBounds::Windowed(frame),
            WindowState::Maximized => WindowBounds::Maximized(frame),
            WindowState::Fullscreen => WindowBounds::Fullscreen(frame),
        }
    }

    /// The frame, if it is a sane size and still on one of `displays` (a monitor may be gone).
    pub fn on_screen(self, displays: &[Bounds<Pixels>]) -> Option<WindowBounds> {
        let frame = self.bounds().get_bounds();
        let sane = [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            && self.width >= 100.
            && self.height >= 100.;
        (sane && displays.iter().any(|display| display.intersects(&frame))).then(|| self.bounds())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    #[serde(default)]
    pub active: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<SavedWindow>,
    #[serde(default)]
    pub tabs: Vec<SavedTab>,
}

/// What the last launch left.
#[derive(Debug, PartialEq)]
pub struct Restored {
    pub saved: SavedWorkspace,
    /// False when the launch before this one ended while it was reopening the same tabs: if
    /// a re-run job crashed kil, running it again would crash it again.
    pub rerun: bool,
}

/// `~/Library/Application Support/kil`: `session.toml`, one `panes/<id>.ansi` per pane, and
/// `restoring` while a launch reopens them.
#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
    /// Saves are numbered when taken and run on any thread; an older one never overwrites a
    /// newer `session.toml`.
    taken: Arc<AtomicU64>,
    written: Arc<Mutex<u64>>,
    /// A hash of each pane's screen as last written. A pane that printed without changing
    /// what it shows (a prompt drawn again, a spinner) is offered again at every save.
    screens: Arc<Mutex<HashMap<String, u64>>>,
}

impl Store {
    pub fn standard() -> Option<Self> {
        let home = std::env::var_os("HOME")?;
        Some(Self::at(
            PathBuf::from(home).join("Library/Application Support/kil"),
        ))
    }

    /// The directory, which the session server shares (DESIGN.md §14.3).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn at(dir: PathBuf) -> Self {
        Self {
            dir,
            taken: Arc::new(AtomicU64::new(0)),
            written: Arc::new(Mutex::new(0)),
            screens: Arc::default(),
        }
    }

    fn session(&self) -> PathBuf {
        self.dir.join("session.toml")
    }

    fn marker(&self) -> PathBuf {
        self.dir.join("restoring")
    }

    fn transcript_path(&self, id: &str) -> PathBuf {
        self.dir.join("panes").join(format!("{id}.ansi"))
    }

    /// Reads what the last launch saved, and marks this launch as reopening it until
    /// [`Self::settled`].
    ///
    /// # Errors
    /// If the file can't be read or doesn't parse.
    pub fn load(&self) -> Result<Option<Restored>, String> {
        let text = match fs::read_to_string(self.session()) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("session.toml: {err}")),
        };
        let rerun = !self.marker().exists();
        if let Err(err) = fs::write(self.marker(), "") {
            log::warn!("restore marker not written: {err}");
        }
        let saved = toml::from_str(&text).map_err(|err| format!("session.toml: {err}"))?;
        Ok(Some(Restored { saved, rerun }))
    }

    /// This launch is past reopening; a crash from now on reruns its jobs next time.
    pub fn settled(&self) {
        remove(&self.marker());
    }

    /// The screen saved for pane `id`; empty if there is none.
    pub fn transcript(&self, id: &str) -> Vec<u8> {
        if !is_valid_id(id) {
            return Vec::new();
        }
        fs::read(self.transcript_path(id)).unwrap_or_default()
    }

    /// Numbers a save, before its contents are taken.
    pub fn ticket(&self) -> u64 {
        self.taken.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Writes the screens, then `saved` (unless a newer save already wrote it), then removes
    /// the screens of panes that are gone. Safe to call from any thread.
    ///
    /// # Errors
    /// If a file can't be written.
    pub fn save(
        &self,
        ticket: u64,
        saved: &SavedWorkspace,
        screens: &[(String, Transcript)],
    ) -> Result<(), String> {
        let fail = |err: io::Error| format!("session.toml: {err}");
        fs::create_dir_all(self.dir.join("panes")).map_err(fail)?;
        // Rendered now, from the live terminals: a late write is never older than an early one.
        for (id, transcript) in screens {
            if let Some(bytes) = transcript.render().filter(|_| is_valid_id(id)) {
                self.write_screen(id, &bytes).map_err(fail)?;
            }
        }
        let text = toml::to_string(saved).map_err(|err| format!("session.toml: {err}"))?;
        let mut written = self
            .written
            .lock()
            .map_err(|_| "session.toml: a save panicked".to_owned())?;
        if ticket < *written {
            return Ok(());
        }
        write_whole(&self.session(), text.as_bytes()).map_err(fail)?;
        *written = ticket;
        self.forget_others(saved);
        Ok(())
    }

    /// Removes everything: the next launch starts fresh. Saves already taken write nothing.
    pub fn clear(&self) {
        let ticket = self.ticket();
        if let Ok(mut written) = self.written.lock() {
            *written = ticket;
        }
        self.screens().clear();
        remove(&self.session());
        remove(&self.marker());
        if let Err(err) = fs::remove_dir_all(self.dir.join("panes"))
            && err.kind() != io::ErrorKind::NotFound
        {
            log::warn!("saved screens not removed: {err}");
        }
    }

    fn forget_others(&self, saved: &SavedWorkspace) {
        let mut kept = HashSet::new();
        for tab in &saved.tabs {
            tab.root
                .clone()
                .map(&mut |pane: SavedPane| kept.insert(pane.id));
        }
        let Ok(entries) = fs::read_dir(self.dir.join("panes")) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("");
            // Only finished screens: a `.partial` may be another save's write in flight.
            if path.extension().is_some_and(|ext| ext == "ansi") && !kept.contains(id) {
                remove(&path);
            }
        }
        self.screens().retain(|id, _| kept.contains(id));
    }

    fn screens(&self) -> std::sync::MutexGuard<'_, HashMap<String, u64>> {
        self.screens.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes `bytes` as pane `id`'s screen, unless they are what its file already holds.
    fn write_screen(&self, id: &str, bytes: &[u8]) -> io::Result<()> {
        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        let hash = hasher.finish();
        // Held across the write, so two saves can't each skip or record the other's.
        let mut screens = self.screens();
        if screens.get(id) == Some(&hash) {
            return Ok(());
        }
        write_whole(&self.transcript_path(id), bytes)?;
        screens.insert(id.to_owned(), hash);
        Ok(())
    }
}

/// Written beside, then renamed over: a crash mid-write never leaves half a file.
fn write_whole(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path)
}

fn remove(path: &Path) {
    if let Err(err) = fs::remove_file(path)
        && err.kind() != io::ErrorKind::NotFound
    {
        log::warn!("{} not removed: {err}", path.display());
    }
}

/// Ids name files: a hand-edited one must not reach outside `panes/`.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Pane ids unique across launches: the launch's start time, then a counter.
pub struct PaneIds {
    launch: u128,
    next: u64,
}

impl PaneIds {
    pub fn new() -> Self {
        let launch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis());
        Self { launch, next: 0 }
    }

    pub fn fresh(&mut self) -> String {
        self.next += 1;
        format!("{:x}-{}", self.launch, self.next)
    }
}

/// `args` as a command line that, typed into a shell, runs the same program with the same
/// arguments. `None` if an argument isn't UTF-8.
pub fn command_line(args: &[OsString]) -> Option<String> {
    let mut line = String::new();
    for arg in args {
        let arg = arg.to_str()?;
        if !line.is_empty() {
            line.push(' ');
        }
        let plain = !arg.is_empty()
            // zsh expands a leading `=` to a command's path.
            && !arg.starts_with('=')
            && arg
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"@+=:,./-_".contains(&byte));
        if plain {
            line.push_str(arg);
        } else {
            line.push('\'');
            line.push_str(&arg.replace('\'', r"'\''"));
            line.push('\'');
        }
    }
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane_tree::Axis;

    fn line(args: &[&str]) -> Option<String> {
        command_line(&args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    fn scratch(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("kil-restore-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Store::at(dir)
    }

    fn pane(id: &str, cwd: &str, command: Option<&str>) -> Shape<SavedPane> {
        Shape::Leaf(SavedPane {
            id: id.to_owned(),
            cwd: Some(cwd.to_owned()),
            command: command.map(str::to_owned),
        })
    }

    #[test]
    fn command_line_should_leave_plain_arguments_bare() {
        assert_eq!(
            line(&["npm", "run", "dev", "--port=3000"]).as_deref(),
            Some("npm run dev --port=3000")
        );
    }

    #[test]
    fn command_line_should_quote_spaces_quotes_and_empty_arguments() {
        assert_eq!(
            line(&["echo", "a b", "it's", "", "$HOME", "=ls"]).as_deref(),
            Some(r"echo 'a b' 'it'\''s' '' '$HOME' '=ls'")
        );
    }

    #[test]
    fn saved_workspace_should_survive_the_file_format() {
        let saved = SavedWorkspace {
            active: 1,
            window: Some(SavedWindow {
                x: 10.,
                y: 20.,
                width: 900.,
                height: 600.,
                state: WindowState::Maximized,
            }),
            tabs: vec![
                SavedTab {
                    name: None,
                    focused: 0,
                    root: Shape::Leaf(SavedPane::default()),
                },
                SavedTab {
                    name: Some("构建".to_owned()),
                    focused: 1,
                    root: Shape::Split {
                        axis: Axis::Horizontal,
                        ratio: 0.3,
                        first: Box::new(pane("a-1", "/tmp", Some("cargo watch -x 'test --lib'"))),
                        second: Box::new(Shape::Split {
                            axis: Axis::Vertical,
                            ratio: 0.5,
                            first: Box::new(pane("a-2", "/", None)),
                            second: Box::new(pane("a-3", "/usr", Some("vim"))),
                        }),
                    },
                },
            ],
        };
        let store = scratch("format");
        store.save(store.ticket(), &saved, &[]).unwrap();
        let restored = store.load().unwrap().unwrap();
        assert_eq!((restored.saved, restored.rerun), (saved, true));
    }

    #[test]
    fn load_should_not_rerun_after_a_launch_that_ended_while_reopening() {
        let store = scratch("crash-loop");
        store
            .save(store.ticket(), &SavedWorkspace::default(), &[])
            .unwrap();
        assert!(store.load().unwrap().unwrap().rerun);
        // That launch died before settling.
        assert!(!store.load().unwrap().unwrap().rerun);
        store.settled();
        assert!(store.load().unwrap().unwrap().rerun);
    }

    #[test]
    fn save_should_not_let_an_older_save_overwrite_a_newer_one() {
        let store = scratch("order");
        let (older, newer) = (store.ticket(), store.ticket());
        let named = |active| SavedWorkspace {
            active,
            ..SavedWorkspace::default()
        };
        store.save(newer, &named(2), &[]).unwrap();
        store.save(older, &named(1), &[]).unwrap();
        assert_eq!(store.load().unwrap().unwrap().saved.active, 2);
    }

    #[test]
    fn save_should_remove_screens_of_panes_that_are_gone() {
        let store = scratch("forget");
        fs::create_dir_all(store.dir.join("panes")).unwrap();
        fs::write(store.transcript_path("old-1"), "x").unwrap();
        fs::write(store.transcript_path("kept-1"), "y").unwrap();
        let saved = SavedWorkspace {
            tabs: vec![SavedTab {
                name: None,
                focused: 0,
                root: pane("kept-1", "/", None),
            }],
            ..SavedWorkspace::default()
        };
        store.save(store.ticket(), &saved, &[]).unwrap();
        assert_eq!(
            (store.transcript("old-1"), store.transcript("kept-1")),
            (Vec::new(), b"y".to_vec())
        );
    }

    #[test]
    fn a_screen_should_be_written_only_when_it_changed() {
        use std::os::unix::fs::MetadataExt;

        let store = scratch("unchanged");
        fs::create_dir_all(store.dir.join("panes")).unwrap();
        // Every write is a new file renamed into place, so a new inode means a rewrite.
        let inode = || fs::metadata(store.transcript_path("a-1")).unwrap().ino();
        store.write_screen("a-1", b"same").unwrap();
        let first = inode();
        store.write_screen("a-1", b"same").unwrap();
        assert_eq!(inode(), first);
        store.write_screen("a-1", b"changed").unwrap();
        assert_ne!(inode(), first);
        // After a clear the file is gone, whatever was written before.
        store.clear();
        fs::create_dir_all(store.dir.join("panes")).unwrap();
        store.write_screen("a-1", b"changed").unwrap();
        assert_eq!(store.transcript("a-1"), b"changed");
    }

    #[test]
    fn transcript_should_refuse_an_id_that_leaves_the_directory() {
        assert!(!is_valid_id("../session"));
        assert!(is_valid_id("18f3a2b4c1d-12"));
    }

    #[test]
    fn on_screen_should_drop_a_frame_on_a_missing_display() {
        let display = Bounds::new(point(px(0.), px(0.)), size(px(1440.), px(900.)));
        let frame = |x| SavedWindow {
            x,
            y: 100.,
            width: 800.,
            height: 600.,
            state: WindowState::Windowed,
        };
        assert!(frame(100.).on_screen(&[display]).is_some());
        assert!(frame(5000.).on_screen(&[display]).is_none());
    }
}
