//! Posting a pane's notification to macOS in Kil's own name, so clicking it brings Kil
//! forward (and the workspace then goes to the pane). `osascript` is told to post it as
//! Kil's bundle; an unbundled development build falls back to a plain notification.

/// Kil's bundle id, as in `packaging/Info.plist`.
const BUNDLE_ID: &str = "dev.kil.terminal";

/// Shows `body` under `title` (the program's name when it gave none, else Kil's).
pub fn post(title: &str, body: &str) {
    if cfg!(test) {
        return;
    }
    let quote = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
    let title = if title.is_empty() { "Kil" } else { title };
    let plain = format!(
        "display notification {} with title {}",
        quote(body),
        quote(title)
    );
    let told = format!("tell application id {} to {plain}", quote(BUNDLE_ID));
    let _ = std::process::Command::new("/usr/bin/osascript")
        .args([
            "-e", "try", "-e", &told, "-e", "on error", "-e", &plain, "-e", "end try",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
