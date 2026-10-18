//! Starting a child on a fresh PTY with clean descriptors, signals and env (DESIGN.md §4.2).

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    os::{
        fd::{AsFd, RawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use crate::{
    child::OwnedChild,
    error::SpawnError,
    proc_info::TtyId,
    pty::winsize,
    session::{Program, SessionConfig},
    sys,
};

/// Extra descriptor numbers swept above the highest one open at spawn time, covering
/// descriptors other threads open between the listing and the fork.
const FD_SWEEP_SLACK: RawFd = 64;
const FD_SWEEP_FALLBACK: RawFd = 4096;

/// Variables other terminals and multiplexers set that would mislead programs in kil.
const FOREIGN_VARS: &[&str] = &[
    "TERM_SESSION_ID",
    "TMUX",
    "TMUX_PANE",
    "WINDOWID",
    "COLUMNS",
    "LINES",
    "__CFBundleIdentifier",
    "CW_TERM",
];
const FOREIGN_PREFIXES: &[&str] = &[
    "ITERM_",
    "VSCODE_",
    "GHOSTTY_",
    "ALACRITTY_",
    "WEZTERM_",
    "KITTY_",
    "OTTY_",
    "LC_TERMINAL",
];

pub(crate) struct Spawned {
    pub(crate) master: File,
    pub(crate) child: OwnedChild,
    pub(crate) tty: TtyId,
}

pub(crate) fn spawn(config: &SessionConfig) -> Result<Spawned, SpawnError> {
    let (master, slave) = sys::openpty(winsize(config.size.into())).map_err(SpawnError::OpenPty)?;
    if let Err(err) = sys::enable_iutf8(master.as_fd()) {
        log::warn!("IUTF8 not set: {err}");
    }
    let tty = TtyId(sys::tty_rdev(slave.as_fd()).map_err(SpawnError::OpenPty)?);

    let (program, mut command) = command_for(&config.program)?;
    command.current_dir(working_directory(config.cwd.as_deref()));
    apply_env(&mut command, &config.env);
    let stdin = slave.try_clone().map_err(SpawnError::OpenPty)?;
    let stdout = slave.try_clone().map_err(SpawnError::OpenPty)?;
    command
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(slave));
    sys::install_child_setup(&mut command, fd_sweep_limit(), true);

    let child = command.spawn().map_err(|source| SpawnError::Spawn {
        program: program.clone(),
        source,
    })?;
    // The parent's copies of the slave must close now, or the master never sees EOF/hangup
    // when the child exits.
    drop(command);
    let child = OwnedChild::new(child).map_err(|source| SpawnError::Spawn { program, source })?;
    Ok(Spawned {
        master: File::from(master),
        child,
        tty,
    })
}

fn command_for(program: &Program) -> Result<(PathBuf, Command), SpawnError> {
    match program {
        Program::LoginShell => {
            let shell = login_shell();
            let mut command = Command::new(&shell);
            // argv[0] starting with '-' makes it a login shell without /usr/bin/login,
            // whose root euid would hide our own child from proc_pidinfo (F1).
            let name = shell.file_name().unwrap_or(OsStr::new("sh"));
            let mut argv0 = OsString::from("-");
            argv0.push(name);
            command.arg0(argv0);
            Ok((shell, command))
        }
        Program::Command { program, args } => {
            if !is_executable(program) {
                return Err(SpawnError::NotExecutable(program.clone()));
            }
            let mut command = Command::new(program);
            command.args(args);
            Ok((program.clone(), command))
        }
    }
}

fn login_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|shell| shell.is_absolute() && is_executable(shell))
        .or_else(|| sys::passwd_shell().filter(|shell| is_executable(shell)))
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"))
}

fn is_executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// std fails the whole spawn on a bad `current_dir`, so fall back instead.
fn working_directory(requested: Option<&Path>) -> PathBuf {
    requested
        .filter(|dir| dir.is_dir())
        .map(Path::to_path_buf)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| home.is_dir())
        })
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn apply_env(command: &mut Command, extra: &[(OsString, OsString)]) {
    for (key, _) in std::env::vars_os() {
        let Some(name) = key.to_str() else { continue };
        if FOREIGN_VARS.contains(&name) || FOREIGN_PREFIXES.iter().any(|p| name.starts_with(p)) {
            command.env_remove(&key);
        }
    }
    command
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("TERM_PROGRAM", "kil")
        .env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
    // Apps launched from Finder get no LANG, and shells then mangle UTF-8.
    if std::env::var_os("LANG").is_none() {
        command.env("LANG", default_lang());
    }
    for (key, value) in extra {
        command.env(key, value);
    }
}

fn default_lang() -> String {
    let installed = |name: &str| {
        Path::new("/usr/share/locale")
            .join(format!("{name}.UTF-8"))
            .is_dir()
    };
    sys::current_locale_identifier()
        .into_iter()
        .flat_map(|id| locale_candidates(&id))
        .find(|candidate| installed(candidate))
        .map_or_else(|| "en_US.UTF-8".to_owned(), |name| format!("{name}.UTF-8"))
}

/// `zh-Hans_CN` → [`zh-Hans_CN`, `zh_CN`]: POSIX locale names carry no script subtag.
fn locale_candidates(id: &str) -> Vec<String> {
    let mut candidates = vec![id.to_owned()];
    if let Some((language, region)) = id.split_once('_') {
        let base = language.split('-').next().unwrap_or(language);
        let region = region.split('@').next().unwrap_or(region);
        candidates.push(format!("{base}_{region}"));
    }
    candidates
}

pub(crate) fn fd_sweep_limit() -> RawFd {
    let pid = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
    sys::open_fds(pid)
        .ok()
        .and_then(|fds| fds.into_iter().max())
        .map_or(FD_SWEEP_FALLBACK, |max| max.saturating_add(FD_SWEEP_SLACK))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_candidates_should_strip_script_subtag() {
        assert_eq!(locale_candidates("zh-Hans_CN"), ["zh-Hans_CN", "zh_CN"]);
    }

    #[test]
    fn locale_candidates_should_keep_plain_identifier_only() {
        assert_eq!(locale_candidates("en"), ["en"]);
    }

    #[test]
    fn working_directory_should_fall_back_when_requested_dir_is_missing() {
        assert_ne!(
            working_directory(Some(Path::new("/nonexistent/kil"))),
            PathBuf::from("/nonexistent/kil")
        );
    }
}
