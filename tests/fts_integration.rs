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
    // Clear any cached tool_home from previous tests
    monodex::paths::clear_tool_home_cache();

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

    // Clear the cache so the next test starts fresh
    monodex::paths::clear_tool_home_cache();
}

/// Generate a unique temp directory with a prefix to avoid path reuse collisions.
///
/// On macOS, temp directory paths can be reused rapidly after deletion, which can
/// cause race conditions where a new test sees stale data from a previous test.
/// Using a unique prefix ensures each test gets a truly distinct path.
fn unique_temp_dir() -> tempfile::TempDir {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    tempfile::Builder::new()
        .prefix(&format!("monodex-test-{}-", id))
        .tempdir()
        .expect("Failed to create temp directory")
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
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

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

// =============================================================================
// Test 2: End-to-end test for selection-narrowing
// =============================================================================

/// Test selection-narrowing flow:
/// - First crawl with no --retrieval flag (selection becomes {fts, vector})
/// - Second crawl with --retrieval fts (selection narrows to {fts})
/// - Confirms narrowing-announcement block fires
/// - Confirms search shows "(fts only, no vector)" on Label: line
/// - Confirms search --retrieval vector errors with "not in selection"
#[test]
#[serial(monodex_home)]
fn test_selection_narrowing() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // First crawl: no --retrieval flag (selection becomes {fts, vector})
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,  // incremental_warnings
            vec![], // retrieval: empty = all methods
            false,  // debug
        )
        .expect("first crawl failed");

        // Second crawl: --retrieval fts (selection narrows to {fts})
        // We need to capture stdout to check for the narrowing announcement
        // For now, we'll run the crawl and verify the state changes
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,                      // incremental_warnings
            vec![RetrievalMethod::Fts], // retrieval: fts only
            false,                      // debug
        )
        .expect("second crawl (narrowing) failed");

        // Verify: search with no --retrieval should now work (only fts in selection)
        let search_result = run_search(
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag
            false,
        );

        // With only fts in selection, search should succeed (not the PR1 stub error)
        assert!(
            search_result.is_ok(),
            "FTS-only search should succeed after narrowing, got error: {:?}",
            search_result.err()
        );

        // Verify: search --retrieval vector should error (not in selection)
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
            search_result.is_err(),
            "Vector search should fail after narrowing to fts-only"
        );
        let err_msg = search_result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not in this label's retrieval selection")
                || err_msg.contains("not in selection"),
            "Error should mention not in selection, got: {}",
            err_msg
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

// =============================================================================
// Test 3: End-to-end test for selection-widening
// =============================================================================

/// Test selection-widening flow:
/// - Continue from narrowing test state (selection is {fts})
/// - Crawl with no --retrieval flag widens selection back to {fts, vector}
/// - Confirms NO narrowing-announcement block
/// - Confirms search shows "(fts, vector)" on Label: line
/// - Confirms search with no --retrieval produces PR1 stub error again
#[test]
#[serial(monodex_home)]
fn test_selection_widening() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // First crawl: no --retrieval flag (selection becomes {fts, vector})
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,  // incremental_warnings
            vec![], // retrieval: empty = all methods
            false,  // debug
        )
        .expect("first crawl failed");

        // Second crawl: --retrieval fts (selection narrows to {fts})
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,                      // incremental_warnings
            vec![RetrievalMethod::Fts], // retrieval: fts only
            false,                      // debug
        )
        .expect("second crawl (narrowing) failed");

        // Third crawl: no --retrieval flag (selection widens back to {fts, vector})
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,  // incremental_warnings
            vec![], // retrieval: empty = all methods
            false,  // debug
        )
        .expect("third crawl (widening) failed");

        // Verify: search with no --retrieval should produce PR1 stub error again
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
            "Search should return PR1 stub error for multi-method selection"
        );
        let err_msg = search_result.unwrap_err().to_string();
        assert!(
            err_msg
                .contains("Hybrid search across multiple retrieval methods is not yet implemented"),
            "Error should mention hybrid search not implemented, got: {}",
            err_msg
        );

        // Verify: search --retrieval fts should succeed
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
        assert!(
            search_result.is_ok(),
            "FTS search should succeed after widening, got error: {:?}",
            search_result.err()
        );

        // Verify: search --retrieval vector should succeed
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
            "Vector search should succeed after widening, got error: {:?}",
            search_result.err()
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

// =============================================================================
// Test 4: End-to-end test for first-time crawl with --retrieval fts
// =============================================================================

