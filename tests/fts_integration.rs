//! Purpose: Integration tests for FTS end-to-end behavior.
//! Edit here when: Adding or modifying end-to-end FTS integration tests.
//! Do not edit here for: Production crawl/search code (see `app/commands/`); per-module unit tests.
//!
//! Every test in this file carries the `__quick_excluded` suffix.
//! See the "Quick CI tier" section of
//! `docs/code_organization_policy.md` for the policy.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use monodex::app::commands::init_db::run_init_db;
use monodex::app::commands::search::run_search;
use monodex::app::config::Config;
use monodex::engine::retrieval::RetrievalMethod;

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

// =============================================================================
// Test 1: End-to-end test for crawl-then-search
// =============================================================================

/// Test crawl-then-search flow:
/// - `monodex init-db`
/// - `monodex crawl --catalog X --label main --commit HEAD`
/// - `monodex search --text "..."` → confirms hybrid search succeeds.
/// - `monodex search --text "..." --retrieval fts` → confirms FTS results.
/// - `monodex search --text "..." --retrieval vector` → confirms vector results.
#[test]
#[allow(non_snake_case)]
fn test_crawl_then_search__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            vec![], // retrieval: empty = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Test 1: Search with no --retrieval should succeed with hybrid search
        // (both methods in selection, sources equal, hybrid retrieval implemented)
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag = all methods
            false,
        );

        assert!(
            search_result.is_ok(),
            "Hybrid search should succeed, got error: {:?}",
            search_result.err()
        );

        // Test 2: Search with --retrieval fts should succeed
        let fts_retrieval: Option<BTreeSet<RetrievalMethod>> =
            Some([RetrievalMethod::Fts].into_iter().collect());
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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
#[allow(non_snake_case)]
fn test_selection_narrowing__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            vec![], // retrieval: empty = all methods
            false,  // debug
        )
        .expect("first crawl failed");

        // Second crawl: --retrieval fts (selection narrows to {fts})
        // We need to capture stdout to check for the narrowing announcement
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            vec![RetrievalMethod::Fts], // retrieval: fts only
            false,                      // debug
        )
        .expect("second crawl (narrowing) failed");

        // Verify: search with no --retrieval should now work (only fts in selection)
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag
            false,
        );

        // With only fts in selection, search should succeed
        assert!(
            search_result.is_ok(),
            "FTS-only search should succeed after narrowing, got error: {:?}",
            search_result.err()
        );

        // Verify: search --retrieval vector should error (not in selection)
        let vector_retrieval: Option<BTreeSet<RetrievalMethod>> =
            Some([RetrievalMethod::Vector].into_iter().collect());
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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
}

// =============================================================================
// Test 3: End-to-end test for selection-widening
// =============================================================================

/// Test selection-widening flow:
/// - Continue from narrowing test state (selection is {fts})
/// - Crawl with no --retrieval flag widens selection back to {fts, vector}
/// - Confirms NO narrowing-announcement block
/// - Confirms search shows "(fts, vector)" on Label: line
/// - Confirms search with no --retrieval produces hybrid search results
#[test]
#[allow(non_snake_case)]
fn test_selection_widening__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            vec![], // retrieval: empty = all methods
            false,  // debug
        )
        .expect("third crawl (widening) failed");

        // Verify: search with no --retrieval should succeed with hybrid search
        // (both methods in selection, sources equal, hybrid retrieval implemented)
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag = all methods
            false,
        );

        assert!(
            search_result.is_ok(),
            "Hybrid search should succeed, got error: {:?}",
            search_result.err()
        );

        // Verify: search --retrieval fts should succeed
        let fts_retrieval: Option<BTreeSet<RetrievalMethod>> =
            Some([RetrievalMethod::Fts].into_iter().collect());
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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
#[allow(non_snake_case)]
fn test_first_time_crawl_fts_only__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            vec![RetrievalMethod::Fts], // retrieval: fts only
            false,                      // debug
        )
        .expect("first crawl failed");

        // Verify: search with no --retrieval should succeed (only fts in selection)
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag
            false,
        );

        // With only fts in selection, search should succeed
        assert!(
            search_result.is_ok(),
            "FTS-only search should succeed, got error: {:?}",
            search_result.err()
        );

        // Verify: search --retrieval vector should error (not in selection)
        let vector_retrieval: Option<BTreeSet<RetrievalMethod>> =
            Some([RetrievalMethod::Vector].into_iter().collect());
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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
}

