//! Property tests and comparison with `git diff` (spec §12).

use proptest::prelude::*;
use std::process::Command;
use wenas::diff::{Whitespace, diff};
use wenas::document::Document;
use wenas::ops::{Dir, Session, Side};

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
    git_hunks_with(a, b, &[])
}

fn git_hunks_with(
    a: &str,
    b: &str,
    flags: &[&str],
) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
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
        .args(flags)
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

fn ours_ws(
    a: &str,
    b: &str,
    ws: Whitespace,
) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    diff(&doc(a.as_bytes()), &doc(b.as_bytes()), ws)
        .into_iter()
        .map(|h| (h.left, h.right))
        .collect()
}

#[test]
fn matches_git_with_whitespace_modes_on_crafted_cases() {
    // `-b` and `-w` against git (spec section 12). The `-b` indentation case is the
    // one a review caught: adding indentation must still be a change.
    let cases = [
        ("foo\n", "    foo\n"),
        ("    foo\n", "foo\n"),
        ("  foo\n", "\tfoo\n"),
        ("foo  \n", "foo\n"),
        ("a  b\n", "a b\n"),
        ("ab\n", "a b\n"),
        ("a\n   \nb\n", "a\n\nb\n"),
        ("fn f() {\n    x;\n}\n", "fn f() {\n        x;\n}\n"),
        ("a b c\nd\n", "a  b\tc\nD\n"),
        ("\tx\n\ty\n", "  x\n  y\n"),
    ];
    for (flag, ws) in [("-b", Whitespace::Change), ("-w", Whitespace::All)] {
        for (a, b) in cases {
            assert_eq!(
                ours_ws(a, b, ws),
                git_hunks_with(a, b, &[flag]),
                "{flag} a={a:?} b={b:?}"
            );
        }
    }
}

/// Whitespace-flavoured lines, so `-b`/`-w` have something to ignore.
fn spacey_text() -> impl Strategy<Value = String> {
    let line = prop::sample::select(vec![
        "a",
        " a",
        "  a",
        "\ta",
        "a ",
        "a  b",
        "a b",
        "ab",
        "",
        " ",
        "x y z",
        "  x  y z ",
        "{",
        "  }",
    ]);
    prop::collection::vec(line, 0..24).prop_map(|ls| {
        let mut s = ls.join("\n");
        if !ls.is_empty() {
            s.push('\n');
        }
        s
    })
}

/// Fraction of deterministic random pairs whose hunks equal git's exactly.
/// Differences are equally valid placements on degenerate ties (spec S1), so a
/// floor rather than strict equality catches regressions without flaking.
fn agreement(
    pairs: usize,
    strat: impl Strategy<Value = (String, String)>,
    flags: &[&str],
    ws: Whitespace,
) -> f64 {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let same = (0..pairs)
        .filter(|_| {
            let (a, b) = strat.new_tree(&mut runner).unwrap().current();
            ours_ws(&a, &b, ws) == git_hunks_with(&a, &b, flags)
        })
        .count();
    same as f64 / pairs as f64
}

#[test]
fn random_pairs_agree_with_git_above_a_floor() {
    let plain = agreement(
        300,
        (text("\n", true), text("\n", true)),
        &[],
        Whitespace::Off,
    );
    let dash_b = agreement(
        200,
        (spacey_text(), spacey_text()),
        &["-b"],
        Whitespace::Change,
    );
    let dash_w = agreement(
        200,
        (spacey_text(), spacey_text()),
        &["-w"],
        Whitespace::All,
    );
    println!("agreement with git: plain {plain:.3}, -b {dash_b:.3}, -w {dash_w:.3}");
    assert!(plain >= 0.95, "plain {plain}");
    // The whitespace corpora are far more degenerate (almost every line ties), so
    // their floors are lower; the crafted cases above pin the exact semantics.
    assert!(dash_b >= 0.85, "-b {dash_b}");
    assert!(dash_w >= 0.75, "-w {dash_w}");
}

