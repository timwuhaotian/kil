# Kil vs other macOS terminals

Kil is a native macOS terminal (Rust, Metal rendering, 8.6 MB installed) whose
jobs keep running after it quits or crashes — reopen it and every pane is back
on its live process.
Choose Kil if you want the smallest full-featured terminal on macOS with
built-in tabs, split panes, and sessions that survive a quit.
Choose something else if you need Linux or Windows (Kil is macOS-only, Apple
silicon), a plugin ecosystem, or AI features — the table and notes below say
which one.

## At a glance

| | Kil | Ghostty | iTerm2 | Warp | Alacritty | kitty | WezTerm |
|---|---|---|---|---|---|---|---|
| Installed size | 8.6 MB | 65.0 MB | 202.9 MB | 913.7 MB | 14.7 MB | 253.0 MB | 266.1 MB |
| Language | Rust | Zig | Objective-C | Rust | Rust | C + Python | Rust |
| GPU rendering | Yes (Metal) | Yes | Yes (Metal) | Yes | Yes | Yes | Yes |
| Tabs | Yes (vertical) | Yes | Yes | Yes | No | Yes | Yes |
| Split panes | Yes (animated) | Yes | Yes | Yes | No | Yes | Yes |
| Sessions survive quit | Yes | No | No | No | No | No | No |
| Idle CPU | 0.00%¹ | — | — | — | — | — | — |
| Open source | Yes (MIT) | Yes (MIT) | Yes (GPL-2.0) | No | Yes (Apache-2.0) | Yes (GPL-3.0) | Yes (MIT) |
| Price | Free | Free | Free | Free / paid AI tiers | Free | Free | Free |