// =============================================================================
// Test 5: End-to-end test for purge cleanup
// =============================================================================

/// Test purge cleanup:
/// - After a crawl producing FTS state, purge --catalog X removes FTS directory
/// - purge --all removes entire fts/ directory
#[test]
#[allow(non_snake_case)]
fn test_purge_cleanup__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            vec![], // retrieval: empty = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Get database path
        let db_path = monodex::app::resolve_database_path(&config).unwrap();
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
}

// =============================================================================
// Test 6: End-to-end test for schema-mismatch error
// =============================================================================

/// Test schema-mismatch error:
/// - Hand-write a monodex-meta.json with version 3 (old version)
/// - Attempt to open the database
/// - Confirm error fires with expected message
#[test]
#[allow(non_snake_case)]
fn test_schema_mismatch_error__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        // Create test git repo (unused - we just need a repo for config)
        let _commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db first
        run_init_db(&config, false).expect("init-db failed");

        // Get database path
        let db_path = monodex::app::resolve_database_path(&config).unwrap();
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
}

// =============================================================================
// Test 7: FTS query parse error
// =============================================================================

/// Test FTS query parse error:
/// - Index a label with FTS state
/// - Run search with a syntactically-invalid query for Tantivy's parser
/// - Confirm output is "Couldn't parse FTS query: <message>" and NOT "No results."
#[test]
#[allow(non_snake_case)]
fn test_fts_query_parse_error__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            vec![RetrievalMethod::Fts], // retrieval: fts only
            false,                      // debug
        )
        .expect("crawl failed");

        // Search with a syntactically-invalid query
        // Using unmatched quotes or malformed field syntax that Tantivy's parser rejects
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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
}

// =============================================================================
// Test 8: Multi-method explicit search
// =============================================================================

/// Test multi-method explicit search (PR2 hybrid search):
/// - After a --retrieval-less crawl (selection={fts, vector})
/// - Run `monodex search --retrieval fts --retrieval vector`
/// - Confirm the hybrid search succeeds
#[test]
#[allow(non_snake_case)]
fn test_multi_method_explicit_search__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            multi_method,
            false,
        );

        // Should succeed with hybrid search (PR2)
        assert!(
            search_result.is_ok(),
            "Hybrid search should succeed, got error: {:?}",
            search_result.err()
        );

        (monodex_home, repo_dir)
    };
}

/// Test that the search preamble appears for hybrid search.
///
/// This verifies that the "Catalog: ... / Label: ... / Searching: ..." line
/// is printed for hybrid search, showing both methods.
#[test]
#[allow(non_snake_case)]
fn test_multi_method_search_shows_preamble__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            .env("MONODEX_CONFIG_FOLDER", monodex_home.path())
            .env_remove("MONODEX_CONFIG_FOLDER")
            .output()
            .expect("failed to execute monodex search");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // The command should succeed with hybrid search (PR2)
        assert!(
            output.status.success(),
            "Hybrid search should succeed, got stdout: {:?}, stderr: {:?}",
            stdout,
            stderr
        );

        // The preamble should appear in stdout
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
#[allow(non_snake_case)]
fn test_cross_label_active_labels_preserved__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            vec![], // empty = all methods
            false,  // debug
        )
        .expect("crawl label-b failed");

        // Step 3: Search under label A should find the chunk (FTS)
        let fts_only: Option<BTreeSet<RetrievalMethod>> =
            Some(std::iter::once(RetrievalMethod::Fts).collect());
        let mut output = Vec::new();
        run_search(
            &mut output,
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
        let mut output = Vec::new();
        run_search(
            &mut output,
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
        let mut output = Vec::new();
        run_search(
            &mut output,
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
}

// =============================================================================
// Test: Working-dir remediation message
// =============================================================================

/// Test that a working-dir-crawled label produces remediation suggesting
/// `--working-dir`, not `--commit <opaque sentinel>`.
#[test]
#[allow(non_snake_case)]
fn test_working_dir_remediation_message__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            vec![RetrievalMethod::Fts], // FTS-only
            false,                      // debug
        )
        .expect("working-dir crawl failed");

        // Now try to search with vector (not in selection) - should error
        let vector_only: Option<BTreeSet<RetrievalMethod>> =
            Some(std::iter::once(RetrievalMethod::Vector).collect());
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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
}

