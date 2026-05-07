//! Purpose: Integration tests for search decision rules — end-to-end search retrieval selection assertions.
//! Edit here when: Adding or modifying end-to-end search decision rule tests.
//! Do not edit here for: Production search code (see `app/commands/search.rs`); per-module unit tests (see the relevant module's `tests.rs` or inline `#[cfg(test)]` block).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use lancedb::connect;
use serial_test::serial;

use monodex::app::commands::init_db::run_init_db;
use monodex::app::commands::search::run_search;
use monodex::app::config::Config;
use monodex::engine::{
    fts::index_chunks_for_fts,
    identifier::LabelId,
    retrieval::RetrievalMethod,
    storage::{ChunkRow, ChunkStorage, LabelMetadataRow, LabelStorage, SOURCE_KIND_GIT_COMMIT},
};

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

fn write_minimal_config(monodex_home: &Path) {
    let config_path = monodex_home.join("config.json");
    std::fs::create_dir_all(monodex_home).ok();
    std::fs::write(&config_path, r#"{"catalogs": {}}"#).unwrap();
}

fn test_chunk_row(row_id: &str, file_id: &str, ordinal: i32, label_id: &str) -> ChunkRow {
    ChunkRow {
        row_id: row_id.to_string(),
        text: "test content for search".to_string(),
        catalog: label_id
            .split(':')
            .next()
            .unwrap_or("test-catalog")
            .to_string(),
        active_label_ids: vec![label_id.to_string()],
        embedder_id: "test-embedder:v1".to_string(),
        chunker_id: "test-chunker:v1".to_string(),
        blob_id: "abc123".to_string(),
        content_hash: "hash-123".to_string(),
        file_id: file_id.to_string(),
        relative_path: "src/test.ts".to_string(),
        package_name: "test-package".to_string(),
        source_uri: "/path/to/test.ts".to_string(),
        chunk_ordinal: ordinal,
        chunk_count: 1,
        start_line: 1,
        end_line: 10,
        symbol_name: Some("testFunction".to_string()),
        chunk_type: "function".to_string(),
        chunk_kind: "content".to_string(),
        breadcrumb: Some("test-package:src/test.ts:testFunction".to_string()),
        split_part_ordinal: None,
        split_part_count: None,
        file_complete: true,
    }
}

/// Create a test label metadata row with explicit catalog and label.
fn test_label_metadata_row(catalog: &str, label: &str) -> LabelMetadataRow {
    LabelMetadataRow {
        label_id: format!("{}:{}", catalog, label),
        catalog: catalog.to_string(),
        label: label.to_string(),
        source_kind: SOURCE_KIND_GIT_COMMIT.to_string(),
        vector_source: Some("abc123def456".to_string()),
        vector_complete: true,
        fts_source: Some("abc123def456".to_string()),
        fts_complete: true,
        updated_at_unix_secs: 1700000000,
    }
}

/// Create a test label metadata row with explicit retrieval selection.
fn test_label_metadata_row_with_selection(
    catalog: &str,
    label: &str,
    vector_source: Option<&str>,
    vector_complete: bool,
    fts_source: Option<&str>,
    fts_complete: bool,
) -> LabelMetadataRow {
    LabelMetadataRow {
        label_id: format!("{}:{}", catalog, label),
        catalog: catalog.to_string(),
        label: label.to_string(),
        source_kind: SOURCE_KIND_GIT_COMMIT.to_string(),
        vector_source: vector_source.map(|s| s.to_string()),
        vector_complete,
        fts_source: fts_source.map(|s| s.to_string()),
        fts_complete,
        updated_at_unix_secs: 1700000000,
    }
}

/// Set up a test database at the default location under MONODEX_HOME.
/// Returns the database path and storage handles.
fn setup_test_db(monodex_home: &Path) -> (tempfile::TempDir, ChunkStorage, LabelStorage) {
    // Run init-db to create the database at the default location
    let config = Config {
        catalogs: std::collections::HashMap::new(),
        database: None,
        embedding_model: Default::default(),
    };
    run_init_db(&config, false).expect("init-db failed");

    // The database is now at <monodex_home>/default-db
    let db_path = monodex_home.join("default-db");

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = connect(db_path.to_str().unwrap())
            .execute()
            .await
            .expect("Failed to open database");

        let chunks_table = db
            .open_table("chunks")
            .execute()
            .await
            .expect("Failed to open chunks table");

        let labels_table = db
            .open_table("label_metadata")
            .execute()
            .await
            .expect("Failed to open label_metadata table");

        // Create a temp dir that will clean up, but the actual db is under monodex_home
        let tmp_dir = tempfile::TempDir::new().unwrap();

        (
            tmp_dir,
            ChunkStorage::new(Arc::new(chunks_table), monodex_home.join("default-db")),
            LabelStorage::new(Arc::new(labels_table), monodex_home.join("default-db")),
        )
    })
}

