//! Purpose: Resolve filesystem paths for monodex tool state (config, context, crawl config, warning state).
//!
//! Edit here when: Changing where tool state lives on disk, adding MONODEX_HOME-style overrides,
//!   or adding accessors for new files under the tool home.
//! Do not edit here for: Config schema (see app/config.rs), crawl filtering (see engine/crawl_config.rs),
//!   or default-context semantics (see app/context.rs).

use anyhow::{Result, anyhow};
use std::path::PathBuf;

#[cfg(not(test))]
use std::sync::OnceLock;

/// Cached tool home path.
/// - In production: Once resolved, it stays consistent for the process lifetime.
/// - In tests: We re-resolve each time so each test can set its own MONODEX_HOME.
#[cfg(not(test))]
static TOOL_HOME: OnceLock<PathBuf> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static TOOL_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Resolve the monodex tool home.
///
/// - If `MONODEX_HOME` is set and non-empty (after trim), returns that path (canonicalized if relative).
/// - Otherwise returns `<home>/.monodex`.
/// - Errors if neither is available.
///
/// The result is cached for the process lifetime to ensure consistency.
#[cfg(not(test))]
pub fn tool_home() -> Result<PathBuf> {
    if let Some(cached) = TOOL_HOME.get() {
        return Ok(cached.clone());
    }

    let resolved = resolve_tool_home_inner()?;

    // Cache the result. If another thread beat us, use their value.
    let actual = TOOL_HOME.get_or_init(|| resolved);
    Ok(actual.clone())
}

/// Test version: re-resolve each time so each test can set its own MONODEX_HOME.
#[cfg(test)]
pub fn tool_home() -> Result<PathBuf> {
    TOOL_HOME.with(|cell| {
        if let Some(cached) = cell.borrow().as_ref() {
            return Ok(cached.clone());
        }

        let resolved = resolve_tool_home_inner()?;
        *cell.borrow_mut() = Some(resolved.clone());
        Ok(resolved)
    })
}

