# kil: design and plan

A native macOS terminal written in Rust. It has vertical tabs in a left sidebar, split panes with
animated transitions, and keyboard shortcuts you can customize. The top priorities are latency,
frame pacing, and **strict ownership of every process, PID, file descriptor, thread and byte it
creates**.

Status: design approved for build. Every claim marked **[measured]** was verified on the target
machine (macOS 27.0, arm64, rustc 1.94.0) with throwaway probes. Those probes become M1's
integration tests.

Build status:
- **M1 is done.** All §4.8 tests pass, and the churn test runs 500 cycles.
- **M2–M5 are implemented and lint/test-clean** (93 tests). They still need hands-on UI verification: the A9 checklist, IME, and animation smoothness.
- **A14–A16** (row menu and rename, Settings window, double-click for a new tab) are implemented. Headless GPUI tests drive the clicks, input and keys. They still need a hands-on pass with a real IME.
- **A17** (sidebar status marks: running, done, failed) is implemented, with the background-tab wakeup fix it needed (§4.3). F13/F14 (children that exit before they are watched) are fixed and covered by `self_exit.rs`. F15 (children inherited a blocked signal mask, so jobs outlived a crashed kil) is fixed and covered by `crash.rs` and `hygiene.rs`. The §6.4 numbers were re-measured after a profiling pass.
- **A18** (reopen everything after any exit: quit, closed window, logout, crash) is implemented (§4.9). `workspace`, `restore`, `transcript`, `terminal` and `lifecycle` tests cover it. A run of the real binary was checked by hand: a `kill -9` and a terminate (as at logout) each reopened the screen and re-ran the job, and a crash during reopening skipped the re-run. No jobs were left behind.
- A dev bundle is built with `scripts/bundle.sh`. The icon source is `packaging/app-icon.png` (1254 px, transparent outside its rounded square); `sips` renders the sizes.
- Release signing and notarization (the rest of M6) is not done yet.
- **0.0.2** adds five themes (A22), a default directory for new tabs (A23), renaming a tab with a double-click (A16), and menus that follow `keymap.toml` plus **Keyboard Shortcuts…** (A8). The session server protocol is 2, and it talks down to 0.0.1's server.
- **§14, the session server** (jobs continue across a quit or an app crash), is built through S3. Its tests pass (`server.rs`, the server half of `crash.rs`, and `workspace` GPUI tests), and the real binary was checked by hand. The app was quit, and separately `kill -9`ed. Each time, three panes kept the same shell PIDs and their jobs kept running, and the next launch went back on them. Killing the server hung its jobs up. Throughput and echo latency through the server match a local session (§6.4). S4's TCC check on a fresh install is not done yet.

---

## 1. Outcome

### Acceptance (v1): observable behavior

| # | Behavior | Proof |
|---|----------|-------|
| A1 | Launch opens one window: a left sidebar with one tab, and one pane running `$SHELL` as a login shell in `$HOME`. | Manual. Time to first prompt ≤ 200 ms (log timestamp). |
| A2 | `cmd-t` opens a tab in the focused pane's cwd. Its sidebar row animates in, and focus moves to it. | Manual + `pwd`. |
| A3 | `cmd-shift-d` splits right and `cmd-d` splits down. The new pane inherits cwd, and the split animates. Each pane gets **one** `SIGWINCH` per layout change, not one per frame. | `trap 'echo WINCH' WINCH` counter. |
| A4 | `cmd-shift-c` or shell `exit 0` closes the pane with an animation. The last pane closes its tab. The last tab closes the window, which quits the app. | Manual. |
| A5 | A non-zero exit or a signal death keeps the pane open with a banner showing `exited 1` or `killed: SIGKILL`. Any key closes it. | `exit 3`. |
| A6 | Focus moves between panes with `cmd-alt-arrows` or a click. The focused pane is the bright one, and the others are dimmed to about half. Dividers can be dragged to resize. | Manual + `workspace` GPUI test (click focuses). |
| A7 | Switch tabs by clicking, `cmd-1…9`, or `cmd-shift-[ / ]`. | Manual. |
| A8 | `~/.config/kil/keymap.toml` overrides any default, including the split and close keys. Invalid entries appear in a banner with the key and the reason, and valid entries still apply. The file is reloaded when the window regains focus, and the menu bar is set again so its items show the keys now bound. **Settings ▸ Keyboard** lists every command: clicking its shortcut and pressing a chord binds that command and applies it at once, the old keys stop working, a key another command already has is refused by name, and **Reset** puts the default back. **Kil ▸ Keyboard Shortcuts…** opens the file in the default text editor, first writing a commented template that lists the commands. | Rebind the split keys, in the app and in the file; `keymap` / `config` tests. |
| A9 | Terminal correctness: vim, htop, less, tmux, 256-color and truecolor, CJK wide characters aligned to the grid, **Chinese IME preedit at the cursor**, selection, copy, bracketed paste, trackpad scrollback. | Manual checklist. |
| A10 | **Process hygiene.** Child fds are exactly 0, 1, 2 plus whatever the shell opens. Signal mask and ignore set are empty. Closing never blocks the UI thread. The session process is reaped within ≤ 3 s even if it ignores SIGHUP or is blocked writing. There are no zombies. | M1 integration tests (§4.8). |
| A11 | **Memory.** Per-pane overhead without scrollback is ≤ 2 MB. Scrollback cost matches the formula in §5 within ±10%. A churn of 500 open/close cycles returns threads, fds, child count and live `TerminalPane` count to baseline. | Churn test plus `footprint`. |
| A12 | **Performance.** Idle CPU is 0.0% with blink off and < 1% with blink on. Animations run at 120 fps with 4 panes streaming output (Metal HUD). Throughput is at least Terminal.app's on the same file. Keypress-to-photon is ≤ 1 frame after the PTY echo. | §6.4 |
| A13 | Release build is signed with **Developer ID Application (team 43VLF3KTFZ)**, notarized and stapled, and `spctl -a -vv` accepts it. | §11 |
| A14 | Right-clicking a sidebar row opens a menu with **Rename Tab…**, **Reset Name** (shown only when the tab has a name) and **Close Tab**. Rename edits the title in place, and IME input works. Enter or a click elsewhere keeps the edit, and Escape cancels it. An empty name goes back to the pane's title. The name also becomes the window title. | `workspace` GPUI tests + manual Pinyin. |
| A15 | **Kil ▸ Settings…** (`cmd-,`) opens a separate Settings window, or brings the open one forward. It edits the theme, font family and size, the default directory (typed, or picked with **Choose…**), scrollback, Option as Meta and cursor blink, and opens `keymap.toml`. Each change is written to `config.toml` and applied to the open window at once. The Kil menu also has Services, Hide, Hide Others and Show All. | `settings_window` / `config` tests + manual. |
| A16 | Double-clicking empty space in the sidebar opens a tab, the same as `cmd-t`. Double-clicking a row renames it in place, as Rename Tab… does: Enter or focus leaving the field saves, and Escape cancels. | `workspace` GPUI tests. |
| A17 | A sidebar row shows a **ring** while any of its panes runs a foreground job, a **dot** when a job ended while the tab was not in view (a background tab, or the window inactive) until the tab is viewed, and a red **!** while a pane shows its exit banner. The marks are static: nothing animates while a job runs. | `workspace` GPUI tests + `take_dirty` test. |
| A18 | However kil ended (`cmd-q`, the window's close button, a logout or restart, a crash or `kill -9`), the next launch reopens what was there: the window frame, the tabs with their names and order, the active tab, each tab's splits, ratios and focused pane, and in each pane the shell's cwd and its old screen and scrollback with colors. A pane that had a foreground job types that job's command line into its new shell again. While kil runs this is saved at most every 2 s, which bounds what a crash can lose. Closing the last tab leaves nothing to reopen. If kil dies within 10 s of reopening, the next launch reopens without re-running the jobs. | `workspace` / `restore` / `transcript` / `lifecycle` tests + manual. |
| A22 | Five themes (Night, Paper, Fjord, Ember, Dusk), picked from swatches in Settings or with `theme` in `config.toml`. A change restyles the chrome and every pane at once, and programs that ask for the terminal's colors (OSC 10/11/4, e.g. vim choosing light or dark) get the new palette. | `theme` tests, `color_queries_should_follow_the_theme_palette`, contrast checked (§7). |
| A23 | With a default directory set, new tabs open there (`cmd-t`, `+ New Tab`, a double-click on the sidebar, the first tab of a fresh launch). Splits still open where their pane is. Without one, a new tab follows the focused pane, as before. | `workspace` GPUI test + manual. |
| A24 | Whenever a tab's panes change — a split (`cmd-shift-d`, `cmd-d`) or a closed pane (`cmd-shift-c`, a shell's `exit 0`) — the tab shares its space equally again among all its panes, the first one included: panes side by side share the width and panes stacked share the height, however the tab was split (`1 | 2 | 3` gets thirds whichever pane was split). Each split animates to its children's share of the panes along its axis; panes stacked across it count as their longest line. Double-clicking any divider does the same. A dragged divider holds until the next pane change or double-click, and a restored layout keeps its saved ratios. | `pane_tree` unit tests + `panes_should_share_a_tab_evenly_again_after_a_pane_closes` + `double_clicking_a_divider_should_share_the_tab_equally_again` + manual. |

### Non-goals (v1)
These are left out and can be added later:
- Multiple windows. Keeping a running process itself across launches: today a job is started again, not resumed (§4.9). Continuing it needs a session server; that is planned in §14.
- SSH and tmux control mode.
- Ligatures, image protocols (sixel, kitty) and the kitty keyboard protocol.
- OSC 133 shell integration, OSC 8 hyperlinks, URL click.
- Scrollback search and tab drag-reorder.
- Themes beyond one built-in dark theme, a keymap editor in the Settings window (it links to `keymap.toml` instead), and plugins.
- Auto-update.
- Intel and universal builds (arm64 only).
- The Mac App Store, because a terminal can't run in the App Sandbox (§11).
- Reading the system "Reduce motion" setting (the motion scale hook exists; see §7).

