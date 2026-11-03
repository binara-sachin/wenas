//! Handing a file to `$EDITOR` (spec §5.2 `editor`, §7.3).
//!
//! The UI decides *when* (the `e` key, with its read-only and unsaved checks)
//! and what to do with the result (reload as one undo step); this module owns
//! *what* is run.

use crate::ops::Side;
use std::path::PathBuf;

/// What the event loop must do outside the UI: run `$EDITOR`, then call `finish_editor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorRequest {
    pub side: Side,
    pub line: usize,
    pub path: PathBuf,
}

/// `$EDITOR`, then `$VISUAL`, then `vi` (spec §7.3).
pub fn resolve_editor() -> String {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    get("EDITOR")
        .or_else(|| get("VISUAL"))
        .unwrap_or_else(|| "vi".into())
}

/// `<editor> +<line> <file>`. Run through `sh` so editors configured with
/// arguments (`code -w`) work; the file and line are passed as real arguments,
/// never interpolated into the command string.
pub fn editor_command(editor: &str, req: &EditorRequest) -> std::process::Command {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg("wenas")
        .arg(format!("+{}", req.line))
        .arg(&req.path);
    cmd
}

/// Run the editor to completion. `Ok(true)` when it exited successfully.
pub fn spawn_editor(editor: &str, req: &EditorRequest) -> Result<bool, String> {
    let mut cmd = editor_command(editor, req);
    // wenas's own stdin may be a pipe (`-` as an input); the editor needs the terminal.
    if let Ok(tty) = std::fs::File::open("/dev/tty") {
        cmd.stdin(tty);
    }
    cmd.status().map(|s| s.success()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(line: usize, path: &str) -> EditorRequest {
        EditorRequest {
            side: Side::Right,
            line,
            path: path.into(),
        }
    }

    #[test]
    fn editor_command_passes_line_and_path_as_arguments() {
        let cmd = editor_command("nvim", &req(41, "it's a file; rm -rf x.rs"));
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "-c",
                "nvim \"$@\"",
                "wenas",
                "+41",
                "it's a file; rm -rf x.rs"
            ]
        );
    }

    #[test]
    fn editor_runs_with_arguments_and_reports_status() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "a\n").unwrap();
        let r = req(3, file.to_str().unwrap());
        // An "editor" with its own arguments that records what it was given.
        let ed = "sh -c 'echo \"$1 $2\" >> \"$2\"' ed";
        assert_eq!(spawn_editor(ed, &r), Ok(true));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("a\n+3 {}\n", file.display())
        );
        assert_eq!(spawn_editor("false", &r), Ok(false));
    }
}
