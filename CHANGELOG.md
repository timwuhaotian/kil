# Changelog

Notable changes to Kil. The download is always the latest build
([`Kil-arm64.dmg`](https://raw.githubusercontent.com/timwuhaotian/kil/main/releases/Kil-arm64.dmg)); the
versions below are how we track releases, and match `Cargo.toml` and the app's Info.plist.

## 0.0.32 — 2026-11-09

- **See when your phone is on a pane.** A phone attached through Agent on Edge now
  marks the panes it watches: a small dot on the tab row and in the pane's corner.
  When the phone types, the mark turns red instead — it is driving, not just
  watching — and falls back to the dot 10 s after its last keystroke. The app polls
  the server every 2 s, detaching clears both marks, and nothing is ever locked: a
  marked pane stays fully usable. The frames are additive and the protocol stays 2,
  so old and new servers, bridges and apps mix.

## 0.0.31 — 2026-11-09

- **A session can have several clients at once.** Attaching a second client (Agent on
  Edge on the phone) used to evict the first (the Kil window), which then showed an
  exit banner for a session that was still running. Output is now fanned out to every
  attached client and input is taken from any; a client that stops reading or falls
  more than a megabyte behind is dropped on its own, and the program never waits for
  a terminal. No protocol change: old and new sides interoperate.

## 0.0.30 — 2026-11-09

- **Agent on Edge in the Settings window.** Settings ▸ Agent on Edge now shows the pairing
  QR code in the app — no terminal command needed. Enable the bridge, tick Public internet
  for a `UPnP` port mapping to the public IP, and the mapping is removed when you switch it
  off or close the window.

- **Agent on Edge over the internet, still no cloud.** `kil --remote --public` asks the
  router (UPnP) for a port mapping to the Mac's public IP and prints a QR that pairs an
  iPhone from anywhere. The bridge throttles failed pairing attempts, so the one-time code
  is safe on a public port, and the mapping is removed when the bridge stops.

## 0.0.29 — 2026-11-09

- **Themes now match killi.** The nine themes are killi's `/theme` skins — Batman, Iced
  Coffee, Creamery Station, Pastel Tweencore, Cappuccino, Shades of Gray, Off White, Red
  Orange and Snow and Ice — each painted from the skin's own palette. Night, Fjord, Ember and
  Dusk are gone: `theme = "night"` no longer parses; use one of the nine names above.
- **Remote panes for Agent on Edge.** `kil --remote` (or the standalone `kil-remote` binary)
  wraps the session server's Unix socket in a paired TLS tunnel: the Mac prints a QR code
  carrying the address, a one-time pairing code and the certificate's fingerprint, and after
  pairing the bridge only pumps bytes between the TLS stream and the socket — the server's
  wire protocol and its `peer_uid` trust check are untouched.

## 0.0.28 — 2026-11-09

- **The sidebar follows the active tab.** With more tabs than fit, `⌘1…9`, `⌘⇧[` / `]`
  and notification clicks scroll the selected row into view. Long folder paths shorten
  from the front, so the current folder stays visible instead of being cut off.
- **The exit banner moved to the top of the pane**, so a crashed program's last lines
  stay readable, and it says `exited 1` / `killed: SIGKILL` as documented. Clicking
  it closes the pane.
- **Selection drags past the pane edge**, with autoscroll, and shift-click extends the
  selection from its anchor.
- **Steady cursors stay steady.** A program that asks for one (vim) no longer blinks,
  and the blink itself fades over 100 ms.
- **Settings fixes.** Recording a shortcut no longer swallows typing in terminals,
  Escape cancels it, and bare letters are refused. Validation errors pin to the
  bottom instead of hiding below the fold, keymap file errors show here too, and
  **Check Now** says **Checking…** while it works. Toggles show their knob on every
  theme, and fields support shift-select, drag-select, word-select, copy and cut.
- **Rebinding fully moves a command.** Both defaults of next/previous tab unbind, all
  bound chords display, and the display matches the menus (`^⌥⇧⌘` order, `⌫ ⌦ ⎋`
  glyphs). The file tolerates `[bindings]` comments and single quotes, and keeps
  trailing comments.
- **Links.** A URL wrapped across rows opens whole, Chinese punctuation no longer
  glues onto URLs, and markdown links with parentheses open whole.
- **Menus.** Quit is last again, the Window menu has Minimize (`⌘M`) and Zoom, and
  the Kil menu has About Kil.
- **Notifications.** Dismissing one no longer jumps to its pane, and repeats from
  the same pane coalesce instead of stacking.
- **Update window.** A background check opens without stealing focus, failures no
  longer paint over the buttons, and offline failures say so plainly.
- **Contrast.** Muted text meets AA on every theme, menu hover and toggle knobs use
  dark-on-accent, and input borders show at rest.
- **F13–F20 work**, double-clicking **New Tab** opens one tab, notices no longer
  click through to the terminal, and a `Motion::scale` of 0 disables animations.

## 0.0.27 — 2026-11-08

- **Updates download again.** GitHub's `github.com/<repo>/raw/<branch>/<file>` link answers
  **504**, so the updater could not download a release (**Kil can't update right now**), and
  the README's download link was dead too. Every file Kil fetches now comes from
  `raw.githubusercontent.com`, which serves the same bytes, and a failure no longer says
  `curl: curl:`.

## 0.0.26 — 2026-11-08

- **A new release now asks.** The periodic check used to download a release silently and
  install it when Kil quit, so a Kil left open for days never said anything. Once a release
  is downloaded and checked, the update window opens on its own — **Update to Kil x.y.z?** —
  with the changelog entries for every version since the running one. **Restart Now**
  updates at once; **Later** keeps the old behavior and installs on quit, and the ask comes
  once per run, not at every check.

## 0.0.25 — 2026-11-08

- **A new app icon.** Kil's icon is now the terminal tile from killi's logo, taken
  whole: the light-blue rounded square with the `>_` prompt. Nothing else changes.

## 0.0.24 — 2026-11-07

- **Cmd-click opens links.** A bare URL, a markdown link's span, or a program's
  OSC 8 hyperlink opens in the default browser (`http`, `https` and `file`
  only), even when a full-screen program owns the mouse. A plain click still
  selects.

## 0.0.23 — 2026-11-04

- **Kil ▸ Restart.** The Kil menu has a **Restart** item: it stops every running job, saves
  the layout, quits and opens Kil again, so a fresh window comes back on the same tabs. A job
  that was running is typed into its new shell again, as after any other ending.

## 0.0.22 — 2026-11-01

- **Batman replaces the Paper theme.** Black background, cream text and the logo's palette,
  with the logo yellow (`#ffff36`) as the accent: the focused pane's border, tab marks and
  Settings controls now use it. `theme = "paper"` no longer parses; use `theme = "batman"`.

## 0.0.21 — 2026-10-31

- **The focused pane's border stays visible beside split dividers.** In side-by-side or
  stacked splits, dividers no longer cover its left or top edge. The border sits 1px
  inward, keeping the same color and thickness and leaving divider drag areas unchanged.

## 0.0.20 — 2026-10-31

- **The focused pane gets a thin accent border.** With several panes, the focused one is
  outlined by a 1px line in the theme's accent color, fading in as focus moves, so it
  shows even on dark themes where the dim alone is faint.

## 0.0.19 — 2026-10-27

- **Programs that ask for drag reports own the selection.** Under 1002/1003
  (killi's fullscreen mode, killi 0.6.7), the press, drags and release reach
  the program as reports — clamped to the grid past the pane's edge, so its
  edge autoscroll keeps moving — and Kil selects nothing. Shift+drag stays
  Kil's own selection, as in xterm; 1000-only programs are untouched.

## 0.0.18 — 2026-10-25

- **Clicking a killi notification no longer opens Script Editor after an upgrade.** The
  session server outlives app upgrades, and it kept stamping new shells with its own old
  `TERM_PROGRAM_VERSION`, so killi judged Kil too old for OSC 9 and fell back to
  `osascript` — whose notifications belong to Script Editor. The app now stamps its own
  name and version into every spawn, over whatever server runs it, so new panes get Kil's
  own notifications again (shells already running keep their env until restarted).
- **Notifications say where they came from.** A program's bare "Done" now shows with the
  foreground job as its title and the tab and folder as a subtitle, instead of a lone
  "Done" with no clue which pane finished.

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
