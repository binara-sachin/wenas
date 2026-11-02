//! Opening and atomically saving documents (spec §7.4).

use crate::document::{DiskId, Document};
use crate::input::{self, Loaded, Source};
use std::fmt;
use std::io::Write;
use std::path::Path;

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

/// Build a document from already-read input. Streams are always read-only.
pub fn document_from_loaded(loaded: Loaded, path: Option<&Path>) -> Document {
    let mtime = path
        .filter(|_| loaded.source == Source::File)
        .and_then(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    let disk = (loaded.source == Source::File).then(|| DiskId::of_bytes(&loaded.bytes, mtime));
    let mut doc = Document::from_bytes(loaded.bytes);
    doc.readonly = loaded.source == Source::Stream;
    if doc.readonly {
        doc.readonly_note = Some("stdin or a pipe");
    }
    doc.disk = disk;
    doc.path = path.map(Path::to_path_buf);
    doc
}

pub fn open(path: &Path) -> anyhow::Result<Document> {
    let loaded = input::read_input(path)?;
    Ok(document_from_loaded(loaded, Some(path)))
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

/// Save to the document's path. Follows symlinks so the real file is written,
/// writes a temp file beside it, fsyncs, copies permissions, then renames.
pub fn save(doc: &mut Document, force: bool) -> Result<(), SaveError> {
    if doc.readonly {
        return Err(SaveError::ReadOnly);
    }
    let path = doc.path.clone().ok_or(SaveError::NoPath)?;
    let real = match std::fs::canonicalize(&path) {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.clone(),
        Err(e) => return Err(e.into()),
    };
    if !force
        && let Some(known) = &doc.disk
        && changed_on_disk(&real, known)?
    {
        return Err(SaveError::ChangedOnDisk);
    }
    let bytes = doc.to_bytes();
    let dir = real
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    if let Ok(meta) = std::fs::metadata(&real) {
        std::fs::set_permissions(tmp.path(), meta.permissions())?;
    }
    tmp.persist(&real).map_err(|e| e.error)?;
    let mtime = std::fs::metadata(&real).and_then(|m| m.modified()).ok();
    doc.disk = Some(DiskId::of_bytes(&bytes, mtime));
    doc.saved_state = doc.state;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_atomically_and_detects_external_change() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.txt");
        std::fs::write(&p, "a\nb\n").unwrap();
        let mut doc = open(&p).unwrap();
        save(&mut doc, false).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"a\nb\n");

        std::fs::write(&p, "changed\n").unwrap();
        assert!(matches!(
            save(&mut doc, false),
            Err(SaveError::ChangedOnDisk)
        ));
        save(&mut doc, true).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"a\nb\n");
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
        let mut doc = open(&link).unwrap();
        let s = doc.append(b"y\n");
        let n = doc.len();
        doc.splice(n..n, &[s]);
        save(&mut doc, false).unwrap();
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
    fn readonly_refuses() {
        let mut doc = Document::from_bytes(b"a".to_vec());
        doc.readonly = true;
        assert!(matches!(save(&mut doc, false), Err(SaveError::ReadOnly)));
    }
}
