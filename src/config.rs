//! Optional TOML config (spec §11.2). Unknown keys warn; a missing file means defaults.

use crate::diff::Whitespace;
use crate::theme::ThemeMode;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WhitespaceSetting {
    Off,
    Change,
    All,
}

impl From<WhitespaceSetting> for Whitespace {
    fn from(w: WhitespaceSetting) -> Self {
        match w {
            WhitespaceSetting::Off => Whitespace::Off,
            WhitespaceSetting::Change => Whitespace::Change,
            WhitespaceSetting::All => Whitespace::All,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DirectoryConfig {
    pub exclude: Vec<String>,
    pub show_identical: bool,
}

impl Default for DirectoryConfig {
    fn default() -> Self {
        DirectoryConfig {
            exclude: vec![".git".into(), "node_modules".into(), "target".into()],
            show_identical: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeMode,
    pub tab_width: usize,
    pub whitespace: WhitespaceSetting,
    pub unified_below_columns: u16,
    /// Colour overrides by name, as `#rrggbb`.
    pub colors: std::collections::BTreeMap<String, String>,
    pub directory: DirectoryConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: ThemeMode::Auto,
            tab_width: 4,
            whitespace: WhitespaceSetting::Off,
            unified_below_columns: 100,
            colors: Default::default(),
            directory: DirectoryConfig::default(),
        }
    }
}

/// Top-level keys, with the keys allowed inside a section (`None` for a plain
/// value or a free-form table whose entries are validated separately).
const SCHEMA: &[(&str, Option<&[&str]>)] = &[
    ("theme", None),
    ("tab_width", None),
    ("whitespace", None),
    ("unified_below_columns", None),
    ("colors", None),
    ("directory", Some(&["exclude", "show_identical"])),
];

/// Warnings for every key the config does not know, at any depth.
fn unknown_keys(table: &toml::Table) -> Vec<String> {
    let mut out = Vec::new();
    for (key, value) in table {
        match SCHEMA.iter().find(|(k, _)| k == key) {
            None => out.push(format!("unknown config key `{key}` ignored")),
            Some((_, Some(allowed))) => {
                if let Some(inner) = value.as_table() {
                    for k in inner.keys().filter(|k| !allowed.contains(&k.as_str())) {
                        out.push(format!("unknown config key `{key}.{k}` ignored"));
                    }
                }
            }
            Some(_) => {}
        }
    }
    out
}

pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/wenas/config.toml"))
}

/// Parse config text. Returns the config plus warnings (unknown keys).
pub fn parse(text: &str) -> Result<(Config, Vec<String>), String> {
    let table: toml::Table = text.parse().map_err(|e| format!("{e}"))?;
    let warnings = unknown_keys(&table);
    let mut cfg: Config = table.try_into().map_err(|e| format!("{e}"))?;
    cfg.tab_width = cfg.tab_width.clamp(1, 16);
    let mut warnings: Vec<String> = warnings;
    cfg.colors.retain(|name, value| {
        if !crate::theme::COLOR_NAMES.contains(&name.as_str()) {
            warnings.push(format!("unknown colour `{name}` ignored"));
            false
        } else if crate::theme::parse_hex(value).is_none() {
            warnings.push(format!(
                "colour `{name}` must look like \"#rrggbb\"; ignored"
            ));
            false
        } else {
            true
        }
    });
    Ok((cfg, warnings))
}

/// Load from an explicit path (must exist) or the default location (may be absent).
/// Problems are printed to stderr and never abort the run, except an explicit
/// `--config` path that cannot be read.
pub fn load(explicit: Option<&Path>) -> anyhow::Result<Config> {
    let (path, required) = match explicit {
        Some(p) => (Some(p.to_path_buf()), true),
        None => (default_path(), false),
    };
    let Some(path) = path else {
        return Ok(Config::default());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Config::default());
        }
        Err(e) => anyhow::bail!("cannot read config {}: {e}", path.display()),
    };
    match parse(&text) {
        Ok((cfg, warnings)) => {
            for w in warnings {
                eprintln!("wenas: {}: {w}", path.display());
            }
            Ok(cfg)
        }
        Err(e) => {
            eprintln!("wenas: {}: {e}; using defaults", path.display());
            Ok(Config::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let (c, w) = parse("").unwrap();
        assert!(w.is_empty());
        assert_eq!((c.tab_width, c.unified_below_columns), (4, 100));
        let (c, _) = parse("theme = \"light\"\ntab_width = 8\nwhitespace = \"all\"\n").unwrap();
        assert_eq!(c.theme, ThemeMode::Light);
        assert_eq!(c.tab_width, 8);
        assert_eq!(c.whitespace, WhitespaceSetting::All);
    }

    #[test]
    fn unknown_keys_warn_but_do_not_fail() {
        let (c, w) = parse("bogus = 1\n[colors]\nchanged_bg = \"#000000\"\n").unwrap();
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("bogus"));
        assert_eq!(c.tab_width, 4);
    }

    #[test]
    fn unknown_keys_inside_sections_warn_too() {
        let (c, w) =
            parse("[directory]\nbogus = 1\nshow_identical = true\nexclude = [\"x\"]\n").unwrap();
        assert_eq!(
            w,
            vec!["unknown config key `directory.bogus` ignored".to_string()]
        );
        assert!(c.directory.show_identical, "known keys still apply");
        let (_, w) = parse("[directory]\nexclude = []\n").unwrap();
        assert!(w.is_empty());
        let (_, w) = parse("top = 1\n[directory]\nnope = 2\n").unwrap();
        assert_eq!(w.len(), 2);
    }

    #[test]
    fn colour_overrides_are_validated() {
        let (c, w) =
            parse("[colors]\nchanged_bg = \"#2a2f3a\"\nnope = \"#ffffff\"\namber = \"red\"\n")
                .unwrap();
        assert_eq!(c.colors.len(), 1);
        assert_eq!(c.colors["changed_bg"], "#2a2f3a");
        assert_eq!(w.len(), 2, "{w:?}");
    }

    #[test]
    fn bad_values_are_errors() {
        assert!(parse("theme = \"purple\"").is_err());
        assert!(parse("tab_width = ").is_err());
    }
}