/// Test that search with both methods in selection produces PR1 stub error.
///
/// This verifies the decision table: when active subset has 2+ methods with equal sources,
/// PR1 should stub-error pointing at --retrieval.
#[test]
#[serial(monodex_home)]
fn test_search_both_methods_stub_error() {
    let monodex_home = tempfile::TempDir::new().unwrap();
    set_monodex_home(monodex_home.path());
    write_minimal_config(monodex_home.path());

    let (_tmp_dir, chunk_storage, label_storage) = setup_test_db(monodex_home.path());

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk with a vector (needed for storage but not for decision-rule error)
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);
    let vector = vec![0.0f32; 768];

    // Use a runtime for async storage operations
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        chunk_storage
            .upsert_with_vectors(&[chunk], &[vector])
            .await
            .unwrap();

        // Create label metadata with both methods complete at same commit
        let label_row = test_label_metadata_row(catalog, label);
        label_storage.upsert(&label_row).await.unwrap();
    });

    // Build a minimal Config
    let config = Config {
        catalogs: std::collections::HashMap::new(),
        database: None,
        embedding_model: Default::default(),
    };

    // Run search without --retrieval (should trigger stub error)
    let result = run_search(
        &config,
        "test query",
        10,
        Some(label),
        Some(catalog),
        None, // no --retrieval flag = all methods
        false,
    );

    assert!(
        result.is_err(),
        "Search should return error for multi-method in PR1"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Hybrid search across multiple retrieval methods is not yet implemented"),
        "Error should mention hybrid search not implemented, got: {}",
        err_msg
    );
    assert!(
        err_msg.contains("--retrieval"),
        "Error should suggest --retrieval flag, got: {}",
        err_msg
    );

    // Explicitly drop to release file handles before cleanup
    drop(chunk_storage);
    drop(label_storage);
    remove_monodex_home();
}

/// Test that search with fts-only selection succeeds.
///
/// This verifies that when selection has only fts, search proceeds without stub error.
#[test]
#[serial(monodex_home)]
fn test_search_fts_only_selection() {
    let monodex_home = tempfile::TempDir::new().unwrap();
    set_monodex_home(monodex_home.path());
    write_minimal_config(monodex_home.path());

    let (_tmp_dir, chunk_storage, label_storage) = setup_test_db(monodex_home.path());
    let db_path = monodex_home.path().join("default-db");

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk with searchable text
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);

    // Use a runtime for async storage operations
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        chunk_storage
            .upsert_with_vectors(std::slice::from_ref(&chunk), &[vec![0.0f32; 768]])
            .await
            .unwrap();

        // Create label metadata with only fts in selection (vector_source = None)
        let label_row = test_label_metadata_row_with_selection(
            catalog,
            label,
            None,                 // vector_source
            false,                // vector_complete (don't care)
            Some("abc123def456"), // fts_source
            true,                 // fts_complete
        );
        label_storage.upsert(&label_row).await.unwrap();

        // Build the FTS index so search can actually query it
        let fts_label_id = LabelId::new(catalog, label).expect("valid label id");
        index_chunks_for_fts(
            &db_path,
            &fts_label_id,
            &chunk_storage,
            true, // is_commit_mode
        )
        .await
        .expect("FTS indexing should succeed");
    });

    // Build a minimal Config
    let config = Config {
        catalogs: std::collections::HashMap::new(),
        database: None,
        embedding_model: Default::default(),
    };

    // Run search - should succeed (FTS-only selection)
    let result = run_search(
        &config,
        "test content",
        10,
        Some(label),
        Some(catalog),
        None, // no --retrieval flag = use selection (fts only)
        false,
    );

    assert!(
        result.is_ok(),
        "FTS-only search should succeed, got error: {:?}",
        result.err()
    );

    // Explicitly drop to release file handles before cleanup
    drop(chunk_storage);
    drop(label_storage);
    remove_monodex_home();
}

