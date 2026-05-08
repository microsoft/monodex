//! Purpose: Resolve filesystem paths for monodex tool state (config, context, crawl config).
//!
//! Edit here when: Changing where tool state lives on disk, adding MONODEX_HOME-style overrides,
//!   or adding accessors for new files under the tool home.
//! Do not edit here for: Config schema (see app/config.rs), crawl filtering (see engine/crawl_config.rs),
//!   or default-context semantics (see app/context.rs).

use anyhow::{Result, anyhow};
use std::path::PathBuf;

/// Resolved filesystem paths for monodex tool state.
///
/// This struct owns the resolved tool home directory and derived paths.
/// It is constructed once at startup (via `resolve_from_env`) or explicitly
/// in tests (via `for_test`). After construction, all path access is deterministic
/// and does not read environment variables.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The tool home directory (resolved from MONODEX_HOME or ~/.monodex)
    pub tool_home: PathBuf,
    /// Path to the config file (resolved from --config, MONODEX_CONFIG, or <tool_home>/config.json)
    pub config_path: PathBuf,
}

impl Paths {
    /// Resolve paths from environment variables and CLI overrides.
    ///
    /// Tool home resolution:
    /// - If `MONODEX_HOME` is set and non-empty (after trim), uses that path.
    ///   Relative paths are converted to absolute using current working directory.
    /// - Otherwise falls back to `<home>/.monodex`.
    ///
    /// Config path resolution (precedence):
    /// 1. `config_override` (from CLI `--config` flag)
    /// 2. `MONODEX_CONFIG` environment variable
    /// 3. `<tool_home>/config.json`
    ///
    /// Relative paths in `config_override` and `MONODEX_CONFIG` are preserved as-is.
    pub fn resolve_from_env(config_override: Option<PathBuf>) -> Result<Self> {
        // Resolve tool home
        let tool_home = Self::resolve_tool_home()?;

        // Resolve config path
        let config_path = match config_override {
            Some(path) => path,
            None => {
                if let Ok(env_val) = std::env::var("MONODEX_CONFIG") {
                    let trimmed = env_val.trim();
                    if !trimmed.is_empty() {
                        PathBuf::from(trimmed)
                    } else {
                        tool_home.join("config.json")
                    }
                } else {
                    tool_home.join("config.json")
                }
            }
        };

        Ok(Paths {
            tool_home,
            config_path,
        })
    }

    /// Construct a `Paths` from an explicit tool-home directory.
    ///
    /// Intended for tests, but safe to use from any code that has already
    /// resolved its own tool home. Sets `config_path` to `<tool_home>/config.json`.
    pub fn for_test(tool_home: PathBuf) -> Self {
        let config_path = tool_home.join("config.json");
        Paths {
            tool_home,
            config_path,
        }
    }

    /// Path to the context file (for `monodex use`).
    ///
    /// The context file stores the default catalog and label.
    pub fn context_file(&self) -> PathBuf {
        self.tool_home.join("context.json")
    }

    /// Path to the user-global crawl config file.
    ///
    /// This file is optional and may not exist.
    pub fn crawl_config(&self) -> PathBuf {
        self.tool_home.join("crawl.json")
    }

    /// Inner tool home resolution logic.
    fn resolve_tool_home() -> Result<PathBuf> {
        // Check MONODEX_HOME env var
        if let Ok(env_val) = std::env::var("MONODEX_HOME") {
            let trimmed = env_val.trim();
            if !trimmed.is_empty() {
                let path = PathBuf::from(trimmed);

                // If relative, convert to absolute using current working directory
                if path.is_relative() {
                    let cwd = std::env::current_dir()?;
                    let absolute = cwd.join(&path);
                    eprintln!(
                        "MONODEX_HOME is a relative path; resolved to {}",
                        absolute.display()
                    );
                    return Ok(absolute);
                }

                return Ok(path);
            }
        }

        // Fall back to <home>/.monodex
        let home = dirs::home_dir().ok_or_else(|| {
            anyhow!("Could not determine home directory. Set MONODEX_HOME or ensure $HOME is set.")
        })?;

        Ok(home.join(".monodex"))
    }
}