---

## 2. Stack

| Concern | Choice | Why |
|---|---|---|
| UI, GPU, text, input | **`gpui = "=0.2.2"`** (Apache-2.0) | Metal renderer with display-link frames and no frames when idle. It has an action and keymap system with key contexts (the customizable-shortcut requirement), `with_animation` and easings, `request_animation_frame`, `AnyView::cached`, `EntityInputHandler` (IME), transparent titlebar and blurred window background, and a `leak-detection` feature for tests. All of these were confirmed in the 0.2.2 source. Shaders compile at build time with the installed Xcode Metal toolchain. Pinned exactly because the API is pre-1.0 and changes between versions. |
| VT parser, grid, PTY I/O loop | **`alacritty_terminal = "=0.26.0"`** (Apache-2.0) | Proven parser, grid, selection and damage tracking. Its `EventLoop` is generic over the public `EventedPty + OnResize` traits. |
| PTY spawn and child lifecycle | **Our own `Pty`**, implementing alacritty's public traits | alacritty's `tty::new` fails on macOS in measured ways (§4.1). We keep its parser and loop and replace only spawn, exit detection and teardown. |
| Darwin syscalls | `libc = "0.2"` | `openpty`, `proc_pidinfo`, `proc_listchildpids`, `tcgetpgrp`, `kevent`. All of these exist in 0.2.183. |
| Exit notification | `polling = "3"`, the same instance alacritty uses, with its kqueue `Process` filter | Per-child `EVFILT_PROC NOTE_EXIT` with no process-wide SIGCHLD handler. |
| Config | `serde`, `toml` | |
| Errors | `thiserror` in `kil-term`; `anyhow` only in `kil/src/main.rs` | Apollo ch.4 |
| Logging | `log`, `env_logger` | GPUI already uses `log`. |

**Rejected:**
- winit + wgpu + a custom UI: you would have to build the animation, layout and input stack yourself.
- iced: weaker IME and keymap story.
- AppKit through objc2: not idiomatic Rust, and Core Animation fights a grid renderer.
- `portable-pty`, `signal-hook`, `mimalloc`: `mimalloc` returned no memory to the OS in our test (§5.1).
- `gpui-component`: the sidebar and splits are small, and we need direct control over their animations.

**Licensing.** Zed's `terminal` and `terminal_view` crates are **GPL-3.0**. Do not copy from them. GPUI and alacritty_terminal are Apache-2.0. Key encoding follows the xterm ctlseqs spec and is written from scratch.

---

## 3. Architecture

```
kil/
├─ Cargo.toml                 workspace: members, [workspace.lints], profiles
├─ rustfmt.toml  clippy.toml
├─ crates/
│  ├─ kil-term/               NO UI deps. Processes, PIDs, PTY, session, snapshots.
│  │  ├─ src/lib.rs           #![deny(missing_docs)]
│  │  ├─ src/session.rs       Session: Term + EventLoop + identity + state
│  │  ├─ src/pty.rs           Pty: master + child, kqueue registration, close-first Drop
│  │  ├─ src/spawn.rs         openpty, env, Command + pre_exec hygiene
│  │  ├─ src/reaper.rs        one thread: adopts closed children, escalates, reaps
│  │  ├─ src/proc_info.rs     ProcIdentity, TtyId, foreground(), cwd()
│  │  ├─ src/snapshot.rs      Frame / RenderCell (UI-agnostic render copy)
│  │  ├─ src/transcript.rs    screen + scrollback as SGR bytes, and their replay
│  │  ├─ src/pane_loop.rs     a session's I/O thread: PTY ⇄ Term, queries, attached client (§14.4)
│  │  ├─ src/state.rs         a whole terminal as bytes, for an attach (§14.5)
│  │  ├─ src/server.rs        `kil --server`: sessions by id, socket, idle exit (§14.3)
│  │  ├─ src/remote.rs        Client, and the feed of a remote session's mirror Term
│  │  ├─ src/wire.rs          frames of the server protocol
│  │  ├─ src/error.rs         SpawnError, SessionError (thiserror)
│  │  ├─ src/sys/darwin.rs    the ONLY module with `unsafe` (FFI), each block // SAFETY:
│  │  └─ tests/               lifecycle, hygiene, hostile children, churn (§4.8)
│  └─ kil/                    GPUI app
│     ├─ src/main.rs          anyhow at the edge only
│     ├─ src/workspace.rs     window root: tabs, active tab, quit/close-confirm flow
│     ├─ src/pane_tree.rs     PURE generic PaneTree<T> (no GPUI types) + unit tests
│     ├─ src/restore.rs       session.toml + panes/*.ansi: saved while running, reopened at launch
│     ├─ src/terminal_pane.rs Entity owning a Session; event pump; title/cwd cache
│     ├─ src/terminal_element.rs  gpui::Element: snapshot → quads + glyph runs
│     ├─ src/sidebar.rs       vertical tab list
│     ├─ src/text_field.rs    one-line IME text field (tab rename, Settings)
│     ├─ src/settings_window.rs  Kil ▸ Settings…: edits config.toml key by key
│     ├─ src/motion.rs        Tween (retargetable), Motion durations/easings
│     ├─ src/keymap.rs        defaults + keymap.toml → gpui KeyBinding
│     ├─ src/keys.rs          keystroke → PTY bytes (modes, option-as-meta)
│     ├─ src/config.rs        config.toml
│     └─ src/theme.rs
├─ packaging/                 Info.plist, kil.entitlements, AppIcon.icns   (M6)
└─ scripts/release.sh         build → sign → notarize → staple → releases/*.dmg (local, no CI)
```

The split has a purpose. `kil-term` holds every invariant about processes and memory, and it compiles
and tests in seconds without GPUI. The app crate only renders and routes input.

### Threads and data flow

```
app (kil)
  main thread (GPUI/AppKit) ─ frames on display link only when notified; input; actions
     ▲ futures::mpsc (Wakeup coalesced to ≤1 in flight; Title, Bell, ChildExit, Clipboard)
     │
  mirror feed × N  (remote.rs, one per pane)
     read socket → vte → mirror Term (Arc<FairMutex>) ; write input frames
  background executor: connect, spawn or attach off the UI thread

session server (kil --server, §14)
  accept × 1       one thread per new connection until it becomes a pane's
  pane loop × N    (pane_loop.rs, one per session)
     read master → vte → Term → forward to the client ; write input ; NOTE_EXIT on kqueue ; answers queries
  reaper  × 1      adopts closed children → NOTE_EXIT or deadline → SIGKILL → reap
```

### Ownership graph (GPUI)

`Workspace` owns `Vec<Tab>`. Each `Tab` owns a `PaneTree<Entity<TerminalPane>>` and a focused pane.
Each `TerminalPane` owns a `Session`, which owns the event-loop sender and a `ProcIdentity`. The
`Pty` (master fd plus `OwnedChild`) is owned by the PTY thread. Background tasks hold
`WeakEntity`, never `Entity`. Subscriptions are stored on their owner (`_subscriptions: Vec<Subscription>`).

---

## 4. Process and PID management

### 4.1 Measured findings (macOS 27.0, arm64)

