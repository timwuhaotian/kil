# Kil FAQ

## What is Kil?

Kil is a native macOS terminal written in Rust. It renders with GPUI on Metal, parses VT sequences with `alacritty_terminal`, and keeps every job running in a session server so panes survive quit, crash, and update.

## How is Kil different from Ghostty?

Kil is 8.6 MB installed against Ghostty's 65.0 MB, and its session server reattaches panes to live processes after a relaunch. Ghostty is cross-platform and mature; Kil is Apple-silicon-only, macOS 14+, and early-stage.

## How is Kil different from iTerm2 and Warp?

Kil is far smaller: 8.6 MB vs iTerm2's 202.9 MB and Warp's 913.7 MB, per the README's measured install sizes. Kil also shows per-tab memory figures and pairs an iPhone remote over a direct TLS tunnel with no cloud account, while iTerm2 offers decades of features and Warp offers cloud-backed AI workflows.

## How is Kil different from Alacritty?

Kil (8.6 MB) is the only terminal near Alacritty's size (14.7 MB), but Kil adds vertical tabs, animated split panes, session persistence, and an iPhone remote, which Alacritty deliberately omits. Alacritty is cross-platform; Kil is macOS-only.

## How does session persistence work?

Every pane's process runs in `kil --server` (the same binary), and `⌘Q`, window close, or a crash only detaches the UI from it. According to DESIGN.md measurements, reattaching costs nothing visible: echo through the server has a p50 of 79 µs vs 87 µs locally, and a 27 MB `cat` takes 191–195 ms vs 178–200 ms locally.

## How big is Kil, and how much CPU does it use?

Kil installs at 8.6 MB, the smallest of the ten terminals measured in the README. According to DESIGN.md measurements, an idle window uses 0.00% CPU with blink off and 0.67% with blink on, and streaming animations target 120 fps with 4 panes at full speed.

## Is Kil free and open source?

Yes. Kil is MIT-licensed, and its two main dependencies, GPUI and `alacritty_terminal`, are Apache-2.0. The source, changelog, and signed release DMGs all live in the public GitHub repository.

## Which platforms does Kil support?

Apple silicon Macs running macOS 14 or later; it is tested on macOS 27. There is no Intel, Linux, or Windows build, and macOS-only is stated as "for now" while Kil is early-stage.

## How do I install and update Kil?

Download `Kil-arm64.dmg` from `releases/`, drag Kil to Applications, and verify with `shasum -a 256 -c Kil-arm64.dmg.sha256`. Kil then updates itself: it checks at launch and every 6 hours, verifies checksum, Developer ID signature, and Apple notarization, and asks before restarting, with jobs surviving the update.

## How does the iPhone remote (Agent on Edge) work?

Settings ▸ Agent on Edge shows a QR code that pairs an iPhone to the same session server through a TLS tunnel, so the phone's panes are the desk's panes. Ticking Public internet forwards a port via the user's own router (UPnP) with no cloud service involved, and the tab row shows a phone badge that turns solid while the phone types.

## Where are Kil's config files?

Keys live in `~/.config/kil/keymap.toml` (Kil ▸ Keyboard Shortcuts… opens it) and settings in `~/.config/kil/config.toml` (`$XDG_CONFIG_HOME` respected), covering theme, font, scrollback lines (default 10,000), sidebar opacity, and auto-update. Saved layout and screens sit under `~/Library/Application Support/kil/`.

## How does Kil compare to tmux?

Both keep jobs alive across UI restarts, but Kil does it natively with a GUI: vertical tabs, animated splits, per-tab memory, and an iPhone remote, with no prefix keys or manual session management. tmux is terminal-agnostic and scriptable over SSH anywhere; Kil's persistence is local to one Mac.

## Related links

- [Kil README](https://github.com/timwuhaotian/kil#readme)
- [Size comparison](https://github.com/timwuhaotian/kil#size)
- [DESIGN.md](https://github.com/timwuhaotian/kil/blob/main/DESIGN.md)
- [Releases](https://github.com/timwuhaotian/kil/tree/main/releases)
- [GitHub issues](https://github.com/timwuhaotian/kil/issues)