/// Called once from main() early in startup. Prints a one-line warning to stderr
/// if any files exist at the old pre-PR locations and no files exist at the
/// new locations.
pub fn warn_old_tool_home_if_present(paths: &Paths) {
    let new_home = &paths.tool_home;

    // Check if any new files exist
    let new_config = new_home.join("config.json");
    let new_context = new_home.join("context.json");
    let new_crawl = new_home.join("crawl.json");

    if new_config.exists() || new_context.exists() || new_crawl.exists() {
        // User has already migrated (at least partially)
        return;
    }

    // Check old locations
    // Old config.json and context.json were at hardcoded ~/.config/monodex/
    let old_hardcoded_dir = dirs::home_dir().map(|h| h.join(".config").join("monodex"));

    // Old crawl.json used dirs::config_dir() (platform-dependent)
    let old_crawl_path = dirs::config_dir().map(|d| d.join("monodex").join("crawl.json"));

    let mut old_files_found: Vec<String> = Vec::new();

    // Check old hardcoded paths
    if let Some(ref old_dir) = old_hardcoded_dir {
        let old_config = old_dir.join("config.json");
        let old_context = old_dir.join("context.json");

        if old_config.exists() {
            old_files_found.push(format!("config: {}", old_config.display()));
        }
        if old_context.exists() {
            old_files_found.push(format!("context: {}", old_context.display()));
        }
    }

    // Check old platform-dependent crawl.json
    if let Some(ref old_crawl) = old_crawl_path
        && old_crawl.exists()
    {
        old_files_found.push(format!("crawl config: {}", old_crawl.display()));
    }

    if !old_files_found.is_empty() {
        // Yellow color: \x1b[33m, Reset: \x1b[0m
        eprintln!("\x1b[33mWarning: Old monodex config files found.\x1b[0m");
        eprintln!(
            "Please migrate to {} by moving your files:",
            new_home.display()
        );
        for file in &old_files_found {
            eprintln!("  {}", file);
        }

        // Provide a helpful migration command if all old files are in the same hardcoded directory
        if let Some(ref old_dir) = old_hardcoded_dir {
            let all_in_hardcoded = old_crawl_path
                .as_ref()
                .map(|p| p.parent().map(|p| p == old_dir).unwrap_or(false))
                .unwrap_or(true);

            let has_old_config = old_dir.join("config.json").exists();
            let has_old_context = old_dir.join("context.json").exists();
            let has_old_hardcoded_files = has_old_config || has_old_context;

            if all_in_hardcoded && has_old_hardcoded_files {
                eprintln!(
                    "  Suggestion: mv {} {}",
                    old_dir.display(),
                    new_home.display()
                );
            }
        }
        eprintln!(); // Blank line before CLI banner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    /// RAII guard to restore MONODEX_HOME and MONODEX_CONFIG on drop.
    /// Captures the current values on construction and restores them (or removes them)
    /// when dropped, even on panic.
    struct EnvGuard {
        original_home: Option<String>,
        original_config: Option<String>,
    }

    impl EnvGuard {
        fn capture() -> Self {
            let original_home = env::var("MONODEX_HOME").ok();
            let original_config = env::var("MONODEX_CONFIG").ok();
            EnvGuard {
                original_home,
                original_config,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // Restore MONODEX_HOME
            if let Some(ref val) = self.original_home {
                unsafe {
                    env::set_var("MONODEX_HOME", val);
                }
            } else {
                unsafe {
                    env::remove_var("MONODEX_HOME");
                }
            }
            // Restore MONODEX_CONFIG
            if let Some(ref val) = self.original_config {
                unsafe {
                    env::set_var("MONODEX_CONFIG", val);
                }
            } else {
                unsafe {
                    env::remove_var("MONODEX_CONFIG");
                }
            }
        }
    }

    #[test]
    #[serial(monodex_home_resolver)]
    fn test_resolve_from_env_uses_monodex_home() {
        let _guard = EnvGuard::capture();
        let temp_dir = tempfile::tempdir().unwrap();

        unsafe {
            env::set_var("MONODEX_HOME", temp_dir.path());
            env::remove_var("MONODEX_CONFIG");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert_eq!(paths.tool_home, temp_dir.path());

        // Config path should default to <tool_home>/config.json
        assert_eq!(paths.config_path, temp_dir.path().join("config.json"));
    }

    #[test]
    #[serial(monodex_home_resolver)]
    fn test_resolve_from_env_falls_back_to_home() {
        let _guard = EnvGuard::capture();

        unsafe {
            env::remove_var("MONODEX_HOME");
            env::remove_var("MONODEX_CONFIG");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert!(paths.tool_home.ends_with(".monodex"));
        assert_eq!(paths.config_path, paths.tool_home.join("config.json"));
    }

    #[test]
    #[serial(monodex_home_resolver)]
    fn test_resolve_from_env_config_override_wins() {
        let _guard = EnvGuard::capture();
        let temp_dir = tempfile::tempdir().unwrap();
        let override_path = PathBuf::from("/override/config.json");

        unsafe {
            env::set_var("MONODEX_HOME", temp_dir.path());
            env::set_var("MONODEX_CONFIG", "/env/config.json");
        }

        let paths = Paths::resolve_from_env(Some(override_path.clone())).unwrap();
        assert_eq!(paths.config_path, override_path);
    }

    #[test]
    #[serial(monodex_home_resolver)]
    fn test_resolve_from_env_uses_monodex_config() {
        let _guard = EnvGuard::capture();
        let temp_dir = tempfile::tempdir().unwrap();

        unsafe {
            env::set_var("MONODEX_HOME", temp_dir.path());
            env::set_var("MONODEX_CONFIG", "/env/config.json");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert_eq!(paths.config_path, PathBuf::from("/env/config.json"));
    }

    #[test]
    #[serial(monodex_home_resolver)]
    fn test_resolve_from_env_empty_monodex_config_falls_back() {
        let _guard = EnvGuard::capture();
        let temp_dir = tempfile::tempdir().unwrap();

        unsafe {
            env::set_var("MONODEX_HOME", temp_dir.path());
            env::set_var("MONODEX_CONFIG", "");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert_eq!(paths.config_path, temp_dir.path().join("config.json"));

        // Also test whitespace-only
        unsafe {
            env::set_var("MONODEX_CONFIG", "   ");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert_eq!(paths.config_path, temp_dir.path().join("config.json"));
    }

    #[test]
    #[serial(monodex_home_resolver)]
    fn test_resolve_from_env_relative_monodex_config_preserved() {
        let _guard = EnvGuard::capture();
        let temp_dir = tempfile::tempdir().unwrap();

        unsafe {
            env::set_var("MONODEX_HOME", temp_dir.path());
            env::set_var("MONODEX_CONFIG", "rel/path.json");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert_eq!(paths.config_path, PathBuf::from("rel/path.json"));
    }

    #[test]
    #[serial(monodex_home_resolver)]
    fn test_resolve_from_env_relative_monodex_home_absolutized() {
        let _guard = EnvGuard::capture();

        unsafe {
            env::set_var("MONODEX_HOME", "relative/path");
            env::remove_var("MONODEX_CONFIG");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        let cwd = std::fs::canonicalize(".").unwrap();
        let expected = cwd.join("relative/path");
        assert_eq!(paths.tool_home, expected);
    }

    #[test]
    fn test_for_test_constructs_paths() {
        let temp_dir = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(temp_dir.path().into());

        assert_eq!(paths.tool_home, temp_dir.path());
        assert_eq!(paths.config_path, temp_dir.path().join("config.json"));
        assert_eq!(paths.context_file(), temp_dir.path().join("context.json"));
        assert_eq!(paths.crawl_config(), temp_dir.path().join("crawl.json"));
    }
}