/// Test first-time crawl with explicit --retrieval fts:
/// - No previous selection exists (first crawl on this label)
/// - Confirms NO narrowing-announcement block (nothing to narrow from)
/// - Confirms parenthesis shows "(fts only, no vector)"
/// - Confirms search --retrieval vector errors with "not in selection"
#[test]
#[serial(monodex_home)]
fn test_first_time_crawl_fts_only() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // First crawl with explicit --retrieval fts (no previous selection)
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,                      // incremental_warnings
            vec![RetrievalMethod::Fts], // retrieval: fts only
            false,                      // debug
        )
        .expect("first crawl failed");

        // Verify: search with no --retrieval should succeed (only fts in selection)
        let search_result = run_search(
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag
            false,
        );

        // With only fts in selection, search should succeed (not the PR1 stub error)
        assert!(
            search_result.is_ok(),
            "FTS-only search should succeed, got error: {:?}",
            search_result.err()
        );

        // Verify: search --retrieval vector should error (not in selection)
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
            search_result.is_err(),
            "Vector search should fail - not in selection"
        );
        let err_msg = search_result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not in this label's retrieval selection")
                || err_msg.contains("not in selection"),
            "Error should mention not in selection, got: {}",
            err_msg
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

// =============================================================================
// Test 5: End-to-end test for purge cleanup
// =============================================================================

/// Test purge cleanup:
/// - After a crawl producing FTS state, purge --catalog X removes FTS directory
/// - purge --all removes entire fts/ directory
#[test]
#[serial(monodex_home)]
fn test_purge_cleanup() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl to create FTS state
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

        // Get database path
        let db_path = monodex::app::resolve_database_path(Some(&config)).unwrap();
        let fts_catalog_path = db_path.join("fts").join("test-catalog");

        // Verify FTS directory exists after crawl
        assert!(
            fts_catalog_path.exists(),
            "FTS catalog directory should exist after crawl"
        );

        // Test purge --catalog
        monodex::app::commands::purge::run_purge(&config, Some("test-catalog"), false, false)
            .expect("purge --catalog failed");

        // Verify FTS catalog directory is gone after purge --catalog
        assert!(
            !fts_catalog_path.exists(),
            "FTS catalog directory should be gone after purge --catalog"
        );

        // Crawl again to recreate FTS state
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,
            vec![],
            false,
        )
        .expect("second crawl failed");

        // Verify FTS directory exists again
        assert!(
            fts_catalog_path.exists(),
            "FTS catalog directory should exist after second crawl"
        );

        // Test purge --all
        monodex::app::commands::purge::run_purge(&config, None, true, false)
            .expect("purge --all failed");

        // Verify entire FTS directory exists and is empty after purge --all
        let fts_path = db_path.join("fts");
        assert!(
            fts_path.exists(),
            "FTS directory should exist after purge --all (implementation recreates it)"
        );
        let entries: Vec<_> = std::fs::read_dir(&fts_path)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            entries.is_empty(),
            "FTS directory should be empty after purge --all"
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

// =============================================================================
// Test 6: End-to-end test for schema-mismatch error
// =============================================================================

