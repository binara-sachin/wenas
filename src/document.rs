//! A file as an append-only byte store plus a list of line spans (spec §5.2 `document`).
//!
//! Original bytes are never copied or modified. Edits replace entries in the
//! span list; inserted text is appended to the store. Concatenating the spans
//! reproduces the file, so untouched bytes are written back unchanged.

use crate::input::{self, Eol, EolStyle, Loaded, Source};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

/// Size, mtime and hash of a file at load time, for change detection on save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskId {
    pub size: u64,
    pub mtime: Option<SystemTime>,
    pub hash: u64,
}

impl DiskId {
    pub fn of_bytes(bytes: &[u8], mtime: Option<SystemTime>) -> Self {
        DiskId {
            size: bytes.len() as u64,
            mtime,
            hash: fnv1a(bytes),
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Why a side cannot be edited. Carried with the access so a read-only side
/// always has a reason to show when a copy into it is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    /// The old-version copy git hands to a difftool.
    GitOldVersion,
    /// Read from stdin or a pipe.
    Stream,
    /// Forced with `--readonly-left`.
    FlagLeft,
    /// Forced with `--readonly-right`.
    FlagRight,
}

impl ReadOnlyReason {
    /// A short phrase for messages: "The left side is <phrase>, so it's read-only."
    pub fn describe(self) -> &'static str {
        match self {
            ReadOnlyReason::GitOldVersion => "git's old version",
            ReadOnlyReason::Stream => "stdin or a pipe",
            ReadOnlyReason::FlagLeft => "marked --readonly-left",
            ReadOnlyReason::FlagRight => "marked --readonly-right",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    #[default]
    Writable,
    ReadOnly(ReadOnlyReason),
}

#[derive(Debug, Clone)]
pub struct Document {
    store: Vec<u8>,
    lines: Vec<Span>,
    pub path: Option<PathBuf>,
    pub label: Option<String>,
    access: Access,
    /// What the file looked like on disk when loaded or last saved.
    disk: Option<DiskId>,
    /// Identifies the current content; equal to `saved_state` means "clean".
    pub(crate) state: u64,
    pub(crate) saved_state: u64,
}

impl Document {
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        assert!(bytes.len() <= u32::MAX as usize, "document too large");
        let mut lines = Vec::new();
        let mut start = 0usize;
        for nl in memchr::memchr_iter(b'\n', &bytes) {
            lines.push(Span {
                start: start as u32,
                end: (nl + 1) as u32,
            });
            start = nl + 1;
        }
        if start < bytes.len() {
            lines.push(Span {
                start: start as u32,
                end: bytes.len() as u32,
            });
        }
        Document {
            store: bytes,
            lines,
            path: None,
            label: None,
            access: Access::Writable,
            disk: None,
            state: 0,
            saved_state: 0,
        }
    }

    /// Build a document from already-read input. Streams are always read-only.
    pub fn from_loaded(loaded: Loaded, path: Option<&Path>) -> Document {
        let is_file = loaded.source == Source::File;
        let mtime = path
            .filter(|_| is_file)
            .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
        let disk = is_file.then(|| DiskId::of_bytes(&loaded.bytes, mtime));
        let mut doc = Document::from_bytes(loaded.bytes);
        if !is_file {
            doc.access = Access::ReadOnly(ReadOnlyReason::Stream);
        }
        doc.disk = disk;
        doc.path = path.map(Path::to_path_buf);
        doc
    }

    /// Read a path (or `-` for stdin) into a document.
    pub fn open(path: &Path) -> anyhow::Result<Document> {
        Ok(Document::from_loaded(input::read_input(path)?, Some(path)))
    }

    /// An empty document for a file that exists only on the other side; it is
    /// created when saved.
    pub fn empty_at(path: &Path) -> Document {
        let mut doc = Document::from_bytes(Vec::new());
        doc.path = Some(path.to_path_buf());
        doc
    }

    pub fn access(&self) -> Access {
        self.access
    }
    pub fn is_readonly(&self) -> bool {
        matches!(self.access, Access::ReadOnly(_))
    }
    pub fn readonly_reason(&self) -> Option<ReadOnlyReason> {
        match self.access {
            Access::ReadOnly(r) => Some(r),
            Access::Writable => None,
        }
    }
    pub fn make_readonly(&mut self, reason: ReadOnlyReason) {
        self.access = Access::ReadOnly(reason);
    }

    pub fn disk_id(&self) -> Option<&DiskId> {
        self.disk.as_ref()
    }
    pub fn set_disk_id(&mut self, disk: Option<DiskId>) {
        self.disk = disk;
    }
    /// The current content is now what is on disk.
    pub fn mark_saved(&mut self, disk: DiskId) {
        self.disk = Some(disk);
        self.saved_state = self.state;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
    pub fn is_dirty(&self) -> bool {
        self.state != self.saved_state
    }

    pub fn span(&self, i: usize) -> Span {
        self.lines[i]
    }
    pub fn spans(&self, range: std::ops::Range<usize>) -> &[Span] {
        &self.lines[range]
    }
    pub fn bytes_of(&self, s: Span) -> &[u8] {
        &self.store[s.start as usize..s.end as usize]
    }

    /// Line `i` including its terminator.
    pub fn raw_line(&self, i: usize) -> &[u8] {
        self.bytes_of(self.lines[i])
    }

    /// Line `i` without its terminator.
    pub fn line(&self, i: usize) -> &[u8] {
        let raw = self.raw_line(i);
        &raw[..raw.len() - eol_len(raw)]
    }

    /// Terminator of line `i`; `None` only for an unterminated final line.
    pub fn eol_of(&self, i: usize) -> Option<Eol> {
        eol_of(self.raw_line(i))
    }

    pub fn style(&self) -> EolStyle {
        let (mut lf, mut crlf) = (0usize, 0usize);
        for i in 0..self.len() {
            match self.eol_of(i) {
                Some(Eol::Lf) => lf += 1,
                Some(Eol::Crlf) => crlf += 1,
                None => {}
            }
        }
        match (lf, crlf) {
            (0, 0) => EolStyle::None,
            (_, 0) => EolStyle::Lf,
            (0, _) => EolStyle::Crlf,
            _ => EolStyle::Mixed,
        }
    }

    /// Most common terminator; LF when there are none or on a tie.
    pub fn dominant_eol(&self) -> Eol {
        let crlf = (0..self.len())
            .filter(|&i| self.eol_of(i) == Some(Eol::Crlf))
            .count();
        let lf = (0..self.len())
            .filter(|&i| self.eol_of(i) == Some(Eol::Lf))
            .count();
        if crlf > lf { Eol::Crlf } else { Eol::Lf }
    }

    /// True when the last line has no terminator.
    pub fn missing_final_newline(&self) -> bool {
        !self.is_empty() && self.eol_of(self.len() - 1).is_none()
    }

    /// Append bytes to the store and return the span covering them.
    pub fn append(&mut self, bytes: &[u8]) -> Span {
        let start = self.store.len();
        assert!(
            start + bytes.len() <= u32::MAX as usize,
            "document too large"
        );
        self.store.extend_from_slice(bytes);
        Span {
            start: start as u32,
            end: self.store.len() as u32,
        }
    }

    /// Replace `range` with `new`, returning the removed spans.
    pub fn splice(&mut self, range: std::ops::Range<usize>, new: &[Span]) -> Vec<Span> {
        self.lines.splice(range, new.iter().copied()).collect()
    }

    /// Replace the entire content. The old spans stay valid (the store is
    /// append-only), so the swap can be undone. Returns `(old, new)` spans.
    pub fn replace_all(&mut self, bytes: &[u8]) -> (Vec<Span>, Vec<Span>) {
        let base = self.append(bytes).start as usize;
        let mut new = Vec::new();
        let mut start = 0usize;
        for nl in memchr::memchr_iter(b'\n', bytes) {
            new.push(Span {
                start: (base + start) as u32,
                end: (base + nl + 1) as u32,
            });
            start = nl + 1;
        }
        if start < bytes.len() {
            new.push(Span {
                start: (base + start) as u32,
                end: (base + bytes.len()) as u32,
            });
        }
        let old = std::mem::replace(&mut self.lines, new.clone());
        (old, new)
    }

    /// The whole file, as it would be written.
    pub fn to_bytes(&self) -> Vec<u8> {
        let total: usize = self.lines.iter().map(|s| (s.end - s.start) as usize).sum();
        let mut out = Vec::with_capacity(total);
        for s in &self.lines {
            out.extend_from_slice(self.bytes_of(*s));
        }
        out
    }
}

pub fn eol_len(raw: &[u8]) -> usize {
    match eol_of(raw) {
        Some(Eol::Crlf) => 2,
        Some(Eol::Lf) => 1,
        None => 0,
    }
}

pub fn eol_of(raw: &[u8]) -> Option<Eol> {
    if raw.ends_with(b"\r\n") {
        Some(Eol::Crlf)
    } else if raw.ends_with(b"\n") {
        Some(Eol::Lf)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_and_round_trips() {
        for text in [
            &b""[..],
            b"a",
            b"a\n",
            b"a\nb",
            b"a\r\nb\n\n",
            b"\n",
            b"a\rb\n",
        ] {
            let d = Document::from_bytes(text.to_vec());
            assert_eq!(d.to_bytes(), text);
        }
        assert_eq!(Document::from_bytes(b"a\rb\n".to_vec()).len(), 1);
    }

    #[test]
    fn line_strips_terminator() {
        let d = Document::from_bytes(b"a\r\nb\nc".to_vec());
        assert_eq!(d.line(0), b"a");
        assert_eq!(d.line(1), b"b");
        assert_eq!(d.line(2), b"c");
        assert_eq!(d.style(), EolStyle::Mixed);
        assert!(d.missing_final_newline());
    }

    #[test]
    fn replace_all_keeps_old_spans_valid() {
        let mut d = Document::from_bytes(b"a\nb\n".to_vec());
        let (old, new) = d.replace_all(b"x\ny\nz");
        assert_eq!(d.to_bytes(), b"x\ny\nz");
        assert_eq!((old.len(), new.len()), (2, 3));
        d.splice(0..3, &old);
        assert_eq!(d.to_bytes(), b"a\nb\n");
    }

    #[test]
    fn dominant_eol_defaults_to_lf() {
        assert_eq!(Document::from_bytes(b"x".to_vec()).dominant_eol(), Eol::Lf);
        assert_eq!(
            Document::from_bytes(b"a\r\nb\r\nc\n".to_vec()).dominant_eol(),
            Eol::Crlf
        );
    }
}
