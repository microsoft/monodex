//! Integration tests for the FTS subsystem.
//!
//! Purpose: End-to-end tests for FTS indexing and search with real LanceDB and Tantivy.
//! Edit here when: Adding new FTS integration tests, testing cross-module interactions.
//! Do not edit here for: Unit tests (co-located with implementation files).

use std::fs::File;
use std::path::Path;

use anyhow::Result;
use tempfile::TempDir;

use crate::engine::fts::index::FtsIndex;
use crate::engine::fts::indexing::index_chunks_for_fts;
use crate::engine::fts::manifest::{FtsManifest, ManifestRead, read_manifest, write_manifest};
use crate::engine::fts::search::{FtsSearchOutcome, fts_search};
use crate::engine::identifier::LabelId;
use crate::engine::schema::{chunks_schema, label_metadata_schema};
use crate::engine::storage::{ChunkRow, ChunkStorage, Database, META_FILE, MetaFile};
use crate::engine::util::{FTS_SCHEMA_ID, FTS_TOKENIZER_ID};

// =============================================================================
// Test helpers
// =============================================================================

fn make_label_id(catalog: &str, label: &str) -> LabelId {
    LabelId::new(catalog, label).expect("valid label id")
}

/// Create a test chunk row with meaningful text for FTS testing.
fn test_chunk_row(
    row_id: &str,
    file_id: &str,
    ordinal: i32,
    label_id: &str,
    text: &str,
) -> ChunkRow {
    let catalog = label_id.split(':').next().unwrap().to_string();
    ChunkRow {
        row_id: row_id.to_string(),
        text: text.to_string(),
        catalog,
        active_label_ids: vec![label_id.to_string()],
        embedder_id: "test-embedder:v1".to_string(),
        chunker_id: "test-chunker:v1".to_string(),
        blob_id: "test-blob-id".to_string(),
        content_hash: "test-content-hash".to_string(),
        file_id: file_id.to_string(),
        relative_path: "src/test.ts".to_string(),
        package_name: "test-package".to_string(),
        source_uri: "/path/to/test.ts".to_string(),
        chunk_ordinal: ordinal,
        chunk_count: 3,
        start_line: 1,
        end_line: 50,
        symbol_name: Some("testFunction".to_string()),
        chunk_type: "function".to_string(),
        chunk_kind: "content".to_string(),
        breadcrumb: Some(format!(
            "test-package:test.ts:testFunction-chunk{}",
            ordinal
        )),
        split_part_ordinal: None,
        split_part_count: None,
        file_complete: ordinal == 1,
    }
}

/// Create a test database with FTS directory structure.
async fn create_test_db_with_fts(db_path: &Path) -> Database {
    use lancedb::connect;

    // Create database directory
    std::fs::create_dir_all(db_path).expect("Failed to create db directory");

    // Create LanceDB tables
    let conn = connect(db_path.to_str().unwrap())
        .execute()
        .await
        .expect("Failed to create database");

    conn.create_empty_table("chunks", chunks_schema())
        .execute()
        .await
        .expect("Failed to create chunks table");

    conn.create_empty_table("label_metadata", label_metadata_schema())
        .execute()
        .await
        .expect("Failed to create label_metadata table");

    // Write meta file
    let meta = MetaFile::new();
    let meta_file = File::create(db_path.join(META_FILE)).expect("Failed to create meta file");
    serde_json::to_writer_pretty(meta_file, &meta).expect("Failed to write meta file");

    // Create FTS directory (normally done by init-db)
    std::fs::create_dir_all(db_path.join("fts")).expect("Failed to create fts directory");

    // Open database (creates LanceDB tables)
    Database::open(db_path)
        .await
        .expect("Failed to open database")
}

/// Insert chunks into the database for FTS testing.
async fn insert_test_chunks(chunk_storage: &ChunkStorage, chunks: &[ChunkRow]) -> Result<()> {
    // Create zero vectors (FTS-only indexing doesn't need real vectors)
    let vectors: Vec<Vec<f32>> = chunks.iter().map(|_| vec![0.0f32; 768]).collect();

    chunk_storage.upsert_with_vectors(chunks, &vectors).await
}

