//! The standalone bridge binary: `kil-remote [--dir <dir>] [--port <port>]
//! [--host <ip>] [--launcher <exe>]`. Prefer `kil --remote`, which also knows
//! how to start the session server.

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};

fn default_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join("Library/Application Support/kil"),
        None => std::env::temp_dir().join("kil"),
    }
}

fn main() -> Result<()> {
    env_logger::init();

    let mut dir = default_dir();
    let mut port = 0;
    let mut host = None;
    let mut launcher = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = PathBuf::from(args.next().context("--dir needs a value")?),
            "--port" => port = args.next().context("--port needs a value")?.parse()?,
            "--host" => host = Some(args.next().context("--host needs a value")?),
            "--launcher" => {
                launcher = Some(PathBuf::from(
                    args.next().context("--launcher needs a value")?,
                ));
            }
            "--help" | "-h" => {
                println!(
                    "usage: kil-remote [--dir <dir>] [--port <port>] [--host <ip>] [--launcher <exe>]"
                );
                return Ok(());
            }
            other => bail!("unknown argument: {other}"),
        }
    }

    kil_remote::run(&kil_remote::Config {
        dir,
        port,
        host,
        ttl: Duration::from_secs(300),
        launcher,
    })?;
    Ok(())
}
