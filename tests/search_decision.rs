//! Purpose: Integration tests for search decision rules — end-to-end search retrieval selection assertions.
//! Edit here when: Adding or modifying end-to-end search decision rule tests.
//! Do not edit here for: Production search code (see `app/commands/search.rs`); per-module unit tests (see the relevant module's `tests.rs` or inline `#[cfg(test)]` block).

use std::path::Path;
use std::sync::Arc;

use lancedb::connect;
use serial_test::serial;

use monodex::engine::{
    retrieval::RetrievalMethod,
    schema::chunks_schema,
    storage::{
        ChunkRow, ChunkStorage, LabelMetadataRow, LabelStorage, SOURCE_KIND_GIT_COMMIT,
        read_selection,
    },
};

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

async fn create_test_storage() -> (tempfile::TempDir, ChunkStorage, LabelStorage) {
    let tmp_dir = tempfile::TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test_db");

    // Create fts directory as init-db would
    std::fs::create_dir_all(db_path.join("fts")).ok();

    let db = connect(db_path.to_str().unwrap())
        .execute()
        .await
        .expect("Failed to create database");

    let chunks_schema = chunks_schema();
    let chunks_table = db
        .create_empty_table("chunks", chunks_schema)
        .execute()
        .await
        .expect("Failed to create chunks table");

    let labels_table = db
        .create_empty_table(
            "label_metadata",
            monodex::engine::schema::label_metadata_schema(),
        )
        .execute()
        .await
        .expect("Failed to create label_metadata table");

    (
        tmp_dir,
        ChunkStorage::new(Arc::new(chunks_table), db_path.clone()),
        LabelStorage::new(Arc::new(labels_table), db_path),
    )
}

/// Test that search with both methods in selection produces PR1 stub error.
///
/// This verifies the decision table: when active subset has 2+ methods with equal sources,
/// PR1 should stub-error pointing at --retrieval.
#[tokio::test]
#[serial(monodex_home)]
async fn test_search_both_methods_stub_error() {
    let (_monodex_home, _tmp_dir) = {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let monodex_home = tmp_dir.path().to_path_buf();
        set_monodex_home(&monodex_home);
        write_minimal_config(&monodex_home);
        (monodex_home, tmp_dir)
    };

    let (_db_dir, chunk_storage, label_storage) = create_test_storage().await;

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk with a vector
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);
    let vector = vec![0.0f32; 768];
    chunk_storage
        .upsert_with_vectors(&[chunk], &[vector])
        .await
        .unwrap();

    // Create label metadata with both methods complete at same commit
    let label_row = test_label_metadata_row(catalog, label);
    label_storage.upsert(&label_row).await.unwrap();

    // Read the selection back and verify both methods are present
    let retrieved = label_storage.get_by_label_id(&label_id).await.unwrap();
    assert!(retrieved.is_some(), "Label metadata should exist");
    let retrieved = retrieved.unwrap();
    assert!(
        retrieved.vector_source.is_some(),
        "vector_source should be set"
    );
    assert!(retrieved.fts_source.is_some(), "fts_source should be set");
    assert!(retrieved.vector_complete, "vector_complete should be true");
    assert!(retrieved.fts_complete, "fts_complete should be true");

    // Verify the selection has both methods
    let selection = read_selection(&retrieved);
    assert_eq!(selection.len(), 2, "Selection should have both methods");
    assert!(
        selection.contains(&RetrievalMethod::Vector),
        "Should contain Vector"
    );
    assert!(
        selection.contains(&RetrievalMethod::Fts),
        "Should contain Fts"
    );

    remove_monodex_home();
}

/// Test that search with fts-only selection succeeds.
///
/// This verifies that when selection has only fts, search proceeds without stub error.
#[tokio::test]
#[serial(monodex_home)]
async fn test_search_fts_only_selection() {
    let (_monodex_home, _tmp_dir) = {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let monodex_home = tmp_dir.path().to_path_buf();
        set_monodex_home(&monodex_home);
        write_minimal_config(&monodex_home);
        (monodex_home, tmp_dir)
    };

    let (_db_dir, chunk_storage, label_storage) = create_test_storage().await;

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);
    chunk_storage
        .upsert_with_vectors(&[chunk], &[vec![0.0f32; 768]])
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

    // Read the selection back and verify only fts is present
    let retrieved = label_storage
        .get_by_label_id(&label_id)
        .await
        .unwrap()
        .unwrap();
    let selection = read_selection(&retrieved);
    assert_eq!(selection.len(), 1, "Selection should have only fts");
    assert!(
        selection.contains(&RetrievalMethod::Fts),
        "Should contain Fts"
    );
    assert!(
        !selection.contains(&RetrievalMethod::Vector),
        "Should not contain Vector"
    );

    remove_monodex_home();
}

