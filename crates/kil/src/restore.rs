//! The window's tabs, splits and running jobs: saved when kil quits, reopened at the next
//! launch (DESIGN.md §4.9).

use std::{ffi::OsString, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::pane_tree::Shape;

/// One pane: where its shell was, and the job it was running.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPane {
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedWorkspace {
    #[serde(default)]
    pub active: usize,
    #[serde(default)]
    pub tabs: Vec<SavedTab>,
}

/// Where the saved workspace waits between launches.
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// `~/Library/Application Support/kil/session.toml`.
    pub fn standard() -> Option<Self> {
        let home = std::env::var_os("HOME")?;
        Some(Self::at(
            PathBuf::from(home).join("Library/Application Support/kil/session.toml"),
        ))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    #[cfg(test)]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Reads and removes the saved workspace, so a crash later in this launch never replays
    /// it: only a quit saves one.
    ///
    /// # Errors
    /// If the file can't be read or doesn't parse; it is removed either way.
    pub fn take(&self) -> Result<Option<SavedWorkspace>, String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("session.toml: {err}")),
        };
        if let Err(err) = fs::remove_file(&self.path) {
            log::warn!("session.toml not removed: {err}");
        }
        toml::from_str(&text)
            .map(Some)
            .map_err(|err| format!("session.toml: {err}"))
    }

    /// Replaces the saved workspace.
    ///
    /// # Errors
    /// If the file can't be written.
    pub fn save(&self, saved: &SavedWorkspace) -> Result<(), String> {
        let text = toml::to_string(saved).map_err(|err| format!("session.toml: {err}"))?;
        // Written whole, then renamed: quitting must never leave half a file.
        let partial = self.path.with_extension("toml.partial");
        self.path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&partial, text))
            .and_then(|()| fs::rename(&partial, &self.path))
            .map_err(|err| format!("session.toml: {err}"))
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
        let pane = |cwd: &str, command: Option<&str>| {
            Shape::Leaf(SavedPane {
                cwd: Some(cwd.to_owned()),
                command: command.map(str::to_owned),
            })
        };
        let saved = SavedWorkspace {
            active: 1,
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
                        first: Box::new(pane("/tmp", Some("cargo watch -x 'test --lib'"))),
                        second: Box::new(Shape::Split {
                            axis: Axis::Vertical,
                            ratio: 0.5,
                            first: Box::new(pane("/", None)),
                            second: Box::new(pane("/usr", Some("vim"))),
                        }),
                    },
                },
            ],
        };
        let path = std::env::temp_dir()
            .join(format!("kil-restore-{}", std::process::id()))
            .join("session.toml");
        let store = Store::at(path.clone());
        store.save(&saved).unwrap();
        assert_eq!(store.take().unwrap(), Some(saved));
        assert!(!path.exists(), "taken once");
    }
}
