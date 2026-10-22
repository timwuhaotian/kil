# Contributing to Kil

Thanks for helping. Bug reports, fixes and features are all welcome. For anything larger than
a fix, open an issue first so we can agree on the shape before you write the code.

## Setup

You need a Mac with Apple silicon, Xcode with its Metal toolchain, and Rust 1.94 or later. The
formatter needs a nightly toolchain for its import options:

```sh
rustup toolchain install nightly --component rustfmt
cargo run -p kil                 # debug build, straight from the tree
scripts/bundle.sh debug          # or a Kil.app in target/debug
```

## Before you open a pull request

CI runs these on Apple silicon; run them first:

```sh
cargo +nightly fmt --all
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The tests in `crates/kil-term/tests/` start real shells and check descriptors, signals,
reaping and leaks. They must leave no process behind. One test expects your login shell
(`$SHELL`) to be zsh.

## How we work

- **[`DESIGN.md`](DESIGN.md) is the spec.** Its acceptance table (A1, A2, …) says what Kil
  does and how each behavior is proven. A change in behavior updates `DESIGN.md`, and
  `README.md` if users see it, in the same pull request.
- **Every fix comes with a test** that fails without it, where the behavior can be tested
  (pure logic, real processes, or GPUI's `TestAppContext`). Things only a person can check,
  like Pinyin input or how an animation feels, go in the pull request's description.
- **Keep the layers.** `kil-term` has no UI and owns processes, PTYs and the session server;
  `kil` is the GPUI app. Unsafe code lives in `kil-term/src/sys/` and in
  `kil/src/notify.rs` (the notification center's Objective-C), each block with a `SAFETY`
  comment.
- **`vendor/gpui`** is gpui 0.2.2 with one change of ours, so idle windows stop asking for
  frames; its `KIL.md` says what changed and how to update it. Mark any change there
  `Modified for Kil`.
- **Comments explain why**, not what. Match the density and style of the code around you.
- **Commit messages** start with a short summary in the imperative or as a statement of what
  now happens ("IME text and candidates follow a hidden cursor"), then explain the problem and
  the fix.
- **Licensing:** Kil is MIT. Don't copy code from Zed's `terminal` or `terminal_view` crates;
  they are GPL-3.0. GPUI and `alacritty_terminal` are Apache-2.0 and fine to build on.

## Releases

Releases are built and notarized locally by the maintainer; [`RELEASING.md`](RELEASING.md)
has the steps. Each one replaces `releases/Kil-arm64.dmg`, bumps the version in `Cargo.toml`,
and dates the `Unreleased` entries in [`CHANGELOG.md`](CHANGELOG.md).

By contributing, you agree that your contributions are licensed under the MIT license, and
to follow the [code of conduct](CODE_OF_CONDUCT.md).