// =============================================================================
// Test 1: FTS index with chunks - ranked search results
// =============================================================================

#[tokio::test]
async fn test_fts_index_with_chunks_ranked_results() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path();
    let label_id = make_label_id("test-catalog", "main");

    // Create database and get chunk storage
    let db = create_test_db_with_fts(db_path).await;
    let chunk_storage = db.chunks_storage().await?;

    // Create three chunks with distinct text for ranking
    let chunks = vec![
        test_chunk_row(
            "file1:1",
            "file1",
            1,
            "test-catalog:main",
            "getUserProfile fetches the user profile from the database",
        ),
        test_chunk_row(
            "file2:1",
            "file2",
            1,
            "test-catalog:main",
            "The profile contains user settings and preferences",
        ),
        test_chunk_row(
            "file3:1",
            "file3",
            1,
            "test-catalog:main",
            "Database connection pooling for performance",
        ),
    ];

    // Insert chunks
    insert_test_chunks(&chunk_storage, &chunks).await?;

    // Open FTS index and index the chunks
    let fts_index = FtsIndex::open_or_create(db_path, &label_id)?;
    let mut writer = fts_index.writer()?;

    // Manually add documents (simulating what index_chunks_for_fts would do)
    for chunk in &chunks {
        use tantivy::doc;
        writer.add_document(doc!(
            fts_index.fields.row_id => chunk.row_id.clone(),
            fts_index.fields.text => chunk.text.clone(),
        ))?;
    }
    writer.commit()?;

    // Write manifest
    let manifest = FtsManifest::new();
    fts_index.write_manifest(&manifest)?;

    // Search for "profile" - should match file1:1 and file2:1
    let result = fts_search(db_path, &label_id, "profile", 10).await?;

    match result {
        FtsSearchOutcome::Found(hits) => {
            // Should have 2 results
            assert_eq!(hits.len(), 2, "Expected 2 hits for 'profile' query");

            // Results should be in BM25 score order
            // The word "profile" appears in both, but "getUserProfile" may rank differently
            let row_ids: Vec<&str> = hits.iter().map(|h| h.row_id.as_str()).collect();
            assert!(row_ids.contains(&"file1:1"), "file1:1 should be in results");
            assert!(row_ids.contains(&"file2:1"), "file2:1 should be in results");

            // Verify scores are positive
            for hit in &hits {
                assert!(hit.score > 0.0, "Score should be positive");
            }
        }
        FtsSearchOutcome::NoIndex => panic!("Expected Found, got NoIndex"),
        FtsSearchOutcome::ParseError(msg) => panic!("Expected Found, got ParseError: {}", msg),
        FtsSearchOutcome::Stale { reason } => {
            panic!("Expected Found, got Stale: {:?}", reason)
        }
    }

    Ok(())
}

// =============================================================================
// Test 2: Manifest mismatch - IdMismatch triggers rebuild
// =============================================================================

#[test]
fn test_manifest_id_mismatch_triggers_rebuild() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path();
    let label_id = make_label_id("test-catalog", "main");

    // Create FTS directory structure
    std::fs::create_dir_all(db_path.join("fts").join("test-catalog").join("main"))?;

    // Create a manifest with mismatched IDs
    let manifest_dir = db_path.join("fts").join("test-catalog").join("main");
    let manifest_path = manifest_dir.join("manifest.json");

    let bad_manifest = FtsManifest {
        fts_schema_id: "old-schema:v1".to_string(),
        fts_tokenizer_id: "old-tokenizer:v1".to_string(),
    };
    write_manifest(&manifest_path, &bad_manifest)?;

    // Verify the manifest shows IdMismatch
    match read_manifest(&manifest_path) {
        ManifestRead::IdMismatch {
            found_schema_id,
            found_tokenizer_id,
        } => {
            assert_eq!(found_schema_id, "old-schema:v1");
            assert_eq!(found_tokenizer_id, "old-tokenizer:v1");
        }
        other => panic!("Expected IdMismatch, got {:?}", other),
    }

    // Open or create should rebuild (delete old state)
    let fts_index = FtsIndex::open_or_create(db_path, &label_id)?;

    // Verify the old manifest is gone and index was created fresh
    let new_manifest_result = fts_index.read_manifest();
    match new_manifest_result {
        ManifestRead::Missing => {
            // Manifest was deleted, new index created without writing manifest yet
        }
        ManifestRead::Present(m) => {
            // Or a new empty manifest was written
            assert_eq!(m.fts_schema_id, FTS_SCHEMA_ID);
            assert_eq!(m.fts_tokenizer_id, FTS_TOKENIZER_ID);
        }
        other => panic!(
            "Expected Missing or Present with correct IDs, got {:?}",
            other
        ),
    }

    // Verify Tantivy state exists (meta.json)
    assert!(
        fts_index.path.join("meta.json").exists(),
        "Tantivy meta.json should exist after open_or_create"
    );

    Ok(())
}

