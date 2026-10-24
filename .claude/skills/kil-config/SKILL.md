---
name: kil-config
description: Everything about Kil's configuration, read fresh from the source and from this Mac. Use when asked where Kil's config lives, which settings, themes or key commands exist, what the current values are, how to change a setting or rebind a key, or why a change did not apply. Never answer a Kil config question from memory.
---

# Kil config

Kil has no hand-kept schema document. The truth is the code in this repo plus the files on
the user's Mac. This skill reads both and reports; it never recites a key list from memory,
because keys, defaults, ranges and commands change between versions.

## 1. Read the source of truth

From the repo root:

```sh
# config.toml: every key, its default and its validation
sed -n '/^pub struct Settings/,/^}/p;/^impl Default for Settings/,/^}/p;/pub fn parse/,/^    }/p' crates/kil/src/settings.rs
# where config.toml and keymap.toml live ($XDG_CONFIG_HOME/kil, else ~/.config/kil)
sed -n '/pub fn config_dir/,/^}/p' crates/kil/src/settings.rs
# theme names accepted by `theme`
sed -n '/^pub enum ThemeName/,/^}/p' crates/kil/src/theme.rs
# keymap.toml: command names, default keys, and the template Kil writes
sed -n '/^const NAMED/,/^];/p;/^const DEFAULTS/,/^];/p;/^pub const TEMPLATE/,/^"#;/p' crates/kil/src/keymap.rs
# how the files are written and reloaded
sed -n '/pub fn apply/,/^}/p' crates/kil/src/config.rs
grep -n 'In place\|modified\|write_setting_at\|unset_setting' crates/kil/src/config.rs
# state, not config: what Kil reopens and the session server's files
grep -n 'Application Support' crates/kil/src/restore.rs
grep -n 'server\.sock\|server\.lock\|server\.log' crates/kil-term/src/server.rs
# the version this source describes vs the app the user runs
grep -n '^version' Cargo.toml
defaults read /Applications/Kil.app/Contents/Info.plist CFBundleShortVersionString 2>/dev/null
```

Then read `DESIGN.md` §8 (Keymap) and §9 (Config) and the README's config section for the
intended behavior. Where prose and code disagree, the code wins; say so in the report.

## 2. Read the user's machine

```sh
dir="${XDG_CONFIG_HOME:-$HOME/.config}/kil"
ls -la "$dir" 2>/dev/null
cat "$dir/config.toml" 2>/dev/null; echo "---"; cat "$dir/keymap.toml" 2>/dev/null
ls -la "$HOME/Library/Application Support/kil" 2>/dev/null
```

`XDG_CONFIG_HOME` counts only when it is absolute. A Kil.app started from the Dock or Finder
sees launchd's environment, not the shell's exports, so it may use `~/.config/kil` while the
shell reports a different directory. Mention this when they differ.

## 3. Report

Answer in the user's language. Cover, in this order:

1. **Paths.** `config.toml`, `keymap.toml`, and the state directory with `session.toml`,
   `panes/`, `server.sock`, `server.lock`, `server.log`. For each: exists or not, mtime.
2. **Every config key.** Name, default (from `impl Default`), accepted values or range (from
   `parse` and `ThemeName`), and the user's current value, or "default" when the key is
   absent. Unknown keys fail the whole file (`deny_unknown_fields`).
3. **Keymap.** Every command, its default keys, and the user's overrides and `unbind`s.
   Keys the file cannot change: the text field, Settings and Update window bindings.
4. **How changes apply.** Both files are re-read when the window regains focus and the
   file's mtime changed. The Settings window writes one key at a time, in place, keeping
   comments and symlinks. An invalid `config.toml` keeps the settings already in effect and
   shows a banner; invalid keymap entries are skipped one by one. Scrollback applies to new
   tabs and panes only.
5. **Versions.** The repo's version and the installed app's. If they differ, the installed
   app may lack keys the source has.

## 4. Editing on the user's behalf

Only when asked. Edit the file in place (no write-and-rename), keep its comments, use only
keys and values the source accepts, then tell the user to click back into Kil's window so it
reloads. Prefer `cmd-,` (Settings) for anything it offers; the file is for the rest, such as
multi-stroke keys.
