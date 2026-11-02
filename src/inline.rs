//! Word-level highlighting inside changed lines (spec §5.2 `inline`).

use imara_diff::{Algorithm, Diff, InternedInput};
use std::collections::HashSet;
use std::ops::Range;

/// Lines longer than this get no intra-line highlighting.
pub const MAX_LINE_BYTES: usize = 2000;
/// Skip similarity pairing for very large unequal hunks.
const MAX_PAIRING_WORK: usize = 400;
const MIN_SIMILARITY: f32 = 0.5;

/// Byte ranges to emphasise, one entry per input line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Inline {
    pub left: Vec<Vec<Range<usize>>>,
    pub right: Vec<Vec<Range<usize>>>,
}

#[derive(PartialEq, Clone, Copy)]
enum Class {
    Word,
    Space,
    Punct,
}

fn class(b: u8) -> Class {
    if b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80 {
        Class::Word
    } else if b.is_ascii_whitespace() {
        Class::Space
    } else {
        Class::Punct
    }
}

/// Words, whitespace runs, and single punctuation bytes.
fn tokens(line: &[u8]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let c = class(line[i]);
        let mut j = i + 1;
        if c != Class::Punct {
            while j < line.len() && class(line[j]) == c {
                j += 1;
            }
        }
        out.push(i..j);
        i = j;
    }
    out
}

fn similarity(a: &[u8], b: &[u8]) -> f32 {
    let words = |l: &[u8]| -> HashSet<Vec<u8>> {
        tokens(l)
            .into_iter()
            .filter(|r| class(l[r.start]) != Class::Space)
            .map(|r| l[r].to_vec())
            .collect()
    };
    let (wa, wb) = (words(a), words(b));
    let union = wa.union(&wb).count();
    if union == 0 {
        return 1.0;
    }
    wa.intersection(&wb).count() as f32 / union as f32
}

/// Changed byte ranges of two lines.
fn diff_pair(a: &[u8], b: &[u8]) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if a.len() > MAX_LINE_BYTES || b.len() > MAX_LINE_BYTES || a == b {
        return (vec![], vec![]);
    }
    let (ta, tb) = (tokens(a), tokens(b));
    let mut input: InternedInput<&[u8]> = InternedInput::default();
    input.update_before(ta.iter().map(|r| &a[r.clone()]));
    input.update_after(tb.iter().map(|r| &b[r.clone()]));
    let diff = Diff::compute(Algorithm::Myers, &input);
    let (mut ra, mut rb): (Vec<Range<usize>>, Vec<Range<usize>>) = (vec![], vec![]);
    let push = |out: &mut Vec<Range<usize>>, toks: &[Range<usize>], r: Range<u32>| {
        if r.is_empty() {
            return;
        }
        let range = toks[r.start as usize].start..toks[r.end as usize - 1].end;
        match out.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => out.push(range),
        }
    };
    for h in diff.hunks() {
        push(&mut ra, &ta, h.before);
        push(&mut rb, &tb, h.after);
    }
    (ra, rb)
}

/// Pair lines of a changed block and diff each pair.
/// Equal counts pair one-to-one; otherwise pair monotonically by similarity.
pub fn inline_diff(left: &[&[u8]], right: &[&[u8]]) -> Inline {
    let mut out = Inline {
        left: vec![vec![]; left.len()],
        right: vec![vec![]; right.len()],
    };
    let pairs: Vec<(usize, usize)> = if left.len() == right.len() {
        (0..left.len()).map(|i| (i, i)).collect()
    } else if left.len() * right.len() <= MAX_PAIRING_WORK {
        let mut pairs = Vec::new();
        let mut next = 0;
        for (i, l) in left.iter().enumerate() {
            let best = (next..right.len())
                .map(|j| (j, similarity(l, right[j])))
                .filter(|&(_, s)| s >= MIN_SIMILARITY)
                .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
            if let Some((j, _)) = best {
                pairs.push((i, j));
                next = j + 1;
            }
        }
        pairs
    } else {
        vec![]
    };
    for (i, j) in pairs {
        let (a, b) = diff_pair(left[i], right[j]);
        out.left[i] = a;
        out.right[j] = b;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(line: &str, ranges: &[Range<usize>]) -> Vec<String> {
        ranges.iter().map(|r| line[r.clone()].to_string()).collect()
    }

    #[test]
    fn highlights_changed_words() {
        let (a, b) = ("let ttl = self.ttl;", "let ttl = self.ttl.max(60);");
        let r = inline_diff(&[a.as_bytes()], &[b.as_bytes()]);
        assert!(
            r.left[0].is_empty(),
            "pure insertion marks nothing on the left"
        );
        assert_eq!(marked(b, &r.right[0]), vec![".max(60)"]);
    }

    #[test]
    fn replaced_word_on_both_sides() {
        let (a, b) = ("a < now()", "a <= now()");
        let r = inline_diff(&[a.as_bytes()], &[b.as_bytes()]);
        assert_eq!(marked(a, &r.left[0]), Vec::<String>::new());
        assert_eq!(marked(b, &r.right[0]), vec!["="]);
        let (a, b) = ("Refresh if it", "Refresh before it");
        let r = inline_diff(&[a.as_bytes()], &[b.as_bytes()]);
        assert_eq!(marked(a, &r.left[0]), vec!["if"]);
        assert_eq!(marked(b, &r.right[0]), vec!["before"]);
    }

    #[test]
    fn unequal_counts_pair_by_similarity() {
        let left: [&[u8]; 1] = [b"let ttl = self.ttl;"];
        let right: [&[u8]; 2] = [
            b"let ttl = self.ttl.max(60);",
            b"log::debug!(\"ttl={ttl}\");",
        ];
        let r = inline_diff(&left, &right);
        assert!(!r.right[0].is_empty());
        assert!(r.right[1].is_empty(), "unrelated line gets no highlight");
    }

    #[test]
    fn long_lines_and_identical_lines_are_skipped() {
        let long = "x ".repeat(MAX_LINE_BYTES);
        let r = inline_diff(&[long.as_bytes()], &[b"y"]);
        assert!(r.left[0].is_empty() && r.right[0].is_empty());
        let r = inline_diff(&[b"same"], &[b"same"]);
        assert!(r.left[0].is_empty());
    }

    #[test]
    fn multibyte_words_stay_whole() {
        let (a, b) = ("héllo wörld", "héllo wörld!");
        let r = inline_diff(&[a.as_bytes()], &[b.as_bytes()]);
        assert_eq!(marked(b, &r.right[0]), vec!["!"]);
    }
}
