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
- **M2–M5 are implemented and lint/test-clean** (66 tests). They still need hands-on UI verification: the A9 checklist, IME, and animation smoothness.
- **A14–A16** (row menu and rename, Settings window, double-click for a new tab) are implemented. Headless GPUI tests drive the clicks, input and keys. They still need a hands-on pass with a real IME.
- A dev bundle is built with `scripts/bundle.sh`. The two-tone icon source is `packaging/icon.svg`.
- Release signing and notarization (the rest of M6) is not done yet.

---

## 1. Outcome

### Acceptance (v1): observable behavior

| # | Behavior | Proof |
|---|----------|-------|
| A1 | Launch opens one window: a left sidebar with one tab, and one pane running `$SHELL` as a login shell in `$HOME`. | Manual. Time to first prompt ≤ 200 ms (log timestamp). |
| A2 | `cmd-t` opens a tab in the focused pane's cwd. Its sidebar row animates in, and focus moves to it. | Manual + `pwd`. |
| A3 | `cmd-d` splits right and `cmd-shift-d` splits down. The new pane inherits cwd, and the split animates. Each pane gets **one** `SIGWINCH` per layout change, not one per frame. | `trap 'echo WINCH' WINCH` counter. |
| A4 | `cmd-w` or shell `exit 0` closes the pane with an animation. The last pane closes its tab. The last tab closes the window, which quits the app. | Manual. |
| A5 | A non-zero exit or a signal death keeps the pane open with a banner showing `exited 1` or `killed: SIGKILL`. Any key closes it. | `exit 3`. |
| A6 | Focus moves between panes with `cmd-alt-arrows` or a click. The focused pane is the bright one, and the others are dimmed to about half. Dividers can be dragged to resize. | Manual + `workspace` GPUI test (click focuses). |
| A7 | Switch tabs by clicking, `cmd-1…9`, or `cmd-shift-[ / ]`. | Manual. |
| A8 | `~/.config/kil/keymap.toml` overrides any default, including the split keys. Invalid entries appear in a banner with the key and the reason, and valid entries still apply. The file is reloaded when the window regains focus. | Rebind the split keys. |
| A9 | Terminal correctness: vim, htop, less, tmux, 256-color and truecolor, CJK wide characters aligned to the grid, **Chinese IME preedit at the cursor**, selection, copy, bracketed paste, trackpad scrollback. | Manual checklist. |
| A10 | **Process hygiene.** Child fds are exactly 0, 1, 2 plus whatever the shell opens. Signal mask and ignore set are empty. Closing never blocks the UI thread. The session process is reaped within ≤ 3 s even if it ignores SIGHUP or is blocked writing. There are no zombies. | M1 integration tests (§4.8). |
| A11 | **Memory.** Per-pane overhead without scrollback is ≤ 2 MB. Scrollback cost matches the formula in §5 within ±10%. A churn of 500 open/close cycles returns threads, fds, child count and live `TerminalPane` count to baseline. | Churn test plus `footprint`. |
| A12 | **Performance.** Idle CPU is 0.0% with blink off and < 1% with blink on. Animations run at 120 fps with 4 panes streaming output (Metal HUD). Throughput is at least Terminal.app's on the same file. Keypress-to-photon is ≤ 1 frame after the PTY echo. | §6.4 |
| A13 | Release build is signed with **Developer ID Application (team 43VLF3KTFZ)**, notarized and stapled, and `spctl -a -vv` accepts it. | §11 |
| A14 | Right-clicking a sidebar row opens a menu with **Rename Tab…**, **Reset Name** (shown only when the tab has a name) and **Close Tab**. Rename edits the title in place, and IME input works. Enter or a click elsewhere keeps the edit, and Escape cancels it. An empty name goes back to the pane's title. The name also becomes the window title. | `workspace` GPUI tests + manual Pinyin. |
| A15 | **Kil ▸ Settings…** (`cmd-,`) opens a separate Settings window, or brings the open one forward. It edits font family and size, scrollback, Option as Meta and cursor blink. Each change is written to `config.toml` and applied to the open window at once. The Kil menu also has Services, Hide, Hide Others and Show All. | `settings_window` / `config` tests + manual. |
| A16 | Double-clicking empty space in the sidebar opens a tab, the same as `cmd-t`. Double-clicking a row does not. | `workspace` GPUI tests. |

