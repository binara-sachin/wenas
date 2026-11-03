//! Editing session: hunk copy, undo/redo (spec §5.3, §7.3).

use crate::diff::{Hunk, Whitespace, diff};
use crate::document::{DiskId, Document, Span};
use std::fmt;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Copy direction: `>` is `LeftToRight`, `<` is `RightToLeft`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    LeftToRight,
    RightToLeft,
}

#[derive(Debug, PartialEq, Eq)]
pub enum OpError {
    ReadOnly(Side),
    NoSuchHunk,
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpError::ReadOnly(Side::Left) => write!(f, "left side is read-only"),
            OpError::ReadOnly(Side::Right) => write!(f, "right side is read-only"),
            OpError::NoSuchHunk => write!(f, "no hunk here"),
        }
    }
}
impl std::error::Error for OpError {}

#[derive(Debug)]
pub enum SaveError {
    ReadOnly,
    NoPath,
    /// The file changed on disk since it was loaded; the caller must ask
    /// whether to reload, overwrite (`force`) or cancel.
    ChangedOnDisk,
    Io(std::io::Error),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::ReadOnly => write!(f, "this side is read-only"),
            SaveError::NoPath => write!(f, "no file to save to"),
            SaveError::ChangedOnDisk => write!(f, "file changed on disk"),
            SaveError::Io(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for SaveError {}
impl From<std::io::Error> for SaveError {
    fn from(e: std::io::Error) -> Self {
        SaveError::Io(e)
    }
}

fn changed_on_disk(real: &Path, known: &DiskId) -> std::io::Result<bool> {
    let meta = match std::fs::metadata(real) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if meta.len() == known.size && meta.modified().ok() == known.mtime {
        return Ok(false);
    }
    let bytes = std::fs::read(real)?;
    Ok(DiskId::of_bytes(&bytes, None).hash != known.hash || bytes.len() as u64 != known.size)
}

fn write_document(doc: &mut Document, force: bool) -> Result<(), SaveError> {
    if doc.is_readonly() {
        return Err(SaveError::ReadOnly);
    }
    let path = doc.path.clone().ok_or(SaveError::NoPath)?;
    let real = match std::fs::canonicalize(&path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.clone(),
        Err(e) => return Err(e.into()),
    };
    if !force
        && let Some(known) = doc.disk_id()
        && changed_on_disk(&real, known)?
    {
        return Err(SaveError::ChangedOnDisk);
    }
    let bytes = doc.to_bytes();
    let dir = real
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // A file that exists only on the other side is created, with its folders.
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    if let Ok(meta) = std::fs::metadata(&real) {
        std::fs::set_permissions(tmp.path(), meta.permissions())?;
    }
    tmp.persist(&real).map_err(|e| e.error)?;
    let mtime = std::fs::metadata(&real).and_then(|m| m.modified()).ok();
    doc.mark_saved(DiskId::of_bytes(&bytes, mtime));
    Ok(())
}

/// Where an undo or redo landed, so the UI can move the cursor there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditLoc {
    pub side: Side,
    pub start: usize,
}

/// One undoable change: `old` spans at `start` were replaced by `new`.
#[derive(Debug, Clone)]
struct Edit {
    side: Side,
    start: usize,
    old: Vec<Span>,
    new: Vec<Span>,
    state_before: u64,
    state_after: u64,
}

pub struct Session {
    pub left: Document,
    pub right: Document,
    pub ws: Whitespace,
    pub hunks: Vec<Hunk>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    next_state: u64,
}

impl Session {
    pub fn new(left: Document, right: Document, ws: Whitespace) -> Self {
        let hunks = diff(&left, &right, ws);
        Self::from_parts(left, right, ws, hunks)
    }

    /// A session whose diff was already computed (e.g. on a worker thread).
    pub fn from_parts(left: Document, right: Document, ws: Whitespace, hunks: Vec<Hunk>) -> Self {
        Session {
            left,
            right,
            ws,
            hunks,
            undo: Vec::new(),
            redo: Vec::new(),
            next_state: 1,
        }
    }

