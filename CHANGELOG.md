# Changelog

Notable changes to Kil. The download is always the latest build
([`Kil-arm64.dmg`](https://github.com/timwuhaotian/kil/raw/main/releases/Kil-arm64.dmg)); the
versions below are how we track releases, and match `Cargo.toml` and the app's Info.plist.

## Unreleased

## 0.0.17 — 2026-10-25

- **Drag a file onto a pane.** Its path is typed where it lands, shell-escaped with a space
  after it, as Terminal.app does; several files come as a run. A program in bracketed-paste
  mode gets them as one paste, so killi 0.4.10 attaches a dropped image as `[Image #N]` and
  names any other file with `@`. Before, a drop did nothing.

## 0.0.16 — 2026-10-24

- **The New Tab button lines up with the tabs.** Its plus is drawn centered in the status
  column where the rows' ring and dot go, and "New Tab" starts where the tab titles do, so
  the button reads as the row that would come next. Hovering it shows the keys that do the
  same (`⌘T`, or whatever `keymap.toml` binds).

## 0.0.15 — 2026-10-24

- **A `/kil-config` skill for coding agents working in the repo.** Asked where Kil's config
  lives, what a key accepts or how to rebind a key, an agent used to answer from memory. The
  skill reads `settings.rs`, `keymap.rs` and the user's own `config.toml`, `keymap.toml` and
  Application Support files, and reports paths, every key with its default, range and current
  value, the keymap, how changes apply, and the repo version against the installed app. No
  change to the app itself.

## 0.0.14 — 2026-10-22

- **Clicking a notification goes to the tab and pane that sent it.** It used to open Script
  Editor: the notification was posted through `osascript`, so macOS counted it as Script
  Editor's. Kil now posts its own, so it shows under Kil, and a click brings Kil forward on
  the pane that sent that notification, not the latest one. The first notification asks macOS
  for permission to show them.

## 0.0.13 — 2026-10-22

- **Cmd-V with an image on the clipboard reaches coding agents.** A program in
  bracketed-paste mode gets an empty paste, so killi attaches the image; before, Kil sent
  nothing and only Ctrl-V worked.

## 0.0.12 — 2026-10-22

- **Restart to Update works from the Kil menu.** Choosing it with the terminal window in
  front did nothing; it now quits, installs the update and reopens Kil, as the update
  window's Restart Now button already did.
- Quitting no longer installs a staged update over a newer Kil that was put in Applications
  by hand since the download.

## 0.0.11 — 2026-10-22

- **Notifications from programs.** A program that posts one (OSC 9 or OSC 777, as killi,
  Claude Code and others do when a long job ends) shows a macOS notification when its tab
  isn't in view, and the tab gets the finished dot. Clicking the notification brings Kil
  forward and goes to that pane. A program in the tab you're looking at stays quiet.

## 0.0.10 — 2026-10-21

- **Dragging a tab shows it moving.** The row lifts off the sidebar and follows the pointer
  up and down, an open slot shows where it will land, the rows it passes slide out of its
  way, and when you let go it settles into place.
- **Split buttons** at the top right of the tab split the focused pane right or down, the
  same as `⌘⇧D` and `⌘D`.
- Tab titles are semibold, so they stand out from the folder line under them.
- A new tab is named for its folder at once, instead of “shell” until its shell started.
- A split made right after another, before the pane it split had its shell, opened in your
  home folder; it now opens where that pane is.
- A pane split off below had its first prompt at the bottom of the pane instead of the top.
  It grew in from nothing, and its shell started one row high: growing pulled the blank
  screen back from scrollback above the prompt. A new pane now takes its size once the
  split has settled.

## 0.0.9 — 2026-10-21

- **Check for Updates shows what it's doing.** A small window opens at once and follows the
  check: a sweeping bar while it asks for the latest version, a bar that fills with the
  download (“1.8 MB of 4.6 MB”), then the signature checks. When the new Kil is ready it
  offers **Restart Now** or **Later**; later, it installs when you quit, so the next launch is
  the new version. **Hide** closes the window and it comes back with the outcome. **Check Now**
  in Settings opens the same window.

## 0.0.8 — 2026-10-20

- **An idle Kil uses no CPU.** A window with nothing to draw stops asking macOS for frames: it
  used to wake 60–120 times a second, 0.4% CPU on an M4 Pro, and now measures 0.0%. A blinking
  cursor redraws only itself instead of the whole pane (1.2% to 0.7%), output in one pane no
  longer lays the sidebar out again, and the memory figures redraw the sidebar only when one it
  shows changes.
- **Less writing to disk.** The session is saved when it changes, not at every redraw: a
  blinking cursor used to rewrite `session.toml` every 2 s. Screens are saved at most every
  30 s while output continues, and only when they changed; the session server keeps them
  through a crash of Kil, and quitting still saves everything at once. Saving a long colored
  scrollback holds its pane half as long.
- The session server sleeps until a session or a client comes or goes, instead of waking
  every second.

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
