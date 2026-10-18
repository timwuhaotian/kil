//! kil's zsh integration: the command line a job was started from, which its arguments may no
//! longer show (DESIGN.md §4.9).

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::proc_info::ProcIdentity;

/// Startup file of the integration; see the file for what it does.
const ZSHENV: &str = include_str!("shell/zshenv");
/// The user's own `ZDOTDIR`, which [`ZSHENV`] puts back.
pub(crate) const USER_ZDOTDIR_VAR: &str = "KIL_ZDOTDIR";

/// Points zsh at the integration, which hands startup back to the user's own files. zsh
/// pointed at a directory without it would skip those files, so it starts plain unless the
/// file is in place.
pub(crate) fn integrate_zsh(command: &mut Command, extra: &[(OsString, OsString)]) {
    let Some(dir) = zsh_dir().filter(|dir| install(dir)) else {
        return;
    };
    let user_dir = extra
        .iter()
        .rev()
        .find(|(key, _)| key == "ZDOTDIR")
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var_os("ZDOTDIR"));
    if let Some(user_dir) = user_dir {
        command.env(USER_ZDOTDIR_VAR, user_dir);
    }
    command.env("ZDOTDIR", dir);
}

/// The line `shell` is running, as it wrote it to `jobs/<PID>`. A file older than the shell
/// was left by an earlier one with the same PID.
pub(crate) fn command_line(shell: ProcIdentity) -> Option<String> {
    let file = zsh_dir()?.join("jobs").join(shell.pid().get().to_string());
    let written = fs::metadata(&file).and_then(|meta| meta.modified()).ok()?;
    if written < shell.started() {
        return None;
    }
    let line = fs::read_to_string(file).ok()?;
    (!line.trim().is_empty()).then_some(line)
}

/// `~/Library/Caches/kil/zsh`.
fn zsh_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    home.is_absolute()
        .then(|| home.join("Library/Caches/kil/zsh"))
}

/// Writes [`ZSHENV`] as `dir/.zshenv` when missing or from another version, and makes
/// `dir/jobs`.
fn install(dir: &Path) -> bool {
    static STAGED: AtomicU64 = AtomicU64::new(0);
    let file = dir.join(".zshenv");
    let current = fs::read(&file).is_ok_and(|current| current == ZSHENV.as_bytes());
    // Staged and renamed, so a shell starting meanwhile reads a whole file.
    let staged = dir.join(format!(
        ".zshenv.{}.{}",
        std::process::id(),
        STAGED.fetch_add(1, Ordering::Relaxed)
    ));
    let installed = fs::create_dir_all(dir.join("jobs")).and_then(|()| {
        if current {
            return Ok(());
        }
        fs::write(&staged, ZSHENV).and_then(|()| fs::rename(&staged, &file))
    });
    if let Err(err) = installed {
        let _ = fs::remove_file(&staged);
        log::warn!("zsh starts without kil's integration: {err}");
        return false;
    }
    true
}
