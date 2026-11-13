//! The `kil` command line: help, version, the internal subcommands, and nothing
//! else. Unknown arguments fail instead of opening the window, so a typo in a
//! script is an error (exit 2) rather than a stray window.

use std::{ffi::OsString, path::PathBuf};

/// Short usage, printed with every usage error.
pub const USAGE: &str = "usage: kil [--help] [--version]\n       kil --server <dir>\n       kil --remote [--host <ip>] [--port <n>] [--public]";

/// Full help, printed for `--help`.
pub const HELP: &str = "Kil: a native macOS terminal.\n\nusage:\n  kil                         open the window\n  kil --server <dir>          run the session server (internal: owns every pane's PTY)\n  kil --remote [--host <ip>] [--port <n>] [--public]\n                              expose the session server to Agent on Edge\n  kil --help, -h              show this help\n  kil --version, -V           show the version\n\nAny other argument is rejected (exit 2) instead of opening the window.";

/// What the process should do.
#[derive(Debug)]
pub enum Command {
    /// Open the window.
    Gui,
    /// Print [`HELP`].
    Help,
    /// Print the version.
    Version,
    /// Run the session server instead of the app (DESIGN.md §14).
    Server {
        /// The directory holding its socket, lock and log.
        dir: PathBuf,
    },
    /// Expose the session server to Agent on Edge.
    Remote {
        /// Override the QR address.
        host: Option<String>,
        /// Port to listen on; 0 picks a free one.
        port: u16,
        /// Set up a `UPnP` port mapping and print a QR for the public IP.
        public: bool,
    },
}

/// Parses `args` (without the executable) into what the process should do.
///
/// # Errors
///
/// If the arguments are unknown, incomplete or contradictory; the message says
/// what was wrong, and the caller prints [`USAGE`] with it.
pub fn parse(args: &[OsString]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Ok(Command::Gui);
    };
    // A legacy Finder launch argument, not user input: still just opens the window.
    if first.to_string_lossy().starts_with("-psn") {
        return Ok(Command::Gui);
    }
    let Some(flag) = first.to_str() else {
        return Err(format!("unknown argument: {}", first.to_string_lossy()));
    };
    match flag {
        "--help" | "-h" => {
            if args.len() == 1 {
                Ok(Command::Help)
            } else {
                Err("kil --help takes no arguments".to_owned())
            }
        }
        "--version" | "-V" => {
            if args.len() == 1 {
                Ok(Command::Version)
            } else {
                Err("kil --version takes no arguments".to_owned())
            }
        }
        "--server" => parse_server(&args[1..]),
        "--remote" => parse_remote(&args[1..]),
        _ => Err(unknown(flag)),
    }
}

fn unknown(flag: &str) -> String {
    if flag.starts_with('-') {
        format!("unknown flag: {flag}")
    } else {
        format!("unknown argument: {flag}")
    }
}

fn parse_server(rest: &[OsString]) -> Result<Command, String> {
    match rest {
        [] => Err("kil --server needs a directory".to_owned()),
        [only]
            if only
                .to_str()
                .is_some_and(|flag| flag == "--help" || flag == "-h") =>
        {
            Ok(Command::Help)
        }
        [dir] => Ok(Command::Server {
            dir: PathBuf::from(dir.as_os_str()),
        }),
        _ => Err("kil --server takes exactly one directory".to_owned()),
    }
}

fn parse_remote(rest: &[OsString]) -> Result<Command, String> {
    let mut host = None;
    let mut port = 0;
    let mut public = false;
    let mut index = 0;
    while let Some(arg) = rest.get(index) {
        let Some(flag) = arg.to_str() else {
            return Err(format!("kil --remote: bad value {}", arg.to_string_lossy()));
        };
        match flag {
            "--help" | "-h" => {
                if rest.len() == 1 {
                    return Ok(Command::Help);
                }
                return Err("kil --remote --help takes no other flags".to_owned());
            }
            "--host" => {
                let raw = rest
                    .get(index + 1)
                    .ok_or_else(|| "kil --remote --host needs a value".to_owned())?;
                let Some(value) = raw.to_str() else {
                    return Err(format!("kil --remote: bad value {}", raw.to_string_lossy()));
                };
                host = Some(value.to_owned());
                index += 2;
            }
            "--port" => {
                let raw = rest
                    .get(index + 1)
                    .ok_or_else(|| "kil --remote --port needs a value".to_owned())?;
                let Some(value) = raw.to_str() else {
                    return Err(format!("kil --remote: bad value {}", raw.to_string_lossy()));
                };
                port = value
                    .parse::<u16>()
                    .map_err(|_| format!("kil --remote: bad port {value}"))?;
                index += 2;
            }
            "--public" => {
                public = true;
                index += 1;
            }
            _ => return Err(remote_unknown(flag)),
        }
    }
    Ok(Command::Remote { host, port, public })
}