| # | Finding | Consequence |
|---|---------|-------------|
| F1 | alacritty `tty::new` on macOS runs `/usr/bin/login -flp $USER /bin/zsh -fc "exec -a -zsh $SHELL"`. `login` runs with root euid, so **`proc_pidinfo` on our own child fails with `EPERM`**. The shell is a grandchild. | We spawn the shell directly as a login shell (`argv[0] = "-zsh"`). The PID we own is then the shell itself. |
| F2 | alacritty's `Drop for Pty` sends `kill(child, SIGHUP)` and then does a **blocking `wait()` while the master is still open**. A child that ignores HUP blocked the dropping thread for more than 4 s, until it was SIGKILLed manually. During that time 1 thread and 2 fds leaked. | Teardown must never wait on a live master. |
| F3 | A child blocked writing to an undrained PTY (`zsh -l` with no reader) **deadlocked** that `Drop`. **Even SIGKILL left zsh in exiting state (`E`)** until the master fd closed. | **Close the master first.** It is the only teardown order that always terminates. |
| F4 | The proposed teardown (close master, SIGHUP, kqueue `NOTE_EXIT`, SIGKILL after grace, reap) was tested. A shell with a foreground job was reaped in 3.5 ms. With undrained output it took 4.2 ms. A shell that **ignores HUP** was SIGKILLed after a 1 s grace and reaped. No blocked thread, no zombie. | This is the design below. |
| F5 | The std `Command` fork path, which any `pre_exec` forces, **leaks non-CLOEXEC fds**: a planted fd 3 showed up in the child. With a CLOEXEC sweep in `pre_exec`, the child had only `[0,1,2,10]`, where 10 is zsh's own tty fd. | We add a CLOEXEC sweep. |
| F6 | `posix_spawn` with `SETSID` and then opening the slave does **not** acquire a controlling tty on macOS (`e_tdev = NODEV`, `tcgetpgrp = 0`). | Pure posix_spawn can't be used without a helper binary. See the trigger in §13. |
| F7 | With a direct shell, the shell's `e_tdev` equals `stat(ptsname).st_rdev`. `tcgetpgrp(master)` and the shell's `e_tpgid` both track the foreground job (`sleep`, `busy=true`). | Tty-identity verification and busy detection are feasible. |
| F8 | Child signal ignore set and mask are empty, because std resets the mask and SIGPIPE. Spawn took 1.3–3.9 ms in a small process. | **Superseded by F15** for the mask. The ignore-set check was `ps -o sigignore=`, a keyword macOS `ps` doesn't have, so it passed without checking anything. |
| F9 | A churn of 40 login sessions took 2.1 s. The worst drop was 3.9 ms. fds, threads and children returned to baseline, and 0 session processes survived. | |
| F10 | libc's `openpty(3)` resolves the slave through `ptsname(3)`'s static buffer, so concurrent spawns can race and even open each other's slave. | kil uses `posix_openpt` + `TIOCPTYGNAME` + `open(O_CLOEXEC)`, which is thread-safe, and the slave is close-on-exec atomically. |
| F11 | Under concurrent spawns on macOS 27, `posix_openpt` occasionally fails with errno −6, XNU's internal `EREDRIVEOPEN` leaking to user space (reproduced in about 1 of 3 test runs). | Bounded retry (16 attempts) on −6 and `EINTR`. 15 consecutive full test runs then passed. |
| F12 | An unbundled binary started from another terminal inherits that terminal's `__CFBundleIdentifier`, and AppKit then names and badges the process as that app (it showed up as "Otty"). | `main` clears the variable when not running inside a `.app`, and child shells no longer inherit `OTTY_*` or `CW_TERM`. |
| F13 | A child that exits before its PTY thread registers `NOTE_EXIT` makes the registration fail with `ESRCH` (2–11 of 64 concurrent `true`/`exit 3` sessions per run). alacritty then ends the loop **without reading the PTY**, and the child is still mid-exit (not yet reapable) when the `Pty` drops. The reaper still reaped it, so no zombie, but the program's output and exit status were lost: `exit 3` showed "Process ended", and a clean exit didn't close the pane. Otty leaks a zombie on this same "died during launch" path. | `Pty::register` falls back to a 5 ms kqueue timer under the child-event token, re-armed until `try_wait` succeeds. The same poll covers a `NOTE_EXIT` that arrives before the child is reapable. The loop keeps draining output meanwhile (F3). |
| F15 | **The child inherits the forking thread's signal mask, and std does not reset it.** The app spawns on GPUI's executor threads (GCD workers), which block most signals: a pane's process had mask `0xfbfee027`, with HUP, INT and QUIT blocked. A blocked SIGHUP survives every hangup. A program that doesn't reset its own mask (`sleep`, a build, a server) ignored the close-time SIGHUP, so only the 3 s SIGKILL ended it. When kil was killed or crashed, **it kept running forever, reparented to launchd**. That is the live-process leak the Otty report checked for. The main-thread measurement in F8 couldn't show this. | `pre_exec` empties the mask with `sigprocmask` (async-signal-safe) after resetting dispositions. Tests spawn from a thread that blocks signals: one checks that SIGHUP ends the child, and one aborts an owner process and checks that its job dies. |
| F14 | `polling` records a filter's source before the kernel accepts it, and keeps it after a one-shot fires. A failed watch left a stale source, and so did every reaped ward. The reaper's source set grew by one per closed session, and a recycled PID could never be watched again, which left only the 250 ms sweep. | `watch_exit` undoes a failed registration. The reaper unwatches each ward before reaping it and re-watches the ones still running. |

### 4.2 Spawn (`spawn.rs`, runs on the spawner thread)

1. **Resolve the shell.** Use `$SHELL` if it is an executable absolute path. Otherwise use `getpwuid_r(getuid()).pw_shell`. Otherwise `/bin/zsh`.
2. **Resolve the cwd.** Use the requested cwd if it exists and is a directory. Otherwise `$HOME`. std fails the spawn on a bad `current_dir`, so we validate first.
3. **`openpty`** with the initial winsize. Set `FD_CLOEXEC` on both ends and `O_NONBLOCK` on the master. Set `IUTF8`. Record `TtyId = fstat(slave).st_rdev`.
4. **Environment.** Inherit the app environment, then:
   - Set `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=kil` and `TERM_PROGRAM_VERSION`.
   - If `LANG` is unset (GUI launches have none), derive it from `CFLocale` as `<id>.UTF-8` and check that `/usr/share/locale/<id>.UTF-8` exists. Fall back to `en_US.UTF-8`.
   - Remove foreign terminal markers: `TERM_SESSION_ID`, `ITERM_*`, `TMUX*`, `VSCODE_*`, `GHOSTTY_*`, `ALACRITTY_*`, `WEZTERM_*`, `KITTY_*`, `__CFBundleIdentifier`.
