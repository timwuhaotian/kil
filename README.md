# Kil

A native macOS terminal written in Rust. It has vertical tabs, animated split panes and
shortcuts you can rebind. Your jobs keep running after Kil quits or crashes: reopen it and
every pane is back on its live process, with vim's unsaved buffer, the ssh connection and the
half-finished build where you left them.

Kil is built on [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) (Metal
rendering) and [`alacritty_terminal`](https://github.com/alacritty/alacritty) (VT parsing). It
owns every process, PID and file descriptor it creates.

> **Status: 0.0.2, early.** macOS only for now: Apple silicon, macOS 14 or later. It is tested
> on macOS 27.

## Features

- **Vertical tabs** in a sidebar. A ring marks a tab with a running job, and a dot marks one
  whose job finished while you weren't looking. Double-click a tab to rename it (IME works,
  Chinese included).
- **Split panes** that animate open and closed. They share the tab evenly whenever a pane
  comes or goes. Drag the dividers; the focused pane is the bright one.
- **Jobs outlive the window.** A small session server (`kil --server`, the same binary) holds
  every pane's terminal. `⌘Q` and closing the window only detach from it, and so does a crash.
  The next launch goes back on the same processes, with the full screen, scrollback and input
  modes. **Kil ▸ Quit and Stop All Jobs** ends everything.
- **Comes back after a restart, too.** The layout, each pane's directory, its screen and
  scrollback, and the window frame are saved every 2 s. After a logout or a reboot, the
  processes themselves are gone, so each pane's last job is typed into a new shell instead.
- **Five themes**: Night, Paper (light), Fjord, Ember and Dusk. Programs that ask for the
  terminal's colors, like vim deciding between light and dark, are told the theme's.
- **A default directory** for new tabs, if you want one. Splits still open where their pane
  is.
- **Rebindable keys** in `~/.config/kil/keymap.toml` (**Kil ▸ Keyboard Shortcuts…** opens it),
  with the menus showing whatever is bound. Settings live in a Settings window (`⌘,`) or
  `~/.config/kil/config.toml`.
- **Strict about processes.** Children get clean descriptors and signal masks. A closed pane is
  hung up, then killed after 3 s if it ignores the hangup. No zombies, no leaked jobs. It is
  covered by integration tests that run real processes.

## Install

Download [`releases/Kil-arm64.dmg`](releases/Kil-arm64.dmg), always the latest Kil, open it, and drag
**Kil** to **Applications**. The app is signed with a Developer ID and notarized by Apple.
`releases/` also holds its `.sha256`:

```sh
shasum -a 256 -c Kil-arm64.dmg.sha256
```

## Build from source

You need:
- A Mac with Apple silicon.
- **Xcode** with its Metal toolchain: GPUI compiles its shaders at build time.
- **Rust 1.94** or later (`rustup` is easiest).

```sh
git clone https://github.com/timwuhaotian/kil.git
cd kil
cargo run -p kil --release           # run it straight from the build
```

To get a `Kil.app` with its icon and Info.plist, and install it:

```sh
scripts/bundle.sh release            # → target/release/Kil.app (ad-hoc signed)
ditto target/release/Kil.app /Applications/Kil.app
```

`bundle.sh debug` builds a debug bundle instead. A locally built app is ad-hoc signed, which is
fine on the machine that built it.

### Making a release (maintainers)

Releases are built locally and committed under `releases/`; there is no CI. With a
Developer ID identity in the login keychain:

```sh
KIL_NOTARY_KEY=~/.appstoreconnect/private_keys/AuthKey_XXXX.p8 \
KIL_NOTARY_KEY_ID=XXXX KIL_NOTARY_ISSUER=<issuer-id> \
  scripts/release.sh                 # → releases/Kil-arm64.dmg + .sha256
```