/// Test that search --retrieval vector errors when vector not in selection.
///
/// This verifies the explicit-flag form: requesting a method not in selection
/// produces a clear error message with a substituted source pointer.
#[test]
#[serial(monodex_home)]
fn test_search_vector_not_in_selection_error() {
    let monodex_home = tempfile::TempDir::new().unwrap();
    set_monodex_home(monodex_home.path());
    write_minimal_config(monodex_home.path());

    let (_tmp_dir, chunk_storage, label_storage) = setup_test_db(monodex_home.path());

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);

    // Use a runtime for async storage operations
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        chunk_storage
            .upsert_with_vectors(&[chunk], &[vec![0.0f32; 768]])
            .await
            .unwrap();

        // Create label metadata with only fts in selection
        let label_row = test_label_metadata_row_with_selection(
            catalog,
            label,
            None,                 // vector_source
            false,                // vector_complete (don't care)
            Some("abc123def456"), // fts_source
            true,                 // fts_complete
        );
        label_storage.upsert(&label_row).await.unwrap();
    });

    // Build a minimal Config
    let config = Config {
        catalogs: std::collections::HashMap::new(),
        database: None,
        embedding_model: Default::default(),
    };

    // Run search with --retrieval vector (not in selection)
    let retrieval: Option<BTreeSet<RetrievalMethod>> =
        Some([RetrievalMethod::Vector].into_iter().collect());
    let result = run_search(
        &config,
        "test query",
        10,
        Some(label),
        Some(catalog),
        retrieval,
        false,
    );

    assert!(
        result.is_err(),
        "Search should error when requesting method not in selection"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("is not in this label's retrieval selection"),
        "Error should mention method not in selection, got: {}",
        err_msg
    );
    // Verify the source pointer is substituted (not literal [source])
    assert!(
        err_msg.contains("--commit abc123def456"),
        "Error should contain substituted source pointer '--commit abc123def456', got: {}",
        err_msg
    );
    assert!(
        !err_msg.contains("[source]"),
        "Error should NOT contain literal '[source]' token, got: {}",
        err_msg
    );

    // Explicitly drop to release file handles before cleanup
    drop(chunk_storage);
    drop(label_storage);
    remove_monodex_home();
}

