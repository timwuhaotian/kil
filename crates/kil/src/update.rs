//! Kil updates itself (DESIGN.md §15). It reads `releases/latest.toml` on `main`; when that
//! names a newer version, it downloads the DMG beside it, checks the checksum, the version,
//! the Developer ID team and Apple's notarization, and keeps the app from it until Kil quits.
//! Quitting then replaces the running bundle, so the next launch is the new version, and the
//! jobs, which run in the session server, never notice.

use std::{
    ffi::OsStr,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use gpui::{App, AppContext as _, Global, MenuItem, actions};
use serde::Deserialize;

use crate::{
    settings::Settings,
    update_window::{self, Shown},
    workspace,
};

actions!(update, [CheckForUpdates, RestartToUpdate]);

/// Raw from `main`, where the release commit puts the DMG and this file together. The raw
/// host serves them: `github.com/<repo>/raw/<branch>/<file>` answers 504 on GitHub's edge,
/// so that route is never used.
const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/timwuhaotian/kil/main/releases/latest.toml";
const DMG_URL: &str =
    "https://raw.githubusercontent.com/timwuhaotian/kil/main/releases/Kil-arm64.dmg";
/// The changelog on `main`; the running build predates the new release's entries, so they
/// cannot be read from anything it already has.
const CHANGELOG_URL: &str = "https://raw.githubusercontent.com/timwuhaotian/kil/main/CHANGELOG.md";
/// After launch, so the check never competes with reopening the session.
const FIRST_CHECK: Duration = Duration::from_secs(5);
/// A terminal stays open for days; one wakeup in hours keeps idle CPU at zero (A12).
const CHECK_EVERY: Duration = Duration::from_hours(6);
/// How often a download's size is read, and the update window redraws its bar.
pub const POLL: Duration = Duration::from_millis(100);

/// What a check is doing, for the update window to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Step {
    #[default]
    Checking,
    /// The DMG's bytes so far, and its size once the server has said it.
    Downloading {
        version: String,
        got: u64,
        total: Option<u64>,
    },
    Verifying {
        version: String,
    },
}

/// A check's current step: written by its thread, read by the update window.
#[derive(Debug, Default)]
pub struct Progress(Mutex<Step>);

impl Progress {
    pub fn step(&self) -> Step {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, step: Step) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = step;
    }
}

/// `releases/latest.toml`: the release's version and its DMG's SHA-256.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: String,
    pub sha256: String,
}

impl Manifest {
    /// # Errors
    /// If `text` isn't a manifest.
    pub fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|err| format!("latest.toml: {err}"))
    }
}

/// `major.minor.patch`, compared as numbers.
fn parse_version(text: &str) -> Option<[u64; 3]> {
    let mut parts = text.trim().split('.').map(|part| part.parse().ok());
    let version = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(version)
}

/// Whether `candidate` is a later version than `current`; anything unreadable is not.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    matches!(
        (parse_version(candidate), parse_version(current)),
        (Some(candidate), Some(current)) if candidate > current
    )
}

/// Changelog text without its markdown: bold markers and backticks go, a link keeps its words.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        if !rest[open + close..].starts_with("](") {
            // Not a link (the changelog's own `[Image #N]`): keep it and scan on.
            out.push_str(&rest[..=open]);
            rest = &rest[open + 1..];
            continue;
        }
        let Some(end) = rest[open + close..].find(')') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..open + close]);
        rest = &rest[open + close + end + 1..];
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "")
}

/// The entries for every version after `current` up to `candidate`, as plain text for the
/// update window: a `Kil x.y.z` line per version, then its bullets unwrapped. Text that
/// doesn't parse gives nothing, and the window asks without notes.
fn release_notes(changelog: &str, current: &str, candidate: &str) -> String {
    let (Some(current), Some(candidate)) = (parse_version(current), parse_version(candidate))
    else {
        return String::new();
    };
    let mut notes: Vec<String> = Vec::new();
    let mut include = false;
    for line in changelog.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            let version = heading.split_whitespace().next().unwrap_or_default();
            include = parse_version(version).is_some_and(|v| v > current && v <= candidate);
            if include {
                if !notes.is_empty() {
                    notes.push(String::new());
                }
                notes.push(format!("Kil {version}"));
            }
            continue;
        }
        if !include {
            continue;
        }
        let line = line.trim();
        if let Some(item) = line.strip_prefix("- ") {
            notes.push(format!("• {}", plain(item)));
        } else if !line.is_empty() {
            match notes.last_mut().filter(|last| last.starts_with('•')) {
                Some(last) => {
                    last.push(' ');
                    last.push_str(&plain(line));
                }
                None => notes.push(plain(line)),
            }
        }
    }
    notes.join("\n").trim().to_owned()
}

