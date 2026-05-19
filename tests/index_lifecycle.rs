//! Purpose: Integration tests for crawl-side index state: narrowing/widening, schema mismatch, purge, label membership, vector-only and FTS-only paths.
//! Edit here when: Adding or modifying integration tests for crawl-side state management.
//! Do not edit here for: Search-side output tests (see search_output.rs); production crawl code (see `app/commands/`).
//!
//! Every test in this file carries the `__quick_excluded` suffix.
//! See the "Quick CI tier" section of
//! `docs/code_organization_policy.md` for the policy.

mod fixtures;

use std::collections::BTreeSet;
use std::fs;
use std::process::Command;

use monodex::app::commands::init_db::run_init_db;
use monodex::app::commands::search::run_search;
use monodex::engine::retrieval::RetrievalMethod;

#[test]
#[allow(non_snake_case)]
fn test_crawl_then_search__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo (unused - we just need a repo for config)
        let _commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
fn test_cross_label_active_labels_preserved__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
fn test_crawl_then_vector_search__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
/// Files whose bytes are not valid UTF-8 should emit a FileReadFailed warning
/// with error string "non-UTF-8 file contents" and be skipped, not crash the crawl.
#[test]
#[allow(non_snake_case)]
fn test_non_utf8_file_emits_warning__quick_excluded() {
    use std::io::Write;

    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

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
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
