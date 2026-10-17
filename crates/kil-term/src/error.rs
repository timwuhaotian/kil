use std::{io, path::PathBuf};

/// Why a session could not be started.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// The kernel refused to allocate or configure a pseudo-terminal.
    #[error("failed to open a pseudo-terminal")]
    OpenPty(#[source] io::Error),
    /// The program to run is not an executable file.
    #[error("not an executable file: {}", .0.display())]
    NotExecutable(PathBuf),
    /// `fork`/`exec` failed.
    #[error("failed to start {}", program.display())]
    Spawn {
        /// The program that failed to start.
        program: PathBuf,
        /// The underlying OS error.
        #[source]
        source: io::Error,
    },
    /// The PTY event loop could not be created.
    #[error("failed to start the PTY event loop")]
    EventLoop(#[source] io::Error),
}