// =============================================================================
// Test 3: Unreadable manifest with Tantivy state - error
// =============================================================================

#[test]
fn test_unreadable_manifest_with_tantivy_state_errors() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path();
    let label_id = make_label_id("test-catalog", "main");

    // First, create a valid FTS index
    let fts_index = FtsIndex::open_or_create(db_path, &label_id)?;

    // Add a document and commit to ensure Tantivy state exists
    use tantivy::doc;
    let mut writer = fts_index.writer()?;
    writer.add_document(doc!(
        fts_index.fields.row_id => "test-row:1",
        fts_index.fields.text => "test content",
    ))?;
    writer.commit()?;

    // Verify Tantivy state exists
    assert!(fts_index.path.join("meta.json").exists());

    // Write a valid manifest
    let manifest = FtsManifest::new();
    fts_index.write_manifest(&manifest)?;

    // Now corrupt the manifest
    let manifest_path = fts_index.manifest_path();
    std::fs::write(&manifest_path, "not valid json {{{{")?;

    // Verify read_manifest shows Unreadable
    match read_manifest(&manifest_path) {
        ManifestRead::Unreadable { .. } => {}
        other => panic!("Expected Unreadable, got {:?}", other),
    }

    // Now try to open_or_create - should error because Tantivy state exists
    let result = FtsIndex::open_or_create(db_path, &label_id);

    match result {
        Err(e) => {
            // Error should mention corruption or unreadable
            let err_string = e.to_string().to_lowercase();
            assert!(
                err_string.contains("unreadable") || err_string.contains("corrupt"),
                "Error should mention unreadable or corrupt: {}",
                e
            );
        }
        Ok(_) => panic!(
            "Expected error when opening index with corrupted manifest and existing Tantivy state"
        ),
    }

    Ok(())
}

// =============================================================================
// Test 4: Zero-token chunk excluded from manifest
// =============================================================================