/// Real history: this repository's own commits, parent against child, for every
/// Rust file a commit touched. Skipped when there is no history (shallow clone).
#[test]
fn matches_git_on_this_repositorys_history() {
    let repo = env!("CARGO_MANIFEST_DIR");
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let Some(log) = git(&["log", "--format=%H", "-n", "25", "--", "*.rs"]) else {
        eprintln!("no git history available; skipping");
        return;
    };
    let (mut total, mut same) = (0, 0);
    for commit in log.lines() {
        let parent = format!("{commit}^");
        let Some(files) = git(&["diff", "--name-only", &parent, commit, "--", "*.rs"]) else {
            continue; // root commit
        };
        for file in files.lines().take(6) {
            let (Some(old), Some(new)) = (
                git(&["show", &format!("{parent}:{file}")]),
                git(&["show", &format!("{commit}:{file}")]),
            ) else {
                continue; // added or deleted
            };
            if old.contains('\r') || new.contains('\r') {
                continue;
            }
            total += 1;
            same += (ours(&old, &new) == git_hunks(&old, &new)) as usize;
        }
    }
    if total == 0 {
        eprintln!("no comparable file pairs in the history; skipping");
        return;
    }
    let rate = same as f64 / total as f64;
    println!("history: {same}/{total} file pairs identical to git ({rate:.3})");
    assert!(
        rate >= 0.85,
        "only {same}/{total} real file pairs match git"
    );
}

/// Arbitrary bytes per line: invalid UTF-8, lone CRs, CRLF/LF mixed, no final newline.
fn raw_file() -> impl Strategy<Value = Vec<u8>> {
    let body = prop::sample::select(vec![
        &b"a"[..],
        b"b",
        b"",
        b"\xff\xfe",
        b"bad \x80 byte",
        b"cr\rin the middle",
        b"tab\tx",
        b"\xe6\x97\xa5\xe6\x9c\xac",
        b"    indented",
    ]);
    let eol = prop::sample::select(vec![&b"\n"[..], b"\r\n"]);
    (prop::collection::vec((body, eol), 0..16), any::<bool>()).prop_map(|(lines, final_newline)| {
        let mut out = Vec::new();
        for (i, (b, e)) in lines.iter().enumerate() {
            out.extend_from_slice(b);
            if i + 1 < lines.len() || final_newline {
                out.extend_from_slice(e);
            }
        }
        out
    })
}

proptest! {
    /// Spec section 12: saving never alters untouched bytes. After copying one
    /// hunk and saving, every line outside the replaced range is byte-identical
    /// to the original file, and the saved file is exactly what the session holds.
    #[test]
    fn saving_never_alters_untouched_bytes(
        left in raw_file(), right in raw_file(), pick in 0usize..8,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("right.bin");
        std::fs::write(&path, &right).unwrap();
        let r = Document::open(&path).unwrap();
        let original: Vec<Vec<u8>> = (0..r.len()).map(|i| r.raw_line(i).to_vec()).collect();
        let mut s = Session::new(doc(&left), r, Whitespace::Off);
        prop_assume!(!s.hunks.is_empty());
        let h = pick % s.hunks.len();
        let range = s.hunks[h].right.clone();
        s.copy_hunk(h, Dir::LeftToRight).unwrap();
        s.save(Side::Right, false).unwrap();
        let saved = std::fs::read(&path).unwrap();
        prop_assert_eq!(&saved, &s.right.to_bytes());

        let prefix: Vec<u8> = original[..range.start].concat();
        let suffix: Vec<u8> = original[range.end..].concat();
        prop_assert!(saved.starts_with(&prefix), "bytes before the hunk changed");
        // A missing final newline may be given one when lines follow it, so compare
        // the suffix only when the replaced range did not touch an unterminated line.
        let touched_unterminated = range.end > range.start
            && original[range.end - 1].last() != Some(&b'\n');
        if !touched_unterminated {
            prop_assert!(saved.ends_with(&suffix), "bytes after the hunk changed");
        }
    }
}