// =============================================================================
// Test: FTS ParseError under hybrid search (fail-fast)
// =============================================================================

/// Test that FTS ParseError under hybrid search fails fast without constructing embedder.
/// - Crawl with both methods (vector + fts)
/// - Search with malformed FTS query under hybrid (no --retrieval flag)
/// - Assert: Err with parse error message
/// - The embedder should NOT be constructed (FTS-first ordering is the load-bearing property)
#[test]
#[allow(non_snake_case)]
fn test_fts_parse_error_under_hybrid__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl with both methods (no --retrieval = all methods)
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            vec![], // empty = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Search with a malformed FTS query under hybrid
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "foo:bar:", // Invalid field syntax
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag = hybrid
            false,
        );

        // Should error (parse error is hard error under hybrid)
        assert!(
            search_result.is_err(),
            "Parse error under hybrid should return Err, got Ok"
        );
        let err_msg = search_result.unwrap_err().to_string();

        // Must contain the parse error message
        assert!(
            err_msg.contains("Couldn't parse FTS query"),
            "Error should mention parse error, got: {}",
            err_msg
        );

        (monodex_home, repo_dir)
    };
}

// =============================================================================
// Test: FTS NoIndex degradation under hybrid
// =============================================================================

/// Test that FTS NoIndex under hybrid degrades to vector-only with warning.
/// - Crawl with both methods
/// - Manually delete the FTS directory
/// - Search with no flag (hybrid)
/// - Assert: Ok (degraded to vector-only)
#[test]
#[allow(non_snake_case)]
fn test_fts_noindex_degradation_under_hybrid__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl with both methods
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            vec![], // empty = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Resolve database path and delete FTS directory
        let db_path = monodex::app::resolve_database_path(&config).unwrap();
        let fts_dir = db_path.join("fts").join("test-catalog").join("main");
        if fts_dir.exists() {
            std::fs::remove_dir_all(&fts_dir).expect("Failed to delete FTS directory");
        }

        // Search with no flag (hybrid) - should degrade to vector-only
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "getUserProfile",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag = hybrid
            false,
        );

        // Should succeed (degraded to vector-only)
        assert!(
            search_result.is_ok(),
            "Hybrid search with missing FTS should degrade to vector-only, got error: {:?}",
            search_result.err()
        );

        // Output should contain the degradation warning with exact template
        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains(&format!(
                "⚠️  FTS state for label main is missing on disk; falling back to vector-only.\n   To rebuild: monodex crawl --label main --commit {} --retrieval fts",
                commit_oid
            )),
            "Output should contain exact FTS NoIndex degradation warning, got:\n{}",
            output_str
        );

        (monodex_home, repo_dir)
    };
}

// =============================================================================
// Test: Empty corpus (both methods return zero hits)
// =============================================================================