/// The `.app` that `exe` runs in, if Kil may replace it: not a bare binary (`cargo run`), not
/// the copy inside the DMG, and not one Gatekeeper translocated to a read-only place.
fn replaceable_bundle(exe: &Path) -> Result<PathBuf, &'static str> {
    let bundle = exe
        .parent()
        .filter(|dir| dir.ends_with("Contents/MacOS"))
        .and_then(|dir| dir.parent()?.parent())
        .filter(|bundle| bundle.extension().is_some_and(|ext| ext == "app"))
        .ok_or("Kil isn't running from Kil.app")?;
    let path = bundle.to_string_lossy();
    if path.starts_with("/Volumes/") || path.contains("/AppTranslocation/") {
        return Err("Kil is running from the disk image: drag it to Applications first");
    }
    Ok(bundle.to_owned())
}

/// Runs `program`; a failure is its stderr, else how it ended.
fn run(program: &str, args: &[&OsStr]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("{program}: {err}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    Err(failure(program, output.status, &output.stderr))
}

fn failure(program: &str, status: ExitStatus, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let name = Path::new(program).file_name().unwrap_or_default().display();
    let why = stderr.trim();
    match why.strip_prefix(&format!("{name}: ")) {
        // curl writes its own "curl: (56) …"; saying it twice helps nobody.
        Some(_) => why.to_owned(),
        None if why.is_empty() => {
            curl_hint(program, status).unwrap_or_else(|| format!("{name}: {status}"))
        }
        None => format!("{name}: {why}"),
    }
}

/// curl's silent exits in plain language: with `-sS` it prints nothing on some failures,
/// and `curl: exit status: 1` says nothing. `None` for other programs and exits.
fn curl_hint(program: &str, status: ExitStatus) -> Option<String> {
    if Path::new(program)
        .file_name()
        .is_none_or(|name| name != "curl")
    {
        return None;
    }
    let hint = match status.code() {
        Some(6 | 7) => {
            format!(
                "Couldn't reach the update server — check your connection (curl exit {})",
                status.code().unwrap_or_default()
            )
        }
        Some(28) => {
            "The update server is taking too long — try again later (curl exit 28)".to_owned()
        }
        _ => return None,
    };
    Some(hint)
}

/// The body size the last response in a `curl -D` dump gives; redirects come before it.
fn content_length(headers: &str) -> Option<u64> {
    let last = headers
        .rsplit_once("\nHTTP/")
        .map_or(headers, |(_, last)| last);
    last.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .filter(|&size| size > 0)
}

/// Downloads `url` to `to`, telling `progress` how much has arrived, by the file's size and
/// the headers curl writes before the body.
fn download(url: &str, to: &Path, version: &str, progress: &Progress) -> Result<(), String> {
    const CURL: &str = "/usr/bin/curl";
    let headers = to.with_extension("headers");
    let mut curl = Command::new(CURL)
        .args(["-fsSL", "--max-time", "900", "-D"])
        .arg(&headers)
        .arg("-o")
        .arg(to)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("{CURL}: {err}"))?;
    let report = || {
        progress.set(Step::Downloading {
            version: version.to_owned(),
            got: fs::metadata(to).map_or(0, |file| file.len()),
            total: fs::read_to_string(&headers)
                .ok()
                .and_then(|headers| content_length(&headers)),
        });
    };
    let status = loop {
        report();
        match curl.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(POLL),
            Err(err) => return Err(format!("{CURL}: {err}")),
        }
    };
    report();
    let _ = fs::remove_file(&headers);
    if status.success() {
        return Ok(());
    }
    let mut stderr = Vec::new();
    if let Some(mut pipe) = curl.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }
    Err(failure(CURL, status, &stderr))
}

/// The Developer ID team that signed `app`; `None` for an ad-hoc signature (a local build).
fn team_of(app: &Path) -> Option<String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=2"])
        .arg(app)
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .filter(|team| *team != "not set")
        .map(str::to_owned)
}

