//! Recognising the temporary copies git hands to a difftool (spec §8.2, spike S3).
//!
//! Observed on Linux with git 2.43 (macOS paths differ only in `$TMPDIR`):
//! - single file: `$LOCAL` is `<tmp>/git-blob-XXXXXX/<name>`, `$REMOTE` is the working file.
//! - `--dir-diff`: `$LOCAL`/`$REMOTE` are `<tmp>/git-difftool.XXXXXX/{left,right}/`.
//!   The right side holds symlinks into the working tree, or copies that git
//!   copies back afterwards with `--no-symlinks`; edits land either way.

use crate::document::Document;
use std::path::{Component, Path};

/// True when `path` is git's temporary copy of an old version.
/// The path is judged as given, never canonicalised: in `--dir-diff` mode the
/// right side is a symlink into the working tree and must stay writable.
pub fn is_git_temp(path: &Path) -> bool {
    let names: Vec<&str> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => n.to_str(),
            _ => None,
        })
        .collect();
    if names.iter().any(|n| n.starts_with("git-blob-")) {
        return true;
    }
    names
        .windows(2)
        .any(|w| w[0].starts_with("git-difftool.") && w[1] == "left")
}

/// Apply the read-only rules to a document loaded from `path`: git's temporary
/// copies are always read-only; `forced` carries a command-line flag's reason.
pub fn mark_readonly(doc: &mut Document, path: &Path, forced: Option<&'static str>) {
    if is_git_temp(path) {
        doc.readonly = true;
        doc.readonly_note = Some("git's old version");
    } else if let Some(why) = forced {
        doc.readonly = true;
        doc.readonly_note = Some(why);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_git_temp_sides() {
        for p in [
            "/tmp/git-blob-Wl3dDB/token.rs",
            "/var/folders/xy/T/git-blob-OgBFtB/token.rs",
            "/tmp/git-difftool.wW7Vhg/left/src/auth/token.rs",
            "/tmp/git-difftool.wW7Vhg/left/",
        ] {
            assert!(is_git_temp(Path::new(p)), "{p}");
        }
        for p in [
            "src/auth/token.rs",
            "/tmp/git-difftool.wW7Vhg/right/src/auth/token.rs",
            "/home/me/git-blob/token.rs",
            "/home/me/left/token.rs",
            "-",
        ] {
            assert!(!is_git_temp(Path::new(p)), "{p}");
        }
    }
}
