//! Purpose: Integration tests for FTS end-to-end behavior — full pipeline tests for Stage 9.
//! Edit here when: Adding or modifying end-to-end FTS integration tests.
//! Do not edit here for: Production crawl/search code (see `app/commands/`); per-module unit tests.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use serial_test::serial;

use monodex::app::commands::init_db::run_init_db;
use monodex::app::commands::search::run_search;
use monodex::app::config::Config;
use monodex::engine::retrieval::RetrievalMethod;

fn set_monodex_home(tmp_dir: &Path) {
    // SAFETY: Tests are serialized via #[serial_test::serial(monodex_home)] attribute
    unsafe {
        std::env::set_var("MONODEX_HOME", tmp_dir);
    }
}

fn remove_monodex_home() {
    // SAFETY: Tests are serialized via #[serial_test::serial(monodex_home)] attribute
    unsafe {
        std::env::remove_var("MONODEX_HOME");
    }
}

/// Create a minimal Git repo with test files and return the commit OID.
fn create_test_git_repo(repo_path: &Path) -> String {
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
fn create_test_config(monodex_home: &Path, catalog_name: &str, repo_path: &Path) -> Config {
    let config_path = monodex_home.join("config.json");
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

    // Load and return the config
    let content = fs::read_to_string(&config_path).expect("Failed to read config");
    let stripped = json_comments::StripComments::new(content.as_bytes());
    serde_json::from_reader(stripped).expect("Failed to parse config")
}

// =============================================================================
// Test 1: End-to-end test for crawl-then-search
// =============================================================================

/// Test crawl-then-search flow:
/// - `monodex init-db`
/// - `monodex crawl --catalog X --label main --commit HEAD`
/// - `monodex search --text "..."` → confirms PR1 stub error.
/// - `monodex search --text "..." --retrieval fts` → confirms FTS results.
/// - `monodex search --text "..." --retrieval vector` → confirms vector results.
#[test]
#[serial(monodex_home)]
fn test_crawl_then_search() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = tempfile::TempDir::new().unwrap();
        let repo_dir = tempfile::TempDir::new().unwrap();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config).expect("init-db failed");

        // Run crawl with no --retrieval flag (defaults to all methods)
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,  // incremental_warnings
            vec![], // retrieval: empty = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Test 1: Search with no --retrieval should produce PR1 stub error
        // (both methods in selection, sources equal, RRF not implemented)
        let search_result = run_search(
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag
            false,
        );

        assert!(
            search_result.is_err(),
            "Search should return error for multi-method in PR1"
        );
        let err_msg = search_result.unwrap_err().to_string();
        assert!(
            err_msg
                .contains("Hybrid search across multiple retrieval methods is not yet implemented"),
            "Error should mention hybrid search not implemented, got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("--retrieval"),
            "Error should suggest --retrieval flag, got: {}",
            err_msg
        );

        // Test 2: Search with --retrieval fts should succeed
        let fts_retrieval: Option<BTreeSet<RetrievalMethod>> =
            Some([RetrievalMethod::Fts].into_iter().collect());
        let search_result = run_search(
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            fts_retrieval,
            false,
        );
        // FTS search should succeed (may or may not have results, but no error)
        // Note: FTS results depend on the index being built, which happens during crawl
        assert!(
            search_result.is_ok(),
            "FTS search should succeed, got error: {:?}",
            search_result.err()
        );

        // Test 3: Search with --retrieval vector should succeed
        let vector_retrieval: Option<BTreeSet<RetrievalMethod>> =
            Some([RetrievalMethod::Vector].into_iter().collect());
        let search_result = run_search(
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            vector_retrieval,
            false,
        );
        assert!(
            search_result.is_ok(),
            "Vector search should succeed, got error: {:?}",
            search_result.err()
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}