#[tokio::test]
async fn test_zero_token_chunk_excluded_from_manifest() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path();
    let label_id = make_label_id("test-catalog", "main");

    // Create database and get chunk storage
    let db = create_test_db_with_fts(db_path).await;
    let chunk_storage = db.chunks_storage().await?;

    // Create two chunks: one with normal text, one that produces zero tokens
    // (text consisting only of ASCII punctuation and whitespace)
    let normal_chunk = test_chunk_row(
        "aaaabbbbcccc1111:1",
        "aaaabbbbcccc1111",
        1,
        label_id.as_ref(),
        "getUserProfile fetches user data", // normal text with tokens
    );
    let zero_token_chunk = test_chunk_row(
        "aaaabbbbcccc2222:1",
        "aaaabbbbcccc2222",
        1,
        label_id.as_ref(),
        "!?;:, ", // punctuation that IS treated as split chars + whitespace = zero tokens
    );

    // Insert both chunks into storage
    insert_test_chunks(
        &chunk_storage,
        &[normal_chunk.clone(), zero_token_chunk.clone()],
    )
    .await?;

    // Run FTS indexing (no warning sink needed - stats carry zero-token info)
    let stats = index_chunks_for_fts(
        db_path,
        &label_id,
        &chunk_storage,
        true, // is_commit_mode
    )
    .await?;

    // Verify stats: one chunk added (normal), one skipped (zero-token)
    assert_eq!(stats.added, 1, "Expected 1 chunk added (the normal one)");
    assert_eq!(
        stats.zero_token_skipped, 1,
        "Expected 1 chunk skipped due to zero tokens"
    );
    assert_eq!(stats.live_row_ids, 1, "Expected 1 live row_id in the index");

    // Verify zero_token_row_ids contains the skipped chunk's row_id
    assert_eq!(
        stats.zero_token_row_ids.len(),
        1,
        "Expected exactly one zero_token_row_id"
    );
    assert_eq!(
        stats.zero_token_row_ids[0], "aaaabbbbcccc2222:1",
        "zero_token_row_ids should contain the zero-token chunk's row_id"
    );

    // Verify the manifest exists and is valid
    use crate::engine::fts::index::FtsOpenExistingOutcome;
    let fts_index = match FtsIndex::open_existing(db_path, &label_id)? {
        FtsOpenExistingOutcome::Open(index) => index,
        FtsOpenExistingOutcome::NoIndex => panic!("FTS index should exist after indexing"),
        FtsOpenExistingOutcome::Stale { reason } => {
            panic!("FTS index should not be stale, got: {:?}", reason)
        }
    };
    match fts_index.read_manifest() {
        ManifestRead::Present(_) => {
            // Manifest exists with valid IDs
        }
        other => panic!("Expected Present, got {:?}", other),
    }

    Ok(())
}

// =============================================================================
// Test 5: Parse error for invalid query syntax
// =============================================================================

#[tokio::test]
async fn test_fts_search_parse_error() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path();
    let label_id = make_label_id("test-catalog", "main");

    // Create FTS index with some documents
    let fts_index = FtsIndex::open_or_create(db_path, &label_id)?;
    let mut writer = fts_index.writer()?;

    use tantivy::doc;
    writer.add_document(doc!(
        fts_index.fields.row_id => "test-row:1",
        fts_index.fields.text => "getUserProfile fetches user data",
    ))?;
    writer.commit()?;

    // Write manifest
    let manifest = FtsManifest::new();
    fts_index.write_manifest(&manifest)?;

    // Test with an invalid query that Tantivy's QueryParser will definitely reject.
    // We use a query with invalid field syntax: a field name followed by a colon
    // and colon (not a valid term). Tantivy's QueryParser rejects this with ParseError.
    // Note: Some queries like "foo:bar:" might parse as term queries, so we use
    // a query that is definitively malformed.
    // The query `nonexistent_field:` (field with empty value after colon) produces
    // a ParseError because there's no term to search for after the field specifier.
    let result = fts_search(db_path, &label_id, "nonexistent_field:", 10).await?;

    match result {
        FtsSearchOutcome::ParseError(msg) => {
            // Should have an error message
            assert!(!msg.is_empty(), "Parse error should have a message");
        }
        FtsSearchOutcome::Found(_) => {
            panic!("Expected ParseError for invalid query, got Found with results");
        }
        FtsSearchOutcome::NoIndex => panic!("Expected ParseError, got NoIndex"),
        FtsSearchOutcome::Stale { reason } => {
            panic!("Expected ParseError, got Stale: {:?}", reason)
        }
    }

    Ok(())
}

// =============================================================================
// Additional test: Open_existing returns None for missing index
// =============================================================================

#[test]
fn test_open_existing_returns_none_for_missing() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let db_path = temp_dir.path();
    let label_id = make_label_id("test-catalog", "missing-label");

    // Don't create any FTS directory

    use crate::engine::fts::index::FtsOpenExistingOutcome;
    let result = FtsIndex::open_existing(db_path, &label_id)?;

    assert!(
        matches!(result, FtsOpenExistingOutcome::NoIndex),
        "Expected NoIndex for missing index"
    );

    Ok(())
}
