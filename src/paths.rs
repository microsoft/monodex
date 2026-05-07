//! Purpose: Resolve filesystem paths for monodex tool state (config, context, crawl config, warning state).
//!
//! Edit here when: Changing where tool state lives on disk, adding MONODEX_HOME-style overrides,
//!   or adding accessors for new files under the tool home.
//! Do not edit here for: Config schema (see app/config.rs), crawl filtering (see engine/crawl_config.rs),
//!   or default-context semantics (see app/context.rs).

use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Cached tool home path.
///
/// Uses RwLock<Option<PathBuf>> instead of OnceLock so that integration tests
/// can clear the cache via `clear_tool_home_cache()`. The cache is per-process
/// and thread-safe.
///
/// In production: Once resolved, it stays consistent for the process lifetime.
/// In tests: `clear_tool_home_cache()` can reset it so each test gets a fresh value.
static TOOL_HOME: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Resolve the monodex tool home.
///
/// - If `MONODEX_HOME` is set and non-empty (after trim), returns that path (canonicalized if relative).
/// - Otherwise returns `<home>/.monodex`.
/// - Errors if neither is available.
///
/// The result is cached for the process lifetime to ensure consistency.
pub fn tool_home() -> Result<PathBuf> {
    // Check cache first (read lock)
    {
        let cache = TOOL_HOME.read().unwrap();
        if let Some(cached) = cache.as_ref() {
            return Ok(cached.clone());
        }
    }

    // Not cached, resolve and cache (write lock)
    let resolved = resolve_tool_home_inner()?;
    {
        let mut cache = TOOL_HOME.write().unwrap();
        // Check again in case another thread resolved while we were waiting
        if cache.is_none() {
            *cache = Some(resolved.clone());
        }
        Ok(cache.as_ref().unwrap().clone())
    }
}

/// Clear the cached tool home.
///
/// This is intended for integration tests that need to reset the cached tool home
/// between test cases. Each test sets its own MONODEX_HOME and needs to ensure
/// the cache doesn't return a stale value from a previous test.
pub fn clear_tool_home_cache() {
    let mut cache = TOOL_HOME.write().unwrap();
    *cache = None;
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

    // Fall back to <home>/.monodex
    let home = dirs::home_dir().ok_or_else(|| {
        anyhow!("Could not determine home directory. Set MONODEX_HOME or ensure $HOME is set.")
    })?;

    Ok(home.join(".monodex"))
}

/// Resolve the path to the config file.
///
/// Resolution order:
/// 1. If MONODEX_CONFIG is set and non-empty, use that path.
/// 2. Otherwise, use `<tool_home>/config.json`.
pub fn config_path() -> Result<PathBuf> {
    if let Ok(env_val) = std::env::var("MONODEX_CONFIG") {
        let trimmed = env_val.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }

    Ok(tool_home()?.join("config.json"))
}

/// Resolve the path to the context file (for `monodex use`).
///
/// The context file stores the default catalog and label.
pub fn context_path() -> Result<PathBuf> {
    Ok(tool_home()?.join("context.json"))
}

/// Resolve the path to the repo-local crawl config file.
///
/// This file is optional and may not exist.
pub fn repo_local_crawl_config_path(repo_root: &Path) -> PathBuf {
    repo_root.join("monodex-crawl.json")
}

/// Resolve the path to the user-global crawl config file.
///
/// This file is optional and may not exist.
pub fn user_global_crawl_config_path() -> Result<PathBuf> {
    Ok(tool_home()?.join("crawl.json"))
}

/// Alias for `user_global_crawl_config_path` for backward compatibility.
pub fn crawl_config_path() -> Result<PathBuf> {
    user_global_crawl_config_path()
}

/// Called once from main() early in startup. Prints a one-line warning to stderr
/// if any files exist at the old pre-PR locations and no files exist at the
/// new locations.
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

/// Resolve the path to the warning state file for a catalog.
///
/// This file tracks files that produced warnings during crawl.
pub fn warning_state_path(catalog_name: &str) -> Result<PathBuf> {
    Ok(tool_home()?.join(format!("warnings-{}.json", catalog_name)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_tool_home_uses_env_var() {
        let temp_dir = tempfile::tempdir().unwrap();
        clear_tool_home_cache();
        unsafe {
            env::set_var("MONODEX_HOME", temp_dir.path());
        }
        let result = tool_home().unwrap();
        assert_eq!(result, temp_dir.path());
        clear_tool_home_cache();
        unsafe {
            env::remove_var("MONODEX_HOME");
        }
    }

    #[test]
    fn test_tool_home_falls_back_to_home() {
        clear_tool_home_cache();
        unsafe {
            env::remove_var("MONODEX_HOME");
        }
        let result = tool_home().unwrap();
        assert!(result.ends_with(".monodex"));
        clear_tool_home_cache();
    }

    #[test]
    fn test_clear_tool_home_cache_works() {
        let temp_dir1 = tempfile::tempdir().unwrap();
        let temp_dir2 = tempfile::tempdir().unwrap();

        // Set and cache first path
        clear_tool_home_cache();
        unsafe {
            env::set_var("MONODEX_HOME", temp_dir1.path());
        }
        let result1 = tool_home().unwrap();
        assert_eq!(result1, temp_dir1.path());

        // Clear cache and set new path
        clear_tool_home_cache();
        unsafe {
            env::set_var("MONODEX_HOME", temp_dir2.path());
        }
        let result2 = tool_home().unwrap();
        assert_eq!(result2, temp_dir2.path());

        // Cleanup
        clear_tool_home_cache();
        unsafe {
            env::remove_var("MONODEX_HOME");
        }
    }
}
