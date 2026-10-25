//! Keystrokes and pastes to PTY bytes, per xterm's control sequences (DESIGN.md §8).
//!
//! Written from the xterm ctlseqs spec; deliberately not derived from Zed's GPL mappings.

use std::borrow::Cow;

use gpui::{Keystroke, Modifiers};
use kil_term::InputMode;

/// Bytes for `keystroke`, or `None` to let it through.
///
/// `None` covers `cmd` chords (left to bindings and the system) and plain text, which
/// arrives through the input handler so IME composition keeps working.
pub fn encode(
    keystroke: &Keystroke,
    mode: InputMode,
    option_as_meta: bool,
) -> Option<Cow<'static, [u8]>> {
    let modifiers = &keystroke.modifiers;
    if modifiers.platform {
        return None;
    }
    let pressed = Mods {
        shift: modifiers.shift,
        alt: modifiers.alt,
        ctrl: modifiers.control,
    };
    let key = keystroke.key.as_str();
    if let Some(bytes) = named_key(key, pressed, mode) {
        return Some(bytes);
    }
    if pressed.ctrl {
        let byte = control_byte(key)?;
        return Some(owned_with_meta(pressed.alt, &[byte]));
    }
    if pressed.alt && option_as_meta {
        let text = if pressed.shift && key.len() == 1 {
            key.to_ascii_uppercase()
        } else {
            key.to_owned()
        };
        return Some(owned_with_meta(true, text.as_bytes()));
    }
    None
}

/// Bytes for pasting `text`: bracketed when the program asked for it, with any ESC removed
/// so a paste cannot end the bracket early and inject commands.
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend(text.bytes().filter(|&byte| byte != 0x1b));
        bytes.extend_from_slice(b"\x1b[201~");
        bytes
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Bytes for Paste: the clipboard's text, or for an image (no text) an empty bracketed paste,
/// so a program that asked for bracketed paste, such as a coding agent, knows Cmd-V was
/// pressed and reads the image from the clipboard itself. Nothing for an image otherwise.
pub fn paste_clipboard(text: Option<&str>, bracketed: bool) -> Option<Vec<u8>> {
    match text {
        Some(text) => Some(paste(text, bracketed)),
        None => bracketed.then(|| paste("", true)),
    }
}

/// Bytes for files dropped on a pane: each path as the shell reads it, a backslash before
/// every character that isn't plain, with a space after each, as Terminal.app types them.
/// A program in bracketed-paste mode, such as killi, gets them as one paste and can tell a
/// drop from typing.
pub fn drop(paths: &[std::path::PathBuf], bracketed: bool) -> Vec<u8> {
    let mut text = String::new();
    for path in paths {
        for c in path.to_string_lossy().chars() {
            if !(c.is_alphanumeric() || "/._-~+,:@=".contains(c)) {
                text.push('\\');
            }
            text.push(c);
        }
        text.push(' ');
    }
    paste(&text, bracketed)
}

/// One wheel notch at viewport `col`, `row` (0-based), reported to a program that turned
/// mouse reporting on: button 64 (up) or 65 (down) plus modifier bits, in SGR encoding when it
/// asked for that, else xterm's byte encoding. `None` where the byte encoding can't reach.
pub fn wheel_report(
    up: bool,
    col: usize,
    row: usize,
    modifiers: Modifiers,
    mode: InputMode,
) -> Option<Vec<u8>> {
    let button = if up { 64 } else { 65 }
        + 4 * u32::from(modifiers.shift)
        + 8 * u32::from(modifiers.alt)
        + 16 * u32::from(modifiers.control);
    let (x, y) = (u32::try_from(col).ok()? + 1, u32::try_from(row).ok()? + 1);
    if mode.sgr_mouse() {
        return Some(format!("\x1b[<{button};{x};{y}M").into_bytes());
    }
    let mut bytes = b"\x1b[M".to_vec();
    for value in [button, x, y] {
        let value = value + 32;
        if mode.utf8_mouse() {
            let mut buf = [0; 4];
            bytes.extend_from_slice(char::from_u32(value)?.encode_utf8(&mut buf).as_bytes());
        } else {
            bytes.push(u8::try_from(value).ok()?);
        }
    }
    Some(bytes)
}

/// Arrow-key bytes for one wheel notch in the alternate screen.
pub fn wheel_arrow(up: bool, mode: InputMode) -> &'static [u8] {
    match (up, mode.app_cursor()) {
        (true, true) => b"\x1bOA",
        (true, false) => b"\x1b[A",
        (false, true) => b"\x1bOB",
        (false, false) => b"\x1b[B",
    }
}

#[derive(Clone, Copy)]
struct Mods {
    shift: bool,
    alt: bool,
    ctrl: bool,
}