Size figures are the README's measured installs; see
[How the size numbers were measured](#how-the-size-numbers-were-measured).
"—" means we didn't measure it — idle CPU depends on build, config, and
machine, so we don't quote numbers we can't reproduce.
"Sessions survive quit" means processes keep running natively after ⌘Q or a
crash: Kil does this with its session server (`kil --server`, same binary);
the others end their jobs on quit unless you run them under tmux.

## Kil vs Ghostty

Both are fast, GPU-rendered, MIT-licensed modern terminals — and "Kil vs
Ghostty" is the comparison most people actually want.
Ghostty is cross-platform (macOS and Linux), written in Zig with a
platform-native UI, and extremely configurable; its 65.0 MB install is a
universal binary carrying Intel and Apple-silicon code.
Kil is macOS-only and Apple-silicon-only, which is part of how it fits in
8.6 MB, and it adds what Ghostty doesn't have: vertical tabs with job-status
marks and a session server, so ⌘Q and crashes only detach — the next launch
reattaches to the same live processes.

- Choose Kil if…
  - you want the smallest install with tabs, splits, and quit-proof sessions
    built in, no tmux required.
  - you live on one Mac (Apple silicon, macOS 14+) and prefer a native,
    opinionated UI over a config file.
- Choose Ghostty if…
  - you also work on Linux and want the same terminal everywhere.
  - you enjoy tuning hundreds of options and following a fast-moving project.

## Kil vs iTerm2

iTerm2 is the veteran everything gets compared against:
free, open source (GPL-2.0), Metal-rendered, with decades of configurability —
profiles, triggers, a Python API, and first-class tmux integration — at
202.9 MB installed.
Kil is the lightweight iTerm2 alternative for people who never touched most of
that: 8.6 MB, vertical tabs, animated splits, rebindable keys, and sessions
that survive quit and crash without setting up tmux.
What Kil can't match is depth — iTerm2's feature list took 20 years to grow,
and Kil is early and macOS-14+/Apple-silicon-only.

- Choose Kil if…
  - you want iTerm2's core (tabs, splits, GPU speed) at ~1/24th the installed
    size, with jobs that outlive the window by default.
  - you prefer zero-setup persistence over learning tmux integration.
- Choose iTerm2 if…
  - you rely on profiles, triggers, the Python API, or its tmux integration.
  - you need Intel Macs, older macOS releases, or twenty years of docs and
    Stack Overflow answers.

## Kil vs Warp

Warp and Kil sit at opposite ends of the 2026 terminal spectrum.
Warp is an AI-first, team-oriented terminal: agent workflows, cloud features,
and an account to match — free for the core terminal with paid AI tiers, at
913.7 MB installed, closed source.
Kil is local-only and deliberately small: 8.6 MB, MIT-licensed, no account, no
network calls except its opt-out update check — and unlike Warp, its jobs
survive a quit or a crash.
They agree on the basics (tabs, splits, GPU rendering); they disagree on what
a terminal is for.

- Choose Kil if…
  - you want a private, offline terminal with no account and nothing to sync.
  - install size and idle CPU matter to you more than AI assistance.
- Choose Warp if…
  - you want AI agents, command generation, and team sharing inside the
    terminal.
  - you work across macOS and Linux with one synced setup.

## Kil vs Alacritty

Alacritty is the only terminal near Kil's size — 14.7 MB vs Kil's 8.6 MB —
and the two share Rust and a GPU renderer (Kil even uses Alacritty's
`alacritty_terminal` crate for VT parsing).
The difference is philosophy: Alacritty has no tabs or split panes by design
and expects you to multiplex with tmux; Kil builds in vertical tabs, animated
split panes, and a session server, so persistence works with plain shells.
Alacritty runs on macOS, Linux, and Windows; Kil is macOS-only.

- Choose Kil if…
  - you want tabs, splits, and quit-proof sessions without running tmux.
  - you want a macOS-native app (sidebar, Settings window, notifications,
    self-update) rather than a config file.
- Choose Alacritty if…
  - you already live in tmux and want the most minimal cross-platform
    renderer for it.
  - you need Linux or Windows support.

## How the size numbers were measured

Copied from the [README's Size section](../README.md#size), which is the
source of truth (chart: `docs/size-light.svg`):

> Versions measured: Kil 0.0.10, Alacritty 0.17.0, Ghostty 1.3.1, iTerm2 3.7.6,
> Hyper 3.4.1, kitty 0.49.2, WezTerm 20240203, Tabby 1.0.238, Wave 0.14.5,
> Warp 0.2026.10.14.
> Kil, Hyper, Tabby and Wave are measured as their Apple silicon builds. The
> others ship one universal app for Intel and Apple silicon, which carries
> each binary twice. Alacritty is the only one near Kil's size, and it has no
> tabs or split panes.

In short: installed `.app` size on disk, measured when Kil was 0.0.10 —
Kil 8.6 MB, Alacritty 14.7 MB, Ghostty 65.0 MB, iTerm2 202.9 MB,
kitty 253.0 MB, WezTerm 266.1 MB, Warp 913.7 MB.
Versions move on, so treat the ranking as a snapshot and re-check the README
for the latest chart.

¹ Kil idle CPU: 0.00% with cursor blink off over 20 s, 0.67% with blink on in
a 1000×700 window (release build, M4 Pro, macOS 27.0) —
[DESIGN.md §6.4](../DESIGN.md).

## FAQ

### Is Kil the smallest macOS terminal?

Of the ten terminals measured for the README chart, yes: Kil 0.0.10 installs
at 8.6 MB, ahead of Alacritty at 14.7 MB — and Alacritty has no tabs or split
panes, while Kil has both.
The honest caveats: it's a snapshot (versions change), universal binaries
carry Intel code Kil doesn't ship, and "smallest" only counts what we
measured.
If install size is your deciding factor for the best macOS terminal in 2026,
Kil is currently the one to beat.

### Is Kil a tmux replacement?

For the most common reason people run tmux — surviving quits, crashes, and
restarts — yes: Kil's session server keeps every pane's process alive across
⌘Q and crashes, reattaching on the next launch, with no prefix keys to learn.
It is not a full tmux replacement: tmux still wins for attaching from another
machine over SSH, scripting layouts, and sharing sessions between users.
And if you love tmux, it runs fine inside Kil — persistence and multiplexing
stack.

### Can I migrate from iTerm2/Warp to Kil?

Yes, and there's little to migrate: Kil runs the same shells ($SHELL as a
login shell), so your dotfiles come along untouched.
Rebind keys to match your muscle memory in `~/.config/kil/keymap.toml`
(**Kil ▸ Keyboard Shortcuts…** opens it), pick one of the nine themes in
Settings, and set a default directory if you want one.
What you give up: iTerm2's profiles, triggers, and Python API, or Warp's AI
features and sync — if those are load-bearing, stay where you are.

## Sources

- Kil features, size chart, and measured versions: [README.md](../README.md),
  [DESIGN.md](../DESIGN.md) (performance numbers: §6.4)
- Ghostty: [github.com/ghostty-org/ghostty](https://github.com/ghostty-org/ghostty)
  (Zig, MIT, GPU-accelerated)
- iTerm2: [iterm2.com](https://iterm2.com)
  ([GPL-2.0](https://github.com/gnachman/iTerm2))
- Warp: [warp.dev/pricing](https://www.warp.dev/pricing) (Free / paid AI tiers)
- Alacritty: [alacritty.org](https://alacritty.org) (Rust, Apache-2.0)
- kitty: [sw.kovidgoyal.net/kitty](https://sw.kovidgoyal.net/kitty/) (GPL-3.0)
- WezTerm: [wezterm.org](https://wezterm.org) (Rust, MIT)
