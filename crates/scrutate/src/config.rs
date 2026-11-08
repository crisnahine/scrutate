//! Configuration files. Values become defaults for the matching command-line options by being
//! exported as the options' environment variables before argument parsing, so the precedence is
//! always: command line > environment > configuration file > built-in default, and every option
//! documents the variable that configures it (`scrutate grep --help`).
//!
//! Resolution order (first file found wins): `$SCRUTATE_CONFIG`, `<root>/.scrutate.toml`,
//! `$XDG_CONFIG_HOME/scrutate/config.toml` (or `~/.config/scrutate/config.toml`).
//!
//! ```toml
//! [grep]
//! hidden = true            # SCRUTATE_GREP_HIDDEN
//! no_ignore = false        # SCRUTATE_GREP_NO_IGNORE
//! threads = 4              # SCRUTATE_THREADS
//! max_filesize = "10M"     # SCRUTATE_GREP_MAX_FILESIZE
//! globs = ["!vendor/**"]   # SCRUTATE_GREP_GLOBS (comma separated)
//! types = ["py", "rs"]     # SCRUTATE_GREP_TYPES
//! smart_case = true        # SCRUTATE_GREP_SMART_CASE
//!
//! [index]
//! exclude_dirs = ["generated", "third_party"]   # SCRUTATE_INDEX_EXCLUDE_DIRS
//! refresh_interval = 2.0                         # SCRUTATE_REFRESH_INTERVAL (seconds, `serve`)
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub grep: Grep,
    #[serde(default)]
    pub index: IndexCfg,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grep {
    pub hidden: Option<bool>,
    pub no_ignore: Option<bool>,
    pub follow: Option<bool>,
    pub smart_case: Option<bool>,
    pub threads: Option<usize>,
    pub max_filesize: Option<String>,
    pub max_depth: Option<usize>,
    #[serde(default)]
    pub globs: Vec<String>,
    #[serde(default)]
    pub types: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexCfg {
    #[serde(default)]
    pub exclude_dirs: Vec<String>,
    pub refresh_interval: Option<f64>,
}

/// The configuration file that applies to `root`, if any.
pub fn locate(root: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SCRUTATE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let local = root.join(".scrutate.toml");
    if local.is_file() {
        return Some(local);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    base.map(|b| b.join("scrutate").join("config.toml")).filter(|p| p.is_file())
}

pub fn load(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

/// Export the configured values as environment variables unless the variable is already set
/// (an explicit environment setting beats the file).
pub fn apply_as_env(cfg: &Config) {
    fn set(name: &str, value: String) {
        if std::env::var_os(name).is_none() {
            // SAFETY (soundness, not memory): called once at startup before any thread is spawned,
            // which is the condition under which setting process environment variables is sound.
            unsafe { std::env::set_var(name, value) };
        }
    }
    let b = |v: bool| if v { "true" } else { "false" }.to_string();
    if let Some(v) = cfg.grep.hidden {
        set("SCRUTATE_GREP_HIDDEN", b(v));
    }
    if let Some(v) = cfg.grep.no_ignore {
        set("SCRUTATE_GREP_NO_IGNORE", b(v));
    }
    if let Some(v) = cfg.grep.follow {
        set("SCRUTATE_GREP_FOLLOW", b(v));
    }
    if let Some(v) = cfg.grep.smart_case {
        set("SCRUTATE_GREP_SMART_CASE", b(v));
    }
    if let Some(v) = cfg.grep.threads {
        set("SCRUTATE_THREADS", v.to_string());
    }
    if let Some(v) = &cfg.grep.max_filesize {
        set("SCRUTATE_GREP_MAX_FILESIZE", v.clone());
    }
    if let Some(v) = cfg.grep.max_depth {
        set("SCRUTATE_GREP_MAX_DEPTH", v.to_string());
    }
    if !cfg.grep.globs.is_empty() {
        set("SCRUTATE_GREP_GLOBS", cfg.grep.globs.join(","));
    }
    if !cfg.grep.types.is_empty() {
        set("SCRUTATE_GREP_TYPES", cfg.grep.types.join(","));
    }
    if !cfg.index.exclude_dirs.is_empty() {
        set("SCRUTATE_INDEX_EXCLUDE_DIRS", cfg.index.exclude_dirs.join(","));
    }
    if let Some(v) = cfg.index.refresh_interval {
        set("SCRUTATE_REFRESH_INTERVAL", v.to_string());
    }
}

/// Root directory named on the command line (`--root`, before parsing), so the local config can be found.
pub fn root_from_args(args: &[String]) -> PathBuf {
    for (i, a) in args.iter().enumerate() {
        if let Some(v) = a.strip_prefix("--root=") {
            return PathBuf::from(v);
        }
        if a == "--root" {
            if let Some(v) = args.get(i + 1) {
                return PathBuf::from(v);
            }
        }
    }
    std::env::var_os("SCRUTATE_ROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_example() {
        let c: Config =
            toml::from_str("[grep]\nhidden = true\nthreads = 4\nglobs = [\"!vendor/**\"]\n[index]\nexclude_dirs = [\"generated\"]\nrefresh_interval = 2.0\n")
                .unwrap();
        assert_eq!(c.grep.hidden, Some(true));
        assert_eq!(c.grep.threads, Some(4));
        assert_eq!(c.grep.globs, vec!["!vendor/**"]);
        assert_eq!(c.index.exclude_dirs, vec!["generated"]);
        assert_eq!(c.index.refresh_interval, Some(2.0));
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(toml::from_str::<Config>("[grep]\nhiden = true\n").is_err());
    }
}
