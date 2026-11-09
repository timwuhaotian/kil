# Kil launch checklist — distribution & cold start

Context: Kil ships one moving file, `releases/Kil-arm64.dmg` on `main` (only stale tags
`v0.0.1`–`v0.0.3` exist; `0.0.34` has none). Homebrew needs an immutable versioned URL +
working `livecheck` + notability (75 stars / 30 forks / 30 watchers; 225/90/90 for
self-submission; repo ≥30 days old). Good news: native arm64 + Developer ID + notarization
already passes the Gatekeeper checks.
Sources: [Acceptable Casks](https://github.com/Homebrew/brew/blob/main/docs/Acceptable-Casks.md),
[Package Acceptance Policy](https://github.com/Homebrew/brew/blob/main/docs/Package-Acceptance-Policy.md).
⚡ = quick win (≤30 min).

## P0 — unblock Homebrew (do first, in order)

- [ ] Tag the release: `git tag v0.0.35 && git push origin v0.0.35` — tags are what every
  downstream tool (cask, livecheck, release notes) keys off.
- [ ] Publish a GitHub Release with the DMG as an immutable asset — the `raw.githubusercontent.com/main/…`
  URL changes every commit and breaks cask `sha256` verification:
  `gh release create v0.0.35 releases/Kil-arm64.dmg releases/Kil-arm64.dmg.sha256 --title "Kil 0.0.35" --notes "<CHANGELOG entries>"`.
- [ ] Keep `releases/latest.toml` committed as today — installed Kil updaters read it; the GitHub
  Release is an *additional* step, not a replacement.
- [ ] ⚡ Create tap `timwuhaotian/homebrew-kil` with the `kil.rb` draft below — works today with
  zero notability gate; install path becomes `brew install --cask timwuhaotian/kil/kil`.
- [ ] Submit `kil` to [homebrew/cask](https://github.com/Homebrew/homebrew-cask) once ≥75 stars
  (or ≥225 for self-submission) — `brew create --cask <release DMG URL>`, then `brew audit --new-cask kil`.

## P1 — GitHub repo itself

- [ ] ⚡ Set About to: "Native macOS terminal in Rust: 8.6 MB, 0% idle CPU, jobs survive quit and
  crash via a session server." — one line that carries all three data hooks.
- [ ] ⚡ Add 12 topics: `macos terminal terminal-emulator rust gpui alacritty apple-silicon
  developer-tools command-line session-manager ssh open-source` — topics drive GitHub search/discovery.
- [ ] ⚡ Upload social preview (Settings → General → Social preview): 1280×640 PNG <1 MB,
  `docs/screenshot.png` as background + `docs/icon.png` top-left + "8.6 MB · 0% idle CPU" text —
  required size per [GitHub Docs](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/customizing-your-repositorys-social-media-preview).
- [ ] ⚡ Pin `kil` on the maintainer profile — pinned repos are the first thing visitors see.
- [ ] Write GitHub Release notes as: 3–5 CHANGELOG bullets + DMG size + "signed & notarized" line —
  cask reviewers and users both read these first.

## P1 — awesome lists (PR to each README)

- [ ] ⚡ [jaywcjlove/awesome-mac](https://github.com/jaywcjlove/awesome-mac) — biggest macOS apps
  list; PR adding one line under Terminal/Utilities.
- [ ] ⚡ [iCHAIT/awesome-macOS](https://github.com/iCHAIT/awesome-macOS) — has a Terminal section
  (iTerm2 is there); PR one line next to it.
- [ ] ⚡ [serhii-londar/open-source-mac-os-apps](https://github.com/serhii-londar/open-source-mac-os-apps) —
  open-source-only macOS list, Kil fits perfectly; PR one line.
- [ ] ⚡ [rust-unofficial/awesome-rust](https://github.com/rust-unofficial/awesome-rust) — the Rust
  list; follow `CONTRIBUTING.md`, PR under Applications.
- [ ] ⚡ [k4m4/terminals-are-sexy](https://github.com/k4m4/terminals-are-sexy) — terminal lovers'
  list; PR one line under Terminal Emulators.
- [ ] ⚡ [cdleon/awesome-terminals](https://github.com/cdleon/awesome-terminals) — terminal-emulator-only
  list (already lists tty7, a similar Rust/GPUI project); PR one line.
- [ ] [alebcay/awesome-shell](https://github.com/alebcay/awesome-shell) — CLI crowd adjacent to
  terminal users; PR one line.

## P2 — directories

- [ ] ⚡ Create an [AlternativeTo](https://alternativeto.net) account *now* (new accounts face an
  anti-spam delay), then Suggest new application → list Kil as an alternative to iTerm2/Warp —
  captures "replace my terminal" search intent.
- [ ] [Slant](https://www.slant.co) — add Kil as an option under "best terminal emulators for Mac"
  (account required); comparison pages rank well on Google.
- [ ] [SaaSHub](https://www.saashub.com) — submit via site form as an iTerm2/Warp alternative;
  another high-intent comparison funnel.
- [ ] formulae.brew.sh indexing — automatic once the official cask merges; nothing to submit.
- Note: "brewawa" could not be verified as a real directory (no site found, Nov 2026); skipped.

## P2 — launch posts (one per channel, spaced days apart)

- [ ] Show HN, weekday 9am–12pm ET, stay 3h to reply — title: "Show HN: Kil – an 8.6MB macOS
  terminal in Rust where jobs survive quit and crash".
- [ ] r/macapps, weekday US morning — title: "Kil – native Apple Silicon terminal, 8.6MB,
  panes come back after reboot — free and MIT open source".
- [ ] r/rust, weekday US morning, engage in comments (90/10 self-promo norm) — title: "Kil: a
  GPUI + Alacritty macOS terminal — 8.6MB, 0% idle CPU, session server in Rust".
- [ ] r/commandline, weekday US morning — title: "Kil: a terminal whose panes come back after a
  crash or reboot (Rust, macOS, MIT)".
- [ ] V2EX 分享创造 node, weekday Beijing daytime, established account — title: "做了个 8.6MB 的
  macOS 原生终端：Rust 编写，退出重开 pane 还在，还能用 iPhone 远程".
- [ ] X, weekday 9am PT with screenshot — hook: "8.6MB. 0% idle CPU. Quit the app — your vim
  buffer, ssh and build are all still there when you reopen. Kil, a native macOS terminal in Rust (MIT)".
- [ ] dev.to + 掘金 long-form (3 topics): (1) session-server deep dive — how PTYs outlive the app;
  (2) "8.6MB vs 913MB" — the size-chart story; (3) "0% idle CPU" — the one-line GPUI idle patch.

## Appendix — `kil.rb` draft (fill version + sha256 per release)

```ruby
cask "kil" do
  version "0.0.35"
  sha256 "<from releases/Kil-arm64.dmg.sha256>" # `shasum -a 256 releases/Kil-arm64.dmg`

  url "https://github.com/timwuhaotian/kil/releases/download/v#{version}/Kil-arm64.dmg"
  name "Kil"
  desc "Native macOS terminal with session server and vertical tabs"
  homepage "https://github.com/timwuhaotian/kil"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: ">= :sonoma"
  app "Kil.app"

  zap trash: [
    "~/Library/Application Support/kil",
    "~/.config/kil",
  ]
end
```
