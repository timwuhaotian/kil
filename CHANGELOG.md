# Changelog

Notable changes to Kil. The download is always the latest build
([`Kil-arm64.dmg`](https://github.com/timwuhaotian/kil/raw/main/releases/Kil-arm64.dmg)); the
versions below are how we track releases, and match `Cargo.toml` and the app's Info.plist.

## Unreleased

## 0.0.7 — 2026-10-20

- The sidebar says what each tab's panes cost: the memory of the program running in it and
  everything that program started, next to the tab's title, and the total for every tab at
  the bottom. Under memory pressure the bottom line also shows how much swap is in use. A
  program that grows to gigabytes — an agent with its own subprocesses — used to be invisible
  until macOS named Kil as the app at fault.

## 0.0.6 — 2026-10-19

- **Kil updates itself.** It checks at launch and every six hours, downloads a new release in
  the background, checks its checksum, Developer ID signature and notarization, and installs
  it when Kil quits; jobs keep running. **Kil ▸ Check for Updates…** checks now and offers
  **Restart Now**. Turn it off with **Settings ▸ Updates** or `auto_update = false`. This
  release has to be installed by hand once; later ones arrive on their own.
- Drag a tab up or down the sidebar to reorder it; the new order is saved with the session.
- Double-click the top of the sidebar to fill the screen, and again to go back (or whatever
  System Settings ▸ Desktop & Dock says a title-bar double-click does).
- **Settings ▸ Theme ▸ Sidebar opacity** (`sidebar_opacity` in `config.toml`, 0–1) sets how
  much of the blurred desktop shows through the sidebar; **Reset** goes back to the theme's.
- The wheel sends mouse wheel events to programs that ask for mouse reporting, as Claude
  Code's full-screen view does, instead of arrow keys that barely scrolled it.

## 0.0.5 — 2026-10-18

- `⇧↩` inserts a newline in Claude Code, kimi and other TUIs instead of sending the line: it
  sends ESC CR, like `⌥↩`.
- The README's download link fetches the DMG directly, and `scripts/release.sh` refuses to
  build a release unless the README links it.
- Open-source project files: contributing guide, code of conduct, security policy, issue and
  pull request templates, CI on Apple silicon, and Dependabot.

## 0.0.4 — 2026-10-18

- A pane reopened after a restart runs the command line its zsh ran, as typed (e.g.
  `kimi -r <session>`). Programs that set their title, like Node's `process.title`, overwrite
  their own arguments, which used to reopen as `kimi-code '' ''`.
- IME composition and the candidate window follow the cursor even when the program hides it
  to draw its own, as TUIs like `kimi` do. They used to fall to the screen's bottom-left.
- Every pane in a tab gets an equal share: `1 | 2 | 3` gets thirds however it was split.
  Double-clicking any divider shares the tab equally again.
- Releases are a single `releases/Kil-arm64.dmg`, replaced by each release.

## 0.0.3 — 2026-10-18

- A tab whose panes change shares its space evenly again.
- **Settings ▸ Keyboard** lists every command: click a shortcut and press a chord to rebind
  it; **Reset** restores the default.
- New default keys: `⌘⇧D` splits right, `⌘D` splits down, `⌘⇧C` closes the pane.
- Settings rows truncate instead of overflowing.

## 0.0.2 — 2026-10-18

- Five themes: Night, Paper (light), Fjord, Ember and Dusk. Programs asking for the
  terminal's colors (OSC 10/11/4) get the theme's.
- A default directory for new tabs.
- Double-click a tab to rename it.
- The menu bar shows the keys actually bound; **Kil ▸ Keyboard Shortcuts…** opens
  `keymap.toml`.
- Session server protocol 2, which still talks to 0.0.1's server so running jobs survive the
  upgrade.

## 0.0.1 — 2026-10-18

- First public build: vertical tabs, animated split panes, rebindable keys, jobs that outlive
  the window through the session server, and a restored layout after a restart.
- Signed with a Developer ID and notarized by Apple. MIT license.