### Non-goals (v1)
These are left out and can be added later:
- Multiple windows and session restore.
- SSH and tmux control mode.
- Ligatures, image protocols (sixel, kitty) and the kitty keyboard protocol.
- OSC 133 shell integration, OSC 8 hyperlinks, URL click.
- Scrollback search and tab drag-reorder.
- Themes beyond one built-in dark theme, a keymap editor in the Settings window (it links to `keymap.toml` instead), and plugins.
- Saving tab names across launches (there is no session restore).
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
│  │  ├─ src/pty.rs           Pty: EventedReadWrite + EventedPty + OnResize + Drop
│  │  ├─ src/spawn.rs         openpty, env, Command + pre_exec hygiene
│  │  ├─ src/reaper.rs        one thread: adopts closed children, escalates, reaps
│  │  ├─ src/proc_info.rs     ProcIdentity, TtyId, foreground(), cwd()
│  │  ├─ src/snapshot.rs      Frame / RenderCell (UI-agnostic render copy)
│  │  ├─ src/error.rs         SpawnError, SessionError (thiserror)
│  │  ├─ src/sys/darwin.rs    the ONLY module with `unsafe` (FFI), each block // SAFETY:
│  │  └─ tests/               lifecycle, hygiene, hostile children, churn (§4.8)
│  └─ kil/                    GPUI app
│     ├─ src/main.rs          anyhow at the edge only
│     ├─ src/workspace.rs     window root: tabs, active tab, quit/close-confirm flow
│     ├─ src/pane_tree.rs     PURE generic PaneTree<T> (no GPUI types) + unit tests
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
└─ scripts/release.sh         build → bundle → sign → notarize → staple   (M6)
```

The split has a purpose. `kil-term` holds every invariant about processes and memory, and it compiles
and tests in seconds without GPUI. The app crate only renders and routes input.

### Threads and data flow

```
main thread (GPUI/AppKit) ─ frames on display link only when notified; input; actions
   ▲ futures::mpsc (Wakeup coalesced to ≤1 in flight; Title, Bell, ChildExit, Clipboard)
   │
PTY thread × N  (alacritty EventLoop, one per session)
   read master → vte → Term (Arc<FairMutex>) ; write input ; NOTE_EXIT on kqueue ; answers PtyWrite inline
spawner × 1     fork+exec off the UI thread (fork of a large GUI process costs ms)
reaper  × 1     adopts closed children → NOTE_EXIT or deadline → SIGKILL → reap
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
| F8 | Child signal ignore set and mask are empty, because std resets the mask and SIGPIPE. Spawn took 1.3–3.9 ms in a small process. | |
| F9 | A churn of 40 login sessions took 2.1 s. The worst drop was 3.9 ms. fds, threads and children returned to baseline, and 0 session processes survived. | |
| F10 | libc's `openpty(3)` resolves the slave through `ptsname(3)`'s static buffer, so concurrent spawns can race and even open each other's slave. | kil uses `posix_openpt` + `TIOCPTYGNAME` + `open(O_CLOEXEC)`, which is thread-safe, and the slave is close-on-exec atomically. |
| F11 | Under concurrent spawns on macOS 27, `posix_openpt` occasionally fails with errno −6, XNU's internal `EREDRIVEOPEN` leaking to user space (reproduced in about 1 of 3 test runs). | Bounded retry (16 attempts) on −6 and `EINTR`. 15 consecutive full test runs then passed. |
| F12 | An unbundled binary started from another terminal inherits that terminal's `__CFBundleIdentifier`, and AppKit then names and badges the process as that app (it showed up as "Otty"). | `main` clears the variable when not running inside a `.app`, and child shells no longer inherit `OTTY_*` or `CW_TERM`. |

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

### 4.4 Exit detection and reaping: no signal handlers anywhere

- `Pty::register` adds the master fd and a kqueue `Process(child, Exit)` filter to alacritty's own `Poller`, under `PTY_CHILD_EVENT_TOKEN`.
- `next_child_event` calls `try_wait()` (reap) and returns `ChildEvent::Exited(status)`.
- There is no process-wide SIGCHLD handler. With N panes, alacritty's signal-pipe design wakes all N loops on every child exit, and this design avoids that.

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
- **Quit (`cmd-q`).** If any session is busy, a confirm sheet lists the processes (for example "vim, cargo"). Then every session closes as in §4.5 and `reaper.shutdown(1s)` runs. Survivors get SIGKILL, are reaped, and the app exits.
- **Crash or abort.** The kernel closes every master, which hangs up every session (F4). Shells exit and launchd reaps them. Release builds use `panic = "abort"`: a half-alive pane is worse than a clean crash, and the kernel is the safety net.
- **Info.plist.** `NSSupportsSuddenTermination = false` and `NSSupportsAutomaticTermination = false`, so macOS never skips this flow. `NSAppSleepDisabled = true`: if App Nap throttles the PTY reader, child builds block on a full PTY. Verify the App Nap behavior in M6.

