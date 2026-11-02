//! Hunks to screen rows (spec §5.2 `align`).
//!
//! A row pairs at most one line from each side. Where one side of a hunk is
//! shorter, the other side's extra rows have no partner and the UI draws
//! filler (`░`) opposite them.

use crate::diff::Hunk;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Same,
    /// Both sides have a line and they differ (`~`).
    Changed,
    /// Only the right side has a line (`+`).
    Added,
    /// Only the left side has a line (`-`).
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub kind: RowKind,
    /// Index into the hunk list for non-`Same` rows.
    pub hunk: Option<usize>,
}

/// Side-by-side rows covering both documents exactly once per line.
pub fn side_by_side(hunks: &[Hunk], left_len: usize, right_len: usize) -> Vec<Row> {
    let mut rows = Vec::with_capacity(left_len.max(right_len));
    let (mut l, mut r) = (0usize, 0usize);
    let same = |rows: &mut Vec<Row>, l: &mut usize, r: &mut usize, until_l: usize| {
        while *l < until_l {
            rows.push(Row {
                left: Some(*l),
                right: Some(*r),
                kind: RowKind::Same,
                hunk: None,
            });
            *l += 1;
            *r += 1;
        }
    };
    for (hi, h) in hunks.iter().enumerate() {
        same(&mut rows, &mut l, &mut r, h.left.start);
        debug_assert_eq!(r, h.right.start);
        let (nl, nr) = (h.left.len(), h.right.len());
        for i in 0..nl.max(nr) {
            let (le, ri) = (
                (i < nl).then(|| h.left.start + i),
                (i < nr).then(|| h.right.start + i),
            );
            let kind = match (le, ri) {
                (Some(_), Some(_)) => RowKind::Changed,
                (None, Some(_)) => RowKind::Added,
                _ => RowKind::Removed,
            };
            rows.push(Row {
                left: le,
                right: ri,
                kind,
                hunk: Some(hi),
            });
        }
        l = h.left.end;
        r = h.right.end;
    }
    same(&mut rows, &mut l, &mut r, left_len);
    debug_assert_eq!(r, right_len);
    rows
}

/// First row index belonging to each hunk (for `J`/`K` navigation).
pub fn hunk_first_rows(rows: &[Row], hunk_count: usize) -> Vec<usize> {
    let mut out = vec![usize::MAX; hunk_count];
    for (i, row) in rows.iter().enumerate() {
        if let Some(h) = row.hunk
            && out[h] == usize::MAX
        {
            out[h] = i;
        }
    }
    out
}

/// The current hunk for a cursor row: the hunk the cursor is inside, else the
/// next one below it (spec §7.3, D15). `None` when no hunk lies at or below.
pub fn current_hunk(rows: &[Row], cursor: usize) -> Option<usize> {
    rows.get(cursor..)?.iter().find_map(|r| r.hunk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_pair_and_fill() {
        let hunks = vec![Hunk {
            left: 1..2,
            right: 1..3,
        }];
        let rows = side_by_side(&hunks, 3, 4);
        let kinds: Vec<_> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                RowKind::Same,
                RowKind::Changed,
                RowKind::Added,
                RowKind::Same
            ]
        );
        assert_eq!(rows[2].left, None);
        assert_eq!(rows[3].left, Some(2));
        assert_eq!(rows[3].right, Some(3));
    }

    #[test]
    fn current_hunk_rule() {
        let hunks = vec![Hunk {
            left: 2..3,
            right: 2..2,
        }];
        let rows = side_by_side(&hunks, 4, 3);
        assert_eq!(current_hunk(&rows, 0), Some(0));
        assert_eq!(current_hunk(&rows, 2), Some(0));
        assert_eq!(current_hunk(&rows, 3), None);
        assert_eq!(hunk_first_rows(&rows, 1), vec![2]);
    }
}
