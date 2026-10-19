//! Kil updates itself (DESIGN.md §15). It reads `releases/latest.toml` on `main`; when that
//! names a newer version, it downloads the DMG beside it, checks the checksum, the version,
//! the Developer ID team and Apple's notarization, and keeps the app from it until Kil quits.
//! Quitting then replaces the running bundle, so the next launch is the new version, and the
//! jobs, which run in the session server, never notice.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

use gpui::{App, AppContext as _, Global, MenuItem, actions};
use serde::Deserialize;

use crate::{settings::Settings, workspace};

actions!(update, [CheckForUpdates, RestartToUpdate]);

/// Raw from `main`, where the release commit puts the DMG and this file together.
const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/timwuhaotian/kil/main/releases/latest.toml";
const DMG_URL: &str = "https://github.com/timwuhaotian/kil/raw/main/releases/Kil-arm64.dmg";
/// After launch, so the check never competes with reopening the session.
const FIRST_CHECK: Duration = Duration::from_secs(5);
/// A terminal stays open for days; one wakeup in hours keeps idle CPU at zero (A12).
const CHECK_EVERY: Duration = Duration::from_hours(6);

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
    let stderr = String::from_utf8_lossy(&output.stderr);
    let name = Path::new(program).file_name().unwrap_or_default().display();
    Err(match stderr.trim() {
        "" => format!("{name}: {}", output.status),
        why => format!("{name}: {why}"),
    })
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

