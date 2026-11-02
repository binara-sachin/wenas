//! Property tests and comparison with `git diff` (spec §12).

use proptest::prelude::*;
use std::process::Command;
use wenas::diff::{Whitespace, diff};
use wenas::document::Document;
use wenas::ops::{Dir, Session};

fn doc(s: &[u8]) -> Document {
    Document::from_bytes(s.to_vec())
}

/// Lines from a small alphabet (so repeats and ambiguous slides are common),
/// some indented, joined with `eol`.
fn text(eol: &'static str, final_newline: bool) -> impl Strategy<Value = String> {
    let line = prop::sample::select(vec![
        "a",
        "b",
        "c",
        "d",
        "",
        "{",
        "}",
        "    x",
        "    y",
        "        z",
        "fn f() {",
        "    return;",
    ]);
    prop::collection::vec(line, 0..24).prop_map(move |ls| {
        let mut s = ls.join(eol);
        if final_newline && !ls.is_empty() {
            s.push_str(eol);
        }
        s
    })
}

proptest! {
    /// Copying every hunk left→right makes the right side equal the left,
    /// byte for byte when both use the same line endings.
    #[test]
    fn copy_all_hunks_left_to_right_is_exact(
        a in text("\n", true), b in text("\n", true),
    ) {
        let mut s = Session::new(doc(a.as_bytes()), doc(b.as_bytes()), Whitespace::Off);
        let mut guard = 0;
        while !s.hunks.is_empty() {
            s.copy_hunk(0, Dir::LeftToRight).unwrap();
            guard += 1;
            prop_assert!(guard < 1000);
        }
        prop_assert_eq!(s.right.to_bytes(), a.as_bytes());
        prop_assert_eq!(s.left.to_bytes(), a.as_bytes());
    }

    /// Same, including a missing final newline on either side.
    #[test]
    fn copy_all_converges_with_unterminated_last_lines(
        a in text("\n", false), b in text("\n", true), flip in any::<bool>(),
    ) {
        let (a, b) = if flip { (b, a) } else { (a, b) };
        let mut s = Session::new(doc(a.as_bytes()), doc(b.as_bytes()), Whitespace::Off);
        let mut guard = 0;
        while !s.hunks.is_empty() {
            s.copy_hunk(0, Dir::LeftToRight).unwrap();
            guard += 1;
            prop_assert!(guard < 1000);
        }
        prop_assert_eq!(s.right.to_bytes(), a.as_bytes());
    }

    /// Mixed CRLF/LF: contents converge (line endings follow the target).
    #[test]
    fn copy_across_line_endings_converges(a in text("\n", true), b in text("\r\n", true)) {
        let mut s = Session::new(doc(a.as_bytes()), doc(b.as_bytes()), Whitespace::Off);
        while !s.hunks.is_empty() {
            s.copy_hunk(0, Dir::LeftToRight).unwrap();
        }
        let got = String::from_utf8(s.right.to_bytes()).unwrap().replace("\r\n", "\n");
        prop_assert_eq!(got, a);
    }

    /// Undoing every step restores both originals; redoing restores the result.
    #[test]
    fn undo_all_restores_originals(
        a in text("\n", true), b in text("\n", true), left_to_right in any::<bool>(),
    ) {
        let mut s = Session::new(doc(a.as_bytes()), doc(b.as_bytes()), Whitespace::Off);
        let dir = if left_to_right { Dir::LeftToRight } else { Dir::RightToLeft };
        let mut steps = 0;
        while !s.hunks.is_empty() {
            s.copy_hunk(0, dir).unwrap();
            steps += 1;
        }
        let after = (s.left.to_bytes(), s.right.to_bytes());
        for _ in 0..steps { prop_assert!(s.undo().is_some()); }
        prop_assert_eq!(s.left.to_bytes(), a.as_bytes());
        prop_assert_eq!(s.right.to_bytes(), b.as_bytes());
        prop_assert!(!s.is_dirty());
        for _ in 0..steps { prop_assert!(s.redo().is_some()); }
        prop_assert_eq!((s.left.to_bytes(), s.right.to_bytes()), after);
    }

    /// Hunks never overlap, are ordered, and cover exactly the differing lines.
    #[test]
    fn hunks_are_well_formed(a in text("\n", true), b in text("\n", true)) {
        let (l, r) = (doc(a.as_bytes()), doc(b.as_bytes()));
        let hunks = diff(&l, &r, Whitespace::Off);
        let (mut pl, mut pr) = (0, 0);
        for h in &hunks {
            prop_assert!(h.left.start >= pl && h.right.start >= pr);
            // The unchanged gap before the hunk is the same on both sides.
            prop_assert_eq!(h.left.start - pl, h.right.start - pr);
            for k in 0..(h.left.start - pl) {
                prop_assert_eq!(l.line(pl + k), r.line(pr + k));
            }
            prop_assert!(!(h.left.is_empty() && h.right.is_empty()));
            pl = h.left.end;
            pr = h.right.end;
        }
        prop_assert_eq!(l.len() - pl, r.len() - pr);
        prop_assert_eq!(hunks.is_empty(), a == b);
    }
}