5. **`Command::new(shell).arg0("-<basename>")`.** This makes a login shell with no `/usr/bin/login` (F1). The tradeoff: no utmp entry (`who` won't list kil sessions) and no "Last login" line.
6. **`pre_exec`.** Only async-signal-safe calls are allowed, because the parent is a multithreaded ObjC process.
   - `setsid()`
   - `ioctl(0, TIOCSCTTY, 0)`
   - CLOEXEC sweep of fds `3..=max_open_fd+64`. The bound is computed **before** fork from `PROC_PIDLISTFDS`. Fds are marked CLOEXEC rather than closed so std's internal exec-error pipe keeps working.
   - Reset `SIGCHLD SIGHUP SIGINT SIGQUIT SIGTERM SIGALRM SIGPIPE` to `SIG_DFL`.
7. Right after spawn, record **`ProcIdentity { pid, start }`** from `pbi_start_tvsec/usec`. The start time is fork time and survives exec.

Spawn runs off the UI thread. The pane appears and animates immediately, and the session attaches
when ready, so the animation hides spawn latency.

### 4.3 Identity and queries (`proc_info.rs`)

Types:
- `Pid(NonZeroI32)`
- `TtyId(u32)`
- `StartTime { sec: u64, usec: u64 }`
- `ProcIdentity { pid, start }`, which is `Copy` and 16 bytes, so it is passed by value.

Queries:
- `foreground(shell: ProcIdentity, tty: TtyId) -> Option<ForegroundProcess { pid, name, cwd }>`
  1. Read `bsdinfo(shell.pid)` and **verify the start time** (this guards against PID reuse).
  2. Take its `e_tpgid` and read `bsdinfo(tpgid)`.
  3. **Verify `e_tdev == tty`.**
  4. Read the name from `pbi_name`/`pbi_comm` and the cwd from `PROC_PIDVNODEPATHINFO`.
- **Busy** means `tpgid != shell pgid` (F7).
- **Cwd for a new tab or pane** is a fresh synchronous query on the focused pane: the foreground leader's cwd, falling back to the shell's cwd.
- **Tab title:** the OSC 0/2 title if set. Otherwise the foreground name when busy. Otherwise the shell cwd with `~` abbreviation. Refreshed at most every 250 ms after a `Wakeup`, and on focus. Never per frame.
- **Background tabs.** A session stays dirty until it is snapshotted and sends no `Wakeup` while dirty, and a pane in a background tab is never drawn. Its title and busy state therefore froze after its first output, so a finished job kept its ring and never got its dot. The info refresh now calls `Session::take_dirty`, which re-arms wakeups at most once per 250 ms per undrawn pane, and marks the pane's frame stale so its next draw snapshots.
- A busy → idle transition emits `PaneEvent::JobFinished`. The tab records it as *done* unless it is the active tab of an active window, and selecting the tab or reactivating the window clears it (A17).

### 4.4 Exit detection and reaping: no signal handlers anywhere

- `Pty::register` adds the master fd and a kqueue `Process(child, Exit)` filter to alacritty's own `Poller`, under `PTY_CHILD_EVENT_TOKEN`.
- `next_child_event` calls `try_wait()` (reap) and returns `ChildEvent::Exited(status)`.
- There is no process-wide SIGCHLD handler. With N panes, alacritty's signal-pipe design wakes all N loops on every child exit, and this design avoids that.
- If `NOTE_EXIT` can't be registered because the child already began exiting (F13), or it fires before the child is reapable, the loop polls with a one-shot kqueue timer under the same token until `try_wait` reaps.

**Ownership is the PID-safety mechanism.** `OwnedChild` wraps `std::process::Child`, is not
`Clone`, and is the **only** way to signal or wait on our child. Reaping consumes it. An unreaped
child's PID cannot be recycled, so code that holds an `OwnedChild` can never signal a stranger.
Non-child targets, such as a foreground job's pgid, are signalled only through
`verified_killpg(pgid, tty)`. It re-checks `e_tdev == tty` right before `killpg`. A residual
TOCTOU window of a few microseconds remains and is documented.

### 4.5 Teardown (`Pty::drop` + `reaper.rs`)

```
close pane ─► Session::close(): Msg::Shutdown to loop; UI keeps last Frame for the close animation
PTY thread exits loop ─► Pty::drop (never blocks):
    1. drop(master)                 hangup: tty revoked, blocked writers get EIO, session gets SIGHUP   (F3, F4)
    2. child.signal(SIGHUP)         explicit, in case the shell is the only one left
    3. reaper.adopt(child, now+3s)  ownership moves to the reaper; drop returns
reaper (one thread, kqueue): NOTE_EXIT ─► try_wait ─► done
                             deadline ─► SIGKILL child (safe: still unreaped) ─► NOTE_EXIT ─► reap
```

- A foreground job that **chose** to ignore HUP (`nohup`, `trap '' HUP`) survives and is reparented to launchd. This matches Unix and Terminal.app semantics, and it was measured (F4, P3). We escalate only against our own child.
- `OnResize` failures, such as `ENOTTY` after a hangup, are logged and ignored. alacritty calls `die!` there, which would exit the process.

### 4.6 Quit, crash, natural exit

- **Natural exit.** NOTE_EXIT fires, the PTY thread reaps and sends `ChildExit(status)`. Status 0 closes the pane with an animation. Anything else shows the banner from A5. This also stops a broken `.zshrc` from creating a crash loop.
- **Quit (`cmd-q`) and the close button** detach: the sessions keep running in the session server (§14). If any is busy, a sheet names the jobs and offers Quit, Stop Jobs and Quit, and Cancel. **Quit and Stop All Jobs** (and the sheet's second button) ends every session in the server as in §4.5: SIGKILL after the grace, reaped, then the app exits.
- **Crash or abort.** The kernel closes every master, which hangs up every session (F4). The next launch reopens the state saved up to 2 s earlier (§4.9). Shells exit and launchd reaps them. Because the children start with an empty signal mask (F15), the hangup's SIGHUP reaches them. `crash.rs` aborts an owner and checks this. Release builds use `panic = "abort"`: a half-alive pane is worse than a clean crash, and the kernel is the safety net.
- **Info.plist.** `NSSupportsSuddenTermination = false` and `NSSupportsAutomaticTermination = false`, so macOS never skips this flow. `NSAppSleepDisabled = true`: if App Nap throttles the PTY reader, child builds block on a full PTY. Verify the App Nap behavior in M6.

### 4.7 Close confirmation

This applies to closing a pane, tab, window or the app when any affected session is busy (§4.3). The
sheet lists the foreground process names. Idle shells close without asking.

### 4.8 Invariants and how they're tested (`kil-term/tests/`, real processes)

| Test (name says the claim) | Asserts |
|---|---|
| `spawn_should_give_child_only_stdio_fds_when_parent_has_leaky_fd` | Child fd set is ⊆ {0,1,2} ∪ the shell's own fds (F5). |
| `spawn_should_reset_signals_ignored_by_parent` | With SIGTERM ignored in the parent, SIGTERM still ends the child. This is a behavioral check, because macOS `ps` has no `sigignore` (F8). |
| `spawn_should_unblock_signals_blocked_by_the_spawning_thread` | Spawned from a thread that blocks SIGHUP, the child still dies of SIGHUP (F15). |
| `crash_of_the_owner_should_hang_up_its_jobs` | An owner process blocks every signal on its spawning thread, starts `exec sleep 600` and aborts. The job is gone within 2 s (F15). Red without the mask reset. |
| `close_should_reap_within_grace_when_child_ignores_sighup` | Reaped ≤ 3.5 s with a SIGKILL status, and the calling thread isn't blocked (F2, F4). |
| `close_should_not_deadlock_when_child_blocks_on_undrained_output` | Reaped ≤ 100 ms (F3). |
| `close_should_leave_nohup_job_running_reparented` | Semantics are preserved. |
| `natural_exit_should_report_status` | `exit 3` gives `ChildExit(3)`. |
| `foreground_should_report_job_and_reject_foreign_tty` | Busy detection plus the identity check (F7). |
| `identity_should_reject_recycled_pid` | Start-time mismatch returns `None`. |
| `churn_500_should_return_to_baseline` | Own fds, threads (`PROC_PIDTASKINFO`) and children (`proc_listchildpids`) are unchanged, and there are no zombies (F9). |
| `no_signal_handlers_installed` | `sigaction(SIGCHLD)` is still `SIG_DFL` after N sessions. |
| `sessions_that_exit_at_startup_should_be_reaped_and_keep_status_and_output` | 64 concurrent sessions that exit at once, none of them closed: each reports its exact status and its last output, and no child is left unreaped (F13). Red before the fix. |

### 4.9 Restore at the next launch (`restore.rs`, `kil-term/src/transcript.rs`)

- **What is saved.**
  - The window frame and state (windowed, maximized, full screen). A frame that no longer meets any display is dropped.
  - The tabs in order with their names, and the active tab.
  - Each tab's split tree: axis, target ratio and focused pane. Closing panes are left out.
  - For each pane, a stable id, the shell's own cwd and the foreground job's command line. A login zsh reports the line as typed: kil starts it with `ZDOTDIR` at `~/Library/Caches/kil/zsh`, whose `.zshenv` (`kil-term/src/shell/zshenv`) puts the user's `ZDOTDIR` back, reads their `.zshenv`, and adds a `preexec` hook that writes the line to `jobs/<shell PID>` and a `precmd` hook that empties it. kil takes that line when the foreground leader is the shell's own child and the file is newer than the shell. This is needed because a program that sets its title (Node's `process.title`, e.g. `kimi`, which becomes `kimi-code '' ''`) overwrites its arguments, and macOS no longer shows other processes' environments. Otherwise, as in bash or fish, the arguments come from `KERN_PROCARGS2` for the foreground group's leader, verified like `foreground()` (§4.3), and are quoted into one command line. With arguments only the running job is captured: in `make && ./run` or `cat log | grep x`, it is the part that was running. A job owned by another user (`sudo`) can't be read, so only its cwd is saved.
  - Each pane's screen, in `panes/<id>.ansi`: the primary screen's scrollback up to the cursor line, as text with SGR attributes. Wrapped lines stay joined, so they re-wrap at any width. While a full-screen program has the alternate screen, the primary screen can't change, so the last file stands (alacritty only exposes it by swapping screens, which resets the alternate one). Rendering 10,000 lines × 200 columns takes ~4.5 ms in release, under the terminal lock, off the UI thread.
- **When.**
  - While kil runs, a change (layout, title, cwd or job, window frame, or new output) schedules a save `SAVE_INTERVAL` (2 s) later, at most one at a time. So a crash loses at most ~2 s, and an idle kil never wakes for it.
  - The state is taken on the UI thread and written on a background thread. Saves are numbered, and an older one never overwrites a newer `session.toml`. Screens are rendered at write time from the live terminals, so a late write is never older.
  - `Quit`, the window's close button and GPUI's app-quit hook (a logout or restart terminates the app without our Quit flow) save synchronously, just before `shutdown_all`, while the jobs still run. `shutdown_all` marks the window ended, and nothing is saved after that.
  - Closing the last tab clears the store, so the next launch starts fresh.
- **Where.** `~/Library/Application Support/kil/`: `session.toml` and `panes/`. Every file is written whole and then renamed. Screens of panes that are gone are removed after each save. Pane ids from the file are checked to be `[A-Za-z0-9-]` before they name a file.
- **Crash loop.** Reopening writes a `restoring` marker, removed 10 s later or at a clean exit. If the marker is still there at launch, the last launch died while reopening, perhaps because of a re-run job. The tabs and screens then reopen, but the jobs don't run, and a notice says so.
- **Re-running.**
  - Each pane starts a normal login shell in its cwd, with the old screen painted into the terminal before the PTY thread starts.
  - Its job is typed into the shell when the line editor first waits: zsh, bash 5.1+ and fish turn on bracketed paste at that moment. The line is sent as a bracketed paste plus Enter, so its characters can't trigger completion or other widgets.
  - A shell that never turns on bracketed paste gets the line as plain typeahead after 3 s.
  - Anything the user types first cancels the re-run. A job still waiting to run is saved as is.

---

## 5. Memory management

### 5.1 Measured

- `size_of::<alacritty_terminal::term::cell::Cell>() == 24` bytes. A history row costs **`cols × 24 B`** no matter what it contains.
- 10,000 lines × 200 columns of filled history added **51 MB** of RSS (46 MB in theory).
- `clear_history()` and dropping the `Term` **did not lower RSS or phys_footprint**. The macOS allocator keeps the pages.
  - Closing 4 of 5 full panes returned about 50% with the system allocator.
  - `mimalloc` returned 0.
  - `malloc_zone_pressure_relief` returned 0 bytes.
  - Freed memory **is reused**: reopening a pane did not grow the footprint.

**Conclusion:** we can't count on memory going back to the OS. Excellence here means **bounding the
high-water mark** and making it visible.

### 5.2 Policy

- `scrollback_lines = 10_000` by default (configurable). The documented per-pane maximum is `scrollback_lines × cols × 24 B`, which is about **32 MiB at 140 columns**.
- **Ledger.** `kil-term` exposes `Session::memory()`, computed as `history_rows × cols × 24 + viewport + frame buffer`. That is O(1) and comes straight from `grid.history_size()`. A debug HUD action shows the per-pane and total figures, and the sidebar tooltip shows the per-tab figure.
- `cmd-k` clears scrollback. The memory is reused by later growth, as measured, and the docs say so.
- OSC 52 is copy-only. Apps can't read your clipboard.

### 5.3 Hot path (render)

- Each pane keeps one reusable `Frame { cells: Vec<RenderCell>, … }`, where `RenderCell` is a 16-byte `Copy` value (`char`, rgba fg, rgba bg, `u16` flags). Under the **fair** lock it is `clear()`ed and refilled, colors are resolved in the same pass, and the lock is released. The lock is held for about 50 µs on a 200×50 viewport. Shaping happens outside the lock.
- After a resize, if `capacity > 2 × needed`, call `shrink_to(needed)` so a window that was briefly huge doesn't pin its peak memory.
- Row text is built in one reused `String`. *As built*, every run on screen is re-shaped each frame through GPUI's content-keyed line cache, so allocations are one `SharedString` per run, not O(changed rows). A per-row `(hash, ShapedLine)` cache is the next lever (see §6.4).
- *As built:* the snapshot is skipped when the session is clean (`Session::is_dirty` also covers this side's scrolls, selections and resizes), so a cursor blink doesn't take the terminal lock. Colors are resolved through a 269-slot table built once per frame, not per cell.

### 5.4 Lifetimes and leaks

- Tasks capture `WeakEntity`, and subscriptions live on their owner, so there are no `Entity` cycles.
- Tests enable `gpui/leak-detection`. A `cfg(test)` live counter on `TerminalPane` (incremented in the constructor, decremented in `Drop`) must return to 0 after the churn test.
- Threads: one PTY thread per session. Its 1 MiB stack read buffer becomes resident on the first heavy output and stays resident for the session. There is also one spawner and one reaper. 50 panes means about 52 threads. A shared I/O thread is a §13 trigger, not v1 work.

---

## 6. Rendering and performance

### 6.1 Event pump

`EventProxy` (implements alacritty's `EventListener` on the PTY thread):
- **`Wakeup`.** `if !dirty.swap(true, AcqRel) { tx.send(Wakeup) }`. That allows at most one message in flight, however hard `cat` floods. The UI clears `dirty` when it takes a snapshot.
- **`PtyWrite`** (DSR/DA replies). Sent straight back through the `EventLoopSender`, which is held in a `OnceLock` set after the loop is constructed. There is **no UI-thread round trip**, so apps that query the terminal at startup (vim, fzf) don't wait on frames.
- Everything else goes to the UI task, which drains the channel, updates state and calls `cx.notify()` once.

### 6.2 `TerminalElement`

- **prepaint:** compute the cell size from font metrics (cached per font family and size in a GPUI global; asking CoreText every frame cost ~15% of the UI thread while streaming), then snapshot.
- **paint:**
  1. Background quads, merging contiguous equal bg per row.
  2. Glyph runs. A run breaks on a style change **and never spans a `WIDE_CHAR`**. Each run is placed at `col × cell_w`, so fallback CJK fonts can't drift the grid.
  3. Selection, cursor and IME preedit (underlined, at the cursor).
- Each pane is wrapped in `AnyView::cached`, so output in one pane doesn't re-prepaint its siblings. Verify this in M4 by counting prepaints.

### 6.3 Resize policy

- *As built:* while a tab's layout is animating, every pane in it keeps its current grid (`TerminalPane::frozen`). Content is clipped to the animated bounds, which reads as a curtain. When the animation settles, the grid resizes once. That gives one `TIOCSWINSZ` per layout change without computing target pixel sizes. A newly created pane takes its first measured size immediately.
- While a divider is dragged, at most one resize is sent per frame, and only when rows or columns actually change.

### 6.4 Budgets and how to measure

| Metric | Target | Tool |
|---|---|---|
| Idle CPU | 0.0% (blink off), < 1% (blink on) | `ps -o %cpu -p` over 60 s |
| Animation | 120 fps on ProMotion with 4 panes streaming | `MTL_HUD_ENABLED=1 target/release/kil` |
| Throughput | ≥ Terminal.app on `seq 1 3000000` | `time cat` |
| Input latency | keydown → frame containing the echo ≤ 1 frame after the echo arrives | debug-build timestamps |
| Startup | ≤ 200 ms to first prompt | log |
| Footprint | idle pane ≤ 2 MB plus scrollback formula | `footprint <pid>`, `vmmap --summary` |
| CPU and allocation profile | No measurable allocation in kil code beyond O(changed rows) per frame | `samply record`, Instruments Allocations |

**Measured** (release, one pane, M4 Pro, macOS 27.0):
- **Throughput.** `seq 1 3000000` takes 3.5–4.0 s, and 300k colored lines with CJK take 0.5 s. That's the macOS PTY limit: BSD `script` relaying the same `seq` through a PTY takes 4.6 s. The PTY thread spends ~70% of its time in `read`/`kevent` and ~22% in the VT parser. The UI thread uses ~0.25 s over the whole run, about 0.5 ms per frame.
- **Idle.** 0.35% with blink off and 0.7% with blink on. The floor is GPUI 0.2.2's `CVDisplayLink`, which runs at every vsync while the window is visible, even with nothing to draw (it stops only when the window is occluded). A12's 0.0% is not reachable without changing GPUI (§13).
- **Blink.** One timer per window, running only while the window is active, for the focused pane only. Before, there was one 530 ms timer per pane, focused or not.
- **Zombies.** 0 after every run.

Always measure with `--release`. Release profile: `opt-level=3`, `lto="thin"`, `codegen-units=1`,
`panic="abort"`, `debug="line-tables-only"` (keeps samply symbols).

---

## 7. UI and motion

**Themes** (`theme.rs`, A22). Each is a terminal palette (16 ANSI colors, foreground, background, cursor) plus chrome colors. **Night** is the original near-black. **Paper** is light, and its inactive panes fade toward the paper instead of darkening. **Fjord** is a cool steel blue, **Ember** a warm brown, **Dusk** a violet. Default text contrasts with the background by at least 11.5:1 in all five. Paper's colors 1–6 and 9–14 are at least 4.5:1 (WCAG AA), and bright black (comments, shell autosuggestions) is ~3.2:1 in the dark themes. A change re-snapshots every pane, because cells are resolved to colors when copied, and sends the palette to each session (§14.4).

**Window**
- `TitlebarOptions { appears_transparent: true, traffic_light_position: (14, 18) }`, so the sidebar runs to the top edge.
- `WindowBackgroundAppearance::Blurred` behind the sidebar. The terminal area stays opaque for readability and to avoid overdraw.

**Sidebar** (220 px)
- 44 px rows with a title (13 pt medium) and the cwd (11 pt, middle-truncated).
- A 10 px status slot before the title (A17): an accent **ring** while a pane runs a job, an accent **dot** for a job that ended out of view, and a red **!** for an exit banner. The marks differ in shape as well as color. They are static, because a spinner would redraw the window every frame for as long as a dev server runs. There is an `×` on hover.
- A footer `+ New Tab` button. Double-clicking empty sidebar space also opens a tab.
- Right-click on a row opens a menu: Rename Tab…, Reset Name, Close Tab. A rename edits the title in place in a `TextField`. The user's name overrides the pane's title in the row, in the closing ghost and in the window title.
- The selection highlight is a rounded 8 px pill that **slides** between rows.

**Panes:** 1 px dividers with a 6 px hit area. When a tab has more than one pane, the others are dimmed by a 50% near-black overlay. Their text drops to about half brightness, and their cursor is hollow while the focused one is solid. That is the focus cue. A lighter dim doesn't work: on the near-black background it moves the background by only ~2/255.

**Even shares.** A tab whose panes change re-splits evenly: every split animates its ratio to
0.5, so panes side by side share the width and panes stacked share the height. That happens on a
split and when a pane closes, for the whole tab, not only for the panes that moved. A divider you
dragged is kept until the next pane change, and a layout restored from the session file keeps the
ratios it was saved with (A18).

**Motion.** Every animation is a retargetable `Tween { from, to, start, dur, ease }`.
`retarget(to, now)` restarts from the *current* value, so pressing a key repeatedly never makes the
UI jump. Animations are driven by `request_animation_frame()` only while a tween is active, so they
cost nothing at rest. A global `Motion::scale` set to 0 turns animation off; that is the
reduce-motion hook.

| Event | Property | Duration | Easing (GPUI) |
|---|---|---|---|
| Tab added | row height 0→44, opacity 0→1 | 180 ms | `ease_out_quint` |
| Tab removed | reverse | 140 ms | `ease_in_out` |
| Tab selected | pill y | 160 ms | `ease_out_quint` |
| Split | ratio 1.0→0.5 | 200 ms | `ease_out_quint` |
| Panes changed | every other split's ratio →0.5 | 200 ms | `ease_out_quint` |
| Pane closed | sibling ratio →1.0, then collapse node | 160 ms | `ease_in_out` |
| Tab switch | content cross-fade plus 6 px slide | 140 ms | `ease_out_quint` |
| Focus change | inactive dim | 120 ms | linear |
| Cursor blink | opacity | 100 ms fades, 530 ms period, paused while typing or when the window is inactive | `ease_in_out` |

`PaneTree<T>` is a binary tree:
```rust
enum Node<T> { Leaf(T), Split(Box<Split<T>>) }
struct Split<T> { axis: Axis, ratio: Tween, first: Node<T>, second: Node<T> }
```
`Split` is boxed so `Node` stays small (clippy `large_enum_variant`). Operations: `split`,
`close` (which collapses the parent in `collapse_finished`), `set_ratio` for a divider drag,
`layout(rect) -> impl Iterator<(&T, Rect)>`, and a geometric `neighbor(target, dir)`. `split` and
`close` also `equalize`, sending every other split to an even share. It is pure and generic, so it
is unit-tested with `T = u32`.

---

## 8. Input and keymap

**Actions** (`actions!(kil, […])`): `NewTab`, `ClosePane`, `CloseTab`, `SplitRight`, `SplitDown`,
`FocusLeft/Right/Up/Down`, `NextTab`, `PrevTab`, `SelectTab(n)`, `Copy`, `Paste`, `ClearScrollback`,
`FontBigger`, `FontSmaller`, `FontReset`, `ToggleMemoryHud`, `OpenSettings`, `Hide`, `HideOthers`, `ShowAll`, `Quit`.

App-level commands (`open_settings`, `hide`, `hide_others`, `quit`) bind with no key context, so they work in the Settings window too. Global fallbacks in `main.rs` handle them there. For example, `quit` runs the terminal window's close confirmation.

**Defaults**

| Key | Command |
|---|---|
| `cmd-t` | `new_tab` |
| `cmd-d` | `split_down` |
| `cmd-shift-d` | `split_right` |
| `cmd-shift-c` | `close_pane` |
| `cmd-shift-w` | `close_tab` |
| `cmd-alt-left/right/up/down` | `focus_*` |
| `cmd-shift-[` / `cmd-shift-]`, `ctrl-shift-tab` / `ctrl-tab` | `prev_tab` / `next_tab` |
| `cmd-1` … `cmd-9` | `select_tab_1…9` |
| `cmd-c` / `cmd-v` | `copy` / `paste` |
| `cmd-k` | `clear_scrollback` |
| `cmd-=` / `cmd--` / `cmd-0` | font size |
| `cmd-,` | `open_settings` |
| `cmd-h` / `cmd-alt-h` | `hide` / `hide_others` |
| `cmd-q` | `quit` |

**`~/.config/kil/keymap.toml`** (`$XDG_CONFIG_HOME` is respected):
```toml
# Entries override defaults; "unbind" removes a default.
[bindings]
"cmd-y"    = "split_right"
"ctrl-a |" = "split_down"   # multi-stroke; verify GPUI 0.2.2 sequence support in M5
"cmd-k"    = "unbind"
```

**Recording.** Settings has a **KEYBOARD** section listing every command and the keys it runs
on. Clicking a shortcut puts that row in *Press keys…* and reads the next chord: it is written to
`keymap.toml` and applied at once, and the menus follow. **Reset** drops the user's entry so the
default runs again. Two rules keep the file honest:

- **Rebinding moves a command.** Every line naming that command goes, and its default is left in
  the file as `"keys" = "unbind"`, so the old keys stop working instead of coming back.
- **A key already taken is refused**, with a message naming the command that has it.

Recording uses an `App::intercept_keystrokes` subscription, armed only while a row is listening.
It runs before GPUI resolves any binding, so `cmd-w` is read as data instead of closing the
Settings window, and it only swallows keys in that window, so typing in a terminal still works.
Only single chords are recorded; multi-stroke keys stay a `keymap.toml` feature.

**Parsing**
- Commands come from `enum Command` with serde `snake_case`, so an unknown name is a typed error.
- Keys are checked with GPUI `Keystroke::parse`.
- Errors are collected as `Vec<KeymapError { key, reason }>` and shown in a banner. Valid entries still apply.
- The merged set is applied with `cx.clear_key_bindings()` and then `cx.bind_keys(defaults ⊕ user)`, in the `Workspace` key context. Bindings therefore win over terminal input, and unbound keys fall through.
- Text field editing keys (`TextField` context: arrows, home/end, backspace, delete, `cmd-a`, `cmd-v`, enter, escape) and the Settings window's `cmd-w` are fixed. They are bound after the merged set and can't be changed in `keymap.toml`.
- **Menus follow the keymap.** GPUI writes a menu item's key equivalent only when the menus are set. `config::apply` therefore sets them again after every bind, or a rebound `split_right` would still show, and trigger, its old key.

**Fallthrough to the PTY** (`keys.rs`, written from xterm ctlseqs):
- App-cursor and app-keypad modes, `CSI 1;mods` modifier encoding, ctrl-letters, function keys.
- `option_as_meta` (default `true`) sends ESC + key. The tradeoff: Option-composed characters such as `é` need the setting off.
- Paste uses bracketed paste when that mode is set, and strips ESC from the pasted text.

**IME:** `TerminalPane` implements `EntityInputHandler`. Marked text is drawn at the cursor. Committed text goes to the PTY. `bounds_for_range` returns the cursor rect so the candidate window sits next to the cursor. A cursor the program hid (DECTCEM, e.g. a TUI such as `kimi` that draws its own) is not drawn but keeps its place in the snapshot (`CursorShape::Hidden`), because such programs leave it at their input; without it the candidate window fell back to the screen's bottom-left corner. The acceptance test is Pinyin input.

## 9. Config (`~/.config/kil/config.toml`)

```toml
theme = "night"                  # night, paper, fjord, ember or dusk
font_family = "Menlo"
font_size = 13.0
default_directory = "~/code"     # where new tabs open; "" or absent: follow the focused pane
scrollback_lines = 10000
option_as_meta = true
cursor_blink = true
```

The Settings window writes one key per change (`config::write_setting`). It replaces the line that assigns the key, or appends one, and keeps every other line, comments included. If the result would not parse, nothing is written. The file is written in place, not renamed over, so a symlinked dotfile stays a symlink. The terminal window then reloads through the same mtime path. Scrollback applies to new tabs and panes. Cursor blink applies at once: the window has one blink timer, which runs only while blink is on and the window is active (A12).

Both files are re-read when the window gains focus and the file's mtime has changed. There is no
watcher thread and no extra dependency.

---

## 10. Rust practice (Apollo handbook, applied)

**Errors**
- `kil-term` exposes `thiserror` enums: `SpawnError { OpenPty(#[source] io::Error), ShellNotExecutable(PathBuf), Fork(#[source] io::Error) }`, `SessionError`, and so on.
- `anyhow` appears only in `main.rs` for startup.
- Runtime failures become a pane banner (for example "Failed to start /bin/zsh: …"). They never panic.

**Unsafe**
- Allowed only in `kil-term/src/sys/darwin.rs`, via `#[expect(unsafe_code, reason = "Darwin FFI")]`. *As built*, there are more scoped `expect`s. One is each registration of a source with a `polling::Poller`, which is `unsafe` because the source must outlive it: `Pty::register`, the pane loop's client, the mirror feed and the server's listener. Each deletes its source before dropping it. The other is the first statement of `main`, because edition 2024 makes env mutation `unsafe`.
- Every block has a `// SAFETY:` comment.
- Everything else is safe Rust under `unsafe_code = "deny"`.

**Ownership**
- `OwnedChild` is not `Clone` (§4.4). `ProcIdentity`, `Pid`, `TtyId` and `RenderCell` are `Copy` and passed by value.
- Parameters take `&str` and `&[T]`.
- No clones in render loops. Buffers are reused (§5.3).

**Dispatch:** generics on hot paths (`PaneTree<T>`, `EventLoop<Pty, EventProxy>`). `dyn` only where GPUI requires it (`Box<dyn Action>`).

**Workspace lints**
```toml
[workspace.lints.rust]
unsafe_code = "deny"
unsafe_op_in_unsafe_fn = "deny"
future-incompatible = { level = "warn", priority = -1 }

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"
expect_used = "deny"
redundant_clone = "deny"
large_enum_variant = "deny"
needless_collect = "deny"
```

`clippy.toml`: `allow-unwrap-in-tests = true`, `allow-expect-in-tests = true`. Lint suppressions use
`#[expect(…, reason = "…")]`, never `allow`.

**Tests**
- Names follow the pattern `<unit>_should_<behavior>_when_<state>`, with one claim per test.
- `pane_tree`, `keymap` parsing and `keys` encoding are pure unit tests.
- Process tests are integration tests (§4.8).

**Gate:** `cargo clippy --workspace --all-targets --locked -- -D warnings` plus `cargo test --workspace`. `rustfmt.toml` sets `group_imports = "StdExternalCrate"` and `imports_granularity = "Crate"`, which need `cargo +nightly fmt`.

**Docs:** `#![deny(missing_docs)]` on `kil-term`. Comments explain *why* (SAFETY, F-numbers), not *what*.

---

## 11. Packaging, signing and notarization (your Apple Developer account)

**What's on this machine:** a valid **`Developer ID Application` identity, team `43VLF3KTFZ`**, in the login keychain, and an App Store Connect API key for notarization.

**Notary credentials**, either of (neither goes in the repo):
```sh
# An App Store Connect API key (Users and Access → Integrations): key file, key id, issuer id.
KIL_NOTARY_KEY=~/.appstoreconnect/private_keys/AuthKey_XXXX.p8 KIL_NOTARY_KEY_ID=XXXX \
  KIL_NOTARY_ISSUER=<issuer-uuid> scripts/release.sh
# Or a keychain profile, stored once (prompts for an app-specific password):
xcrun notarytool store-credentials kil-notary --apple-id <your-apple-id> --team-id 43VLF3KTFZ
```
With neither set, the script uses the `asc` CLI's key from the keychain when `asc` is installed (`asc notarization submit --wait`), which is how 0.0.1 was notarized.

**Why Developer ID and not the Mac App Store:** a terminal has to exec arbitrary binaries anywhere on disk, and the App Sandbox (which the App Store requires) forbids that. Distribution is a notarized Developer ID DMG.

**Releases are built locally and committed; there is no CI.** `scripts/release.sh` builds arm64 only. It writes `releases/Kil-arm64.dmg` and its `.sha256`, both tracked by git. Each release replaces the last, so the download link always gives the latest; the version is internal (Info.plist, commit messages). README links the file directly (`github.com/timwuhaotian/kil/raw/main/releases/Kil-arm64.dmg`), and the script stops before building if that link is missing:
1. `bundle.sh release`, then `codesign --options runtime --timestamp --entitlements packaging/kil.entitlements` with the Developer ID identity, then `codesign --verify --strict`.
2. The app is zipped, notarized and stapled first, so a copy dragged out of the DMG carries its own ticket and opens offline.
3. A DMG of the stapled app plus an `Applications` link is created, signed, notarized and stapled.
4. `spctl` assesses the app (`execute`) and the DMG (`open`). The script fails unless Apple returns `Accepted`.

**Info.plist**
- `CFBundleIdentifier`: choose a reverse-DNS id you control.
- `CFBundleExecutable=kil`.
- `LSMinimumSystemVersion=14.0`. Only 27.0 is tested.
- `NSHighResolutionCapable`.
- `LSApplicationCategoryType=public.app-category.developer-tools`.
- `NSSupportsSuddenTermination=false` and `NSSupportsAutomaticTermination=false` (§4.6).
- `NSAppSleepDisabled=true`.
- Usage descriptions: `NSDesktopFolderUsageDescription`, `NSDocumentsFolderUsageDescription`, `NSDownloadsFolderUsageDescription`, `NSRemovableVolumesUsageDescription`, `NSNetworkVolumesUsageDescription`, `NSCameraUsageDescription`, `NSMicrophoneUsageDescription`, `NSAppleEventsUsageDescription`.

kil is the TCC *responsible process* for every CLI it runs. Without these keys, a child that touches
`~/Desktop` or the camera is denied or killed.

**Entitlements (hardened runtime):** `com.apple.security.device.camera`,
`com.apple.security.device.audio-input`, `com.apple.security.automation.apple-events`. No JIT, no
dyld environment, no library-validation exemptions. Verify the TCC behavior in M6 by running
`ls ~/Desktop` and `ffmpeg -f avfoundation -list_devices true -i ""` from a fresh install.

---

## 12. Milestones

Each milestone ends on a proof, not on a feature count.

| M | Deliverable | Exit proof |
|---|---|---|
| **M0** (½ d) | Workspace, lints, `rustfmt`/`clippy` config, and a GPUI window with transparent titlebar and an empty sidebar. | clippy `-D warnings` clean; the window opens. |
| **M1** (3 d) | **`kil-term`:** `spawn.rs`, `Pty` (alacritty traits, kqueue exit, close-first `Drop`), `reaper.rs`, `proc_info.rs`, `Session`, `EventProxy` (coalesced wakeups, inline `PtyWrite`), `Frame` snapshot, memory ledger. No UI. | Every §4.8 test passes. Churn of 500 returns to baseline. Spawn latency is logged. |
| **M2** (3 d) | A single pane: `TerminalElement`, `keys.rs`, IME, selection, copy, bracketed paste, wheel scroll and alternate scroll, cursor blink, exit banner. | The A9 checklist passes. Idle CPU is 0. Throughput is at least Terminal.app's. |
| **M3** (2 d) | Tabs and sidebar: titles, cwd, busy and activity dots, cwd inheritance, close-confirm, quit flow. Motion for row insert/remove, the sliding pill and tab cross-fade. | A2, A7 and A4 (tab/window part) pass. Quit leaves 0 session processes. |
| **M4** (2 d) | `PaneTree` (pure, tested), split, close, focus navigation, divider drag, the resize policy, split and close animations, cached pane views. | A3 (one WINCH per layout change), A6, and 120 fps with 4 streaming panes. |
| **M5** (1 d) | Keymap and config: defaults, TOML overrides, error banner, reload on focus. Profile-driven render optimizations, only if samply says so. | A8 passes. Rebinding the split keys works. |
| **M6** (1–2 d) | Memory HUD, Info.plist, entitlements, `release.sh`, notarization. | A10–A13: `spctl` accepted, stapled, TCC prompts work on a fresh install. |

---

## 13. Risks and triggers

| Risk | Mitigation now | Trigger to change course |
|---|---|---|
| GPUI API churn | Exact pin `=0.2.2`. GPUI is used only in the `kil` crate. | Upgrade deliberately, one bump per PR. |
| Fork latency of a large GUI process | Spawn on the spawner thread, with the animation masking it. | p95 spawn > 10 ms in the app → switch to `posix_spawn(SETSID\|CLOEXEC_DEFAULT\|SETSIGDEF\|SETSIGMASK)` plus a tiny `kil-exec` helper (TIOCSCTTY, then exec). posix_spawn measured 0.34 ms. |
| Allocator retains freed scrollback | Line budget, ledger, HUD (§5). | Total scrollback > 1 GiB in real use → add a global cap that trims the least-recently-viewed panes' history. |
| One thread per session | Documented cost. | Regularly > 100 panes → a shared kqueue I/O thread with a custom loop. |
| `AnyView::cached` doesn't isolate panes | Count prepaints in M4. | Siblings re-prepaint → per-row shaped-line cache (§5.3). |
| GPUI's display link runs whenever the window is visible | Measured floor: 0.35% idle CPU (§6.4). | Idle CPU matters more than the GPUI pin → patch GPUI to stop the link when no frame is requested, and restart it on `request_frame`. |
| Re-shaping every run each frame (§5.3) | UI thread ~0.5 ms per frame while streaming. | Frame drops with 4 streaming panes at 120 fps → per-row shaped-line cache keyed by row content. |
| Residual TOCTOU on `killpg` of a non-child group | Identity re-check right before the signal. | None needed: by default we never kill non-child groups. |
| TCC and App Nap behavior on macOS 27 | Info.plist keys and entitlements. | Fails the M6 fresh-install check → adjust the keys and entitlements. |
| Zed GPL code contamination | Write from specs; never paste from `zed/crates/terminal*`. | |

---

## 14. The session server (jobs that outlive the window)

Status: **S1–S3 built; S4 partly** (§14.8). Without it, §4.9 reopens the layout and the screens, but each job starts again. The server makes the job itself continue: vim keeps its unsaved buffer, ssh stays connected, and a build keeps going.

### 14.1 Outcome

| # | Behavior | Proof |
|---|----------|-------|
| A19 | `cmd-q`, the window's close button, or a crash or `kill -9` of the app leaves every pane's process running. The next launch puts each pane back on its live process with the same screen (primary and alternate), scrollback, cursor, title and input modes. Typing continues where it left off. Output written while no window was open is in the scrollback, and no program blocked on its output meanwhile. | `server` integration tests (§14.7) + manual: `vim` with unsaved edits, `ssh`, `npm run dev`. |
| A20 | Closing a pane or tab still ends its process (hangup, then the reaper, as in §4.5). **Kil ▸ Quit and Stop All Jobs** ends every process, then quits: today's Quit. If jobs are running, `cmd-q` asks with three buttons: Quit (the jobs keep running), Quit and Stop Jobs, and Cancel. | `workspace` GPUI tests + manual. |
| A21 | After a logout or restart, or if the server dies, the next launch falls back to §4.9: the layout and screens come back, and the jobs run again. | `server` test (kill the server) + manual. |

**Not possible:** continuing across a logout or a restart. macOS ends every user process then, and it has no process checkpointing (no CRIU equivalent), so a process's memory can't be kept. §4.9 is the fallback for these cases.

**Non-goals:** several apps or windows attached to one session at once, remote attach, a `tmux`-like CLI, and resuming a pane on another machine.

### 14.2 Why a server, and what it changes

Today the app owns every PTY and child (§3, §4). When the app dies, the kernel closes the masters and the jobs die. F15 made sure of that on purpose. A job can outlive the window only if something that outlives the window holds its PTY master. That is a **session server**: the same binary run as `kil --server`. It owns every `OwnedChild`, every PTY master and the authoritative terminal state, and the app becomes its client. iTerm2 (session restoration), tmux and zellij work the same way.

The invariants of §4 move to the server unchanged: spawn hygiene, ownership-only signalling, close-the-master-first teardown, and the reaper. What changes is which process is the owner:

| Invariant | Today | With the server |
|---|---|---|
| Who owns children and PTY masters | the app | the server. **The app never holds a PTY fd.** |
| App crashes or is killed | jobs hang up and die (F15) | jobs continue; they are detached |
| Server crashes or is killed | — | jobs hang up and die. F15 applies to the server, and `crash.rs` moves to it. |
| `cmd-q` | stops every job | detaches; *Quit and Stop All Jobs* stops them |
| Close a pane or tab | stops its job | the same, through the server |
| Last session closes | — | the server exits once no session and no client remain for 5 s |

### 14.3 Processes and lifecycle

- **Starting.** At launch the app connects to `~/Library/Application Support/kil/server.sock`. If nothing answers, it runs `kil --server <dir>` with the same hygiene as a shell (§4.2: `setsid`, an empty signal mask, default dispositions, a CLOEXEC sweep), with stderr in `server.log`. The app waits ≤ 3 s for the socket. Two launches racing to start a server are serialized by `flock` on `server.lock`, and the loser exits, leaving the winner. If no server can be started, a server on a thread of the app stands in, and a notice says that jobs will end with Kil.
- **No zombie, no ownership by the app.** `kil --server` forks first thing, while it is still single-threaded, and the parent exits. The app reaps that parent at once. The server itself is launchd's child from the start, so it never needs reaping by the app, and nothing in the app could kill it.
- **Handshake.** The first frame on every connection is `Hello { protocol }`. The protocol is versioned apart from the app. It is 2 now (0.0.2), and protocol 1 was 0.0.1's. Both sides speak down to `MIN_PROTOCOL` = 1:
  - A server accepts any client protocol from `MIN_PROTOCOL` up to its own, and answers with its own.
  - A protocol-1 server hangs up on any other hello, after answering it. The client then connects again in protocol 1. So an app upgraded while 0.0.1's server still runs keeps its jobs. It only loses palette updates, because protocol 2 added the `Palette` frame and a palette at the end of `Spawn`, which a protocol-1 server ignores (`client_should_speak_down_to_a_protocol_1_server`).
  - **Open, before `MIN_PROTOCOL` is ever raised:** the hand-over. The app would take a §4.9 snapshot from the old server, stop its sessions, start the new server and reopen by re-running. Without it, an old server's jobs would keep running unseen.
- **Detach.** `cmd-q` and the close button save the layout (§4.9), close the connections and exit. The sessions keep running.
- **Reattach.** On launch the app asks the server for its sessions. Every pane in `session.toml` whose id the server still has is **attached**. Any other pane falls back to §4.9 (replay its screen, re-run its job). A session the server has but the layout doesn't is opened in an extra tab, so a running job is never left unseen.
- **Losing the server.** Each pane's connection ends: its program ended, as far as the app can tell, so the pane shows the exit banner. The next launch finds no live sessions and reopens every pane through §4.9, as after a restart.
- **Security.** The directory is `0700` and the socket `0600`. Each accepted connection is checked with `getpeereid` against the server's uid, and a mismatch is closed before any frame is read. The socket path is at most 104 bytes (`sun_path`). If the directory makes it longer, the socket moves to `$TMPDIR/kil-<uid>/` (exercised by the manual runs, whose `$HOME` was long).

### 14.4 Data path

There are two Terms per pane, fed the same bytes. **The server's** is the truth: it parses everything the program writes, whether or not a window is open, answers terminal queries, and is what a reattach is rebuilt from. **The app's** is a mirror: it is what the existing renderer, selection and scrollback code read, without change (`snapshot_into`, `is_dirty`, `take_dirty`).

```
program ─► PTY master ─► server pane loop: read ─► lock Term ─► parse ─► forward to client ─► unlock
                                                                              │ (attached only)
app pane loop: read socket ─► parse into mirror Term ─► Wakeup (as today)  ◄──┘
keys ─► app ─► socket ─► server ─► PTY master
```

- **The server needs its own pane loop, not alacritty's `EventLoop`.** alacritty reads the PTY *before* it takes the terminal lock (`event_loop.rs`, `pty_read`). If the output were copied to the client at read time, an attach could land between that read and the parse, and those bytes would be lost or doubled. The server loop reads, parses and forwards under one lock, and an attach (render the state, then install the client) takes the same lock, so the hand-off is exact. The loop is small (read, write queue, resize, child exit through `Pty`'s kqueue), and it is the "custom loop" §13 already names as a trigger.
- **Only the server answers.** Both Terms see every query (DA, CPR, OSC 10/11/4). The mirror's `PtyWrite`, size and color requests are dropped (`Role::Mirror`), and the server answers alone. Colors come from the program's own overrides, then the session's palette. That is the app's theme, sent in `Spawn` and again in a `Palette` frame when it changes or a client attaches (protocol 2). A query asked while no window is open is still answered, with the last theme.
- **One connection per pane, plus one control connection.** A pane connection starts with `Spawn` or `Attach`. After that it carries output and exit status (server to app) and input, resize and `Close` (app to server). Control carries `List` and `StopAll`. Frames are `u32` length + `u8` kind + payload, hand-written little-endian: no new dependency. Output is encoded and parsed in place, without a copy per frame.
- **Flow control.** The server writes to a client without blocking. While more than 1 MiB waits for it, the server stops reading the PTY, so the program waits, as with any terminal that falls behind. A client that takes nothing for 2 s while output waits (the app is stopped or hung) is dropped. Its pane is detached, the PTY is read again, and the app reattaches later with the full state. A first version dropped any client 4 MiB behind instead, and a healthy client fell that far behind within milliseconds of `cat`ting a large file.
- **Cost (measured, release).** `cat` of 27 MB takes 191–195 ms through the server against 178–200 ms locally. Keypress echo through `cat` has a p50 of 79 µs against 87 µs locally, and the p95 is ~170 µs for both. A new session's round trip (connect, spawn, attach) is ~1.7 ms. Scrollback is held twice, by the server and by the mirror, so A11's per-pane formula doubles.

### 14.5 Rebuilding a screen on attach

On `Attach`, the server takes its Term lock, renders a state stream, installs the connection and releases the lock. The mirror parses the stream from a fresh Term. The stream is:
The mirror is created at the server Term's size (sent in `Attached`), so the rows split between history and screen exactly as they do in the server (`state.rs`).
1. The primary screen: every history and screen row, wrapped rows joined, then the primary cursor.
2. If the alternate screen is active: `CSI ? 1049 h`, then every row at an absolute position, then the alt cursor.
   - alacritty exposes the inactive grid only through `swap_alt`. Leaving alt is a plain swap. Entering it resets the alt grid and copies the cursor, so the alt rows, cursor and saved cursor are cloned first and put back into the grid after the second swap (`grid_mut`), exactly.
   - alacritty doesn't expose the scroll region either. So after an attach with the alternate screen up, the server shrinks the PTY by a row and restores it 30 ms later. The program redraws and sets its region again, in both Terms alike.
3. The cursor's position, pending wrap and SGR pen, and its DECSCUSR shape.
4. Modes, each set or reset explicitly: DECCKM, DECKPAM, bracketed paste, mouse reporting 1000/1002/1003 with encodings 1005/1006, focus reporting 1004, alternate scroll 1007, DECAWM, cursor visibility, insert mode, LNM.
5. Colors the program redefined (OSC 4, 10, 11, 12), and the title (OSC 2).

Left out: charsets, origin mode, custom tab stops and hyperlinks. They are rare, and the next redraw usually restores them.

### 14.6 App side

- `Session` keeps its API and gains a second kind. A *local* session has its PTY in this process, run by `pane_loop.rs`; that is what the server runs, and what the kil-term tests use. A *remote* session (`Client::spawn`, `Client::attach`) has a mirror Term fed by `remote.rs`. Calls that read the terminal go to the Term either way. `write`, `resize` and `close` become frames for a remote one. `foreground()`, `busy_job`, `job_args` and `shell_cwd` work unchanged, because the server sends each session's `ProcIdentity` and `TtyId`, and `proc_pidinfo` on another process of the same user needs no ownership.
- Dropping a remote session only detaches it. `Session::close` ends a session of either kind, and `TerminalPane::shutdown` uses it, so closing a pane or tab still ends its program.
- `spawn.rs`, `reaper.rs`, `proc_info.rs`, `snapshot.rs` and `transcript.rs` are unchanged. `pty.rs` lost its alacritty traits.
- §4.9 stays as is: `session.toml` and the screens are still saved every 2 s. They are the fallback for a logout, a restart or a lost server, and the layout is needed for an attach as well.

### 14.7 Tests (real processes, `kil-term/tests/server.rs`, the server on a thread)

| Test | Asserts |
|---|---|
| `detach_should_keep_the_job_running` | A client spawns `sleep`, then drops its session. The job is alive 2 s later and listed by the server. `stop_all` ends it. |
| `attach_should_rebuild_the_primary_screen_with_output_from_while_detached` | Output from before detaching, *and from while detached*, is in the reattached mirror, in order. |
| `attach_should_rebuild_the_alternate_screen_and_modes` | A program enters 1049, draws, and sets DECCKM, bracketed paste and mouse 1002+1006. A second client's mirror has the same text and `InputMode` as the first. |
| `detached_output_should_not_block_the_program` | 200k lines with no client: the program finishes, and the last line is there on attach. |
| `queries_should_be_answered_once_by_the_server` | A CPR gets exactly one reply, and OSC 11 is answered with the theme's background. |
| `a_client_that_stops_reading_should_be_dropped_not_stall_the_program` | A raw client attaches and never reads. The program gets far past the 1 MiB high-water mark, and the client's connection reaches EOF. |
| `child_exit_should_reach_the_client_with_its_status` | `exit 3` arrives as `ChildExited(3)`, and the server forgets the session. |
| `close_should_end_the_session_in_the_server` | `Session::close` reaps the job and empties the server. |
| `spawn_should_paint_the_replay_before_the_program_output` | §4.9's replay works through the server. |
| `the_socket_should_be_private_and_vanish_when_idle` | The socket is `0600`, and an idle server removes it within 10 s. |
| `crash_of_the_session_server_should_hang_up_its_jobs` (`crash.rs`) | An owner process hosts a server, starts a job through a client, detaches and aborts. The job is gone within 2 s (F15). |
| GPUI: `quit_should_leave_the_sessions_running_in_the_server`, `reopening_should_go_back_on_the_sessions_the_server_kept`, `launch_should_open_a_tab_for_a_session_no_saved_tab_claims` | Quit sends no `Close`. The next workspace attaches by id to the same PID. A session no saved tab claims gets its own tab. |

Not automated: a peer with another uid (needs a second user), and the three-button quit sheet (manual).

### 14.8 Milestones

| M | Deliverable | Exit proof |
|---|---|---|
| **S1** ✅ | `kil-term`: `pane_loop.rs` replaces alacritty's `EventLoop` for sessions (same tests, still in-process), then `kil --server` with the protocol, remote sessions, `flock` and `getpeereid`. | All §4.8 tests pass through the pane loop, plus §14.7. |
| **S2** ✅ | Attach state stream (§14.5), with the alt screen and modes. | `attach_should_rebuild_*`, `queries_should_be_answered_once_by_the_server`, `state.rs` unit tests. |
| **S3** ✅ | App: connect or spawn the server, attach on launch, quit detaches, *Quit and Stop All Jobs*, close pane or tab, the extra tab for unknown sessions. | GPUI tests, and the real binary by hand: quit, `kill -9` and a killed server (see Build status). |
| **S4** (partly) | Measured: throughput and echo latency (§14.4). **Open:** memory and the server's idle wakeups (one per second from its idle check), the TCC fresh-install check (§11) for a server that outlived its app, a hands-on pass with vim, ssh and a dev server, and the protocol hand-over (§14.3). | `ls ~/Desktop` from a reattached pane works on a fresh install. |

### 14.9 Risks and triggers

| Risk | Mitigation now | Trigger to change course |
|---|---|---|
| TCC: the server outlives the app it was spawned by. Its children may lose the app's privacy grants. | The server is the same signed binary, spawned by the app, so it inherits the app as responsible process. | The S4 fresh-install check fails → register the server as an `SMAppService` agent inside the bundle, so launchd starts it with the bundle's identity. |
| The `swap_alt` dance in §14.5 has a side effect | Rows are rendered first and painted back, under the server lock. | A §14.7 alt-screen test shows drift → use the `SIGWINCH` redraw fallback instead. |
| Scrollback is held twice | Lazy growth: a pane pays only for history it has. | Total scrollback > 1 GiB → the mirror keeps the screen plus 1,000 lines and pages older history from the server on scroll. |
| Two parses and one more socket per output chunk | Measured: on par with a local session (§14.4). | Throughput below Terminal.app → the server passes the master fd to the attached app (`SCM_RIGHTS`) and parses only while detached. |
| Jobs keep running after a quit and are forgotten | The quit sheet names them when any are busy. | Users report surprise → a menu-bar or Dock badge while detached jobs run. |
| A protocol change kills jobs once | The protocol is versioned separately from the app, and changes rarely. | Frequent changes → a negotiated minor version with additive frames. |