fn remote_unknown(flag: &str) -> String {
    if flag.starts_with('-') {
        format!("kil --remote: unknown flag {flag}")
    } else {
        format!("kil --remote: unexpected argument {flag}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(flags: &[&str]) -> Vec<OsString> {
        flags.iter().map(OsString::from).collect()
    }

    #[test]
    fn parse_should_open_the_window_when_given_no_arguments() {
        assert!(matches!(parse(&[]), Ok(Command::Gui)));
    }

    #[test]
    fn parse_should_open_the_window_for_a_legacy_finder_launch_argument() {
        assert!(matches!(parse(&args(&["-psn_0_12345"])), Ok(Command::Gui)));
    }

    #[test]
    fn parse_should_show_help_for_the_help_flags() {
        for flag in ["--help", "-h"] {
            assert!(matches!(parse(&args(&[flag])), Ok(Command::Help)));
        }
    }

    #[test]
    fn parse_should_reject_extra_arguments_after_help() {
        assert!(parse(&args(&["--help", "--version"])).is_err());
    }

    #[test]
    fn parse_should_show_the_version_for_the_version_flags() {
        for flag in ["--version", "-V"] {
            assert!(matches!(parse(&args(&[flag])), Ok(Command::Version)));
        }
        assert!(parse(&args(&["--version", "extra"])).is_err());
    }

    #[test]
    fn parse_should_reject_an_unknown_first_argument() {
        let err = parse(&args(&["--bogus"])).unwrap_err();
        assert!(err.contains("unknown flag"), "{err}");
        let err = parse(&args(&["."])).unwrap_err();
        assert!(err.contains("unknown argument"), "{err}");
    }

    #[test]
    fn parse_should_run_the_server_for_a_directory() {
        let command = parse(&args(&["--server", "/tmp/kil"])).unwrap();
        assert!(matches!(command, Command::Server { dir } if dir.as_os_str() == "/tmp/kil"));
    }

    #[test]
    fn parse_should_reject_a_server_invocation_without_exactly_one_directory() {
        assert!(parse(&args(&["--server"])).is_err());
        assert!(parse(&args(&["--server", "a", "b"])).is_err());
    }

    #[test]
    fn parse_should_show_help_for_a_server_help_flag() {
        assert!(matches!(
            parse(&args(&["--server", "--help"])),
            Ok(Command::Help)
        ));
    }

    #[test]
    fn parse_should_run_the_bridge_with_defaults_when_given_no_bridge_flags() {
        let command = parse(&args(&["--remote"])).unwrap();
        assert!(matches!(command, Command::Remote { host, port, public }
                if host.is_none() && port == 0 && !public));
    }

    #[test]
    fn parse_should_pass_the_bridge_flags_through() {
        let command = parse(&args(&[
            "--remote", "--host", "10.0.0.2", "--port", "8080", "--public",
        ]))
        .unwrap();
        assert!(matches!(command, Command::Remote { host, port, public }
                if host.as_deref() == Some("10.0.0.2") && port == 8080 && public));
    }

    #[test]
    fn parse_should_reject_a_bad_bridge_invocation() {
        for rest in [
            vec!["--bogus"],
            vec!["--host"],
            vec!["--port"],
            vec!["--port", "eighty"],
            vec!["--public", "extra-positional"],
            vec!["--help", "--public"],
        ] {
            let mut input = vec!["--remote"];
            input.extend(rest);
            assert!(parse(&args(&input)).is_err(), "{input:?}");
        }
    }

    #[test]
    fn parse_should_show_help_for_a_lone_bridge_help_flag() {
        assert!(matches!(
            parse(&args(&["--remote", "--help"])),
            Ok(Command::Help)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn parse_should_keep_a_non_utf8_server_directory_but_reject_a_non_utf8_flag() {
        use std::os::unix::ffi::OsStringExt;
        let raw = OsString::from_vec(vec![0xff]);
        let mut with_raw_dir = vec![OsString::from("--server")];
        with_raw_dir.push(raw.clone());
        assert!(matches!(parse(&with_raw_dir), Ok(Command::Server { .. })));
        assert!(parse(std::slice::from_ref(&raw)).is_err());
    }
}