    pub fn doc(&self, side: Side) -> &Document {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
    fn doc_mut(&mut self, side: Side) -> &mut Document {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    pub fn set_whitespace(&mut self, ws: Whitespace) {
        self.ws = ws;
        self.rediff();
    }

    pub fn rediff(&mut self) {
        self.hunks = diff(&self.left, &self.right, self.ws);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Replace hunk `idx` on the destination side with the source side's lines.
    pub fn copy_hunk(&mut self, idx: usize, dir: Dir) -> Result<(), OpError> {
        let hunk = self.hunks.get(idx).cloned().ok_or(OpError::NoSuchHunk)?;
        let (src_side, dst_side, src_range, dst_range) = match dir {
            Dir::LeftToRight => (Side::Left, Side::Right, hunk.left, hunk.right),
            Dir::RightToLeft => (Side::Right, Side::Left, hunk.right, hunk.left),
        };
        if self.doc(dst_side).is_readonly() {
            return Err(OpError::ReadOnly(dst_side));
        }

        let eol = self.doc(dst_side).dominant_eol();
        let lines: Vec<(Vec<u8>, bool)> = {
            let src = self.doc(src_side);
            src_range
                .clone()
                .map(|i| (src.line(i).to_vec(), src.eol_of(i).is_some()))
                .collect()
        };

        let mut start = dst_range.start;
        let end = dst_range.end;
        let dst_len = self.doc(dst_side).len();
        let mut bytes: Vec<Vec<u8>> = Vec::with_capacity(lines.len() + 1);

        // Inserting after an unterminated last line: give that line a terminator
        // so the new lines don't fuse onto it.
        if !lines.is_empty() && start == dst_len && start > 0 && end == start {
            let dst = self.doc(dst_side);
            if dst.eol_of(start - 1).is_none() {
                let mut fixed = dst.line(start - 1).to_vec();
                fixed.extend_from_slice(eol.bytes());
                bytes.push(fixed);
                start -= 1;
            }
        }
        let last = lines.len().wrapping_sub(1);
        for (n, (content, had_eol)) in lines.into_iter().enumerate() {
            let mut b = content;
            // Carry "no newline at end of file" across, but never leave an
            // unterminated line in the middle of the target.
            if had_eol || end < dst_len || n != last {
                b.extend_from_slice(eol.bytes());
            }
            bytes.push(b);
        }

        let dst = self.doc_mut(dst_side);
        let new: Vec<Span> = bytes.iter().map(|b| dst.append(b)).collect();
        let old = dst.splice(start..end, &new);
        let state_before = dst.state;
        let state_after = self.next_state;
        self.next_state += 1;
        self.doc_mut(dst_side).state = state_after;
        self.undo.push(Edit {
            side: dst_side,
            start,
            old,
            new,
            state_before,
            state_after,
        });
        self.redo.clear();
        self.rediff();
        Ok(())
    }

    /// Undo the most recent edit.
    pub fn undo(&mut self) -> Option<EditLoc> {
        let e = self.undo.pop()?;
        let doc = self.doc_mut(e.side);
        doc.splice(e.start..e.start + e.new.len(), &e.old);
        doc.state = e.state_before;
        let loc = EditLoc {
            side: e.side,
            start: e.start,
        };
        self.redo.push(e);
        self.rediff();
        Some(loc)
    }

    pub fn redo(&mut self) -> Option<EditLoc> {
        let e = self.redo.pop()?;
        let doc = self.doc_mut(e.side);
        doc.splice(e.start..e.start + e.old.len(), &e.new);
        doc.state = e.state_after;
        let loc = EditLoc {
            side: e.side,
            start: e.start,
        };
        self.undo.push(e);
        self.rediff();
        Some(loc)
    }

    /// Number of undoable steps that touched `side`.
    pub fn edit_count(&self, side: Side) -> usize {
        self.undo.iter().filter(|e| e.side == side).count()
    }

    /// Sides with unsaved changes.
    pub fn dirty_sides(&self) -> Vec<Side> {
        [Side::Left, Side::Right]
            .into_iter()
            .filter(|&s| self.doc(s).is_dirty())
            .collect()
    }

    /// Swap a side's whole content for `bytes` just read from disk (after an
    /// `$EDITOR` trip, or a reload). One undo step; the side is clean afterwards,
    /// and undoing makes it dirty again. Returns false if nothing changed.
    pub fn replace_content(&mut self, side: Side, bytes: &[u8], disk: DiskId) -> bool {
        let state_after = self.next_state;
        let doc = self.doc_mut(side);
        doc.set_disk_id(Some(disk));
        if doc.to_bytes() == bytes {
            return false;
        }
        let state_before = doc.state;
        let (old, new) = doc.replace_all(bytes);
        doc.state = state_after;
        doc.saved_state = state_after;
        self.next_state += 1;
        self.undo.push(Edit {
            side,
            start: 0,
            old,
            new,
            state_before,
            state_after,
        });
        self.redo.clear();
        self.rediff();
        true
    }

    /// Save one side to its path (spec §7.4). Follows symlinks so the real file
    /// is written, writes a temp file beside it, fsyncs, copies permissions and
    /// renames over the original. Unless `force`, refuses with `ChangedOnDisk`
    /// when the file changed since it was loaded.
    pub fn save(&mut self, side: Side, force: bool) -> Result<(), SaveError> {
        write_document(self.doc_mut(side), force)
    }

    pub fn is_dirty(&self) -> bool {
        self.left.is_dirty() || self.right.is_dirty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ReadOnlyReason;

    fn session(a: &str, b: &str) -> Session {
        Session::new(
            Document::from_bytes(a.into()),
            Document::from_bytes(b.into()),
            Whitespace::Off,
        )
    }

    #[test]
    fn copy_right_then_undo_redo() {
        let mut s = session("a\nB\nc\n", "a\nb\nc\n");
        s.copy_hunk(0, Dir::LeftToRight).unwrap();
        assert_eq!(s.right.to_bytes(), b"a\nB\nc\n");
        assert!(s.hunks.is_empty());
        assert!(s.right.is_dirty() && !s.left.is_dirty());
        s.undo();
        assert_eq!(s.right.to_bytes(), b"a\nb\nc\n");
        assert!(!s.is_dirty());
        s.redo();
        assert_eq!(s.right.to_bytes(), b"a\nB\nc\n");
        assert!(s.is_dirty());
    }

    #[test]
    fn copy_converts_line_endings_to_target() {
        let mut s = session("a\nX\nY\n", "a\r\nb\r\n");
        s.copy_hunk(0, Dir::LeftToRight).unwrap();
        assert_eq!(s.right.to_bytes(), b"a\r\nX\r\nY\r\n");
    }

    #[test]
    fn readonly_target_is_refused() {
        let mut s = session("a\n", "b\n");
        s.right.make_readonly(ReadOnlyReason::FlagRight);
        assert_eq!(
            s.copy_hunk(0, Dir::LeftToRight),
            Err(OpError::ReadOnly(Side::Right))
        );
        assert_eq!(s.copy_hunk(5, Dir::RightToLeft), Err(OpError::NoSuchHunk));
        assert!(s.copy_hunk(0, Dir::RightToLeft).is_ok());
    }

    #[test]
    fn no_newline_at_eof_is_carried_and_never_fuses() {
        let mut s = session("a\nb", "a\n");
        s.copy_hunk(0, Dir::LeftToRight).unwrap();
        assert_eq!(s.right.to_bytes(), b"a\nb");

        let mut s = session("a\n", "a\nb");
        s.copy_hunk(0, Dir::LeftToRight).unwrap();
        assert_eq!(s.right.to_bytes(), b"a\n");

        // Insertion after an unterminated last line.
        let mut s = session("a\nb\nc\n", "a");
        s.copy_hunk(0, Dir::LeftToRight).unwrap();
        assert_eq!(s.right.to_bytes(), b"a\nb\nc\n");

        // Unterminated line copied into the middle gets a terminator.
        let mut s = session("x", "x\ny\n");
        s.copy_hunk(0, Dir::LeftToRight).unwrap();
        assert!(s.hunks.is_empty());
    }

    #[test]
    fn replace_content_is_one_clean_undoable_step() {
        let mut s = session("a\nb\n", "a\nX\n");
        assert!(s.replace_content(
            Side::Right,
            b"a\nb\nc\n",
            DiskId::of_bytes(b"a\nb\nc\n", None)
        ));
        assert!(
            !s.right.is_dirty(),
            "content came from disk, so it is clean"
        );
        assert_eq!(s.hunks.len(), 1);
        assert_eq!(s.edit_count(Side::Right), 1);
        s.undo();
        assert_eq!(s.right.to_bytes(), b"a\nX\n");
        assert!(s.right.is_dirty(), "undoing leaves it different from disk");
        s.redo();
        assert!(!s.right.is_dirty());
        assert!(!s.replace_content(Side::Right, b"a\nb\nc\n", DiskId::of_bytes(b"", None)));
    }

    #[test]
    fn undo_reports_where_it_landed() {
        let mut s = session("1\n2\n3\n", "1\nx\n3\n");
        s.copy_hunk(0, Dir::LeftToRight).unwrap();
        assert_eq!(
            s.undo(),
            Some(EditLoc {
                side: Side::Right,
                start: 1
            })
        );
        assert_eq!(
            s.redo(),
            Some(EditLoc {
                side: Side::Right,
                start: 1
            })
        );
        assert_eq!(s.dirty_sides(), vec![Side::Right]);
    }

    #[test]
    fn edit_after_undo_clears_redo() {
        let mut s = session("1\n2\n", "1\nx\n");
        s.copy_hunk(0, Dir::LeftToRight).unwrap();
        s.undo();
        s.copy_hunk(0, Dir::RightToLeft).unwrap();
        assert!(!s.can_redo());
        assert_eq!(s.left.to_bytes(), b"1\nx\n");
    }

    fn file_session(path: &Path) -> Session {
        Session::new(
            Document::open(path).unwrap(),
            Document::from_bytes(Vec::new()),
            Whitespace::Off,
        )
    }

    #[test]
    fn saves_atomically_and_detects_external_change() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "a\nb\n").unwrap();
        let mut s = file_session(&p);
        s.save(Side::Left, false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"a\nb\n");

        std::fs::write(&p, "changed\n").unwrap();
        assert!(matches!(
            s.save(Side::Left, false),
            Err(SaveError::ChangedOnDisk)
        ));
        s.save(Side::Left, true).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"a\nb\n");
    }

    #[test]
    fn saving_creates_missing_folders() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new/deeper/f.txt");
        let mut s = Session::new(
            Document::from_bytes(b"x\n".to_vec()),
            Document::empty_at(&p),
            Whitespace::Off,
        );
        s.copy_hunk(0, Dir::LeftToRight).unwrap();
        s.save(Side::Right, false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"x\n");
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinks_and_keeps_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&real, "x\n").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o750)).unwrap();
        symlink(&real, &link).unwrap();
        let mut s = file_session(&link);
        let span = s.left.append(b"y\n");
        let n = s.left.len();
        s.left.splice(n..n, &[span]);
        s.save(Side::Left, false).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&real).unwrap(), b"x\ny\n");
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[test]
    fn read_only_sides_refuse_to_save() {
        let mut s = session("a", "b");
        s.left.make_readonly(ReadOnlyReason::GitOldVersion);
        assert!(matches!(
            s.save(Side::Left, false),
            Err(SaveError::ReadOnly)
        ));
        assert!(matches!(s.save(Side::Right, false), Err(SaveError::NoPath)));
    }
}
