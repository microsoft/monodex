//! Purpose: Integration tests for vector preservation during FTS-only upserts.
//! Edit here when: Adding or modifying tests for the vector-preservation invariant.
//! Do not edit here for: Production storage code (see `engine/storage/chunks/`); other storage tests (see `tests/label_add.rs`).

use std::sync::Arc;

use lancedb::connect;

use monodex::engine::{
    Chunk,
    identifier::LabelId,
    schema::chunks_schema,
    storage::{ChunkRow, ChunkStorage},
};

fn write_minimal_config(monodex_home: &std::path::Path) {
    let config_path = monodex_home.join("config.json");
    std::fs::create_dir_all(monodex_home).ok();
    std::fs::write(&config_path, r#"{"catalogs": {}}"#).unwrap();
}

fn test_chunk(
    path: &str,
    text: &str,
    catalog: &str,
    label: &str,
    ordinal: usize,
    count: usize,
) -> Chunk {
    let file_id = format!("test-{}", path.replace('/', "-"));
    Chunk {
        text: text.to_string(),
        catalog: catalog.to_string(),
        active_label_ids: vec![format!("{}:{}", catalog, label)],
        embedder_id: "test-embedder:v1".to_string(),
        chunker_id: "test-chunker:v1".to_string(),
        blob_id: "abc123".to_string(),
        content_hash: format!("hash-{}", text.len()),
        file_id: file_id.clone(),
        relative_path: path.to_string(),
        package_name: "test-package".to_string(),
        source_uri: format!("/path/to/{}", path),
        chunk_ordinal: ordinal,
        chunk_count: count,
        start_line: 1,
        end_line: 10,
        symbol_name: Some("testFunction".to_string()),
        chunk_type: "function".to_string(),
        chunk_kind: "content".to_string(),
        breadcrumb: format!("test-package:{}:testFunction", path),
        split_part_ordinal: None,
        split_part_count: None,
    }
}

async fn create_test_storage() -> (tempfile::TempDir, ChunkStorage) {
    let tmp_dir = tempfile::TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test_db");

    let db = connect(db_path.to_str().unwrap())
        .execute()
        .await
        .expect("Failed to create database");

    let schema = chunks_schema();
    let table = db
        .create_empty_table("chunks", schema)
        .execute()
        .await
        .expect("Failed to create table");

    // Pass db_path for commit mutex acquisition in write methods
    (tmp_dir, ChunkStorage::new(Arc::new(table), db_path))
}

/// Test that upsert_without_vectors preserves existing vectors.
///
/// This verifies the vector-preservation invariant from Cluster 6b:
/// when we upsert a chunk without a vector (FTS-only path), any existing
/// vector on that row must be preserved, not overwritten with NULL.
///
/// Scenario:
/// 1. Insert a chunk with a vector via upsert_with_vectors
/// 2. Upsert the same chunk without a vector via upsert_without_vectors
/// 3. Verify vector search still finds the chunk (proving vector was preserved)
#[tokio::test]
async fn test_upsert_without_vectors_preserves_vector() {
    // Use a blocking scope to set up the test environment, then drop the lock
    // before any async operations
    let (_monodex_home, _tmp_dir) = {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let monodex_home = tmp_dir.path().to_path_buf();
        write_minimal_config(&monodex_home);
        (monodex_home, tmp_dir)
    };

    // Create test storage
    let (_db_dir, chunk_storage) = create_test_storage().await;

    let catalog = "test-catalog";
    let label = "test-label";

    // Create a test chunk
    let chunk = test_chunk(
        "src/test.ts",
        "getUserProfile configuration",
        catalog,
        label,
        1,
        1,
    );

    // Create a simple unit-like vector: [1.0, 0.0, 0.0, ...]
    let mut vector = vec![0.0f32; 768];
    vector[0] = 1.0;

    // Build the ChunkRow
    let row = ChunkRow {
        row_id: chunk.row_id(),
        text: chunk.text.clone(),
        catalog: chunk.catalog.clone(),
        active_label_ids: chunk.active_label_ids.clone(),
        embedder_id: chunk.embedder_id.clone(),
        chunker_id: chunk.chunker_id.clone(),
        blob_id: chunk.blob_id.clone(),
        content_hash: chunk.content_hash.clone(),
        file_id: chunk.file_id.clone(),
        relative_path: chunk.relative_path.clone(),
        package_name: chunk.package_name.clone(),
        source_uri: chunk.source_uri.clone(),
        chunk_ordinal: chunk.chunk_ordinal as i32,
        chunk_count: chunk.chunk_count as i32,
        start_line: chunk.start_line as i32,
        end_line: chunk.end_line as i32,
        symbol_name: chunk.symbol_name.clone(),
        chunk_type: chunk.chunk_type.clone(),
        chunk_kind: chunk.chunk_kind.clone(),
        breadcrumb: Some(chunk.breadcrumb.clone()),
        split_part_ordinal: chunk.split_part_ordinal.map(|n| n as i32),
        split_part_count: chunk.split_part_count.map(|n| n as i32),
        file_complete: true,
    };

    // Step 1: Insert chunk with vector
    chunk_storage
        .upsert_with_vectors(std::slice::from_ref(&row), &[vector.clone()])
        .await
        .unwrap();

    // Verify initial search works
    let label_id = LabelId::new(catalog, label).unwrap();
    let results = chunk_storage
        .vector_search(&vector, label_id.as_str(), 10)
        .await
        .unwrap();
    assert_eq!(
        results.len(),
        1,
        "Should find the chunk after initial insert"
    );
    assert_eq!(
        results[0].chunk.row_id, row.row_id,
        "Should find the correct chunk"
    );

    // Step 2: Upsert same chunk without vector (simulating FTS-only crawl)
    // The text is unchanged, only metadata would change (e.g., active_label_ids)
    chunk_storage
        .upsert_without_vectors(std::slice::from_ref(&row))
        .await
        .unwrap();

    // Step 3: Verify vector search still finds the chunk
    let results_after = chunk_storage
        .vector_search(&vector, label_id.as_str(), 10)
        .await
        .unwrap();
    assert_eq!(
        results_after.len(),
        1,
        "Should still find the chunk after upsert_without_vectors (vector was preserved)"
    );
    assert_eq!(
        results_after[0].chunk.row_id, row.row_id,
        "Should find the correct chunk"
    );
}

/// Test that FTS-only crawl clears partial vectors to maintain the per-file invariant.
///
/// Scenario:
/// 1. Simulate an interrupted vector-phase crawl: write chunks F:1 and F:2 with vectors,
///    but do NOT write F:3 and do NOT flip the sentinel (file_complete=false).
/// 2. Run the FTS-only upsert path with all 3 chunks.
/// 3. Verify: all 3 chunks end up with vector=NULL, and sentinel has file_complete=true.
///
/// This tests the fix for the per-file vector-presence invariant: when an FTS-only
/// crawl reprocesses a file that was partially indexed by a previous vector crawl,
/// the existing vectors must be cleared so the file ends up with uniform NULL vectors.
#[tokio::test]
async fn test_fts_only_clears_partial_vectors() {
    let (_monodex_home, _tmp_dir) = {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let monodex_home = tmp_dir.path().to_path_buf();
        write_minimal_config(&monodex_home);
        (monodex_home, tmp_dir)
    };

    // Create test storage
    let (_db_dir, chunk_storage) = create_test_storage().await;

    let catalog = "test-catalog";
    let label = "test-label";

    // Create 3 chunks for file F
    let chunks: Vec<Chunk> = (1..=3)
        .map(|ordinal| {
            test_chunk(
                "src/partial.ts",
                &format!("chunk {} content", ordinal),
                catalog,
                label,
                ordinal,
                3, // chunk_count = 3
            )
        })
        .collect();

    // Create a simple unit-like vector: [1.0, 0.0, 0.0, ...]
    let mut vector = vec![0.0f32; 768];
    vector[0] = 1.0;

    // Step 1: Simulate interrupted vector-phase crawl
    // Insert F:1 and F:2 with vectors, but NOT F:3
    // Also, file_complete stays false (simulating interrupted crawl)
    let rows_with_vectors: Vec<ChunkRow> = chunks
        .iter()
        .take(2) // Only first 2 chunks
        .map(|chunk| ChunkRow {
            row_id: chunk.row_id(),
            text: chunk.text.clone(),
            catalog: chunk.catalog.clone(),
            active_label_ids: chunk.active_label_ids.clone(),
            embedder_id: chunk.embedder_id.clone(),
            chunker_id: chunk.chunker_id.clone(),
            blob_id: chunk.blob_id.clone(),
            content_hash: chunk.content_hash.clone(),
            file_id: chunk.file_id.clone(),
            relative_path: chunk.relative_path.clone(),
            package_name: chunk.package_name.clone(),
            source_uri: chunk.source_uri.clone(),
            chunk_ordinal: chunk.chunk_ordinal as i32,
            chunk_count: chunk.chunk_count as i32,
            start_line: chunk.start_line as i32,
            end_line: chunk.end_line as i32,
            symbol_name: chunk.symbol_name.clone(),
            chunk_type: chunk.chunk_type.clone(),
            chunk_kind: chunk.chunk_kind.clone(),
            breadcrumb: Some(chunk.breadcrumb.clone()),
            split_part_ordinal: chunk.split_part_ordinal.map(|n| n as i32),
            split_part_count: chunk.split_part_count.map(|n| n as i32),
            file_complete: false, // Interrupted crawl - sentinel not complete
        })
        .collect();

    // Insert only F:1 and F:2 with vectors
    let vectors: Vec<Vec<f32>> = vec![vector.clone(), vector.clone()];
    chunk_storage
        .upsert_with_vectors(&rows_with_vectors, &vectors)
        .await
        .unwrap();

    // Verify F:1 and F:2 have vectors
    let row1 = chunk_storage
        .get_by_row_id(&chunks[0].row_id())
        .await
        .unwrap();
    let row2 = chunk_storage
        .get_by_row_id(&chunks[1].row_id())
        .await
        .unwrap();
    assert!(row1.is_some(), "F:1 should exist");
    assert!(row2.is_some(), "F:2 should exist");

    // Step 2: Run FTS-only upsert path with all 3 chunks
    // This simulates the FTS-only slow path reprocessing the file
    let all_rows: Vec<ChunkRow> = chunks
        .iter()
        .map(|chunk| ChunkRow {
            row_id: chunk.row_id(),
            text: chunk.text.clone(),
            catalog: chunk.catalog.clone(),
            active_label_ids: chunk.active_label_ids.clone(),
            embedder_id: chunk.embedder_id.clone(),
            chunker_id: chunk.chunker_id.clone(),
            blob_id: chunk.blob_id.clone(),
            content_hash: chunk.content_hash.clone(),
            file_id: chunk.file_id.clone(),
            relative_path: chunk.relative_path.clone(),
            package_name: chunk.package_name.clone(),
            source_uri: chunk.source_uri.clone(),
            chunk_ordinal: chunk.chunk_ordinal as i32,
            chunk_count: chunk.chunk_count as i32,
            start_line: chunk.start_line as i32,
            end_line: chunk.end_line as i32,
            symbol_name: chunk.symbol_name.clone(),
            chunk_type: chunk.chunk_type.clone(),
            chunk_kind: chunk.chunk_kind.clone(),
            breadcrumb: Some(chunk.breadcrumb.clone()),
            split_part_ordinal: chunk.split_part_ordinal.map(|n| n as i32),
            split_part_count: chunk.split_part_count.map(|n| n as i32),
            file_complete: false, // Will be set to true by pipeline
        })
        .collect();

    // Simulate the pipeline: null vectors first, then upsert without vectors
    let row_ids: Vec<&str> = all_rows.iter().map(|r| r.row_id.as_str()).collect();
    chunk_storage
        .null_vectors_for_row_ids(&row_ids)
        .await
        .unwrap();
    chunk_storage
        .upsert_without_vectors(&all_rows)
        .await
        .unwrap();

    // Flip sentinel to complete (simulating pipeline behavior)
    let sentinel_row_id = format!("{}:1", chunks[0].file_id);
    chunk_storage
        .update_file_complete(&sentinel_row_id, true)
        .await
        .unwrap();

    // Step 3: Verify all 3 chunks have vector=NULL
    for (i, chunk) in chunks.iter().enumerate() {
        let row = chunk_storage.get_by_row_id(&chunk.row_id()).await.unwrap();
        assert!(row.is_some(), "Chunk {} should exist", i + 1);
        let _row = row.unwrap();

        // Check vector is NULL by attempting vector search
        let label_id = LabelId::new(catalog, label).unwrap();
        let results = chunk_storage
            .vector_search(&vector, label_id.as_str(), 10)
            .await
            .unwrap();

        // None of our chunks should be found by vector search (all vectors are NULL)
        let found = results.iter().any(|r| r.chunk.row_id == chunk.row_id());
        assert!(
            !found,
            "Chunk {} should NOT be found by vector search (vector should be NULL)",
            i + 1
        );
    }

    // Verify sentinel has file_complete=true
    let sentinel = chunk_storage
        .get_by_row_id(&sentinel_row_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        sentinel.file_complete,
        "Sentinel should have file_complete=true"
    );
}