impl Mods {
    /// xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

fn named_key(key: &str, pressed: Mods, mode: InputMode) -> Option<Cow<'static, [u8]>> {
    let bytes: Cow<'static, [u8]> = match key {
        // Shift-Enter is ESC CR, like Option-Enter: the newline-without-sending that Claude
        // Code, kimi and other TUIs read, and zsh inserts as a line break. A plain CR would
        // send the line.
        "enter" => with_meta(pressed.alt || pressed.shift, b"\r"),
        "tab" if pressed.shift => Cow::Borrowed(b"\x1b[Z"),
        "tab" => with_meta(pressed.alt, b"\t"),
        "backspace" => with_meta(pressed.alt, if pressed.ctrl { b"\x08" } else { b"\x7f" }),
        "escape" => Cow::Borrowed(b"\x1b"),
        "space" if pressed.ctrl => with_meta(pressed.alt, b"\0"),
        "up" => cursor_key(b'A', pressed, mode),
        "down" => cursor_key(b'B', pressed, mode),
        "right" => cursor_key(b'C', pressed, mode),
        "left" => cursor_key(b'D', pressed, mode),
        "home" => cursor_key(b'H', pressed, mode),
        "end" => cursor_key(b'F', pressed, mode),
        "insert" => tilde_key(2, pressed),
        "delete" => tilde_key(3, pressed),
        "pageup" => tilde_key(5, pressed),
        "pagedown" => tilde_key(6, pressed),
        "f1" => ss3_key(b'P', pressed),
        "f2" => ss3_key(b'Q', pressed),
        "f3" => ss3_key(b'R', pressed),
        "f4" => ss3_key(b'S', pressed),
        "f5" => tilde_key(15, pressed),
        "f6" => tilde_key(17, pressed),
        "f7" => tilde_key(18, pressed),
        "f8" => tilde_key(19, pressed),
        "f9" => tilde_key(20, pressed),
        "f10" => tilde_key(21, pressed),
        "f11" => tilde_key(23, pressed),
        "f12" => tilde_key(24, pressed),
        _ => return None,
    };
    Some(bytes)
}

fn with_meta(alt: bool, bytes: &'static [u8]) -> Cow<'static, [u8]> {
    if alt {
        owned_with_meta(true, bytes)
    } else {
        Cow::Borrowed(bytes)
    }
}

/// `bytes`, prefixed with ESC when `alt` acts as Meta.
fn owned_with_meta(alt: bool, bytes: &[u8]) -> Cow<'static, [u8]> {
    let mut owned = Vec::with_capacity(bytes.len() + 1);
    if alt {
        owned.push(0x1b);
    }
    owned.extend_from_slice(bytes);
    Cow::Owned(owned)
}

fn cursor_key(code: u8, pressed: Mods, mode: InputMode) -> Cow<'static, [u8]> {
    match pressed.param() {
        1 if mode.app_cursor() => Cow::Owned(vec![0x1b, b'O', code]),
        1 => Cow::Owned(vec![0x1b, b'[', code]),
        param => Cow::Owned(format!("\x1b[1;{param}{}", char::from(code)).into_bytes()),
    }
}

fn ss3_key(code: u8, pressed: Mods) -> Cow<'static, [u8]> {
    match pressed.param() {
        1 => Cow::Owned(vec![0x1b, b'O', code]),
        param => Cow::Owned(format!("\x1b[1;{param}{}", char::from(code)).into_bytes()),
    }
}

fn tilde_key(number: u8, pressed: Mods) -> Cow<'static, [u8]> {
    match pressed.param() {
        1 => Cow::Owned(format!("\x1b[{number}~").into_bytes()),
        param => Cow::Owned(format!("\x1b[{number};{param}~").into_bytes()),
    }
}