### 4.7 Close confirmation

This applies to closing a pane, tab, window or the app when any affected session is busy (§4.3). The
sheet lists the foreground process names. Idle shells close without asking.

### 4.8 Invariants and how they're tested (`kil-term/tests/`, real processes)

| Test (name says the claim) | Asserts |
|---|---|
| `spawn_should_give_child_only_stdio_fds_when_parent_has_leaky_fd` | Child fd set is ⊆ {0,1,2} ∪ the shell's own fds (F5). |
| `spawn_should_reset_signal_mask_and_ignores` | `ps -o sigignore=,sigmask=` of `sh -c` is `0` (F8). |
| `close_should_reap_within_grace_when_child_ignores_sighup` | Reaped ≤ 3.5 s with a SIGKILL status, and the calling thread isn't blocked (F2, F4). |
| `close_should_not_deadlock_when_child_blocks_on_undrained_output` | Reaped ≤ 100 ms (F3). |
| `close_should_leave_nohup_job_running_reparented` | Semantics are preserved. |
| `natural_exit_should_report_status` | `exit 3` gives `ChildExit(3)`. |
| `foreground_should_report_job_and_reject_foreign_tty` | Busy detection plus the identity check (F7). |
| `identity_should_reject_recycled_pid` | Start-time mismatch returns `None`. |
| `churn_500_should_return_to_baseline` | Own fds, threads (`PROC_PIDTASKINFO`) and children (`proc_listchildpids`) are unchanged, and there are no zombies (F9). |
| `no_signal_handlers_installed` | `sigaction(SIGCHLD)` is still `SIG_DFL` after N sessions. |

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
- Row text is built in one reused `String`. Allocations per frame are **O(changed rows)**, because GPUI needs a `SharedString` per shaped line. A per-row `(hash, ShapedLine)` cache is added **only if** samply shows shaping or allocation above 10% of frame time (M5).

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

- **prepaint:** compute the cell size from font metrics (cached per font config), then snapshot.
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

Always measure with `--release`. Release profile: `opt-level=3`, `lto="thin"`, `codegen-units=1`,
`panic="abort"`, `debug="line-tables-only"` (keeps samply symbols).

---

## 7. UI and motion

**Window**
- `TitlebarOptions { appears_transparent: true, traffic_light_position: (14, 18) }`, so the sidebar runs to the top edge.
- `WindowBackgroundAppearance::Blurred` behind the sidebar. The terminal area stays opaque for readability and to avoid overdraw.

**Sidebar** (220 px)
- 44 px rows with a title (13 pt medium) and the cwd (11 pt, middle-truncated).
- An accent dot when the tab is **busy**, a dim dot for unseen output in a background tab, and an `×` on hover.
- A footer `+ New Tab` button. Double-clicking empty sidebar space also opens a tab.
- Right-click on a row opens a menu: Rename Tab…, Reset Name, Close Tab. A rename edits the title in place in a `TextField`. The user's name overrides the pane's title in the row, in the closing ghost and in the window title.
- The selection highlight is a rounded 8 px pill that **slides** between rows.