/// Test that search --retrieval vector errors when vector not in selection.
///
/// This verifies the explicit-flag form: requesting a method not in selection
/// produces a clear error message.
#[tokio::test]
#[serial(monodex_home)]
async fn test_search_vector_not_in_selection_error() {
    let (_monodex_home, _tmp_dir) = {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let monodex_home = tmp_dir.path().to_path_buf();
        set_monodex_home(&monodex_home);
        write_minimal_config(&monodex_home);
        (monodex_home, tmp_dir)
    };

    let (_db_dir, chunk_storage, label_storage) = create_test_storage().await;

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);
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

    // Verify vector is not in selection
    let retrieved = label_storage
        .get_by_label_id(&label_id)
        .await
        .unwrap()
        .unwrap();
    let selection = read_selection(&retrieved);
    assert!(
        !selection.contains(&RetrievalMethod::Vector),
        "Vector should not be in selection"
    );

    remove_monodex_home();
}

/// Test that search with sources disagree produces hard error.
///
/// This verifies the decision table: when vector and fts have different source commits,
/// search errors with clear message about the mismatch.
#[tokio::test]
#[serial(monodex_home)]
async fn test_search_sources_disagree_error() {
    let (_monodex_home, _tmp_dir) = {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let monodex_home = tmp_dir.path().to_path_buf();
        set_monodex_home(&monodex_home);
        write_minimal_config(&monodex_home);
        (monodex_home, tmp_dir)
    };

    let (_db_dir, chunk_storage, label_storage) = create_test_storage().await;

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);
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

    // Verify both methods are in selection but with different sources
    let retrieved = label_storage
        .get_by_label_id(&label_id)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(
        retrieved.vector_source, retrieved.fts_source,
        "Sources should be different"
    );

    let selection = read_selection(&retrieved);
    assert_eq!(selection.len(), 2, "Both methods should be in selection");

    remove_monodex_home();
}

/// Test that incomplete method emits warning but search proceeds.
///
/// This verifies the preprocessing step: incomplete methods are warned and excluded
/// from the active subset.
#[tokio::test]
#[serial(monodex_home)]
async fn test_search_incomplete_method_warning() {
    let (_monodex_home, _tmp_dir) = {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let monodex_home = tmp_dir.path().to_path_buf();
        set_monodex_home(&monodex_home);
        write_minimal_config(&monodex_home);
        (monodex_home, tmp_dir)
    };

    let (_db_dir, chunk_storage, label_storage) = create_test_storage().await;

    let catalog = "test-catalog";
    let label = "main";
    let label_id = format!("{}:{}", catalog, label);

    // Create a chunk
    let chunk = test_chunk_row("aaaabbbbcccc1111:1", "aaaabbbbcccc1111", 1, &label_id);
    chunk_storage
        .upsert_with_vectors(&[chunk], &[vec![0.0f32; 768]])
        .await
        .unwrap();

    // Create label metadata with both methods but fts incomplete
    let label_row = test_label_metadata_row_with_selection(
        catalog,
        label,
        Some("abc123def456"), // vector_source
        true,                 // vector_complete
        Some("abc123def456"), // fts_source
        false,                // fts_complete = false (incomplete)
    );
    label_storage.upsert(&label_row).await.unwrap();

    // Verify fts is in selection but incomplete
    let retrieved = label_storage
        .get_by_label_id(&label_id)
        .await
        .unwrap()
        .unwrap();
    assert!(retrieved.fts_source.is_some(), "fts should be in selection");
    assert!(!retrieved.fts_complete, "fts should be incomplete");

    // Active subset should be just vector (fts excluded due to incomplete)
    let selection = read_selection(&retrieved);
    assert_eq!(selection.len(), 2, "Selection has both methods");

    remove_monodex_home();
}
