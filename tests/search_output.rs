//! Purpose: Integration tests for search-side output rendering: preambles, warnings, sentinels, hybrid degradation, parse-error paths.
//! Edit here when: Adding or modifying integration tests for search output behavior.
//! Do not edit here for: Crawl-side state tests (see index_lifecycle.rs); production search code (see `app/commands/`).
//!
//! Every test in this file carries the `__quick_excluded` suffix.
//! See the "Quick CI tier" section of
//! `docs/code_organization_policy.md` for the policy.

mod fixtures;

use std::collections::BTreeSet;
use std::process::Command;

use monodex::app::commands::init_db::run_init_db;
use monodex::app::commands::search::run_search;
use monodex::engine::retrieval::RetrievalMethod;

#[test]
#[allow(non_snake_case)]
fn test_fts_query_parse_error__quick_excluded() {
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

/// Test multi-method explicit search:
/// - After a --retrieval-less crawl (selection={fts, vector})
/// - Run `monodex search --retrieval fts --retrieval vector`
/// - Confirm the hybrid search succeeds
#[test]
#[allow(non_snake_case)]
fn test_multi_method_explicit_search__quick_excluded() {
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

        // Should succeed with hybrid search
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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        // Using the binary directly exercises end-to-end argv parsing and process behavior
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
            .output()
            .expect("failed to execute monodex search");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // The command should succeed with hybrid search
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
fn test_working_dir_remediation_message__quick_excluded() {
    let (_monodex_home, _repo_dir) = {
        // Set up temp directories
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo (we crawl working-dir, but need git for the repo structure)
        let _commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

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
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
        let monodex_home = fixtures::unique_temp_dir();
        let repo_dir = fixtures::unique_temp_dir();

        // Create test git repo (small corpus)
        let commit_oid = fixtures::create_test_git_repo(repo_dir.path());

        // Create config pointing to the repo
        let config =
            fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
fn test_hybrid_search_degrades_on_stale_fts__quick_excluded() {
    use monodex::engine::fts::{FtsIndex, FtsManifest};
    use monodex::engine::identity::FTS_TOKENIZER_ID;

    let monodex_home = fixtures::unique_temp_dir();
    let repo_dir = fixtures::unique_temp_dir();

    // Create a Git repo with a TypeScript file
    fixtures::create_test_git_repo(repo_dir.path());

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
    let config = fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
    use monodex::engine::identity::FTS_TOKENIZER_ID;
    use monodex::engine::retrieval::RetrievalMethod;

    let monodex_home = fixtures::unique_temp_dir();
    let repo_dir = fixtures::unique_temp_dir();

    // Create a Git repo with a TypeScript file
    fixtures::create_test_git_repo(repo_dir.path());

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
    let config = fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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

    let monodex_home = fixtures::unique_temp_dir();
    let repo_dir = fixtures::unique_temp_dir();

    // Create a Git repo with a TypeScript file
    fixtures::create_test_git_repo(repo_dir.path());

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
    let config = fixtures::create_test_config(monodex_home.path(), "test-catalog", repo_dir.path());

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