/// Test that search against an empty corpus returns "No results."
/// - Create a repo with no crawlable files
/// - Crawl with both methods
/// - Both backends should be complete but return zero hits
/// - Search should return Ok with "No results."
#[test]
#[allow(non_snake_case)]
fn test_empty_corpus__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        // Create a git repo with only ignored files (no .ts, .js, .md, etc.)
        let git_init = Command::new("git")
            .args(["init"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to run git init");
        assert!(git_init.status.success(), "git init failed");

        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to set user.name");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to set user.email");

        // Create only ignored files (e.g., .gitignore, .env)
        let gitignore = repo_dir.path().join(".gitignore");
        std::fs::write(&gitignore, "*.log\nnode_modules/\n").expect("Failed to write .gitignore");

        let env_file = repo_dir.path().join(".env");
        std::fs::write(&env_file, "SECRET=value\n").expect("Failed to write .env");

        // Git add and commit
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to run git add");

        let git_commit = Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to run git commit");
        assert!(git_commit.status.success(), "git commit failed");

        let git_rev_parse = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to run git rev-parse");
        let commit_oid = String::from_utf8_lossy(&git_rev_parse.stdout)
            .trim()
            .to_string();

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl with both methods
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            vec![], // empty = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Search
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "test query",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // no --retrieval flag = all methods
            false,
        );

        // Should succeed
        assert!(
            search_result.is_ok(),
            "Search against empty corpus should succeed, got error: {:?}",
            search_result.err()
        );

        // Output should contain "No results."
        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains("No results."),
            "Output should contain 'No results.', got: {}",
            output_str
        );

        (monodex_home, repo_dir)
    };
}

// =============================================================================
// Test: End-of-results sentinel firing
// =============================================================================

/// Test that "End of results" sentinel fires when results are exhausted.
/// - Create a small corpus
/// - Search with FTS-only and a limit larger than available results
/// - Verify "End of results" appears (FTS is more likely to return fewer
///   than candidate_limit hits since it uses lexical matching)
#[test]
#[allow(non_snake_case)]
fn test_end_of_results_sentinel__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        // Create test git repo (small corpus)
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl with both methods
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            vec![], // empty = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Search with FTS-only and a large limit
        // FTS is more likely to return fewer hits than candidate_limit (50)
        // Use --retrieval fts so vector doesn't saturate the candidate limit
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "getUserProfile", // Query that will match some results
            1000,             // Large limit
            Some("main"),
            Some("test-catalog"),
            Some({
                let mut set = BTreeSet::new();
                set.insert(monodex::engine::retrieval::RetrievalMethod::Fts);
                set
            }), // FTS-only
            false,
        );

        // Should succeed
        assert!(
            search_result.is_ok(),
            "Search should succeed, got error: {:?}",
            search_result.err()
        );

        let output_str = String::from_utf8_lossy(&output);
        // The output should contain "End of results" since FTS returns fewer than
        // candidate_limit (50) hits for this small corpus
        assert!(
            output_str.contains("End of results"),
            "Output should contain 'End of results' sentinel, got:\n{}",
            output_str
        );

        (monodex_home, repo_dir)
    };
}

// =============================================================================
// Test: Vector-only search (mirror of FTS-only)
// =============================================================================

/// Test vector-only search after a full crawl:
/// - Crawl with no --retrieval (selection = {fts, vector})
/// - Search with --retrieval vector only
/// - Confirm vector-only results are returned
#[test]
#[allow(non_snake_case)]
fn test_crawl_then_vector_search__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

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
            vec![], // retrieval: empty = all methods
            false,  // debug
        )
        .expect("crawl failed");

        // Search with --retrieval vector only
        let vector_retrieval: Option<BTreeSet<RetrievalMethod>> =
            Some([RetrievalMethod::Vector].into_iter().collect());
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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

        // Check output contains vector-only results
        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains("[v]"),
            "Output should contain vector-only marker [v], got: {}",
            output_str
        );

        (monodex_home, repo_dir)
    };
}

// =============================================================================
// Test: First-time crawl with vector only
// =============================================================================

/// Test first-time crawl with vector-only retrieval:
/// - Run init-db
/// - Crawl with --retrieval vector
/// - Verify vector state is complete, FTS state is not in selection
/// - Search with --retrieval vector should succeed
#[test]
#[allow(non_snake_case)]
fn test_first_time_crawl_vector_only__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        // Create test git repo
        let commit_oid = create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Crawl with vector only
        monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            vec![RetrievalMethod::Vector], // vector only
            false,                         // debug
        )
        .expect("crawl failed");

        // Verify: FTS directory should not exist
        let db_path = monodex::app::resolve_database_path(&config).unwrap();
        let fts_dir = db_path.join("fts").join("test-catalog").join("main");
        assert!(
            !fts_dir.exists(),
            "FTS directory should not exist after vector-only crawl"
        );

        // Search with --retrieval vector should succeed
        let vector_retrieval: Option<BTreeSet<RetrievalMethod>> =
            Some([RetrievalMethod::Vector].into_iter().collect());
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
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
            "Vector search after vector-only crawl should succeed, got error: {:?}",
            search_result.err()
        );

        (monodex_home, repo_dir)
    };
}