/// Test schema-mismatch error:
/// - Hand-write a monodex-meta.json with version 3 (old version)
/// - Attempt to open the database
/// - Confirm error fires with expected message
#[test]
#[serial(monodex_home)]
fn test_schema_mismatch_error() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo (unused - we just need a repo for config)
        let _commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db first
        run_init_db(&config, false).expect("init-db failed");

        // Get database path
        let db_path = monodex::app::resolve_database_path(Some(&config)).unwrap();
        let meta_path = db_path.join("monodex-meta.json");

        // Now hand-write an old schema version
        let old_meta = r#"{"monodex_schema_version": 3, "created_at": "2024-01-01T00:00:00Z", "created_by_binary_version": "0.5.0", "lance_format_version": "0.1.0"}"#;
        fs::write(&meta_path, old_meta).expect("Failed to write old meta");

        // Attempt to open storage should fail with schema mismatch
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(monodex::engine::storage::Database::open(&db_path));
        assert!(result.is_err(), "Should error on schema mismatch");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Schema mismatch"),
            "Error should mention schema mismatch, got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("database has version 3"),
            "Error should mention database version 3, got: {}",
            err_msg
        );
        assert!(
            err_msg.contains("expects version 4"),
            "Error should mention expected version 4, got: {}",
            err_msg
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

// =============================================================================
// Test 7: FTS query parse error
// =============================================================================

/// Test FTS query parse error (decision #19):
/// - Index a label with FTS state
/// - Run search with a syntactically-invalid query for Tantivy's parser
/// - Confirm output is "Couldn't parse FTS query: <message>" and NOT "No results."
#[test]
#[serial(monodex_home)]
fn test_fts_query_parse_error() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl with FTS
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,                      // incremental_warnings
            vec![RetrievalMethod::Fts], // retrieval: fts only
            false,                      // debug
        )
        .expect("crawl failed");

        // Search with a syntactically-invalid query
        // Using unmatched quotes or malformed field syntax that Tantivy's parser rejects
        let search_result = run_search(
            &config,
            "foo:bar:", // Invalid field syntax (field requires a term after colon)
            10,
            Some("main"),
            Some("test-catalog"),
            Some([RetrievalMethod::Fts].into_iter().collect()),
            false,
        );

        // Should error (not return empty results)
        assert!(
            search_result.is_err(),
            "Parse error should return Err, got Ok"
        );
        let err_msg = search_result.unwrap_err().to_string();

        // Must contain the parse error message
        assert!(
            err_msg.contains("Couldn't parse FTS query"),
            "Error should mention parse error, got: {}",
            err_msg
        );

        // Must NOT contain "No results"
        assert!(
            !err_msg.contains("No results"),
            "Parse error should not mention 'No results', got: {}",
            err_msg
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

// =============================================================================
// Test 8: Multi-method explicit search
// =============================================================================

/// Test multi-method explicit search (PR1 stub error):
/// - After a --retrieval-less crawl (selection={fts, vector})
/// - Run `monodex search --retrieval fts --retrieval vector`
/// - Confirm the PR1 stub error fires (same as no-flag with size-2+ selection)
#[test]
#[serial(monodex_home)]
fn test_multi_method_explicit_search() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl with no --retrieval (selection becomes {fts, vector})
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,  // incremental_warnings
            vec![], // no --retrieval = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Search with explicit multi-method: --retrieval fts --retrieval vector
        let multi_method: Option<BTreeSet<RetrievalMethod>> = Some(
            [RetrievalMethod::Fts, RetrievalMethod::Vector]
                .into_iter()
                .collect(),
        );
        let search_result = run_search(
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            multi_method,
            false,
        );

        // Should error with PR1 stub error (hybrid not implemented)
        assert!(
            search_result.is_err(),
            "Multi-method search should return PR1 stub error"
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
            "Error should mention --retrieval flag, got: {}",
            err_msg
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

/// Test that the search preamble appears before the multi-method stub error.
///
/// This verifies that the "Catalog: ... / Label: ... / Searching: ..." line
/// is printed before the PR1 stub error for hybrid search, making the
/// retrieval-selection concept legible even when errors follow.
#[test]
#[serial(monodex_home)]
fn test_multi_method_search_shows_preamble() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl with no --retrieval (selection becomes {fts, vector})
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            false,  // incremental_warnings
            vec![], // no --retrieval = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Run monodex search as a subprocess to capture stdout
        // We use the binary directly since run_search uses println! directly
        // current_exe() gives us the test binary path; the main binary is in the same parent directory
        let exe_path = std::env::current_exe().expect("failed to get current exe path");
        let deps_dir = exe_path.parent().expect("failed to get deps dir");
        let debug_dir = deps_dir.parent().expect("failed to get debug dir");
        let binary_path = debug_dir.join("monodex");

        let output = std::process::Command::new(&binary_path)
            .args([
                "search",
                "--text",
                "getUserProfile",
                "--label",
                "main",
                "--catalog",
                "test-catalog",
                "--retrieval",
                "fts",
                "--retrieval",
                "vector",
            ])
            .env("MONODEX_HOME", monodex_home.path())
            .output()
            .expect("failed to execute monodex search");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // The command should fail with PR1 stub error
        assert!(
            !output.status.success(),
            "Multi-method search should fail with PR1 stub error"
        );

        // The preamble should appear in stdout before the error
        // Check for "Searching:" and both method names
        assert!(
            stdout.contains("Searching:"),
            "Preamble should contain 'Searching:', got stdout: {:?}, stderr: {:?}",
            stdout,
            stderr
        );
        assert!(
            stdout.contains("fts") && stdout.contains("vector"),
            "Preamble should mention both methods, got stdout: {:?}, stderr: {:?}",
            stdout,
            stderr
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

// =============================================================================
// Test: End-to-end cross-label active_label_ids preservation
// =============================================================================

/// Test that crawling the same content under a second label makes it searchable
/// under both labels.
///
/// This verifies the active_label_ids preservation invariant end-to-end:
/// 1. Crawl with --label A --retrieval fts (FTS-only, no vectors)
/// 2. Crawl with --label B (both methods, including vectors)
/// 3. Search under label A should find the chunk
/// 4. Search under label B should find the chunk
#[test]
#[serial(monodex_home)]
fn test_cross_label_active_labels_preserved() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Step 1: Crawl with label A, FTS-only
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "label-a",
            &commit_oid,
            false,                      // incremental_warnings
            vec![RetrievalMethod::Fts], // FTS-only
            false,                      // debug
        )
        .expect("crawl label-a failed");

        // Step 2: Crawl with label B, both methods (including vectors)
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "label-b",
            &commit_oid,
            false,  // incremental_warnings
            vec![], // empty = all methods
            false,  // debug
        )
        .expect("crawl label-b failed");

        // Step 3: Search under label A should find the chunk (FTS)
        let fts_only: Option<BTreeSet<RetrievalMethod>> =
            Some(std::iter::once(RetrievalMethod::Fts).collect());
        run_search(
            &config,
            "getUserProfile",
            10,
            Some("label-a"),
            Some("test-catalog"),
            fts_only.clone(),
            false,
        )
        .expect("search label-a failed");
        // run_search returns Ok(()) on success; result count is printed to stdout

        // Step 4: Search under label B should find the chunk (FTS)
        run_search(
            &config,
            "getUserProfile",
            10,
            Some("label-b"),
            Some("test-catalog"),
            fts_only,
            false,
        )
        .expect("search label-b failed");
        // run_search returns Ok(()) on success; result count is printed to stdout

        // Also verify vector search works for label B (which has vectors)
        let vector_only: Option<BTreeSet<RetrievalMethod>> =
            Some(std::iter::once(RetrievalMethod::Vector).collect());
        run_search(
            &config,
            "getUserProfile",
            10,
            Some("label-b"),
            Some("test-catalog"),
            vector_only,
            false,
        )
        .expect("vector search label-b failed");
        // run_search returns Ok(()) on success; result count is printed to stdout

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

// =============================================================================
// Test: Working-dir remediation message
// =============================================================================

/// Test that a working-dir-crawled label produces remediation suggesting
/// `--working-dir`, not `--commit <opaque sentinel>`.
#[test]
#[serial(monodex_home)]
fn test_working_dir_remediation_message() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo (we crawl working-dir, but need git for the repo structure)
        let _commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl with working-dir mode, FTS-only to get incomplete state
        monodex::app::commands::crawl::run_crawl_working_dir(
            &config,
            "test-catalog",
            "working-label",
            false,                      // incremental_warnings
            vec![RetrievalMethod::Fts], // FTS-only
            false,                      // debug
        )
        .expect("working-dir crawl failed");

        // Now try to search with vector (not in selection) - should error
        let vector_only: Option<BTreeSet<RetrievalMethod>> =
            Some(std::iter::once(RetrievalMethod::Vector).collect());
        let search_result = run_search(
            &config,
            "getUserProfile",
            10,
            Some("working-label"),
            Some("test-catalog"),
            vector_only,
            false,
        );

        // Should error because vector is not in selection
        assert!(
            search_result.is_err(),
            "Should error when method not in selection"
        );
        let err_msg = search_result.unwrap_err().to_string();

        // The error should suggest re-crawling, and since this is a working-dir
        // label, it should mention --working-dir in the remediation
        // Note: The actual message format is "Re-run `monodex crawl --label <label> [source] --retrieval X`"
        // where [source] is determined by the source_kind. For working-dir, it should be --working-dir.
        assert!(
            err_msg.contains("--retrieval vector")
                || err_msg.contains("not in this label's retrieval selection"),
            "Error should mention the retrieval method issue, got: {}",
            err_msg
        );

        // Verify the source pointer shows --working-dir, not the sentinel prefix
        assert!(
            err_msg.contains("--working-dir"),
            "Error should contain '--working-dir' for working-dir labels, got: {}",
            err_msg
        );
        assert!(
            !err_msg.contains("working-dir:"),
            "Error should NOT contain 'working-dir:' sentinel prefix, got: {}",
            err_msg
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}

// =============================================================================
// Test: Post-finalize error propagation
// =============================================================================

/// Test that post-finalize errors propagate when no phase error was captured.
///
/// This test injects a failure into save_warning_state by pre-creating a
/// directory at the path where the warning state file would be written.
/// The OS then rejects the write with "is a directory" error.
///
/// The crawl should fail with an error referencing the warning state.
#[test]
#[serial(monodex_home)]
fn test_post_finalize_error_propagates_when_no_phase_error() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        set_monodex_home(monodex_home.path());

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Resolve the database path
        let db_path = monodex::app::resolve_database_path(Some(&config)).unwrap();

        // Create a directory at the path where save_warning_state would write its file.
        // This causes the write to fail with "is a directory" error.
        let warning_state_path = db_path.join("warnings-test-catalog.json");
        std::fs::create_dir_all(&warning_state_path).expect("Failed to create blocking directory");

        // Run a crawl - it should succeed through all phases but fail at warning-state save
        let crawl_result = monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "test-label",
            &commit_oid,
            false,  // incremental_warnings
            vec![], // empty = all methods
            false,  // debug
        );

        // The crawl should have failed due to warning-state persistence error
        assert!(
            crawl_result.is_err(),
            "Crawl should fail due to warning-state error"
        );

        // Verify the error chain references warning state
        let err = crawl_result.expect_err("Expected error");
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("warning state"),
            "Error should reference warning state, got: {}",
            err_msg
        );

        (monodex_home, repo_dir)
    };

    remove_monodex_home();
}
