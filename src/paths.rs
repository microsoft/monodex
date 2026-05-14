//! Purpose: Resolve filesystem paths for monodex tool state (config, context, crawl config).
//!
//! Edit here when: Changing where tool state lives on disk, adding MONODEX_CONFIG_FOLDER-style overrides,
//!   or adding accessors for new files under the config folder.
//! Do not edit here for: Config schema (see app/config.rs), crawl filtering (see engine/crawl_config.rs),
//!   or default-context semantics (see app/context.rs).

use anyhow::{Result, anyhow};
use std::path::PathBuf;

/// Resolved filesystem paths for monodex tool state.
///
/// This struct owns the resolved config folder and provides accessors for derived paths.
/// It is constructed once at startup (via `resolve_from_env`) or explicitly
/// in tests (via `for_test`). After construction, all path access is deterministic
/// and does not read environment variables.
#[derive(Debug, Clone)]
pub struct Paths {
    /// The config folder (resolved from MONODEX_CONFIG_FOLDER or ~/.monodex)
    pub config_folder: PathBuf,
}

impl Paths {
    /// Resolve paths from environment variables and CLI overrides.
    ///
    /// Config folder resolution (precedence):
    /// 1. `config_folder_override` (from CLI `--config-folder` flag)
    /// 2. `MONODEX_CONFIG_FOLDER` environment variable
    /// 3. `<home>/.monodex` (default)
    ///
    /// Relative paths from both the flag and env var are resolved against
    /// the current working directory at process start.
    /// Empty or whitespace-only values are treated as unset.
    pub fn resolve_from_env(config_folder_override: Option<PathBuf>) -> Result<Self> {
        let config_folder = Self::resolve_config_folder(config_folder_override)?;
        Ok(Paths { config_folder })
    }

    /// Construct a `Paths` from an explicit config folder.
    ///
    /// Intended for tests, but safe to use from any code that has already
    /// resolved its own config folder.
    pub fn for_test(config_folder: PathBuf) -> Self {
        Paths { config_folder }
    }

    /// Path to the config file.
    pub fn config_file(&self) -> PathBuf {
        self.config_folder.join("monodex-config.json")
    }

    /// Path to the context file (for `monodex use`).
    ///
    /// The context file stores the default catalog and label.
    pub fn context_file(&self) -> PathBuf {
        self.config_folder.join("monodex-state.json")
    }

    /// Path to the user-global crawl config file.
    ///
    /// This file is optional and may not exist.
    pub fn crawl_config(&self) -> PathBuf {
        self.config_folder.join("monodex-crawl-config.json")
    }

    /// Inner config folder resolution logic.
    fn resolve_config_folder(override_path: Option<PathBuf>) -> Result<PathBuf> {
        // Check CLI override first (trim and treat empty/whitespace as unset)
        if let Some(path) = override_path {
            let path_str = path.to_string_lossy();
            let trimmed = path_str.trim();
            if !trimmed.is_empty() {
                return Self::absolutize_if_relative(PathBuf::from(trimmed));
            }
            // Empty/whitespace override falls through to env var
        }

        // Check MONODEX_CONFIG_FOLDER env var
        if let Ok(env_val) = std::env::var("MONODEX_CONFIG_FOLDER") {
            let trimmed = env_val.trim();
            if !trimmed.is_empty() {
                return Self::absolutize_if_relative(PathBuf::from(trimmed));
            }
        }

        // Fall back to <home>/.monodex
        let home = dirs::home_dir().ok_or_else(|| {
            anyhow!("Could not determine home directory. Set MONODEX_CONFIG_FOLDER or ensure $HOME is set.")
        })?;

        Ok(home.join(".monodex"))
    }