/// The version `app`'s Info.plist shows.
fn bundle_version(app: &Path) -> Result<String, String> {
    let plist = app.join("Contents/Info.plist");
    run(
        "/usr/libexec/PlistBuddy",
        &[
            "-c".as_ref(),
            "Print :CFBundleShortVersionString".as_ref(),
            plist.as_os_str(),
        ],
    )
}

/// Checks that `app` is Kil `version`, signed by `team` and notarized by Apple.
fn verify(app: &Path, version: &str, team: &str) -> Result<(), String> {
    let shown = bundle_version(app)?;
    if shown != version {
        return Err(format!("the download is Kil {shown}, not {version}"));
    }
    let requirement =
        format!("=anchor apple generic and certificate leaf[subject.OU] = \"{team}\"");
    run(
        "/usr/bin/codesign",
        &[
            "--verify".as_ref(),
            "--deep".as_ref(),
            "--strict".as_ref(),
            "-R".as_ref(),
            requirement.as_ref(),
            app.as_os_str(),
        ],
    )
    .map_err(|err| format!("the download isn't signed as Kil is ({err})"))?;
    run(
        "/usr/sbin/spctl",
        &[
            "--assess".as_ref(),
            "--type".as_ref(),
            "execute".as_ref(),
            app.as_os_str(),
        ],
    )
    .map_err(|err| format!("Apple hasn't notarized the download ({err})"))?;
    Ok(())
}

/// The text at `url`.
fn fetch_text(url: &str) -> Result<String, String> {
    run(
        "/usr/bin/curl",
        &[
            "-fsSL".as_ref(),
            "--max-time".as_ref(),
            "30".as_ref(),
            url.as_ref(),
        ],
    )
}

/// Reads the manifest at `url`.
///
/// # Errors
/// If it can't be fetched or parsed.
pub fn fetch_manifest(url: &str) -> Result<Manifest, String> {
    Manifest::parse(&fetch_text(url)?)
}

/// What changed between `current` and `candidate`, from the changelog on `main`. Notes are
/// decoration on the ask, so any failure is just no notes, never a failed update.
fn fetch_notes(current: &str, candidate: &str) -> String {
    fetch_text(CHANGELOG_URL)
        .map(|changelog| release_notes(&changelog, current, candidate))
        .unwrap_or_default()
}

/// Downloads the DMG at `dmg_url` into `dir`, checks it against `manifest` and `team`, and
/// returns the app copied out of it. An app staged earlier for the same version is reused.
///
/// # Errors
/// If the download fails or any check does.
pub fn stage(
    manifest: &Manifest,
    dmg_url: &str,
    dir: &Path,
    team: &str,
    progress: &Progress,
) -> Result<PathBuf, String> {
    let version = &manifest.version;
    let verifying = || {
        progress.set(Step::Verifying {
            version: version.clone(),
        });
    };
    let app = dir.join("Kil.app");
    if app.exists() {
        verifying();
        if verify(&app, version, team).is_ok() {
            return Ok(app);
        }
    }
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let dmg = dir.join("Kil.dmg");
    download(dmg_url, &dmg, version, progress)?;
    verifying();
    let sum = run(
        "/usr/bin/shasum",
        &["-a".as_ref(), "256".as_ref(), dmg.as_os_str()],
    )?;
    if !sum
        .split_whitespace()
        .next()
        .is_some_and(|sum| sum.eq_ignore_ascii_case(&manifest.sha256))
    {
        return Err("the download doesn't match its checksum".to_owned());
    }
    let mount = dir.join("mnt");
    run(
        "/usr/bin/hdiutil",
        &[
            "attach".as_ref(),
            "-nobrowse".as_ref(),
            "-readonly".as_ref(),
            "-noautoopen".as_ref(),
            "-quiet".as_ref(),
            "-mountpoint".as_ref(),
            mount.as_os_str(),
            dmg.as_os_str(),
        ],
    )?;
    let copied = run(
        "/usr/bin/ditto",
        &[mount.join("Kil.app").as_os_str(), app.as_os_str()],
    );
    let detach = |force: bool| {
        let mut args = vec!["detach".as_ref(), "-quiet".as_ref(), mount.as_os_str()];
        if force {
            args.push("-force".as_ref());
        }
        run("/usr/bin/hdiutil", &args)
    };
    if detach(false).is_err() {
        let _ = detach(true);
    }
    copied?;
    let _ = fs::remove_file(&dmg);
    verify(&app, version, team).inspect_err(|_| {
        let _ = fs::remove_dir_all(&app);
    })?;
    Ok(app)
}

