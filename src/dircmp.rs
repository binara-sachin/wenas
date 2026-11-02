//! Directory comparison (spec §5.2 `dircmp`, D9).
//!
//! Both trees are walked together in name order, so entries stream out sorted
//! and the UI can show results while the scan is still running. File contents
//! are compared by size where that settles it, otherwise in chunks, stopping at
//! the first differing byte.

use crate::input::{BINARY_SNIFF_LEN, has_utf16_bom, is_binary};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const CHUNK: usize = 64 * 1024;
/// Report scan progress after this many files.
const PROGRESS_EVERY: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Identical,
    /// Text file whose contents differ.
    Modified,
    LeftOnly,
    RightOnly,
    /// Binary file whose contents differ; `first_diff` is the byte offset.
    Binary {
        first_diff: u64,
    },
    /// Different kinds on each side, e.g. "file on left, directory on right".
    Mismatch(String),
    /// Both sides are symlinks with different targets.
    SymlinkDiffers {
        left: String,
        right: String,
    },
    /// Could not be read; the walk continues.
    Error(String),
}

impl Status {
    pub fn differs(&self) -> bool {
        *self != Status::Identical
    }

    /// The one-letter marker shown in the list.
    pub fn letter(&self) -> char {
        match self {
            Status::Identical => '=',
            Status::Modified | Status::SymlinkDiffers { .. } => 'M',
            Status::LeftOnly => 'L',
            Status::RightOnly => 'R',
            Status::Binary { .. } => 'B',
            Status::Mismatch(_) => 'T',
            Status::Error(_) => '!',
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Status::Identical | Status::Modified => String::new(),
            Status::LeftOnly => "only on left".into(),
            Status::RightOnly => "only on right".into(),
            Status::Binary { first_diff } => {
                format!(
                    "binary, differs at byte {}",
                    crate::ui::render::thousands(*first_diff as usize)
                )
            }
            Status::Mismatch(m) => m.clone(),
            Status::SymlinkDiffers { left, right } => format!("symlink: {left} vs {right}"),
            Status::Error(e) => e.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub rel: PathBuf,
    pub status: Status,
}

#[derive(Debug)]
pub enum Msg {
    Entry(Entry),
    /// Files looked at so far.
    Scanned(usize),
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

fn kind_of(path: &Path) -> io::Result<Kind> {
    let t = fs::symlink_metadata(path)?.file_type();
    Ok(if t.is_symlink() {
        Kind::Symlink
    } else if t.is_dir() {
        Kind::Dir
    } else if t.is_file() {
        Kind::File
    } else {
        Kind::Other
    })
}

/// "Permission denied (os error 13)" -> "can't read: permission denied".
fn read_error(e: &io::Error) -> String {
    let s = e.to_string();
    let s = s.split(" (os error").next().unwrap_or(&s);
    let mut chars = s.chars();
    let lowered: String = chars
        .next()
        .map(|c| c.to_lowercase().collect::<String>() + chars.as_str())
        .unwrap_or_default();
    format!("can't read: {lowered}")
}

/// Fill `buf` as far as the reader allows; returns the bytes read.
fn read_fully(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Compare two regular files (following links given as paths).
pub fn compare_files(a: &Path, b: &Path) -> io::Result<Status> {
    let (mut fa, mut fb) = (fs::File::open(a)?, fs::File::open(b)?);
    let (size_a, size_b) = (fa.metadata()?.len(), fb.metadata()?.len());
    let (mut ha, mut hb) = (vec![0u8; BINARY_SNIFF_LEN], vec![0u8; BINARY_SNIFF_LEN]);
    let (na, nb) = (read_fully(&mut fa, &mut ha)?, read_fully(&mut fb, &mut hb)?);
    let (ha, hb) = (&ha[..na], &hb[..nb]);
    let binary = [ha, hb].iter().any(|h| is_binary(h) || has_utf16_bom(h));
    if size_a != size_b && !binary {
        return Ok(Status::Modified);
    }
    let differ = |at: u64| {
        if binary {
            Status::Binary { first_diff: at }
        } else {
            Status::Modified
        }
    };
    let common = na.min(nb);
    if let Some(i) = ha[..common]
        .iter()
        .zip(&hb[..common])
        .position(|(x, y)| x != y)
    {
        return Ok(differ(i as u64));
    }
    if na != nb {
        return Ok(differ(common as u64));
    }
    if na < BINARY_SNIFF_LEN {
        return Ok(Status::Identical);
    }
    let mut offset = na as u64;
    let (mut ba, mut bb) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    loop {
        let (na, nb) = (read_fully(&mut fa, &mut ba)?, read_fully(&mut fb, &mut bb)?);
        let common = na.min(nb);
        if let Some(i) = ba[..common]
            .iter()
            .zip(&bb[..common])
            .position(|(x, y)| x != y)
        {
            return Ok(differ(offset + i as u64));
        }
        if na != nb {
            return Ok(differ(offset + common as u64));
        }
        if na == 0 {
            return Ok(Status::Identical);
        }
        offset += na as u64;
    }
}

struct Walker<'a> {
    exclude: &'a [String],
    cancel: &'a AtomicBool,
    emit: &'a mut dyn FnMut(Msg),
    files: usize,
}

impl Walker<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn excluded(&self, name: &OsString) -> bool {
        name == ".git"
            || self
                .exclude
                .iter()
                .any(|e| name.to_str() == Some(e.as_str()))
    }

    fn entry(&mut self, rel: PathBuf, status: Status) {
        self.files += 1;
        (self.emit)(Msg::Entry(Entry { rel, status }));
        if self.files.is_multiple_of(PROGRESS_EVERY) {
            (self.emit)(Msg::Scanned(self.files));
        }
    }

    fn names(&self, dir: &Path) -> io::Result<BTreeMap<OsString, Kind>> {
        let mut out = BTreeMap::new();
        for e in fs::read_dir(dir)? {
            let e = e?;
            let name = e.file_name();
            if self.excluded(&name) {
                continue;
            }
            out.insert(name, kind_of(&e.path()).unwrap_or(Kind::Other));
        }
        Ok(out)
    }

    /// Every file under a directory that exists on one side only.
    fn one_sided(&mut self, dir: &Path, rel: &Path, status: &Status) {
        match self.names(dir) {
            Err(e) => self.entry(rel.to_path_buf(), Status::Error(read_error(&e))),
            Ok(names) => {
                for (name, kind) in names {
                    if self.cancelled() {
                        return;
                    }
                    let (p, r) = (dir.join(&name), rel.join(&name));
                    if kind == Kind::Dir {
                        self.one_sided(&p, &r, status);
                    } else {
                        self.entry(r, status.clone());
                    }
                }
            }
        }
    }

    fn walk(&mut self, left: &Path, right: &Path, rel: &Path) {
        let (ln, rn) = match (self.names(left), self.names(right)) {
            (Ok(l), Ok(r)) => (l, r),
            (Err(e), _) | (_, Err(e)) => {
                self.entry(rel.to_path_buf(), Status::Error(read_error(&e)));
                return;
            }
        };
        let mut all: BTreeMap<&OsString, (Option<Kind>, Option<Kind>)> = BTreeMap::new();
        for (n, k) in &ln {
            all.entry(n).or_default().0 = Some(*k);
        }
        for (n, k) in &rn {
            all.entry(n).or_default().1 = Some(*k);
        }
        for (name, (l, r)) in all {
            if self.cancelled() {
                return;
            }
            let (lp, rp, rel) = (left.join(name), right.join(name), rel.join(name));
            let status = match (l, r) {
                (Some(Kind::Dir), Some(Kind::Dir)) => {
                    self.walk(&lp, &rp, &rel);
                    continue;
                }
                (Some(Kind::Dir), None) => {
                    self.one_sided(&lp, &rel, &Status::LeftOnly);
                    continue;
                }
                (None, Some(Kind::Dir)) => {
                    self.one_sided(&rp, &rel, &Status::RightOnly);
                    continue;
                }
                (Some(_), None) => Status::LeftOnly,
                (None, Some(_)) => Status::RightOnly,
                (Some(l), Some(r)) => self.both(l, r, &lp, &rp),
                (None, None) => continue,
            };
            self.entry(rel, status);
        }
    }

    fn both(&self, l: Kind, r: Kind, lp: &Path, rp: &Path) -> Status {
        let describe = |k: Kind| match k {
            Kind::File => "file",
            Kind::Dir => "directory",
            Kind::Symlink => "symlink",
            Kind::Other => "special file",
        };
        let mismatch =
            || Status::Mismatch(format!("{} on left, {} on right", describe(l), describe(r)));
        let files = |a: &Path, b: &Path| match compare_files(a, b) {
            Ok(s) => s,
            Err(e) => Status::Error(read_error(&e)),
        };
        match (l, r) {
            (Kind::File, Kind::File) => files(lp, rp),
            (Kind::Symlink, Kind::Symlink) => {
                let target = |p: &Path| fs::read_link(p).map(|t| t.display().to_string());
                match (target(lp), target(rp)) {
                    (Ok(a), Ok(b)) if a == b => Status::Identical,
                    (Ok(left), Ok(right)) => Status::SymlinkDiffers { left, right },
                    (Err(e), _) | (_, Err(e)) => Status::Error(read_error(&e)),
                }
            }
            // `git difftool --dir-diff` hands over symlinks to working-tree
            // files, so a link opposite a regular file is followed.
            (Kind::File, Kind::Symlink) | (Kind::Symlink, Kind::File) => {
                let link = if l == Kind::Symlink { lp } else { rp };
                match fs::metadata(link) {
                    Ok(m) if m.is_file() => files(lp, rp),
                    Ok(_) => mismatch(),
                    Err(e) => Status::Error(format!(
                        "broken symlink: {}",
                        read_error(&e).trim_start_matches("can't read: ")
                    )),
                }
            }
            _ => mismatch(),
        }
    }
}

/// Run the comparison to completion on this thread, sending messages to `emit`.
pub fn compare(
    left: &Path,
    right: &Path,
    exclude: &[String],
    cancel: &AtomicBool,
    emit: &mut dyn FnMut(Msg),
) {
    let mut w = Walker {
        exclude,
        cancel,
        emit,
        files: 0,
    };
    w.walk(left, right, Path::new(""));
    let n = w.files;
    (w.emit)(Msg::Scanned(n));
    (w.emit)(Msg::Done);
}

/// A scan running on a worker thread.
pub struct Scan {
    pub rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
}

impl Scan {
    pub fn spawn(left: PathBuf, right: PathBuf, exclude: Vec<String>) -> Scan {
        let (tx, rx): (Sender<Msg>, Receiver<Msg>) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            // A closed receiver (UI gone) just makes sends fail; the scan finishes quietly.
            compare(&left, &right, &exclude, &flag, &mut |m| {
                let _ = tx.send(m);
            });
        });
        Scan { rx, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Collect messages for up to `limit`, stopping early at `Done`.
    /// Returns whether the scan finished.
    pub fn drain_for(&self, limit: Duration, out: &mut Vec<Msg>) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(Msg::Done) => {
                    out.push(Msg::Done);
                    return true;
                }
                Ok(m) => out.push(m),
                Err(_) => return false,
            }
        }
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write(root: &Path, rel: &str, content: &[u8]) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn scan(l: &Path, r: &Path) -> Vec<(String, Status)> {
        let mut out = vec![];
        let cancel = AtomicBool::new(false);
        let mut done = false;
        compare(l, r, &[], &cancel, &mut |m| match m {
            Msg::Entry(e) => out.push((e.rel.display().to_string(), e.status)),
            Msg::Done => done = true,
            Msg::Scanned(_) => {}
        });
        assert!(done);
        out
    }

