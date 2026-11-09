//! Links under the pointer: what a modifier-click opens (DESIGN.md §8).
//!
//! Two shapes, both plain text in the grid: a bare `http(s)://` URL, and a markdown link
//! `[text](target)` whose target opens wherever in its span the pointer lands. OSC 8
//! hyperlinks are read from the session instead (`Session::hyperlink_at`).

use kil_term::{CellFlags, Frame, RenderCell};

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

/// The URL under `col` of viewport `row`, joining rows the terminal wrapped so a split URL
/// opens whole. Joining follows `CellFlags::WRAPPED` on a row's last cell only.
#[must_use]
pub fn link_at_frame(frame: &Frame, row: usize, col: usize) -> Option<String> {
    if row >= frame.rows || frame.cols == 0 {
        return None;
    }
    let wrapped = |r: usize| {
        frame
            .row(r)
            .last()
            .is_some_and(|cell| cell.flags.contains(CellFlags::WRAPPED))
    };
    let mut first = row;
    while first > 0 && wrapped(first - 1) {
        first -= 1;
    }
    let mut last = row;
    while last + 1 < frame.rows && wrapped(last) {
        last += 1;
    }
    let joined: Vec<RenderCell> = (first..=last).flat_map(|r| frame.row(r)).copied().collect();
    link_at(&joined, (row - first) * frame.cols + col)
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
            && let Some(end) = balanced_end(&chars[i + 2..], i + 2)
        {
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

/// The `)` closing a target: parens nest, so a Wikipedia `_(x)` inside one doesn't end it.
/// `None` when nothing balances — an unbalanced `](` is no markdown link.
fn balanced_end(chars: &[char], base: usize) -> Option<usize> {
    let mut depth = 0;
    for (offset, &c) in chars.iter().enumerate() {
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            if depth == 0 {
                return Some(base + offset);
            }
            depth -= 1;
        }
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
    // CJK ideographs stay URL chars: https://例子.测试 must keep opening.
    !c.is_whitespace() && !matches!(c, '"' | '<' | '>' | '`') && !is_cjk_delimiter(c)
}

/// CJK punctuation and spacing — `。、！？「」『』（）【】…`, fullwidth forms, curly
/// quotes — which terminal output runs against a URL without a space. `https://a.org，明天`
/// opens `https://a.org`, not a 404 with `明天` glued on.
fn is_cjk_delimiter(c: char) -> bool {
    matches!(
        c,
        '\u{3000}'..='\u{303F}' | '\u{FF00}'..='\u{FFEF}' | '\u{2018}'..='\u{201F}'
    )
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
            _ if is_cjk_delimiter(last) => {
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

    fn make_frame(lines: &[&str], cols: usize) -> Frame {
        let mut cells = Vec::new();
        for line in lines {
            let mut row: Vec<RenderCell> = line
                .chars()
                .map(|ch| RenderCell {
                    ch,
                    ..RenderCell::default()
                })
                .collect();
            row.resize(
                cols,
                RenderCell {
                    ch: ' ',
                    ..RenderCell::default()
                },
            );
            cells.extend(row);
        }
        Frame {
            cols,
            rows: lines.len(),
            cells,
            ..Frame::default()
        }
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
    fn link_at_frame_should_open_a_wrapped_url_whole_from_either_row() {
        let first = "see https://example.com/some/very/lo";
        let second = "ng/path?q=1 for more";
        let url = "https://example.com/some/very/long/path?q=1";
        let mut frame = make_frame(&[first, second], first.len());
        frame.cells[first.len() - 1].flags = CellFlags::WRAPPED;
        assert_eq!(link_at_frame(&frame, 0, 6).as_deref(), Some(url));
        assert_eq!(link_at_frame(&frame, 1, 2).as_deref(), Some(url));
        assert_eq!(link_at_frame(&frame, 2, 0), None);
    }

    #[test]
    fn link_at_frame_should_not_join_a_full_row_when_unwrapped() {
        let frame = make_frame(&["https://a.org/x", "hello"], 15);
        assert_eq!(
            link_at_frame(&frame, 0, 2).as_deref(),
            Some("https://a.org/x")
        );
        assert_eq!(link_at_frame(&frame, 1, 0), None);
    }

    #[test]
    fn link_at_frame_should_not_join_rows_that_end_or_start_with_space() {
        let frame = make_frame(&["see https://a.org", "hello"], 24);
        assert_eq!(
            link_at_frame(&frame, 0, 6).as_deref(),
            Some("https://a.org")
        );
        assert_eq!(link_at_frame(&frame, 1, 0), None);
        // A full row followed by an indented one is no wrap either.
        let frame = make_frame(&["https://a.org/x", " next"], 15);
        assert_eq!(
            link_at_frame(&frame, 0, 2).as_deref(),
            Some("https://a.org/x")
        );
    }

    #[test]
    fn bare_url_should_stop_before_cjk_punctuation() {
        let text = "see https://a.org，明天";
        let cells = row(text);
        assert_eq!(
            link_at(&cells, col_of(text, "https", 0) + 2).as_deref(),
            Some("https://a.org")
        );
        let ming = text.chars().position(|c| c == '明').unwrap_or(0);
        assert_eq!(link_at(&cells, ming), None);
        let text = "（https://b.org/x）。";
        let cells = row(text);
        assert_eq!(
            link_at(&cells, col_of(text, "https", 0)).as_deref(),
            Some("https://b.org/x")
        );
    }

    #[test]
    fn markdown_link_should_match_the_balanced_paren() {
        let text = "see [wiki](https://w.org/x_(y)) ok";
        let cells = row(text);
        assert_eq!(
            link_at(&cells, col_of(text, "wiki", 0)).as_deref(),
            Some("https://w.org/x_(y)")
        );
    }

    #[test]
    fn markdown_link_should_ignore_an_unbalanced_target() {
        let text = "see [a](https://a.org/x_(y) ok";
        let cells = row(text);
        assert_eq!(link_at(&cells, col_of(text, "[a]", 0) + 1), None);
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