/// Replaces `bundle` with `staged`: copied in beside it, then two renames, so `bundle` is never
/// missing or half-written, and a failure leaves the old app in place. The running app keeps
/// the code it loaded.
///
/// # Errors
/// If the folder can't be written or a rename fails.
pub fn install(staged: &Path, bundle: &Path) -> Result<(), String> {
    let (Some(parent), Some(name)) = (bundle.parent(), bundle.file_name()) else {
        return Err(format!("{}: not an app", bundle.display()));
    };
    let name = name.to_string_lossy();
    let new = parent.join(format!(".{name}.new"));
    let old = parent.join(format!(".{name}.old"));
    for leftover in [&new, &old] {
        let _ = fs::remove_dir_all(leftover);
    }
    run("/usr/bin/ditto", &[staged.as_os_str(), new.as_os_str()])?;
    if let Err(err) = fs::rename(bundle, &old) {
        let _ = fs::remove_dir_all(&new);
        return Err(format!("{}: {err}", bundle.display()));
    }
    if let Err(err) = fs::rename(&new, bundle) {
        let _ = fs::rename(&old, bundle);
        let _ = fs::remove_dir_all(&new);
        return Err(format!("{}: {err}", bundle.display()));
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

/// Whether Kil can create files in `dir`, which replacing the bundle needs.
fn writable(dir: &Path) -> Result<(), String> {
    let probe = dir.join(format!(".kil-update-{}", std::process::id()));
    fs::create_dir(&probe)
        .and_then(|()| fs::remove_dir(&probe))
        .map_err(|err| format!("Kil can't replace itself in {} ({err})", dir.display()))
}

/// `~/Library/Caches/dev.kil.terminal/update`.
fn cache_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Caches/dev.kil.terminal/update"))
        .ok_or_else(|| "no home directory".to_owned())
}

/// A newer Kil, checked and waiting for Kil to quit.
#[derive(Clone)]
struct Ready {
    version: String,
    /// Its changelog entries, plain text; empty when they couldn't be fetched or parsed.
    notes: String,
    app: PathBuf,
    bundle: PathBuf,
}

enum Found {
    UpToDate,
    Ready(Ready),
}

/// One check, start to end; blocks, so it runs on a background thread.
fn find(current: &str, progress: &Progress) -> Result<Found, String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let bundle = replaceable_bundle(&exe)?;
    let team = team_of(&bundle)
        .ok_or("this copy of Kil isn't signed with a Developer ID, so it doesn't update itself")?;
    let manifest = fetch_manifest(MANIFEST_URL)?;
    if !is_newer(&manifest.version, current) {
        return Ok(Found::UpToDate);
    }
    writable(bundle.parent().unwrap_or(&bundle))?;
    let app = stage(&manifest, DMG_URL, &cache_dir()?, &team, progress)?;
    let notes = fetch_notes(current, &manifest.version);
    Ok(Found::Ready(Ready {
        version: manifest.version,
        notes,
        app,
        bundle,
    }))
}

#[derive(Default)]
struct Updater {
    /// The check under way.
    checking: Option<Arc<Progress>>,
    /// Someone chose Check for Updates, and is told the outcome.
    asked: bool,
    ready: Option<Ready>,
    /// Restart to Update: open the new app once this one has quit.
    reopen: bool,
    /// Completion signals for the check under way (Settings' Check Now).
    done: Vec<DoneCallback>,
}

type DoneCallback = Box<dyn FnOnce(&mut App) + 'static>;

impl Global for Updater {}

/// Starts the checks and installs a staged update whenever Kil quits.
pub fn init(cx: &mut App) {
    cx.set_global(Updater::default());
    cx.on_action(|_: &CheckForUpdates, cx| check_now(cx));
    cx.on_action(|_: &RestartToUpdate, cx| restart(cx));
    cx.on_app_quit(|cx| {
        install_on_quit(cx);
        async {}
    })
    .detach();
    cx.spawn(async move |cx| {
        let mut wait = FIRST_CHECK;
        loop {
            cx.background_executor().timer(wait).await;
            wait = CHECK_EVERY;
            let checked = cx.update(|cx| {
                if cx.global::<Settings>().auto_update {
                    check(false, None, cx);
                }
            });
            if checked.is_err() {
                return;
            }
        }
    })
    .detach();
}