/// Clear the cached tool home (test-only).
#[cfg(test)]
pub fn clear_tool_home_cache() {
    TOOL_HOME.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

/// Inner resolution logic, uncached.
fn resolve_tool_home_inner() -> Result<PathBuf> {
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

    // Fall back to home directory
    let home = dirs::home_dir().ok_or_else(|| {
        anyhow!("Cannot determine monodex tool home: MONODEX_HOME is not set and your home directory could not be determined.")
    })?;

    Ok(home.join(".monodex"))
}

/// Convenience accessor for config.json.
///
/// Returns `<tool_home>/config.json`.
/// This is a pure path constructor — it does NOT create parent directories.
pub fn config_path() -> Result<PathBuf> {
    Ok(tool_home()?.join("config.json"))
}

/// Convenience accessor for context.json.
///
/// Returns `<tool_home>/context.json`.
/// This is a pure path constructor — it does NOT create parent directories.
pub fn context_path() -> Result<PathBuf> {
    Ok(tool_home()?.join("context.json"))
}

/// Convenience accessor for crawl.json.
///
/// Returns `<tool_home>/crawl.json`.
/// This is a pure path constructor — it does NOT create parent directories.
pub fn crawl_config_path() -> Result<PathBuf> {
    Ok(tool_home()?.join("crawl.json"))
}

/// Called once from main() early in startup. Prints a one-line warning to stderr
/// if any files exist at the old pre-PR locations and no files exist at the
/// effective new tool home. Not suppressible.
///
/// This function is deliberately located alongside the pure path helpers rather
/// than in a dedicated module, because it will be deleted once users have
/// migrated (expected within a few months of shipping). A short-lived function
/// does not justify its own module.
pub fn warn_old_tool_home_if_present() {
    // Resolve the new tool home (honoring MONODEX_HOME)
    let new_home = match tool_home() {
        Ok(path) => path,
        Err(_) => return, // Can't resolve tool home, nothing to warn about
    };

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

    // MONODEX_HOME is process-global, so tests that modify it must be serialized
    // We use serial_test to ensure these tests don't run in parallel

    /// Helper to save and restore MONODEX_HOME
    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn new(key: &'static str) -> Self {
            // SAFETY: Reading env var is safe
            let original = env::var(key).ok();
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: Restoring env var in test cleanup is safe
            unsafe {
                if let Some(ref val) = self.original {
                    env::set_var(self.key, val);
                } else {
                    env::remove_var(self.key);
                }
            }
            // Clear the cached tool home for next test
            // NOTE: OnceLock doesn't have a clear method, so we use a separate approach
        }
    }

    #[test]
    #[serial(monodex_home)]
    fn test_monodex_home_absolute_path() {
        let _guard = EnvGuard::new("MONODEX_HOME");

        // Clear the cached value by using a fresh OnceLock in this test context
        // Since we can't clear OnceLock, we test the resolution function directly
        // SAFETY: Setting env var in test is safe
        unsafe {
            env::set_var("MONODEX_HOME", "/tmp/test-monodex-home");
        }

        // Test the inner resolution directly (bypasses cache)
        let result = resolve_tool_home_inner().unwrap();
        assert_eq!(result, PathBuf::from("/tmp/test-monodex-home"));

        // Clean up
        // SAFETY: Removing env var in test is safe
        unsafe {
            env::remove_var("MONODEX_HOME");
        }
    }

    #[test]
    #[serial(monodex_home)]
    fn test_monodex_home_relative_path() {
        let _guard = EnvGuard::new("MONODEX_HOME");

        // SAFETY: Setting env var in test is safe
        unsafe {
            env::set_var("MONODEX_HOME", "./tmp-home");
        }

        // Test the inner resolution directly (bypasses cache)
        let result = resolve_tool_home_inner().unwrap();

        // Result should be absolute
        assert!(
            result.is_absolute(),
            "Result should be absolute: {:?}",
            result
        );

        // Result should end with tmp-home
        assert!(
            result.ends_with("tmp-home"),
            "Result should end with tmp-home: {:?}",
            result
        );

        // Compute expected path dynamically based on current directory
        let cwd = std::env::current_dir().expect("current_dir should work");
        let expected = cwd.join("tmp-home");
        assert_eq!(result, expected);

        // Clean up
        // SAFETY: Removing env var in test is safe
        unsafe {
            env::remove_var("MONODEX_HOME");
        }
    }

    #[test]
    #[serial(monodex_home)]
    fn test_monodex_home_empty_string_treated_as_unset() {
        let _guard = EnvGuard::new("MONODEX_HOME");

        // SAFETY: Setting env var in test is safe
        unsafe {
            env::set_var("MONODEX_HOME", "");
        }

        // Empty string should be treated as unset
        let result = resolve_tool_home_inner().unwrap();
        let expected = dirs::home_dir()
            .expect("home_dir should work in test")
            .join(".monodex");
        assert_eq!(result, expected);

        // SAFETY: Removing env var in test is safe
        unsafe {
            env::remove_var("MONODEX_HOME");
        }
    }

    #[test]
    #[serial(monodex_home)]
    fn test_monodex_home_whitespace_treated_as_unset() {
        let _guard = EnvGuard::new("MONODEX_HOME");

        // SAFETY: Setting env var in test is safe
        unsafe {
            env::set_var("MONODEX_HOME", "   ");
        }

        // Whitespace-only should be treated as unset
        let result = resolve_tool_home_inner().unwrap();
        let expected = dirs::home_dir()
            .expect("home_dir should work in test")
            .join(".monodex");
        assert_eq!(result, expected);

        // SAFETY: Removing env var in test is safe
        unsafe {
            env::remove_var("MONODEX_HOME");
        }
    }

    #[test]
    #[serial(monodex_home)]
    fn test_monodex_home_unset_uses_home_dir() {
        let _guard = EnvGuard::new("MONODEX_HOME");

        // SAFETY: Removing env var in test is safe
        unsafe {
            env::remove_var("MONODEX_HOME");
        }

        let result = resolve_tool_home_inner().unwrap();
        let expected = dirs::home_dir()
            .expect("home_dir should work in test")
            .join(".monodex");
        assert_eq!(result, expected);
    }

    #[test]
    fn test_config_path() {
        // This test doesn't modify env vars, so it's safe to run in parallel
        // It tests that the path constructor works correctly
        let path = config_path().unwrap();
        assert!(path.ends_with("config.json"));
    }

    #[test]
    fn test_context_path() {
        let path = context_path().unwrap();
        assert!(path.ends_with("context.json"));
    }

    #[test]
    fn test_crawl_config_path() {
        let path = crawl_config_path().unwrap();
        assert!(path.ends_with("crawl.json"));
    }
}