fn git_hunks(a: &str, b: &str) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&pa, a).unwrap();
    std::fs::write(&pb, b).unwrap();
    let out = Command::new("git")
        .args([
            "diff",
            "--no-index",
            "--histogram",
            "--indent-heuristic",
            "-U0",
            "--no-color",
        ])
        .arg(&pa)
        .arg(&pb)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let range = |spec: &str| -> std::ops::Range<usize> {
        let spec = &spec[1..];
        let (s, c) = spec.split_once(',').map_or((spec, "1"), |(s, c)| (s, c));
        let (s, c): (usize, usize) = (s.parse().unwrap(), c.parse().unwrap());
        if c == 0 { s..s } else { s - 1..s - 1 + c }
    };
    text.lines()
        .filter(|l| l.starts_with("@@ "))
        .map(|l| {
            let mut it = l.split_whitespace().skip(1);
            (range(it.next().unwrap()), range(it.next().unwrap()))
        })
        .collect()
}

fn ours(a: &str, b: &str) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    diff(&doc(a.as_bytes()), &doc(b.as_bytes()), Whitespace::Off)
        .into_iter()
        .map(|h| (h.left, h.right))
        .collect()
}

#[test]
fn matches_git_on_crafted_cases() {
    let cases = [
        ("", "a\n"),
        ("a\n", ""),
        ("a\nb\nc\n", "a\nX\nc\n"),
        (
            "fn a() {\n    x;\n}\n",
            "fn a() {\n    x;\n}\n\nfn b() {\n    y;\n}\n",
        ),
        (
            "{\n    a;\n}\n{\n    b;\n}\n",
            "{\n    a;\n}\n{\n    c;\n}\n{\n    b;\n}\n",
        ),
        ("a\nb\nb\nb\nc\n", "a\nb\nb\nc\n"),
        ("1\n2\n3\n4\n5\n6\n7\n8\n", "1\n2\nX\n4\n5\n6\nY\n8\n"),
    ];
    for (a, b) in cases {
        assert_eq!(ours(a, b), git_hunks(a, b), "a={a:?} b={b:?}");
    }
}

/// Measures (does not fail on) how often hunk placement differs from git.
#[test]
#[ignore = "diagnostic: cargo test --test engine -- --ignored --nocapture"]
fn report_git_divergence() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strat = (text("\n", true), text("\n", true));
    let (mut total, mut same, mut same_size) = (0, 0, 0);
    for _ in 0..400 {
        let (a, b) = strat.new_tree(&mut runner).unwrap().current();
        let (o, g) = (ours(&a, &b), git_hunks(&a, &b));
        let size = |h: &Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>| {
            h.iter().map(|(l, r)| l.len() + r.len()).sum::<usize>()
        };
        total += 1;
        same += (o == g) as usize;
        same_size += (size(&o) == size(&g)) as usize;
    }
    println!("identical to git: {same}/{total}; same total changed lines: {same_size}/{total}");
}