/// Test that search with sources disagree produces hard error.
///
/// This verifies the decision table: when vector and fts have different source commits,
/// search errors with clear message about the mismatch including substituted source pointer.
#[test]
#[serial(monodex_home)]
fn test_search_sources_disagree_error() {
    let monodex_home = tempfile::TempDir::new().unwrap();
    set_monodex_home(monodex_home.path());
    write_minimal_config(monodex_home.path());

    let (_tmp_dir, chunk_storage, label_storage) = setup_test_db(monodex_home.path());

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);

    // Use a runtime for async storage operations
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        chunk_storage
            .upsert_with_vectors(&[chunk], &[vec![0.0f32; 768]])
            .await
            .unwrap();

        // Create label metadata with different sources for vector and fts
        let label_row = test_label_metadata_row_with_selection(
            catalog,
            label,
            Some("commit_aaa111"), // vector_source
            true,                  // vector_complete
            Some("commit_bbb222"), // fts_source (different!)
            true,                  // fts_complete
        );
        label_storage.upsert(&label_row).await.unwrap();
    });

    // Build a minimal Config
    let config = Config {
        catalogs: std::collections::HashMap::new(),
        database: None,
        embedding_model: Default::default(),
    };

    // Run search without --retrieval (should detect source mismatch)
    let result = run_search(
        &config,
        "test query",
        10,
        Some(label),
        Some(catalog),
        None,
        false,
    );

    assert!(result.is_err(), "Search should error when sources disagree");
    let err_msg = result.unwrap_err().to_string();
    // Should mention both sources
    assert!(
        err_msg.contains("commit_aaa111"),
        "Error should mention vector source commit_aaa111, got: {}",
        err_msg
    );
    assert!(
        err_msg.contains("commit_bbb222"),
        "Error should mention fts source commit_bbb222, got: {}",
        err_msg
    );
    // Should have substituted source pointer (not literal [source])
    assert!(
        !err_msg.contains("[source]"),
        "Error should NOT contain literal '[source]' token, got: {}",
        err_msg
    );

    // Explicitly drop to release file handles before cleanup
    drop(chunk_storage);
    drop(label_storage);
    remove_monodex_home();
}

/// Test that incomplete method with explicit --retrieval warns but proceeds.
///
/// This verifies the explicit-flag bug fix from item #1: when the user explicitly
/// requests an incomplete method via --retrieval, the search should warn and proceed,
/// NOT hard-error with "all in-selection methods incomplete".
#[test]
#[serial(monodex_home)]
fn test_search_incomplete_method_warning() {
    let monodex_home = tempfile::TempDir::new().unwrap();
    set_monodex_home(monodex_home.path());
    write_minimal_config(monodex_home.path());

    let (_tmp_dir, chunk_storage, label_storage) = setup_test_db(monodex_home.path());
    let db_path = monodex_home.path().join("default-db");

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk with searchable text
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);

    // Use a runtime for async storage operations
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        chunk_storage
            .upsert_with_vectors(std::slice::from_ref(&chunk), &[vec![0.0f32; 768]])
            .await
            .unwrap();

        // Create label metadata with FTS in selection but incomplete
        let label_row = test_label_metadata_row_with_selection(
            catalog,
            label,
            None,                 // vector_source (not in selection)
            false,                // vector_complete (don't care)
            Some("abc123def456"), // fts_source
            false,                // fts_complete = false (incomplete!)
        );
        label_storage.upsert(&label_row).await.unwrap();

        // Build the FTS index so search can actually query it
        // (even though it's marked incomplete, we need an actual index for the test)
        let fts_label_id = LabelId::new(catalog, label).expect("valid label id");
        index_chunks_for_fts(
            &db_path,
            &fts_label_id,
            &chunk_storage,
            true, // is_commit_mode
        )
        .await
        .expect("FTS indexing should succeed");
    });

    // Build a minimal Config
    let config = Config {
        catalogs: std::collections::HashMap::new(),
        database: None,
        embedding_model: Default::default(),
    };

    // Run search with explicit --retrieval fts (incomplete method)
    // This is the key test: pre-fix, this would hard-error
    // Post-fix, it should warn and proceed
    let retrieval: Option<BTreeSet<RetrievalMethod>> =
        Some([RetrievalMethod::Fts].into_iter().collect());
    let result = run_search(
        &config,
        "test content",
        10,
        Some(label),
        Some(catalog),
        retrieval,
        false,
    );

    // Post-fix: should succeed (warns on stdout, returns Ok)
    assert!(
        result.is_ok(),
        "Search with explicit --retrieval on incomplete method should succeed, got error: {:?}",
        result.err()
    );

    // Explicitly drop to release file handles before cleanup
    drop(chunk_storage);
    drop(label_storage);
    remove_monodex_home();
}
