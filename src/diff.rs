//! Line diff (spec §5.2 `diff`): normalise, intern, histogram, indent heuristic.

use crate::document::Document;
use imara_diff::{Algorithm, Diff, InternedInput};
use std::borrow::Cow;
use std::ops::Range;

/// Whitespace handling, cycled with `W`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Whitespace {
    #[default]
    Off,
    /// `-b`: ignore changes in the amount of whitespace.
    Change,
    /// `-w`: ignore all whitespace.
    All,
}

impl Whitespace {
    pub fn next(self) -> Self {
        match self {
            Whitespace::Off => Whitespace::Change,
            Whitespace::Change => Whitespace::All,
            Whitespace::All => Whitespace::Off,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Whitespace::Off => "off",
            Whitespace::Change => "change",
            Whitespace::All => "all",
        }
    }
}

/// A changed block: `left` lines were replaced by `right` lines.
/// Either range may be empty (pure insertion or removal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub left: Range<usize>,
    pub right: Range<usize>,
}

/// Comparison key for a line: terminator removed (so CRLF == LF) and
/// whitespace normalised. An unterminated final line is kept distinct from a
/// terminated one, as git does.
fn key(doc: &Document, i: usize, ws: Whitespace) -> Cow<'_, [u8]> {
    let line = doc.line(i);
    let unterminated = doc.eol_of(i).is_none();
    let needs_work =
        unterminated || (ws != Whitespace::Off && line.iter().any(u8::is_ascii_whitespace));
    if !needs_work {
        return Cow::Borrowed(line);
    }
    let mut out = Vec::with_capacity(line.len() + 1);
    match ws {
        Whitespace::Off => out.extend_from_slice(line),
        Whitespace::All => out.extend(line.iter().copied().filter(|b| !b.is_ascii_whitespace())),
        Whitespace::Change => {
            let mut in_ws = false;
            for &b in line {
                if b.is_ascii_whitespace() {
                    in_ws = true;
                } else {
                    if in_ws && !out.is_empty() {
                        out.push(b' ');
                    }
                    in_ws = false;
                    out.push(b);
                }
            }
        }
    }
    if unterminated {
        // Cannot occur in a normalised key otherwise: NUL then a marker.
        out.extend_from_slice(b"\0<no-eol>");
    }
    Cow::Owned(out)
}

pub fn diff(left: &Document, right: &Document, ws: Whitespace) -> Vec<Hunk> {
    let before: Vec<Cow<[u8]>> = (0..left.len()).map(|i| key(left, i, ws)).collect();
    let after: Vec<Cow<[u8]>> = (0..right.len()).map(|i| key(right, i, ws)).collect();
    let mut input: InternedInput<Cow<[u8]>> = InternedInput::default();
    input.reserve(before.len() as u32, after.len() as u32);
    input.update_before(before.into_iter());
    input.update_after(after.into_iter());
    let mut d = Diff::compute(Algorithm::Histogram, &input);
    d.postprocess_lines(&input);
    d.hunks()
        .map(|h| Hunk {
            left: h.before.start as usize..h.before.end as usize,
            right: h.after.start as usize..h.after.end as usize,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(a: &str, b: &str, ws: Whitespace) -> Vec<(Range<usize>, Range<usize>)> {
        let (a, b) = (
            Document::from_bytes(a.into()),
            Document::from_bytes(b.into()),
        );
        diff(&a, &b, ws)
            .into_iter()
            .map(|h| (h.left, h.right))
            .collect()
    }

    #[test]
    fn identical_has_no_hunks() {
        assert!(d("a\nb\n", "a\nb\n", Whitespace::Off).is_empty());
        assert!(d("", "", Whitespace::Off).is_empty());
    }

    #[test]
    fn basic_hunks() {
        assert_eq!(
            d("a\nb\nc\n", "a\nX\nc\n", Whitespace::Off),
            vec![(1..2, 1..2)]
        );
        assert_eq!(
            d("a\nc\n", "a\nb\nc\n", Whitespace::Off),
            vec![(1..1, 1..2)]
        );
        assert_eq!(
            d("a\nb\nc\n", "a\nc\n", Whitespace::Off),
            vec![(1..2, 1..1)]
        );
        assert_eq!(d("", "a\n", Whitespace::Off), vec![(0..0, 0..1)]);
    }

    #[test]
    fn crlf_equals_lf() {
        assert!(d("a\r\nb\r\n", "a\nb\n", Whitespace::Off).is_empty());
    }

    #[test]
    fn whitespace_modes() {
        assert_eq!(d("a  b\n", "a b\n", Whitespace::Off).len(), 1);
        assert!(d("a  b\n", "a b\n", Whitespace::Change).is_empty());
        assert!(d("a b \n", "a b\n", Whitespace::Change).is_empty());
        assert_eq!(d("ab\n", "a b\n", Whitespace::Change).len(), 1);
        assert!(d("ab\n", "a b\n", Whitespace::All).is_empty());
    }

    #[test]
    fn missing_final_newline_is_a_difference() {
        assert_eq!(d("a\nb", "a\nb\n", Whitespace::Off), vec![(1..2, 1..2)]);
    }
}
