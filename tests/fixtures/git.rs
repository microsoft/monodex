//! Purpose: Shared Git-repo and config fixtures for integration tests.
//! Edit here when: Adding or modifying Git-repo or config setup helpers for integration tests.
//! Do not edit here for: Storage fixtures (see `storage.rs`), test cases (see `tests/*.rs`).

use std::fs;
use std::path::Path;
use std::process::Command;

use monodex::app::config::Config;

/// Generate a unique temp directory with a prefix to avoid path reuse collisions.
///
/// On macOS, temp directory paths can be reused rapidly after deletion, which can
/// cause race conditions where a new test sees stale data from a previous test.
/// Using a unique prefix ensures each test gets a truly distinct path.
#[allow(dead_code)]
pub fn unique_temp_dir() -> tempfile::TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    tempfile::Builder::new()
        .prefix(&format!("monodex-test-{}-", id))
        .tempdir()
        .expect("Failed to create temp directory")
}

/// Create a minimal Git repo with test files and return the commit OID.
#[allow(dead_code)]
pub fn create_test_git_repo(repo_path: &Path) -> String {
    // Initialize git repo
    let git_init = Command::new("git")
        .args(["init"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git init");
    assert!(git_init.status.success(), "git init failed");

    // Configure local user
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to set user.name");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to set user.email");

    // Create a TypeScript file
    let ts_file = repo_path.join("src").join("example.ts");
    fs::create_dir_all(ts_file.parent().unwrap()).unwrap();
    fs::write(
        &ts_file,
        r#"// Example TypeScript file for testing
export function getUserProfile(userId: string): UserProfile | null {
  if (!userId) {
    return null;
  }
  return database.query(userId);
}

export function parseUserInput(input: string): string[] {
  return input.split(' ').filter(s => s.length > 0);
}
"#,
    )
    .expect("Failed to write test file");

    // Create a markdown file
    let md_file = repo_path.join("README.md");
    fs::write(
        &md_file,
        r#"# Test Project

This is a test project for Monodex FTS integration testing.

## Features

- User profile management
- Input parsing utilities
"#,
    )
    .expect("Failed to write markdown file");

    // Git add and commit
    Command::new("git")
        .args(["add", "."])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git add");

    let git_commit = Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git commit");
    assert!(git_commit.status.success(), "git commit failed");

    // Get the commit OID
    let git_rev_parse = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .expect("Failed to run git rev-parse");
    assert!(git_rev_parse.status.success(), "git rev-parse failed");

    String::from_utf8_lossy(&git_rev_parse.stdout)
        .trim()
        .to_string()
}

/// Create a test config with a catalog pointing to the given repo path.
#[allow(dead_code)]
pub fn create_test_config(monodex_home: &Path, catalog_name: &str, repo_path: &Path) -> Config {
    let config_path = monodex_home.join("monodex-config.json");
    fs::create_dir_all(monodex_home).unwrap();

    let config_content = format!(
        r#"{{
  "catalogs": {{
    "{}": {{
      "type": "monorepo",
      "path": "{}"
    }}
  }}
}}"#,
        catalog_name,
        repo_path.to_str().unwrap().replace('\\', "\\\\")
    );

    fs::write(&config_path, &config_content).expect("Failed to write config");

    // Use the proper load_config path to get a Config with Paths
    let paths = monodex::paths::Paths::for_test(monodex_home.to_path_buf());
    monodex::app::config::load_config(paths).expect("Failed to load config")
}