    /// Convert relative paths to absolute using current working directory.
    fn absolutize_if_relative(path: PathBuf) -> Result<PathBuf> {
        if path.is_relative() {
            let cwd = std::env::current_dir()?;
            let absolute = cwd.join(&path);
            eprintln!(
                "Config folder path is relative; resolved to {}",
                absolute.display()
            );
            Ok(absolute)
        } else {
            Ok(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    /// RAII guard to restore MONODEX_CONFIG_FOLDER on drop.
    /// Captures the current value on construction and restores it (or removes it)
    /// when dropped, even on panic.
    struct EnvGuard {
        original: Option<String>,
    }

    impl EnvGuard {
        fn capture() -> Self {
            let original = env::var("MONODEX_CONFIG_FOLDER").ok();
            EnvGuard { original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(ref val) = self.original {
                unsafe {
                    env::set_var("MONODEX_CONFIG_FOLDER", val);
                }
            } else {
                unsafe {
                    env::remove_var("MONODEX_CONFIG_FOLDER");
                }
            }
        }
    }

    #[test]
    #[serial(monodex_config_folder)]
    fn test_resolve_from_env_uses_monodex_config_folder() {
        let _guard = EnvGuard::capture();
        let temp_dir = tempfile::tempdir().unwrap();

        unsafe {
            env::set_var("MONODEX_CONFIG_FOLDER", temp_dir.path());
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert_eq!(paths.config_folder, temp_dir.path());
        assert_eq!(
            paths.config_file(),
            temp_dir.path().join("monodex-config.json")
        );
    }

    #[test]
    #[serial(monodex_config_folder)]
    fn test_resolve_from_env_falls_back_to_home() {
        let _guard = EnvGuard::capture();

        unsafe {
            env::remove_var("MONODEX_CONFIG_FOLDER");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert!(paths.config_folder.ends_with(".monodex"));
        assert_eq!(
            paths.config_file(),
            paths.config_folder.join("monodex-config.json")
        );
    }

    #[test]
    #[serial(monodex_config_folder)]
    fn test_resolve_from_env_config_folder_override_wins() {
        let _guard = EnvGuard::capture();
        let temp_dir = tempfile::tempdir().unwrap();
        let override_path = PathBuf::from("/override/config-folder");

        unsafe {
            env::set_var("MONODEX_CONFIG_FOLDER", temp_dir.path());
        }

        let paths = Paths::resolve_from_env(Some(override_path.clone())).unwrap();
        assert_eq!(paths.config_folder, override_path);
    }

    #[test]
    #[serial(monodex_config_folder)]
    fn test_resolve_from_env_empty_monodex_config_folder_falls_back() {
        let _guard = EnvGuard::capture();
        let _temp_dir = tempfile::tempdir().unwrap();

        unsafe {
            env::set_var("MONODEX_CONFIG_FOLDER", "");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert!(paths.config_folder.ends_with(".monodex"));

        // Also test whitespace-only
        unsafe {
            env::set_var("MONODEX_CONFIG_FOLDER", "   ");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        assert!(paths.config_folder.ends_with(".monodex"));
    }

    #[test]
    #[serial(monodex_config_folder)]
    fn test_resolve_from_env_relative_monodex_config_folder_absolutized() {
        let _guard = EnvGuard::capture();

        unsafe {
            env::set_var("MONODEX_CONFIG_FOLDER", "relative/path");
        }

        let paths = Paths::resolve_from_env(None).unwrap();
        let cwd = std::fs::canonicalize(".").unwrap();
        let expected = cwd.join("relative/path");
        assert_eq!(paths.config_folder, expected);
    }

    #[test]
    #[serial(monodex_config_folder)]
    fn test_resolve_from_env_relative_override_absolutized() {
        let _guard = EnvGuard::capture();

        unsafe {
            env::remove_var("MONODEX_CONFIG_FOLDER");
        }

        let paths = Paths::resolve_from_env(Some(PathBuf::from("relative/path"))).unwrap();
        let cwd = std::fs::canonicalize(".").unwrap();
        let expected = cwd.join("relative/path");
        assert_eq!(paths.config_folder, expected);
    }

    #[test]
    #[serial(monodex_config_folder)]
    fn test_resolve_from_env_empty_override_falls_back_to_env() {
        let _guard = EnvGuard::capture();
        let temp_dir = tempfile::tempdir().unwrap();

        // Set env var to a specific path
        unsafe {
            env::set_var("MONODEX_CONFIG_FOLDER", temp_dir.path());
        }

        // Empty override should fall through to env var
        let paths = Paths::resolve_from_env(Some(PathBuf::from(""))).unwrap();
        assert_eq!(paths.config_folder, temp_dir.path());
    }

    #[test]
    #[serial(monodex_config_folder)]
    fn test_resolve_from_env_whitespace_override_falls_back_to_env() {
        let _guard = EnvGuard::capture();
        let temp_dir = tempfile::tempdir().unwrap();

        // Set env var to a specific path
        unsafe {
            env::set_var("MONODEX_CONFIG_FOLDER", temp_dir.path());
        }

        // Whitespace-only override should fall through to env var
        let paths = Paths::resolve_from_env(Some(PathBuf::from("   "))).unwrap();
        assert_eq!(paths.config_folder, temp_dir.path());
    }

    #[test]
    fn test_for_test_constructs_paths() {
        let temp_dir = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(temp_dir.path().into());

        assert_eq!(paths.config_folder, temp_dir.path());
        assert_eq!(
            paths.config_file(),
            temp_dir.path().join("monodex-config.json")
        );
        assert_eq!(
            paths.context_file(),
            temp_dir.path().join("monodex-state.json")
        );
        assert_eq!(
            paths.crawl_config(),
            temp_dir.path().join("monodex-crawl-config.json")
        );
    }
}