/// `ctrl-<key>`: letters map to 1–26; the classic punctuation aliases to the rest.
fn control_byte(key: &str) -> Option<u8> {
    match key.as_bytes() {
        [letter @ b'a'..=b'z'] => Some(letter & 0x1f),
        b"@" | b"2" => Some(0x00),
        b"[" | b"3" => Some(0x1b),
        b"\\" | b"4" => Some(0x1c),
        b"]" | b"5" => Some(0x1d),
        b"^" | b"6" => Some(0x1e),
        b"_" | b"-" | b"7" | b"/" => Some(0x1f),
        b"8" | b"?" => Some(0x7f),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(spec: &str) -> Keystroke {
        Keystroke::parse(spec).unwrap()
    }

    fn bytes(spec: &str, mode: InputMode) -> Option<Vec<u8>> {
        encode(&key(spec), mode, true).map(Cow::into_owned)
    }

    fn normal() -> InputMode {
        InputMode::default()
    }

    /// The input mode of a real terminal after a program printed `modes` (DECSET sequences).
    fn mode_after(modes: &str) -> InputMode {
        let mut config = kil_term::SessionConfig::new(kil_term::GridSize {
            rows: 24,
            cols: 80,
            cell_width: 8,
            cell_height: 16,
        });
        config.program = kil_term::Program::Command {
            program: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                format!("printf '{modes}'; exec /bin/sleep 600").into(),
            ],
        };
        let session = kil_term::Session::spawn(&config, |_| {}).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while session.input_mode() == normal() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        session.input_mode()
    }

    fn report(up: bool, col: usize, row: usize, modifiers: &str, mode: InputMode) -> Vec<u8> {
        let modifiers = Keystroke::parse(&format!("{modifiers}a"))
            .unwrap()
            .modifiers;
        wheel_report(up, col, row, modifiers, mode).unwrap()
    }

    #[test]
    fn encode_should_send_csi_arrow_in_normal_mode() {
        assert_eq!(bytes("up", normal()), Some(b"\x1b[A".to_vec()));
    }

    #[test]
    fn encode_should_send_modifier_param_for_ctrl_arrow() {
        assert_eq!(bytes("ctrl-left", normal()), Some(b"\x1b[1;5D".to_vec()));
    }

    #[test]
    fn encode_should_map_ctrl_letter_to_control_byte() {
        assert_eq!(bytes("ctrl-c", normal()), Some(vec![0x03]));
    }

    #[test]
    fn encode_should_prefix_esc_for_option_as_meta() {
        assert_eq!(bytes("alt-b", normal()), Some(b"\x1bb".to_vec()));
    }

    #[test]
    fn encode_should_send_esc_cr_for_shift_enter_so_tuis_insert_a_newline() {
        assert_eq!(bytes("enter", normal()), Some(b"\r".to_vec()));
        assert_eq!(bytes("shift-enter", normal()), Some(b"\x1b\r".to_vec()));
        assert_eq!(bytes("alt-enter", normal()), Some(b"\x1b\r".to_vec()));
    }

    #[test]
    fn wheel_report_should_use_sgr_buttons_64_and_65_at_the_pointer() {
        let sgr = mode_after("\\033[?1003h\\033[?1006h");
        assert!(sgr.mouse_reporting() && sgr.sgr_mouse());
        assert_eq!(report(true, 0, 0, "", sgr), b"\x1b[<64;1;1M");
        assert_eq!(report(false, 9, 4, "", sgr), b"\x1b[<65;10;5M");
        assert_eq!(report(true, 300, 2, "ctrl-", sgr), b"\x1b[<80;301;3M");
    }

    #[test]
    fn wheel_report_should_use_xterm_bytes_without_sgr_and_stop_past_column_223() {
        let plain = mode_after("\\033[?1000h");
        assert!(plain.mouse_reporting() && !plain.sgr_mouse());
        assert_eq!(
            report(true, 9, 4, "", plain),
            [0x1b, b'[', b'M', 96, 42, 37]
        );
        let modifiers = Keystroke::parse("a").unwrap().modifiers;
        assert_eq!(wheel_report(true, 223, 0, modifiers, plain), None);
    }

    #[test]
    fn encode_should_leave_option_chars_to_input_when_meta_is_off() {
        assert_eq!(encode(&key("alt-b"), normal(), false), None);
    }

    #[test]
    fn encode_should_send_backtab_for_shift_tab() {
        assert_eq!(bytes("shift-tab", normal()), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn encode_should_send_tilde_sequence_with_modifier_for_shift_f5() {
        assert_eq!(bytes("shift-f5", normal()), Some(b"\x1b[15;2~".to_vec()));
    }

    #[test]
    fn encode_should_leave_cmd_chords_to_bindings() {
        assert_eq!(bytes("cmd-c", normal()), None);
    }

    #[test]
    fn encode_should_leave_plain_text_to_input_handler() {
        assert_eq!(bytes("a", normal()), None);
    }

    #[test]
    fn paste_should_strip_escape_inside_brackets() {
        assert_eq!(
            paste("a\x1b[201~b", true),
            b"\x1b[200~a[201~b\x1b[201~".to_vec()
        );
    }

    #[test]
    fn paste_should_turn_newlines_into_returns_when_unbracketed() {
        assert_eq!(paste("a\r\nb\nc", false), b"a\rb\rc".to_vec());
    }

    #[test]
    fn drop_should_escape_each_path_for_the_shell() {
        let paths = [
            std::path::PathBuf::from("/Users/me/My Shot (1).png"),
            std::path::PathBuf::from("/tmp/a.rs"),
        ];
        assert_eq!(
            drop(&paths, false),
            b"/Users/me/My\\ Shot\\ \\(1\\).png /tmp/a.rs ".to_vec()
        );
        assert_eq!(
            drop(&paths[1..], true),
            b"\x1b[200~/tmp/a.rs \x1b[201~".to_vec()
        );
    }

    #[test]
    fn paste_should_send_an_empty_bracket_for_an_image() {
        assert_eq!(
            paste_clipboard(None, true),
            Some(b"\x1b[200~\x1b[201~".to_vec())
        );
        assert_eq!(paste_clipboard(None, false), None);
        assert_eq!(paste_clipboard(Some("ls"), false), Some(b"ls".to_vec()));
    }
}