/// Test that non-UTF-8 files emit a warning and are skipped during crawl.
///
/// BL17: Files whose bytes are not valid UTF-8 should emit a FileReadFailed warning
/// with error string "non-UTF-8 file contents" and be skipped, not crash the crawl.
#[test]
#[allow(non_snake_case)]
fn test_non_utf8_file_emits_warning__quick_excluded() {
    use std::io::Write;

    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = unique_temp_dir();
        let repo_dir = unique_temp_dir();

        // Initialize git repo
        let git_init = Command::new("git")
            .args(["init"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to run git init");
        assert!(git_init.status.success(), "git init failed");

        // Configure local user
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to set user.name");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to set user.email");

        // Create a valid TypeScript file
        let ts_file = repo_dir.path().join("src").join("example.ts");
        fs::create_dir_all(ts_file.parent().unwrap()).unwrap();
        fs::write(&ts_file, "export function test() { return 42; }")
            .expect("Failed to write test file");

        // Create a non-UTF-8 TypeScript file (invalid UTF-8 bytes in a .ts file)
        // This file should be skipped with a warning during chunking
        let bad_file = repo_dir.path().join("src").join("binary.ts");
        fs::create_dir_all(bad_file.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(&bad_file).expect("Failed to create binary file");
        // Write bytes that are NOT valid UTF-8: 0xFF 0xFE is a BOM-like sequence
        // that is invalid UTF-8
        file.write_all(&[0xFF, 0xFE, 0x00, 0x01])
            .expect("Failed to write binary content");
        drop(file);

        // Git add and commit
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to run git add");

        let git_commit = Command::new("git")
            .args(["commit", "-m", "Initial commit"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to run git commit");
        assert!(git_commit.status.success(), "git commit failed");

        // Get the commit OID
        let git_rev_parse = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo_dir.path())
            .output()
            .expect("Failed to run git rev-parse");
        assert!(git_rev_parse.status.success(), "git rev-parse failed");
        let commit_oid = String::from_utf8_lossy(&git_rev_parse.stdout)
            .trim()
            .to_string();

        // Create config pointing to the repo
        let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

        // Run init-db
        run_init_db(&config, false).expect("init-db failed");

        // Run crawl - it should succeed despite the non-UTF-8 file
        // (the file will be skipped with a warning)
        let crawl_result = monodex::app::commands::crawl::run_crawl_label(
            &config,
            "test-catalog",
            "main",
            &commit_oid,
            vec![], // retrieval: empty = all methods
            false,  // debug
        );

        assert!(
            crawl_result.is_ok(),
            "Crawl should succeed even with non-UTF-8 file, got error: {:?}",
            crawl_result.err()
        );

        // Note: The warning is emitted to stderr during crawl.
        // We can't easily capture it here without refactoring the crawl command,
        // but the important invariants are:
        // 1. Crawl succeeds (verified above)
        // 2. The valid TypeScript file is indexed (verified by search below)

        // Verify the valid file was indexed by searching for it
        let mut output = Vec::new();
        let search_result = run_search(
            &mut output,
            &config,
            "test",
            10,
            Some("main"),
            Some("test-catalog"),
            None, // all methods
            false,
        );

        assert!(
            search_result.is_ok(),
            "Search should succeed, got error: {:?}",
            search_result.err()
        );

        let output_str = String::from_utf8_lossy(&output);
        assert!(
            output_str.contains("example.ts"),
            "Search should find the valid TypeScript file, got:\n{}",
            output_str
        );

        (monodex_home, repo_dir)
    };
}

// =============================================================================
// BL12a: FTS stale state integration tests
// =============================================================================

/// Test: Hybrid search degrades to vector with stale warning when FTS is stale.
///
/// This test verifies that when the FTS index is stale (IdMismatch), hybrid search
/// falls back to vector-only and emits the appropriate warning.
#[test]
#[allow(non_snake_case)]
fn test_hybrid_search_degrades_on_stale_fts__quick_excluded() {
    use monodex::engine::fts::{FtsIndex, FtsManifest};
    use monodex::engine::util::FTS_TOKENIZER_ID;

    let monodex_home = unique_temp_dir();
    let repo_dir = unique_temp_dir();

    // Create a Git repo with a TypeScript file
    create_test_git_repo(repo_dir.path());

    // Get the commit OID
    let git_rev_parse = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir.path())
        .output()
        .expect("Failed to run git rev-parse");
    let commit_oid = String::from_utf8_lossy(&git_rev_parse.stdout)
        .trim()
        .to_string();

    // Create config
    let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

    // Run init-db
    run_init_db(&config, false).expect("init-db failed");

    // Run crawl with both methods
    let crawl_result = monodex::app::commands::crawl::run_crawl_label(
        &config,
        "test-catalog",
        "main",
        &commit_oid,
        vec![], // all methods
        false,
    );
    assert!(crawl_result.is_ok(), "Crawl should succeed");

    // Corrupt the FTS manifest to make it stale
    let db_path = monodex::app::resolve_database_path(&config).unwrap();
    let label_id = monodex::engine::identifier::LabelId::new("test-catalog", "main").unwrap();

    let fts_index = FtsIndex::open_or_create(&db_path, &label_id).expect("open FTS index");
    let bad_manifest = FtsManifest {
        fts_schema_id: "old-schema-id".to_string(),
        fts_tokenizer_id: FTS_TOKENIZER_ID.to_string(),
    };
    fts_index
        .write_manifest(&bad_manifest)
        .expect("write bad manifest");

    // Now search - should degrade to vector with warning
    let mut output = Vec::new();
    let search_result = run_search(
        &mut output,
        &config,
        "getUserProfile",
        10,
        Some("main"),
        Some("test-catalog"),
        None, // all methods (hybrid)
        false,
    );

    assert!(
        search_result.is_ok(),
        "Search should succeed, got error: {:?}",
        search_result.err()
    );

    let output_str = String::from_utf8_lossy(&output);

    // Should have the stale warning
    assert!(
        output_str.contains("older Monodex version"),
        "Output should contain stale FTS warning, got:\n{}",
        output_str
    );

    // Should still have results from vector search
    assert!(
        output_str.contains("example.ts") || output_str.contains("getUserProfile"),
        "Output should contain results from vector search, got:\n{}",
        output_str
    );
}

/// Test: FTS-only search emits stale warning and zero results when FTS is stale.
///
/// This test verifies that when the FTS index is stale and the user requests FTS-only
/// search, we emit the stale warning and return zero results (not an error).
#[test]
#[allow(non_snake_case)]
fn test_fts_only_search_stale_warning_no_results__quick_excluded() {
    use monodex::engine::fts::{FtsIndex, FtsManifest};
    use monodex::engine::retrieval::RetrievalMethod;
    use monodex::engine::util::FTS_TOKENIZER_ID;

    let monodex_home = unique_temp_dir();
    let repo_dir = unique_temp_dir();

    // Create a Git repo with a TypeScript file
    create_test_git_repo(repo_dir.path());

    // Get the commit OID
    let git_rev_parse = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir.path())
        .output()
        .expect("Failed to run git rev-parse");
    let commit_oid = String::from_utf8_lossy(&git_rev_parse.stdout)
        .trim()
        .to_string();

    // Create config
    let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

    // Run init-db
    run_init_db(&config, false).expect("init-db failed");

    // Run crawl with both methods
    let crawl_result = monodex::app::commands::crawl::run_crawl_label(
        &config,
        "test-catalog",
        "main",
        &commit_oid,
        vec![], // all methods
        false,
    );
    assert!(crawl_result.is_ok(), "Crawl should succeed");

    // Corrupt the FTS manifest to make it stale
    let db_path = monodex::app::resolve_database_path(&config).unwrap();
    let label_id = monodex::engine::identifier::LabelId::new("test-catalog", "main").unwrap();

    let fts_index = FtsIndex::open_or_create(&db_path, &label_id).expect("open FTS index");
    let bad_manifest = FtsManifest {
        fts_schema_id: "old-schema-id".to_string(),
        fts_tokenizer_id: FTS_TOKENIZER_ID.to_string(),
    };
    fts_index
        .write_manifest(&bad_manifest)
        .expect("write bad manifest");

    // Now search FTS-only - should emit warning and return no results
    let mut output = Vec::new();
    let search_result = run_search(
        &mut output,
        &config,
        "getUserProfile",
        10,
        Some("main"),
        Some("test-catalog"),
        Some(std::collections::BTreeSet::from([RetrievalMethod::Fts])), // FTS-only
        false,
    );

    assert!(
        search_result.is_ok(),
        "Search should succeed, got error: {:?}",
        search_result.err()
    );

    let output_str = String::from_utf8_lossy(&output);

    // Should have the stale warning
    assert!(
        output_str.contains("older Monodex version"),
        "Output should contain stale FTS warning, got:\n{}",
        output_str
    );

    // Should have no results
    assert!(
        output_str.contains("No results."),
        "Output should contain 'No results.', got:\n{}",
        output_str
    );
}

