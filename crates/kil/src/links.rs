//! Links under the pointer: what a modifier-click opens (DESIGN.md §8).
//!
//! Two shapes, both plain text in the grid: a bare `http(s)://` URL, and a markdown link
//! `[text](target)` whose target opens wherever in its span the pointer lands. OSC 8
//! hyperlinks are read from the session instead (`Session::hyperlink_at`).

use kil_term::{CellFlags, RenderCell};

/// The URL under `col` in a viewport row, if the pointer is on a link.
#[must_use]
pub fn link_at(cells: &[RenderCell], col: usize) -> Option<String> {
    let mut chars = Vec::with_capacity(cells.len());
    let mut starts = Vec::with_capacity(cells.len());
    for (index, cell) in cells.iter().enumerate() {
        // A wide character's second cell holds no character of its own.
        if cell.flags.contains(CellFlags::WIDE_SPACER) {
            continue;
        }
        starts.push(index);
        chars.push(cell.ch);
    }
    let at = starts
        .partition_point(|&start| start <= col)
        .checked_sub(1)?;
    markdown_link_at(&chars, at).or_else(|| bare_url_at(&chars, at))
}

/// Whether `open_url` may open `url`: only web pages and files, never an app scheme or
/// `javascript:` from untrusted terminal output.
#[must_use]
pub fn is_openable(url: &str) -> bool {
    let scheme = url.split([':', '/', '?', '#']).next().unwrap_or_default();
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "file"
    )
}

/// The target of the `[text](target)` span holding character `at`, if it opens.
fn markdown_link_at(chars: &[char], at: usize) -> Option<String> {
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == ']'
            && chars[i + 1] == '('
            && let Some(offset) = chars[i + 2..].iter().position(|&c| c == ')')
        {
            let end = offset + i + 2;
            if let Some(start) = chars[..i].iter().rposition(|&c| c == '[')
                && start <= at
                && at <= end
            {
                let target: String = chars[i + 2..end].iter().collect();
                if !target.chars().any(char::is_whitespace) && is_openable(&target) {
                    return Some(target);
                }
            }
            i = end;
        }
        i += 1;
    }
    None
}

/// The bare URL holding character `at`, if there is one.
fn bare_url_at(chars: &[char], at: usize) -> Option<String> {
    let mut start = 0;
    while start < chars.len() {
        let Some(len) = scheme_len(&chars[start..]) else {
            start += 1;
            continue;
        };
        let mut end = start + len;
        while end < chars.len() && is_url_char(chars[end]) {
            end += 1;
        }
        end = start + trim_end(&chars[start..end]);
        if start <= at && at < end {
            return Some(chars[start..end].iter().collect());
        }
        start = end;
    }
    None
}

/// Length of the `http://`, `https://` or `file://` prefix, in any letter case.
fn scheme_len(chars: &[char]) -> Option<usize> {
    for scheme in ["https://", "http://", "file://"] {
        if chars.len() >= scheme.len()
            && scheme
                .chars()
                .enumerate()
                .all(|(i, s)| chars[i].to_ascii_lowercase() == s)
        {
            return Some(scheme.len());
        }
    }
    None
}

/// Characters a bare URL runs through: anything but whitespace and the delimiters programs
/// wrap URLs in. Brackets stay: IPv6 hosts need them, and a trailing one is trimmed below.
fn is_url_char(c: char) -> bool {
    !c.is_whitespace() && !matches!(c, '"' | '<' | '>' | '`')
}

/// Drops trailing punctuation (`.` `,` `;` `:` `!` `?` quotes) and closers without their
/// opener, so `(see https://a/b.)` opens `https://a/b` but a Wikipedia `_(x)` keeps its `)`.
/// Returns the trimmed length.
fn trim_end(url: &[char]) -> usize {
    let mut end = url.len();
    while let Some(&last) = url[..end].last() {
        let opener = match last {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '`' => {
                end -= 1;
                continue;
            }
            _ => None,
        };
        let Some(opener) = opener else {
            break;
        };
        // A closer its opener balances stays: a Wikipedia `_(x)` keeps its `)`.
        let (mut opens, mut closes) = (0, 0);
        for &c in &url[..end] {
            if c == opener {
                opens += 1;
            } else if c == last {
                closes += 1;
            }
        }
        if opens >= closes {
            break;
        }
        end -= 1;
    }
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Vec<RenderCell> {
        text.chars()
            .map(|ch| RenderCell {
                ch,
                ..RenderCell::default()
            })
            .collect()
    }

    fn col_of(text: &str, needle: &str, nth: usize) -> usize {
        text.match_indices(needle).nth(nth).map_or(0, |(i, _)| i)
    }

    #[test]
    fn bare_url_should_open_from_anywhere_inside_it() {
        let text = "see https://example.com/docs?q=1 for more";
        let cells = row(text);
        let start = col_of(text, "https", 0);
        for col in [start, start + 10, start + 27] {
            assert_eq!(
                link_at(&cells, col).as_deref(),
                Some("https://example.com/docs?q=1"),
                "col {col}"
            );
        }
        assert_eq!(link_at(&cells, 0), None);
        assert_eq!(link_at(&cells, start + 28), None);
    }

    #[test]
    fn bare_url_should_drop_trailing_punctuation_but_keep_balanced_parens() {
        let text = "(see https://a/b.), next https://w.org/x_(y), end";
        let cells = row(text);
        assert_eq!(
            link_at(&cells, col_of(text, "https", 0) + 2).as_deref(),
            Some("https://a/b")
        );
        assert_eq!(
            link_at(&cells, col_of(text, "https", 1) + 2).as_deref(),
            Some("https://w.org/x_(y)")
        );
    }

    #[test]
    fn markdown_link_should_open_from_its_text_too() {
        let text = "read [the guide](https://example.org/g) please";
        let cells = row(text);
        let open = col_of(text, "[", 0);
        let close = col_of(text, ")", 0);
        for col in [open, open + 5, close - 2, close] {
            assert_eq!(
                link_at(&cells, col).as_deref(),
                Some("https://example.org/g"),
                "col {col}"
            );
        }
        assert_eq!(link_at(&cells, close + 1), None);
    }

    #[test]
    fn markdown_link_should_ignore_unopenable_targets() {
        let cells = row("[rel](../docs/a.md) and [js](javascript:alert(1))");
        assert_eq!(link_at(&cells, 2), None);
        assert_eq!(link_at(&cells, 24), None);
    }

    #[test]
    fn two_links_should_each_open() {
        let text = "https://a.org and [b](https://b.org/x)";
        let cells = row(text);
        assert_eq!(
            link_at(&cells, col_of(text, "https", 0)).as_deref(),
            Some("https://a.org")
        );
        assert_eq!(
            link_at(&cells, col_of(text, "[b]", 0) + 1).as_deref(),
            Some("https://b.org/x")
        );
    }

    #[test]
    fn wide_characters_should_not_shift_the_click() {
        let mut cells = row("ab中 https://e.com");
        // A wide character's second cell holds no character: the grid's spacer.
        cells.insert(
            3,
            RenderCell {
                ch: ' ',
                flags: CellFlags::WIDE_SPACER,
                ..RenderCell::default()
            },
        );
        assert_eq!(link_at(&cells, 5).as_deref(), Some("https://e.com"));
        assert_eq!(link_at(&cells, 3), None);
    }

    #[test]
    fn only_web_and_file_urls_should_open() {
        assert!(is_openable("https://a.org"));
        assert!(is_openable("http://a.org:8080/x"));
        assert!(is_openable("file:///etc/hosts"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("x-man-page://ls"));
        assert!(!is_openable("notaurl"));
    }
}