**Panes:** 1 px dividers with a 6 px hit area. When a tab has more than one pane, the others are dimmed by a 50% near-black overlay. Their text drops to about half brightness, and their cursor is hollow while the focused one is solid. That is the focus cue. A lighter dim doesn't work: on the near-black background it moves the background by only ~2/255.

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
`remove` (which collapses the parent), `layout(rect) -> impl Iterator<(&T, Rect)>`, and a geometric
`neighbor(target, dir)`. It is pure and generic, so it is unit-tested with `T = u32`.

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
| `cmd-d` | `split_right` |
| `cmd-shift-d` | `split_down` |
| `cmd-w` | `close_pane` |
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
"cmd-d"       = "split_down"
"cmd-shift-d" = "split_right"
"ctrl-a |"    = "split_right"   # multi-stroke; verify GPUI 0.2.2 sequence support in M5
"cmd-k"       = "unbind"
```

**Parsing**
- Commands come from `enum Command` with serde `snake_case`, so an unknown name is a typed error.
- Keys are checked with GPUI `Keystroke::parse`.
- Errors are collected as `Vec<KeymapError { key, reason }>` and shown in a banner. Valid entries still apply.
- The merged set is applied with `cx.clear_key_bindings()` and then `cx.bind_keys(defaults ⊕ user)`, in the `Workspace` key context. Bindings therefore win over terminal input, and unbound keys fall through.
- Text field editing keys (`TextField` context: arrows, home/end, backspace, delete, `cmd-a`, `cmd-v`, enter, escape) and the Settings window's `cmd-w` are fixed. They are bound after the merged set and can't be changed in `keymap.toml`.

**Fallthrough to the PTY** (`keys.rs`, written from xterm ctlseqs):
- App-cursor and app-keypad modes, `CSI 1;mods` modifier encoding, ctrl-letters, function keys.
- `option_as_meta` (default `true`) sends ESC + key. The tradeoff: Option-composed characters such as `é` need the setting off.
- Paste uses bracketed paste when that mode is set, and strips ESC from the pasted text.

**IME:** `TerminalPane` implements `EntityInputHandler`. Marked text is drawn at the cursor. Committed text goes to the PTY. `bounds_for_range` returns the cursor rect so the candidate window sits next to the cursor. The acceptance test is Pinyin input.

## 9. Config (`~/.config/kil/config.toml`)

```toml
font_family = "Menlo"
font_size = 13.0
scrollback_lines = 10000
option_as_meta = true
cursor_blink = true
```

The Settings window writes one key per change (`config::write_setting`). It replaces the line that assigns the key, or appends one, and keeps every other line, comments included. If the result would not parse, nothing is written. The file is written in place, not renamed over, so a symlinked dotfile stays a symlink. The terminal window then reloads through the same mtime path. Scrollback and cursor blink apply to new tabs and panes, because a blink timer for existing panes would break idle CPU (A12).

Both files are re-read when the window gains focus and the file's mtime has changed. There is no
watcher thread and no extra dependency.

---

## 10. Rust practice (Apollo handbook, applied)

**Errors**
- `kil-term` exposes `thiserror` enums: `SpawnError { OpenPty(#[source] io::Error), ShellNotExecutable(PathBuf), Fork(#[source] io::Error) }`, `SessionError`, and so on.
- `anyhow` appears only in `main.rs` for startup.
- Runtime failures become a pane banner (for example "Failed to start /bin/zsh: …"). They never panic.

**Unsafe**
- Allowed only in `kil-term/src/sys/darwin.rs`, via `#[expect(unsafe_code, reason = "Darwin FFI")]`. *As built*, there are two more scoped `expect`s: `Pty::register`, because alacritty's trait method is itself `unsafe`, and the first statement of `main`, because edition 2024 makes env mutation `unsafe`.
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

**What's on this machine:** a valid **`Developer ID Application` identity, team `43VLF3KTFZ`**, in the login keychain. `notarytool` is 1.1.2. No notary profile is stored yet.

**One-time step that you run** (the password goes to the Keychain, not to the repo or to me):
```sh
xcrun notarytool store-credentials kil-notary --apple-id <your-apple-id> --team-id 43VLF3KTFZ
# prompts for an app-specific password (account.apple.com → Sign-In and Security → App-Specific Passwords)
```
For CI, use an App Store Connect API key instead (`--key/--key-id/--issuer`).

**Why Developer ID and not the Mac App Store:** a terminal has to exec arbitrary binaries anywhere on disk, and the App Sandbox (which the App Store requires) forbids that. Distribution is notarized Developer ID, as a zip or dmg.

**`scripts/release.sh`** (M6). The build is arm64 only:
```sh
cargo build --release -p kil
APP=dist/Kil.app; rm -rf dist && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/kil "$APP/Contents/MacOS/kil"
cp packaging/Info.plist "$APP/Contents/"; cp packaging/AppIcon.icns "$APP/Contents/Resources/"
codesign --force --timestamp --options runtime \
  --entitlements packaging/kil.entitlements \
  --sign "Developer ID Application" "$APP"            # unique identity in keychain (team 43VLF3KTFZ)
codesign --verify --strict --verbose=2 "$APP"
ditto -c -k --keepParent "$APP" dist/Kil.zip
xcrun notarytool submit dist/Kil.zip --keychain-profile kil-notary --wait
xcrun stapler staple "$APP"
spctl -a -vv "$APP"                                    # expect: accepted, source=Notarized Developer ID
```

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
| Residual TOCTOU on `killpg` of a non-child group | Identity re-check right before the signal. | None needed: by default we never kill non-child groups. |
| TCC and App Nap behavior on macOS 27 | Info.plist keys and entitlements. | Fails the M6 fresh-install check → adjust the keys and entitlements. |
| Zed GPL code contamination | Write from specs; never paste from `zed/crates/terminal*`. | |