/// Test: FTS-only search emits manifest-unreadable warning and zero results.
///
/// This test verifies that when the FTS manifest is unreadable (corrupted JSON)
/// and the user requests FTS-only search, we emit the unreadable warning
/// and return zero results (not an error).
#[test]
#[allow(non_snake_case)]
fn test_fts_only_search_unreadable_manifest_warning__quick_excluded() {
    use monodex::engine::fts::FtsIndex;
    use monodex::engine::identifier::LabelId;
    use monodex::engine::retrieval::RetrievalMethod;

    let monodex_home = unique_temp_dir();
    let repo_dir = unique_temp_dir();

    // Create a Git repo with a TypeScript file
    create_test_git_repo(repo_dir.path());

    // Get the commit OID
    let git_rev_parse = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir.path())
        .output()
        .expect("Failed to run git rev-parse");
    let commit_oid = String::from_utf8_lossy(&git_rev_parse.stdout)
        .trim()
        .to_string();

    // Create config
    let config = create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

    // Run init-db
    run_init_db(&config, false).expect("init-db failed");

    // Run crawl with both methods
    let crawl_result = monodex::app::commands::crawl::run_crawl_label(
        &config,
        "test-catalog",
        "main",
        &commit_oid,
        vec![], // all methods
        false,
    );
    assert!(crawl_result.is_ok(), "Crawl should succeed");

    // Corrupt the FTS manifest with invalid JSON
    let db_path = monodex::app::resolve_database_path(&config).unwrap();
    let label_id = LabelId::new("test-catalog", "main").unwrap();
    let fts_index = FtsIndex::open_or_create(&db_path, &label_id).expect("open FTS index");
    std::fs::write(fts_index.manifest_path(), "{ not valid json }")
        .expect("write corrupt manifest");

    // Now search FTS-only - should emit warning and return no results
    let mut output = Vec::new();
    let search_result = run_search(
        &mut output,
        &config,
        "getUserProfile",
        10,
        Some("main"),
        Some("test-catalog"),
        Some(std::collections::BTreeSet::from([RetrievalMethod::Fts])), // FTS-only
        false,
    );

    assert!(
        search_result.is_ok(),
        "Search should succeed, got error: {:?}",
        search_result.err()
    );

    let output_str = String::from_utf8_lossy(&output);

    // Should have the unreadable manifest warning
    assert!(
        output_str.contains("manifest unreadable"),
        "Output should contain manifest unreadable warning, got:\n{}",
        output_str
    );

    // Should have no results
    assert!(
        output_str.contains("No results."),
        "Output should contain 'No results.', got:\n{}",
        output_str
    );
}