/// The Kil menu's update item: Check for Updates…, or Restart to Update once one is ready.
pub fn menu_item(cx: &App) -> MenuItem {
    let ready = cx
        .try_global::<Updater>()
        .is_some_and(|updater| updater.ready.is_some());
    if ready {
        MenuItem::action("Restart to Update", RestartToUpdate)
    } else {
        MenuItem::action("Check for Updates…", CheckForUpdates)
    }
}

/// Check for Updates…: a check whose progress and outcome the update window shows.
pub fn check_now(cx: &mut App) {
    check(true, None, cx);
}

/// As [`check_now`], running `on_done` when the check ends: at once if an update is
/// already ready, else when the check under way (or the one it starts) resolves.
pub fn check_now_with_callback(cx: &mut App, on_done: impl FnOnce(&mut App) + 'static) {
    check(true, Some(Box::new(on_done)), cx);
}

/// Restart to Update: quits without asking about jobs, installs, and opens the new Kil.
pub fn restart(cx: &mut App) {
    if cx.global::<Updater>().ready.is_some() {
        cx.global_mut::<Updater>().reopen = true;
        workspace::detach_and_quit(cx);
    }
}

fn check(asked: bool, on_done: Option<DoneCallback>, cx: &mut App) {
    let updater = cx.global_mut::<Updater>();
    if let Some(ready) = &updater.ready {
        if asked {
            let shown = Shown::Ready {
                version: ready.version.clone(),
                notes: ready.notes.clone(),
            };
            update_window::show(shown, cx);
        }
        if let Some(done) = on_done {
            done(cx);
        }
        return;
    }
    updater.asked |= asked;
    if let Some(done) = on_done {
        updater.done.push(done);
    }
    let progress = match updater.checking.clone() {
        Some(progress) => progress,
        None => start(cx),
    };
    if asked {
        update_window::show(Shown::Working(progress), cx);
    }
}

/// Starts a check on a background thread; `checked` gets its outcome.
fn start(cx: &mut App) -> Arc<Progress> {
    let progress = Arc::new(Progress::default());
    cx.global_mut::<Updater>().checking = Some(progress.clone());
    let found = cx.background_spawn({
        let progress = progress.clone();
        async move { find(env!("CARGO_PKG_VERSION"), &progress) }
    });
    cx.spawn(async move |cx| {
        let found = found.await;
        let _ = cx.update(|cx| checked(found, cx));
    })
    .detach();
    progress
}

fn checked(found: Result<Found, String>, cx: &mut App) {
    let updater = cx.global_mut::<Updater>();
    updater.checking = None;
    let asked = std::mem::take(&mut updater.asked);
    let shown = match found {
        Ok(Found::Ready(ready)) => {
            log::info!("Kil {} is ready to install", ready.version);
            let shown = Shown::Ready {
                version: ready.version.clone(),
                notes: ready.notes.clone(),
            };
            updater.ready = Some(ready);
            cx.set_menus(crate::menus(cx));
            shown
        }
        Ok(Found::UpToDate) => Shown::UpToDate,
        Err(err) => {
            log::warn!("update check: {err}");
            Shown::Failed(err)
        }
    };
    // A ready update asks even when the check was Kil's own periodic one; nothing else
    // from a background check is worth interrupting for. The ask happens once per run:
    // later checks stop at the ready update in `check` and never reach here again.
    // The background ask opens passively, so it never steals focus.
    if asked {
        update_window::show(shown, cx);
    } else if matches!(shown, Shown::Ready { .. }) {
        update_window::show_passive(shown, cx);
    }
    let done = std::mem::take(&mut cx.global_mut::<Updater>().done);
    for run in done {
        run(cx);
    }
}