/// Checks that `app` is Kil `version`, signed by `team` and notarized by Apple.
fn verify(app: &Path, version: &str, team: &str) -> Result<(), String> {
    let plist = app.join("Contents/Info.plist");
    let shown = run(
        "/usr/libexec/PlistBuddy",
        &[
            "-c".as_ref(),
            "Print :CFBundleShortVersionString".as_ref(),
            plist.as_os_str(),
        ],
    )?;
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

/// Reads the manifest at `url`.
///
/// # Errors
/// If it can't be fetched or parsed.
pub fn fetch_manifest(url: &str) -> Result<Manifest, String> {
    let text = run(
        "/usr/bin/curl",
        &[
            "-fsSL".as_ref(),
            "--max-time".as_ref(),
            "30".as_ref(),
            url.as_ref(),
        ],
    )?;
    Manifest::parse(&text)
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
) -> Result<PathBuf, String> {
    let app = dir.join("Kil.app");
    if app.exists() && verify(&app, &manifest.version, team).is_ok() {
        return Ok(app);
    }
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let dmg = dir.join("Kil.dmg");
    run(
        "/usr/bin/curl",
        &[
            "-fsSL".as_ref(),
            "--max-time".as_ref(),
            "900".as_ref(),
            "-o".as_ref(),
            dmg.as_os_str(),
            dmg_url.as_ref(),
        ],
    )?;
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
    verify(&app, &manifest.version, team).inspect_err(|_| {
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
    app: PathBuf,
    bundle: PathBuf,
}

enum Found {
    UpToDate,
    Ready(Ready),
}

/// One check, start to end; blocks, so it runs on a background thread.
fn find(current: &str) -> Result<Found, String> {
    let exe = std::env::current_exe().map_err(|err| err.to_string())?;
    let bundle = replaceable_bundle(&exe)?;
    let team = team_of(&bundle)
        .ok_or("this copy of Kil isn't signed with a Developer ID, so it doesn't update itself")?;
    let manifest = fetch_manifest(MANIFEST_URL)?;
    if !is_newer(&manifest.version, current) {
        return Ok(Found::UpToDate);
    }
    writable(bundle.parent().unwrap_or(&bundle))?;
    let app = stage(&manifest, DMG_URL, &cache_dir()?, &team)?;
    Ok(Found::Ready(Ready {
        version: manifest.version,
        app,
        bundle,
    }))
}

#[derive(Default)]
struct Updater {
    checking: bool,
    /// Someone chose Check for Updates, and is told the outcome.
    asked: bool,
    ready: Option<Ready>,
    /// Restart to Update: open the new app once this one has quit.
    reopen: bool,
}

impl Global for Updater {}

/// Starts the checks and installs a staged update whenever Kil quits.
pub fn init(cx: &mut App) {
    cx.set_global(Updater::default());
    cx.on_action(|_: &CheckForUpdates, cx| check(true, cx));
    cx.on_action(|_: &RestartToUpdate, cx| {
        if cx.global::<Updater>().ready.is_some() {
            cx.global_mut::<Updater>().reopen = true;
            workspace::detach_and_quit(cx);
        }
    });
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
                    check(false, cx);
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
    match cx
        .try_global::<Updater>()
        .and_then(|updater| updater.ready.as_ref())
    {
        Some(ready) => MenuItem::action(
            format!("Restart to Update to {}", ready.version),
            RestartToUpdate,
        ),
        None => MenuItem::action("Check for Updates…", CheckForUpdates),
    }
}

fn check(asked: bool, cx: &mut App) {
    let updater = cx.global_mut::<Updater>();
    if let Some(ready) = updater.ready.clone() {
        if asked {
            offer_restart(&ready.version, cx);
        }
        return;
    }
    updater.asked |= asked;
    if updater.checking {
        return;
    }
    updater.checking = true;
    let found = cx.background_spawn(async { find(env!("CARGO_PKG_VERSION")) });
    cx.spawn(async move |cx| {
        let found = found.await;
        let _ = cx.update(|cx| checked(found, cx));
    })
    .detach();
}

fn checked(found: Result<Found, String>, cx: &mut App) {
    let updater = cx.global_mut::<Updater>();
    updater.checking = false;
    let asked = std::mem::take(&mut updater.asked);
    match found {
        Ok(Found::Ready(ready)) => {
            log::info!("Kil {} is ready to install", ready.version);
            let version = ready.version.clone();
            updater.ready = Some(ready);
            cx.set_menus(crate::menus(cx));
            if asked {
                offer_restart(&version, cx);
            }
        }
        Ok(Found::UpToDate) if asked => workspace::tell(
            cx,
            "Kil is up to date.",
            concat!(env!("CARGO_PKG_VERSION"), " is the newest version."),
        ),
        Ok(Found::UpToDate) => {}
        Err(err) => {
            log::warn!("update check: {err}");
            if asked {
                workspace::tell(cx, "Kil can't update right now.", &err);
            }
        }
    }
}

fn offer_restart(version: &str, cx: &mut App) {
    workspace::ask(
        cx,
        &format!("Kil {version} is ready."),
        "It installs when Kil quits. Restart now? Running jobs keep going.",
        &["Restart Now", "Later"],
        |answer, cx| {
            if answer == 0 {
                cx.dispatch_action(&RestartToUpdate);
            }
        },
    );
}

fn install_on_quit(cx: &mut App) {
    let Some(updater) = cx.try_global::<Updater>() else {
        return;
    };
    let Some(ready) = updater.ready.clone() else {
        return;
    };
    let reopen = updater.reopen;
    if let Err(err) = install(&ready.app, &ready.bundle) {
        log::error!("installing Kil {}: {err}", ready.version);
        return;
    }
    if let Some(dir) = ready.app.parent() {
        let _ = fs::remove_dir_all(dir);
    }
    if reopen {
        // Once this process is gone, so the new Kil finds the session server free of it.
        let spawned = Command::new("/bin/sh")
            .args([
                "-c",
                r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open "$2""#,
                "sh",
            ])
            .arg(std::process::id().to_string())
            .arg(&ready.bundle)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Err(err) = spawned {
            log::error!("reopening Kil: {err}");
        }
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

    /// The release on `main` end to end, from the repository instead of GitHub: the manifest
    /// matches the DMG, and the app in it passes every check an update makes.
    #[test]
    fn the_committed_release_should_pass_the_checks_an_update_makes() {
        let releases = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../releases");
        let manifest =
            Manifest::parse(&fs::read_to_string(releases.join("latest.toml")).unwrap()).unwrap();
        let dmg = fs::canonicalize(releases.join("Kil-arm64.dmg")).unwrap();
        let dir = scratch("stage");
        let staged = stage(
            &manifest,
            &format!("file://{}", dmg.display()),
            &dir,
            "43VLF3KTFZ",
        )
        .unwrap();
        assert!(staged.join("Contents/MacOS/kil").is_file());
        assert!(verify(&staged, &manifest.version, "AAAAAAAAAA").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
