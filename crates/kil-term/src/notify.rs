//! Notifications a program posts with OSC 9 (`ESC ] 9 ; body BEL`) or OSC 777
//! (`ESC ] 777 ; notify ; title ; body BEL`), the way iTerm2, `WezTerm`, Ghostty and rxvt take
//! them. alacritty's parser drops OSCs it doesn't know, so these are picked out of the bytes
//! beside it, across reads that split a sequence.

/// How many bytes of an unfinished sequence are kept for the next read before it is given
/// up as not a notification.
const MAX_CARRY: usize = 4096;

/// A notification a program posted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// What the program called it; empty for OSC 9, which has no title.
    pub title: String,
    /// The text to show.
    pub body: String,
}

/// Picks notifications out of a session's output, one read at a time.
#[derive(Debug, Default)]
pub(crate) struct Scanner {
    /// The start of a sequence the last read ended inside.
    carry: Vec<u8>,
}

impl Scanner {
    /// The notifications in `bytes`, with any sequence the previous read left unfinished.
    pub(crate) fn scan(&mut self, bytes: &[u8]) -> Vec<Notification> {
        let mut found = Vec::new();
        let input: Vec<u8>;
        let mut rest: &[u8] = if self.carry.is_empty() {
            bytes
        } else {
            input = [self.carry.as_slice(), bytes].concat();
            self.carry.clear();
            &input
        };
        while let Some(start) = find(rest, b"\x1b]") {
            let after = &rest[start + 2..];
            if !(after.starts_with(b"9;")
                || after.starts_with(b"777;")
                || b"9;".starts_with(after)
                || b"777;".starts_with(after))
            {
                rest = &rest[start + 2..];
                continue;
            }
            let Some((payload, used)) = terminated(after) else {
                if rest.len() - start <= MAX_CARRY {
                    self.carry = rest[start..].to_vec();
                }
                return found;
            };
            if let Some(notification) = parse(payload) {
                found.push(notification);
            }
            rest = &after[used..];
        }
        if rest.last() == Some(&0x1b) {
            self.carry = vec![0x1b];
        }
        found
    }
}

/// The payload before the first BEL or `ESC \`, and how many bytes it took with its end.
fn terminated(after: &[u8]) -> Option<(&[u8], usize)> {
    for (i, &byte) in after.iter().enumerate() {
        if byte == 0x07 {
            return Some((&after[..i], i + 1));
        }
        if byte == 0x1b {
            return match after.get(i + 1) {
                Some(b'\\') => Some((&after[..i], i + 2)),
                Some(_) => Some((&after[..i], i)),
                None => None,
            };
        }
    }
    None
}

fn parse(payload: &[u8]) -> Option<Notification> {
    let text = String::from_utf8_lossy(payload);
    if let Some(body) = text.strip_prefix("9;") {
        return Some(Notification {
            title: String::new(),
            body: body.trim().to_string(),
        });
    }
    let rest = text.strip_prefix("777;notify;")?;
    let (title, body) = rest.split_once(';').unwrap_or((rest, ""));
    Some(Notification {
        title: title.trim().to_string(),
        body: body.trim().to_string(),
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(title: &str, body: &str) -> Vec<Notification> {
        vec![Notification {
            title: title.into(),
            body: body.into(),
        }]
    }

    #[test]
    fn osc_9_and_777_with_either_terminator() {
        let mut scanner = Scanner::default();
        assert_eq!(
            scanner.scan(b"hi\x1b]9;killi: Done\x07there"),
            one("", "killi: Done")
        );
        assert_eq!(
            scanner.scan(b"\x1b]777;notify;killi;Done\x1b\\"),
            one("killi", "Done")
        );
        assert_eq!(
            scanner.scan(b"\x1b]777;notify;no body\x07"),
            one("no body", "")
        );
    }

    #[test]
    fn other_oscs_and_plain_output_are_not_notifications() {
        let mut scanner = Scanner::default();
        assert_eq!(
            scanner.scan(b"\x1b]0;title\x07\x1b]52;c;aGk=\x07\x1b[31mred\x1b[0m\x1b]11;?\x1b\\"),
            Vec::new()
        );
        assert_eq!(scanner.scan(b"\x1b]99;kitty\x07"), Vec::new());
        assert_eq!(scanner.carry, Vec::new());
    }

    #[test]
    fn a_sequence_split_across_reads_is_put_back_together() {
        let mut scanner = Scanner::default();
        assert_eq!(scanner.scan(b"out\x1b]9;Do"), Vec::new());
        assert_eq!(scanner.scan(b"ne\x07more"), one("", "Done"));
        let mut scanner = Scanner::default();
        assert_eq!(scanner.scan(b"x\x1b"), Vec::new());
        assert_eq!(scanner.scan(b"]"), Vec::new());
        assert_eq!(scanner.scan(b"77"), Vec::new());
        assert_eq!(scanner.scan(b"7;notify;a;b\x1b\\"), one("a", "b"));
    }

    #[test]
    fn an_endless_sequence_is_given_up() {
        let mut scanner = Scanner::default();
        assert_eq!(
            scanner.scan(&[b"\x1b]9;".as_slice(), &[b'x'; MAX_CARRY]].concat()),
            Vec::new()
        );
        assert_eq!(scanner.carry, Vec::new());
        assert_eq!(scanner.scan(b"\x07\x1b]9;ok\x07"), one("", "ok"));
    }
}