fn install_on_quit(cx: &mut App) {
    let Some(updater) = cx.try_global::<Updater>() else {
        return;
    };
    let Some(ready) = updater.ready.clone() else {
        return;
    };
    let reopen = updater.reopen;
    // The bundle may have been replaced by hand since the update was staged (a maintainer
    // installing a release); a newer app there stays, and the stale download goes.
    let on_disk = bundle_version(&ready.bundle).ok();
    let stale = on_disk
        .as_deref()
        .is_some_and(|on_disk| !is_newer(&ready.version, on_disk));
    if stale {
        log::info!(
            "Kil {} is installed already; not replacing it with {}",
            on_disk.unwrap_or_default(),
            ready.version
        );
    } else if let Err(err) = install(&ready.app, &ready.bundle) {
        log::error!("installing Kil {}: {err}", ready.version);
        return;
    }
    if let Some(dir) = ready.app.parent() {
        let _ = fs::remove_dir_all(dir);
    }
    if reopen {
        // Once this process is gone, so the new Kil finds the session server free of it.
        crate::open_after_exit(&ready.bundle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_should_compare_numbers_not_text() {
        assert_eq!(
            [
                is_newer("0.0.10", "0.0.9"),
                is_newer("0.1.0", "0.0.99"),
                is_newer("0.0.5", "0.0.5"),
                is_newer("0.0.4", "0.0.5"),
                is_newer("0.0.x", "0.0.5"),
                is_newer("0.0.6.1", "0.0.5"),
            ],
            [true, true, false, false, false, false]
        );
    }

    #[test]
    fn release_notes_should_take_the_versions_between_current_and_candidate() {
        let changelog = "# Changelog\n\n## Unreleased\n\n- **Not yet.**\n\n## 0.0.27 — 2026-12-01\n\n- Too new.\n\n## 0.0.26 — 2026-11-20\n\n- **Bold thing** with `code` and [a link](https://example.com)\n  wrapped onto a second line.\n- Second.\n\n## 0.0.25 — 2026-11-08\n\n- Middle.\n\n## 0.0.24 — 2026-11-07\n\n- Already have it.\n";
        assert_eq!(
            release_notes(changelog, "0.0.24", "0.0.26"),
            "Kil 0.0.26\n• Bold thing with code and a link wrapped onto a second line.\n• Second.\n\nKil 0.0.25\n• Middle."
        );
        assert_eq!(
            plain("Press `[x]`, then [go](https://example.com)"),
            "Press [x], then go"
        );
        assert_eq!(
            (
                release_notes(changelog, "0.0.26", "0.0.26"),
                release_notes(changelog, "x", "0.0.26"),
                release_notes("no headings", "0.0.24", "0.0.26")
            ),
            (String::new(), String::new(), String::new())
        );
    }

    #[test]
    fn the_committed_changelog_should_have_notes_for_the_committed_release() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifest =
            Manifest::parse(&fs::read_to_string(root.join("releases/latest.toml")).unwrap())
                .unwrap();
        let changelog = fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
        let notes = release_notes(&changelog, "0.0.0", &manifest.version);
        assert!(notes.starts_with(&format!("Kil {}", manifest.version)));
        assert!(!notes.contains("**") && !notes.contains("##"));
    }

    #[gpui::test]
    fn a_background_check_should_ask_once_it_is_ready(cx: &mut gpui::TestAppContext) {
        use gpui::AnyWindowHandle;

        use crate::{theme::Theme, update_window::UpdateWindow};

        let count = |cx: &mut gpui::TestAppContext| {
            cx.windows()
                .iter()
                .filter_map(AnyWindowHandle::downcast::<UpdateWindow>)
                .count()
        };
        cx.update(|cx| {
            cx.set_global(Theme::default());
            cx.set_global(Updater::default());
            cx.bind_keys(update_window::bindings());
        });
        // Nothing to install: a background check opens nothing.
        cx.update(|cx| checked(Ok(Found::UpToDate), cx));
        cx.run_until_parked();
        assert_eq!(count(cx), 0);

        // Ready: the window asks on its own, notes and all.
        let ready = Ready {
            version: "9.9.9".into(),
            notes: "Kil 9.9.9\n• A new thing.".into(),
            app: PathBuf::from("/tmp/Kil.app"),
            bundle: PathBuf::from("/Applications/Kil.app"),
        };
        cx.update(|cx| checked(Ok(Found::Ready(ready)), cx));
        cx.run_until_parked();
        assert_eq!(count(cx), 1);

        // Later, then the next periodic check: no second ask this run. Asking by hand does.
        let [window] = cx
            .windows()
            .iter()
            .filter_map(AnyWindowHandle::downcast::<UpdateWindow>)
            .collect::<Vec<_>>()[..]
        else {
            panic!("one update window");
        };
        window
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        cx.update(|cx| check(false, None, cx));
        cx.run_until_parked();
        assert_eq!(count(cx), 0);
        cx.update(|cx| check(true, None, cx));
        cx.run_until_parked();
        assert_eq!(count(cx), 1);
    }

    #[gpui::test]
    fn check_now_should_run_on_done_at_once_when_ready(cx: &mut gpui::TestAppContext) {
        use std::{cell::Cell, rc::Rc};

        use crate::theme::Theme;

        cx.update(|cx| {
            cx.set_global(Theme::default());
            cx.set_global(Updater {
                ready: Some(Ready {
                    version: "9.9.9".into(),
                    notes: String::new(),
                    app: PathBuf::from("/tmp/Kil.app"),
                    bundle: PathBuf::from("/Applications/Kil.app"),
                }),
                ..Updater::default()
            });
            cx.bind_keys(update_window::bindings());
        });
        let done = Rc::new(Cell::new(false));
        let flag = Rc::clone(&done);
        cx.update(|cx| check_now_with_callback(cx, move |_| flag.set(true)));
        cx.run_until_parked();
        assert!(done.get());
    }

    #[gpui::test]
    fn check_now_should_run_on_done_when_check_finishes(cx: &mut gpui::TestAppContext) {
        use std::{cell::Cell, rc::Rc};

        use crate::theme::Theme;

        cx.update(|cx| {
            cx.set_global(Theme::default());
            cx.set_global(Updater {
                checking: Some(Arc::new(Progress::default())),
                ..Updater::default()
            });
            cx.bind_keys(update_window::bindings());
        });
        let done = Rc::new(Cell::new(false));
        let flag = Rc::clone(&done);
        cx.update(|cx| check_now_with_callback(cx, move |_| flag.set(true)));
        cx.run_until_parked();
        assert!(!done.get(), "pending until the check resolves");
        cx.update(|cx| checked(Ok(Found::UpToDate), cx));
        cx.run_until_parked();
        assert!(done.get());
    }

    #[test]
    fn the_release_urls_should_come_from_the_raw_host() {
        // github.com/<repo>/raw/<branch>/<file> answers 504 on GitHub's edge, so every
        // file Kil fetches comes from raw.githubusercontent.com instead.
        for url in [MANIFEST_URL, DMG_URL, CHANGELOG_URL] {
            assert!(
                url.starts_with("https://raw.githubusercontent.com/"),
                "{url}"
            );
        }
    }

    #[test]
    fn a_failure_should_not_name_the_program_twice() {
        let status = Command::new("/usr/bin/false").status().unwrap();
        assert_eq!(
            failure(
                "/usr/bin/curl",
                status,
                b"curl: (56) The requested URL returned error: 504\n",
            ),
            "curl: (56) The requested URL returned error: 504"
        );
        assert_eq!(
            failure("/usr/bin/curl", status, b""),
            "curl: exit status: 1"
        );
        assert_eq!(
            failure("/usr/bin/codesign", status, b"bundle not signed\n"),
            "codesign: bundle not signed"
        );
    }

    #[test]
    fn curl_exits_should_say_what_happened_in_plain_language() {
        let status = |code: i32| {
            std::process::Command::new("/bin/sh")
                .args(["-c", &format!("exit {code}")])
                .status()
                .unwrap()
        };
        assert_eq!(
            failure("/usr/bin/curl", status(6), b""),
            "Couldn't reach the update server — check your connection (curl exit 6)"
        );
        assert_eq!(
            failure("/usr/bin/curl", status(7), b""),
            "Couldn't reach the update server — check your connection (curl exit 7)"
        );
        assert_eq!(
            failure("/usr/bin/curl", status(28), b""),
            "The update server is taking too long — try again later (curl exit 28)"
        );
        // Anything else keeps the old shape.
        assert_eq!(
            failure("/usr/bin/curl", status(1), b""),
            "curl: exit status: 1"
        );
    }

    #[test]
    fn manifest_should_parse_and_reject_unknown_keys() {
        assert_eq!(
            Manifest::parse("version = \"0.0.6\"\nsha256 = \"ab\"\n"),
            Ok(Manifest {
                version: "0.0.6".into(),
                sha256: "ab".into()
            })
        );
        assert!(Manifest::parse("version = \"0.0.6\"\nurl = \"x\"\n").is_err());
    }

    #[test]
    fn only_an_installed_bundle_should_be_replaceable() {
        let bundle = |exe: &str| replaceable_bundle(Path::new(exe)).ok();
        assert_eq!(
            [
                bundle("/Applications/Kil.app/Contents/MacOS/kil"),
                bundle("/Users/me/code/kil/target/release/kil"),
                bundle("/Volumes/Kil/Kil.app/Contents/MacOS/kil"),
                bundle("/private/var/folders/x/AppTranslocation/1/d/Kil.app/Contents/MacOS/kil"),
            ],
            [
                Some(PathBuf::from("/Applications/Kil.app")),
                None,
                None,
                None
            ]
        );
    }

    #[test]
    fn content_length_should_be_the_last_responses() {
        let redirected = "HTTP/2 302 \r\ncontent-length: 0\r\n\r\nHTTP/2 200 \r\nContent-Length: 4570610\r\n\r\n";
        assert_eq!(
            [
                content_length(redirected),
                content_length("HTTP/2 302 \r\ncontent-length: 0\r\n\r\n"),
                content_length("HTTP/2 302 \r\ncontent-length: 9\r\n\r\nHTTP/2 200 \r\n"),
                content_length("Content-Length: 94\r\nAccept-ranges: bytes\r\n"),
            ],
            [Some(4_570_610), None, None, Some(94)]
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kil-update-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_app(at: &Path, version: &str) {
        fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
        fs::write(at.join("Contents/MacOS/kil"), version).unwrap();
    }

    #[test]
    fn install_should_swap_the_bundle_and_leave_nothing_beside_it() {
        let dir = scratch("install");
        let (bundle, staged) = (dir.join("Apps/Kil.app"), dir.join("cache/Kil.app"));
        fake_app(&bundle, "old");
        fake_app(&staged, "new");
        install(&staged, &bundle).unwrap();
        let mut left: Vec<_> = fs::read_dir(dir.join("Apps"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(
            (
                fs::read_to_string(bundle.join("Contents/MacOS/kil")).unwrap(),
                left
            ),
            ("new".to_owned(), vec![std::ffi::OsString::from("Kil.app")])
        );
    }

    #[test]
    fn install_should_keep_the_old_app_when_the_new_one_is_missing() {
        let dir = scratch("missing");
        let bundle = dir.join("Kil.app");
        fake_app(&bundle, "old");
        assert!(install(&dir.join("nowhere/Kil.app"), &bundle).is_err());
        assert_eq!(
            fs::read_to_string(bundle.join("Contents/MacOS/kil")).unwrap(),
            "old"
        );
    }

    #[test]
    fn download_should_report_every_byte_of_a_known_size() {
        let dmg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../releases/Kil-arm64.dmg");
        let size = fs::metadata(&dmg).unwrap().len();
        let to = scratch("download").join("Kil.dmg");
        let progress = Progress::default();
        let url = format!("file://{}", fs::canonicalize(&dmg).unwrap().display());
        download(&url, &to, "9.9.9", &progress).unwrap();
        assert_eq!(
            (progress.step(), to.with_extension("headers").exists()),
            (
                Step::Downloading {
                    version: "9.9.9".into(),
                    got: size,
                    total: Some(size),
                },
                false
            )
        );
        let missing = download("file:///nowhere/Kil.dmg", &to, "9.9.9", &progress);
        assert!(missing.is_err_and(|err| err.starts_with("curl: ")));
    }

    /// The release on `main` end to end, from the repository instead of GitHub: the manifest
    /// matches the DMG, and the app in it passes every check an update makes.
    #[test]
    fn the_committed_release_should_pass_the_checks_an_update_makes() {
        let releases = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../releases");
        let manifest =
            Manifest::parse(&fs::read_to_string(releases.join("latest.toml")).unwrap()).unwrap();
        let dmg = fs::canonicalize(releases.join("Kil-arm64.dmg")).unwrap();
        let dir = scratch("stage");
        let url = format!("file://{}", dmg.display());
        let progress = Progress::default();
        let staged = stage(&manifest, &url, &dir, "43VLF3KTFZ", &progress).unwrap();
        assert!(staged.join("Contents/MacOS/kil").is_file());
        assert_eq!(
            progress.step(),
            Step::Verifying {
                version: manifest.version.clone()
            }
        );
        assert!(verify(&staged, &manifest.version, "AAAAAAAAAA").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[gpui::test]
    fn menu_item_should_say_restart_to_update_when_ready(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            cx.set_global(Updater {
                ready: Some(Ready {
                    version: "9.9.9".into(),
                    notes: String::new(),
                    app: PathBuf::from("/tmp/Kil.app"),
                    bundle: PathBuf::from("/Applications/Kil.app"),
                }),
                ..Updater::default()
            });
            assert!(matches!(
                menu_item(cx),
                MenuItem::Action { name, .. } if name == "Restart to Update"
            ));
        });
    }
}
