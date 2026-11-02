//! Reading inputs and classifying their bytes (spec §5.2 `input`, §10).

use std::io::Read;
use std::path::Path;

/// Bytes inspected for a NUL when deciding if a file is binary (git's rule).
pub const BINARY_SNIFF_LEN: usize = 8 * 1024;
/// Ask before loading anything larger than this.
pub const LARGE_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// Refuse anything larger than this.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
}

impl Eol {
    pub fn bytes(self) -> &'static [u8] {
        match self {
            Eol::Lf => b"\n",
            Eol::Crlf => b"\r\n",
        }
    }
}

/// Line-ending style of a whole file, as shown in the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EolStyle {
    /// No line ending at all (empty file or a single unterminated line).
    None,
    Lf,
    Crlf,
    Mixed,
}

impl EolStyle {
    pub fn label(self) -> &'static str {
        match self {
            EolStyle::None => "none",
            EolStyle::Lf => "LF",
            EolStyle::Crlf => "CRLF",
            EolStyle::Mixed => "mixed",
        }
    }
}

/// NUL byte in the first 8 KB.
pub fn is_binary(bytes: &[u8]) -> bool {
    memchr::memchr(0, &bytes[..bytes.len().min(BINARY_SNIFF_LEN)]).is_some()
}

/// UTF-16 byte-order mark. v1 treats these as binary (spec D24).
pub fn has_utf16_bom(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF])
}

/// Index of the first differing byte, or `None` if the slices are equal.
pub fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    let common = a.len().min(b.len());
    match a[..common]
        .iter()
        .zip(&b[..common])
        .position(|(x, y)| x != y)
    {
        Some(i) => Some(i),
        None if a.len() == b.len() => None,
        None => Some(common),
    }
}

/// Where the bytes came from; decides read-only-ness and size checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    File,
    /// stdin or a named pipe: read fully, always read-only, no size check.
    Stream,
}

pub struct Loaded {
    pub bytes: Vec<u8>,
    pub source: Source,
}

/// "148 MB", "1.5 GB" (binary units, one decimal above 1 GB).
pub fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= 1024.0 * MB {
        format!("{:.1} GB", b / (1024.0 * MB))
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024).max(1))
    }
}

/// How a file's size affects loading it (spec §9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeCheck {
    Ok,
    /// Over the "ask first" threshold.
    Large(u64),
}

/// Look at a path's size without reading it. Streams (stdin, pipes) are never
/// checked. Files over `max` are refused outright.
pub fn check_size(path: &Path, large: u64, max: u64) -> anyhow::Result<SizeCheck> {
    if path.as_os_str() == "-" {
        return Ok(SizeCheck::Ok);
    }
    let Ok(meta) = std::fs::metadata(path) else {
        // Missing or unreadable: reading will report it properly.
        return Ok(SizeCheck::Ok);
    };
    if !meta.is_file() {
        return Ok(SizeCheck::Ok);
    }
    if meta.len() > max {
        anyhow::bail!(
            "{} is {}, over the {} limit",
            path.display(),
            human_size(meta.len()),
            human_size(max)
        );
    }
    Ok(if meta.len() > large {
        SizeCheck::Large(meta.len())
    } else {
        SizeCheck::Ok
    })
}

/// Read a path (or `-` for stdin) fully into memory.
pub fn read_input(arg: &Path) -> anyhow::Result<Loaded> {
    use anyhow::Context;
    if arg.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut bytes)
            .context("reading stdin")?;
        return Ok(Loaded {
            bytes,
            source: Source::Stream,
        });
    }
    let meta = std::fs::metadata(arg).with_context(|| format!("cannot read {}", arg.display()))?;
    if meta.is_dir() {
        anyhow::bail!("{} is a directory", arg.display());
    }
    if is_stream(&meta) {
        let mut bytes = Vec::new();
        std::fs::File::open(arg)
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .with_context(|| format!("cannot read {}", arg.display()))?;
        return Ok(Loaded {
            bytes,
            source: Source::Stream,
        });
    }
    if meta.len() > MAX_FILE_BYTES {
        anyhow::bail!("{} is larger than 1 GB", arg.display());
    }
    let bytes = std::fs::read(arg).with_context(|| format!("cannot read {}", arg.display()))?;
    Ok(Loaded {
        bytes,
        source: Source::File,
    })
}

#[cfg(unix)]
fn is_stream(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let t = meta.file_type();
    t.is_fifo() || t.is_char_device() || t.is_socket()
}
#[cfg(not(unix))]
fn is_stream(_: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_detection_uses_first_8k_only() {
        assert!(is_binary(b"ab\0cd"));
        let mut v = vec![b'a'; BINARY_SNIFF_LEN];
        v.push(0);
        assert!(!is_binary(&v));
    }

    #[test]
    fn first_difference_cases() {
        assert_eq!(first_difference(b"abc", b"abc"), None);
        assert_eq!(first_difference(b"abc", b"abd"), Some(2));
        assert_eq!(first_difference(b"abc", b"ab"), Some(2));
        assert_eq!(first_difference(b"", b"x"), Some(0));
    }

    #[test]
    fn size_formatting_and_checks() {
        assert_eq!(human_size(100), "1 KB");
        assert_eq!(human_size(150 * 1024 * 1024), "150 MB");
        assert_eq!(human_size(3 * 512 * 1024 * 1024), "1.5 GB");
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        std::fs::write(&f, vec![b'x'; 100]).unwrap();
        assert_eq!(check_size(&f, 1000, 10_000).unwrap(), SizeCheck::Ok);
        assert_eq!(check_size(&f, 50, 10_000).unwrap(), SizeCheck::Large(100));
        assert!(
            check_size(&f, 10, 50)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
        assert_eq!(check_size(Path::new("-"), 0, 0).unwrap(), SizeCheck::Ok);
        assert_eq!(
            check_size(&d.path().join("missing"), 0, 0).unwrap(),
            SizeCheck::Ok
        );
        assert_eq!(check_size(d.path(), 0, 0).unwrap(), SizeCheck::Ok);
    }

    #[test]
    fn utf16_bom() {
        assert!(has_utf16_bom(&[0xFF, 0xFE, b'a', 0]));
        assert!(!has_utf16_bom(b"abc"));
    }
}