The script signs with the hardened runtime, notarizes and staples the app, then the DMG. It
fails unless Apple accepts both. Instead of the API key variables, it can use a
`notarytool store-credentials` profile (`KIL_NOTARY_PROFILE`), or the key stored by the
[`asc`](https://asccli.sh) CLI when that is installed. The
version comes from `Cargo.toml`.

## Using Kil

| Keys | Action |
|---|---|
| `⌘T` | New tab, in the default directory, else the focused pane's |
| `⌘⇧D` / `⌘D` | Split right / split down |
| `⌘⇧C` / `⌘⇧W` | Close pane / close tab |
| `⌘⌥←→↑↓` | Focus the pane in that direction |
| `⌘1`…`⌘9`, `⌘⇧[` / `⌘⇧]` | Select a tab, previous / next tab |
| `⌘C` / `⌘V` | Copy / paste (bracketed paste when the program asks for it) |
| `⌘K` | Clear scrollback |
| `⌘=` / `⌘-` / `⌘0` | Font bigger / smaller / reset |
| `⌘,` | Settings |
| `⌘Q` | Quit; running jobs keep going |

Double-click a tab to rename it (Enter or clicking away saves, Esc cancels), right-click it
for Rename / Close, and double-click empty sidebar space for a new tab.

**Rebinding is in the app.** **Settings ▸ Keyboard** lists every command and the keys it runs on.
Click a shortcut, press the new one, and it is saved and applied at once — the old keys stop
working, a key another command already has is refused by name, and **Reset** brings the default
back.

**`~/.config/kil/keymap.toml`** overrides any default, and is what the recorder writes. **Kil ▸
Keyboard Shortcuts…** opens it, creating a commented template the first time. Unknown keys or
commands show up in a banner, and the valid entries still apply. It is reloaded when the window
regains focus, and the menu bar then shows the new keys.

```toml
[bindings]
"cmd-y" = "split_right"
"cmd-k" = "unbind"
```

**`~/.config/kil/config.toml`** (`$XDG_CONFIG_HOME` is respected):

```toml
theme = "night"                  # night, paper, fjord, ember or dusk
font_family = "Menlo"
font_size = 13.0
default_directory = "~/code"     # where new tabs open; leave out to follow the focused pane
scrollback_lines = 10000
option_as_meta = true
cursor_blink = true
```

**`~/Library/Application Support/kil/`** holds what Kil reopens: `session.toml` (layout),
`panes/` (saved screens) and the session server's socket, lock and log. To start the next
launch fresh, quit with **Quit and Stop All Jobs** and delete `session.toml`.

## How it works

```
crates/
├─ kil-term/   no UI: PTY spawn, process lifecycle, reaper, the pane I/O loop, terminal
│              snapshots, the session server and its client. Unsafe code is Darwin FFI in
│              src/sys/darwin.rs plus a few kqueue registrations, each with a SAFETY comment.
└─ kil/        the GPUI app: window, sidebar, split tree, rendering, input and IME, keymap,
               settings, saving and restoring the session.
```

- Each pane's program runs in the **session server**, which owns its PTY and child. The app
  keeps a mirror of each terminal, fed over a Unix socket. On attach the server sends the
  whole terminal: scrollback, screen, the alternate screen, cursor and modes. Throughput and
  keypress echo through the server measure the same as a local session.
- If the server dies, its jobs are hung up, as on a crash. Nothing it started outlives it.
- [`DESIGN.md`](DESIGN.md) has the full design: measured findings about macOS PTYs, the process
  and teardown invariants, the memory and rendering budgets, the session server protocol, and
  the tests behind each claim.

## Development

```sh
cargo test --workspace                                       # unit, GPUI and real-process tests
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo +nightly fmt                                           # rustfmt.toml uses nightly options
```

The process tests in `crates/kil-term/tests/` spawn real shells and check fds, signals,
reaping, crashes and leaks. They should leave no process behind; if one does, that's a bug.

Contributions are welcome. Please keep `cargo clippy -D warnings` and the tests green. Explain
*why* in comments, not *what*. Don't copy code from Zed's `terminal` crates: they are GPL.

## License

[MIT](LICENSE). GPUI and `alacritty_terminal` are Apache-2.0.