    fn trees() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let (l, r) = (d.path().join("l"), d.path().join("r"));
        fs::create_dir_all(&l).unwrap();
        fs::create_dir_all(&r).unwrap();
        (d, l, r)
    }

    #[test]
    fn classifies_files() {
        let (_d, l, r) = trees();
        write(&l, "same.txt", b"x\n");
        write(&r, "same.txt", b"x\n");
        write(&l, "src/mod.rs", b"a\n");
        write(&r, "src/mod.rs", b"b\n");
        write(&l, "docs/old/flow.md", b"gone");
        write(&r, "new.rs", b"new");
        write(&r, "deep/er/file", b"new");
        let got = scan(&l, &r);
        let s = |name: &str| got.iter().find(|(n, _)| n == name).unwrap().1.clone();
        assert_eq!(s("same.txt"), Status::Identical);
        assert_eq!(s("src/mod.rs"), Status::Modified);
        assert_eq!(
            s("docs/old/flow.md"),
            Status::LeftOnly,
            "one-sided trees are flattened to files"
        );
        assert_eq!(s("new.rs"), Status::RightOnly);
        assert_eq!(s("deep/er/file"), Status::RightOnly);
        assert_eq!(got.len(), 5);
    }

    #[test]
    fn entries_stream_in_sorted_order() {
        let (_d, l, r) = trees();
        for n in ["b", "a/z", "a/b", "c"] {
            write(&l, n, b"1");
            write(&r, n, b"2");
        }
        let names: Vec<_> = scan(&l, &r).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["a/b", "a/z", "b", "c"]);
    }

    #[test]
    fn size_settles_text_files_without_reading_them_fully() {
        let (_d, l, r) = trees();
        write(&l, "big.txt", &vec![b'a'; 300_000]);
        let mut longer = vec![b'a'; 300_000];
        longer.push(b'b');
        write(&r, "big.txt", &longer);
        assert_eq!(scan(&l, &r)[0].1, Status::Modified);
    }

    #[test]
    fn identical_large_files_and_late_differences() {
        let (_d, l, r) = trees();
        let big = vec![b'x'; 500_000];
        write(&l, "same", &big);
        write(&r, "same", &big);
        let mut other = big.clone();
        other[400_000] = b'y';
        write(&l, "late", &big);
        write(&r, "late", &other);
        let got = scan(&l, &r);
        assert_eq!(got[0], ("late".into(), Status::Modified));
        assert_eq!(got[1], ("same".into(), Status::Identical));
    }

    #[test]
    fn binary_reports_the_first_differing_byte() {
        let (_d, l, r) = trees();
        let mut a = vec![0u8; 100_000];
        let mut b = a.clone();
        a[70_000] = 1;
        b[70_000] = 2;
        write(&l, "logo.png", &a);
        write(&r, "logo.png", &b);
        write(&l, "short.bin", b"\0\0ab");
        write(&r, "short.bin", b"\0\0abcd");
        write(&l, "u16.txt", &[0xFF, 0xFE, b'a', 0]);
        write(&r, "u16.txt", &[0xFF, 0xFE, b'b', 0]);
        let got = scan(&l, &r);
        let s = |n: &str| got.iter().find(|(x, _)| x == n).unwrap().1.clone();
        assert_eq!(s("logo.png"), Status::Binary { first_diff: 70_000 });
        assert_eq!(s("short.bin"), Status::Binary { first_diff: 4 });
        assert_eq!(s("u16.txt"), Status::Binary { first_diff: 2 });
        assert_eq!(s("logo.png").detail(), "binary, differs at byte 70,000");
    }

    #[test]
    fn type_mismatches() {
        let (_d, l, r) = trees();
        write(&l, "config", b"file");
        write(&r, "config/inner", b"x");
        let got = scan(&l, &r);
        assert_eq!(
            got,
            vec![(
                "config".into(),
                Status::Mismatch("file on left, directory on right".into())
            )]
        );
    }

    #[test]
    fn symlinks_are_compared_by_target_and_followed_opposite_files() {
        let (_d, l, r) = trees();
        symlink("target-a", l.join("same")).unwrap();
        symlink("target-a", r.join("same")).unwrap();
        symlink("one", l.join("diff")).unwrap();
        symlink("two", r.join("diff")).unwrap();
        // git dd: regular file on the left, symlink to the working-tree file on the right.
        write(&l, "wt.rs", b"old\n");
        let work = r.parent().unwrap().join("worktree.rs");
        fs::write(&work, b"new\n").unwrap();
        symlink(&work, r.join("wt.rs")).unwrap();
        write(&l, "wt_same.rs", b"s\n");
        let work2 = r.parent().unwrap().join("work2.rs");
        fs::write(&work2, b"s\n").unwrap();
        symlink(&work2, r.join("wt_same.rs")).unwrap();
        let got = scan(&l, &r);
        let s = |n: &str| got.iter().find(|(x, _)| x == n).unwrap().1.clone();
        assert_eq!(s("same"), Status::Identical);
        assert_eq!(
            s("diff"),
            Status::SymlinkDiffers {
                left: "one".into(),
                right: "two".into()
            }
        );
        assert_eq!(s("wt.rs"), Status::Modified);
        assert_eq!(s("wt_same.rs"), Status::Identical);
    }

    #[test]
    fn broken_symlinks_and_unreadable_entries_are_reported_not_fatal() {
        let (_d, l, r) = trees();
        write(&l, "a", b"1");
        symlink("/nonexistent/target", r.join("a")).unwrap();
        write(&l, "z", b"same");
        write(&r, "z", b"same");
        let got = scan(&l, &r);
        assert!(
            matches!(&got[0].1, Status::Error(m) if m.contains("broken symlink")),
            "{got:?}"
        );
        assert_eq!(
            got[1].1,
            Status::Identical,
            "the walk continues after an error"
        );
    }

    #[test]
    fn git_and_configured_names_are_skipped() {
        let (_d, l, r) = trees();
        write(&l, ".git/config", b"1");
        write(&r, ".git/config", b"2");
        write(&l, "target/debug/x", b"1");
        write(&l, "keep", b"1");
        write(&r, "keep", b"1");
        let mut out = vec![];
        let cancel = AtomicBool::new(false);
        compare(&l, &r, &["target".to_string()], &cancel, &mut |m| {
            if let Msg::Entry(e) = m {
                out.push(e.rel.display().to_string());
            }
        });
        assert_eq!(out, ["keep"]);
    }

    #[test]
    fn permission_only_changes_are_ignored() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, l, r) = trees();
        write(&l, "x.sh", b"#!/bin/sh\n");
        write(&r, "x.sh", b"#!/bin/sh\n");
        fs::set_permissions(l.join("x.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(scan(&l, &r)[0].1, Status::Identical);
    }

    #[test]
    fn cancelling_stops_the_scan_early_and_the_thread_reports_done() {
        let (_d, l, r) = trees();
        for i in 0..50 {
            write(&l, &format!("f{i:02}"), b"1");
            write(&r, &format!("f{i:02}"), b"2");
        }
        let cancel = AtomicBool::new(false);
        let mut seen = 0;
        compare(&l, &r, &[], &cancel, &mut |m| {
            if let Msg::Entry(_) = m {
                seen += 1;
                if seen == 3 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        });
        assert_eq!(seen, 3);

        let s = Scan::spawn(l, r, vec![]);
        let mut msgs = vec![];
        assert!(s.drain_for(Duration::from_secs(5), &mut msgs));
        assert_eq!(
            msgs.iter().filter(|m| matches!(m, Msg::Entry(_))).count(),
            50
        );
    }

    #[test]
    fn error_text_is_friendly() {
        let e = io::Error::from_raw_os_error(13);
        assert_eq!(read_error(&e), "can't read: permission denied");
    }
}
