# Releasing Kil

Maintainers build, sign and notarize releases locally; CI never builds them. Users always get
one file, [`Kil-arm64.dmg`](https://github.com/timwuhaotian/kil/raw/main/releases/Kil-arm64.dmg),
and never see a version number: versions are for us (`Cargo.toml`, Info.plist, `CHANGELOG.md`,
commit messages).

A release, in order. "Patch" means bumping the last number (0.0.4 → 0.0.5).

1. **Start clean and green.** Everything to ship is committed on `main`, and these pass:
   ```sh
   cargo +nightly fmt --all --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   ```
   With Homebrew's `cargo` first on `PATH`, `+nightly` isn't understood; use
   `rustup run nightly cargo fmt --all --check` instead.
2. **Bump the version** in the root `Cargo.toml` (`[workspace.package] version`). `Cargo.lock`
   follows during the build.
3. **Changelog.** In `CHANGELOG.md`, put a `## x.y.z — YYYY-MM-DD` heading under
   `## Unreleased`, so the release takes its entries and `Unreleased` starts empty.
4. **README.** It must describe what this release does: features, keys, config. Keep its
   download link exactly `https://github.com/timwuhaotian/kil/raw/main/releases/Kil-arm64.dmg`
   (step 5 refuses to start otherwise), and put no version number in it.
5. **Build:** `scripts/release.sh`. It signs with Developer ID and the hardened runtime,
   notarizes and staples the app, then the DMG, checks both with `spctl`, and overwrites
   `releases/Kil-arm64.dmg` and its `.sha256`. It fails unless Apple accepts both. Notarization
   takes a few minutes; credentials are described at the top of the script.
6. **Install it here**, from the DMG, as a user would get it:
   ```sh
   mnt=$(hdiutil attach -nobrowse -readonly releases/Kil-arm64.dmg | awk -F'\t' '/\/Volumes\//{print $NF}')
   rm -rf /Applications/Kil.app && ditto "$mnt/Kil.app" /Applications/Kil.app
   hdiutil detach -quiet "$mnt"
   defaults read /Applications/Kil.app/Contents/Info.plist CFBundleShortVersionString
   spctl -a -vv /Applications/Kil.app     # must say: Notarized Developer ID
   ```
   A running Kil keeps its old code until it is quit and opened again, and its session server
   until that server exits (**Quit and Stop All Jobs**, or a reboot).
7. **Commit** on `main`, everything at once (version, lock, changelog, README, DMG, sha256), as
   `Kil x.y.z: <what changed, briefly>`, with a body listing the changes and the notarization
   result.
8. **Push** `main`, and check that CI passes and the download link serves the new DMG:
   ```sh
   git push origin main
   curl -sIL https://github.com/timwuhaotian/kil/raw/main/releases/Kil-arm64.dmg | grep -i content-length
   ```
   The length matches `stat -f %z releases/Kil-arm64.dmg`.
