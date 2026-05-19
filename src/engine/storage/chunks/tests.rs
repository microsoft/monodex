//! Purpose: Test suite for chunks storage operations.
//! Edit here when: Adding or modifying ChunkStorage tests.
//! Do not edit here for: Production storage code — edit storage.rs.
//! Size note: 1258 production lines. 25 tests averaging 42 lines each due to per-test LanceDB setup. The file's edit intent mirrors storage.rs (operations against the chunks table); no clean split below the test-function level. Revisit at 1358.

use super::storage::*;
use std::sync::Arc;

use crate::engine::schema::VECTOR_DIMENSION;
use crate::engine::schema::chunks_schema;
use crate::engine::storage::ChunkRow;
use lancedb::connect;
use tempfile::TempDir;

async fn create_test_storage() -> (TempDir, ChunkStorage) {
    let tmp_dir = TempDir::new().unwrap();
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

fn test_chunk_row(row_id: &str, file_id: &str, ordinal: i32) -> ChunkRow {
    ChunkRow {
        row_id: row_id.to_string(),
        text: format!("Test content for {}", row_id),
        catalog: "test-catalog".to_string(),
        active_label_ids: vec!["test-catalog:main".to_string()],
        embedder_id: "test-embedder:v1".to_string(),
        chunker_id: "test-chunker:v1".to_string(),
        blob_id: "abc123".to_string(),
        content_hash: "def456".to_string(),
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
        breadcrumb: Some("test-package:test.ts:testFunction".to_string()),
        split_part_ordinal: None,
        split_part_count: None,
        file_complete: ordinal == 1,
    }
}

/// Helper to create a zero vector for tests that don't exercise vector_search
fn zero_vector() -> Vec<f32> {
    vec![0.0f32; VECTOR_DIMENSION]
}

#[tokio::test]
async fn test_upsert_and_get() {
    let (_tmp_dir, storage) = create_test_storage().await;

    let row = test_chunk_row("file1:1", "file1", 1);
    storage
        .upsert_with_vectors(std::slice::from_ref(&row), &[zero_vector()])
        .await
        .unwrap();

    let retrieved = storage.get_by_row_id("file1:1").await.unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.row_id, "file1:1");
    assert_eq!(retrieved.text, row.text);
}

#[tokio::test]
async fn test_get_nonexistent() {
    let (_tmp_dir, storage) = create_test_storage().await;

    let retrieved = storage.get_by_row_id("nonexistent:1").await.unwrap();
    assert!(retrieved.is_none());
}

#[tokio::test]
async fn test_get_chunks_by_file_id_with_label() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Insert chunks for file1
    storage
        .upsert_with_vectors(&[test_chunk_row("file1:1", "file1", 1)], &[zero_vector()])
        .await
        .unwrap();
    storage
        .upsert_with_vectors(&[test_chunk_row("file1:2", "file1", 2)], &[zero_vector()])
        .await
        .unwrap();
    storage
        .upsert_with_vectors(&[test_chunk_row("file1:3", "file1", 3)], &[zero_vector()])
        .await
        .unwrap();

    // Insert chunk for different file
    storage
        .upsert_with_vectors(&[test_chunk_row("file2:1", "file2", 1)], &[zero_vector()])
        .await
        .unwrap();

    let chunks = storage
        .get_chunks_by_file_id_with_label("file1", "test-catalog:main")
        .await
        .unwrap();

    assert_eq!(chunks.len(), 3);
    // Verify sorted by ordinal
    assert_eq!(chunks[0].chunk_ordinal, 1);
    assert_eq!(chunks[1].chunk_ordinal, 2);
    assert_eq!(chunks[2].chunk_ordinal, 3);
}

#[tokio::test]
async fn test_get_chunks_for_label() {
    let (_tmp_dir, storage) = create_test_storage().await;

    storage
        .upsert_with_vectors(&[test_chunk_row("file1:1", "file1", 1)], &[zero_vector()])
        .await
        .unwrap();
    storage
        .upsert_with_vectors(&[test_chunk_row("file1:2", "file1", 2)], &[zero_vector()])
        .await
        .unwrap();

    let chunks = storage
        .get_chunks_for_label("test-catalog:main", None)
        .await
        .unwrap();

    assert_eq!(chunks.len(), 2);
}

#[tokio::test]
async fn test_get_chunks_for_label_with_ordinal_filter() {
    let (_tmp_dir, storage) = create_test_storage().await;

    storage
        .upsert_with_vectors(&[test_chunk_row("file1:1", "file1", 1)], &[zero_vector()])
        .await
        .unwrap();
    storage
        .upsert_with_vectors(&[test_chunk_row("file1:2", "file1", 2)], &[zero_vector()])
        .await
        .unwrap();

    let chunks = storage
        .get_chunks_for_label("test-catalog:main", Some(1))
        .await
        .unwrap();

    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].chunk_ordinal, 1);
}

#[tokio::test]
async fn test_update_active_labels() {
    let (_tmp_dir, storage) = create_test_storage().await;

    storage
        .upsert_with_vectors(&[test_chunk_row("file1:1", "file1", 1)], &[zero_vector()])
        .await
        .unwrap();

    // Add another label
    storage
        .update_active_labels(
            "file1:1",
            &[
                "test-catalog:main".to_string(),
                "test-catalog:feature".to_string(),
            ],
        )
        .await
        .unwrap();

    let retrieved = storage.get_by_row_id("file1:1").await.unwrap().unwrap();
    assert_eq!(retrieved.active_label_ids.len(), 2);
    assert!(
        retrieved
            .active_label_ids
            .contains(&"test-catalog:main".to_string())
    );
    assert!(
        retrieved
            .active_label_ids
            .contains(&"test-catalog:feature".to_string())
    );
}

#[tokio::test]
async fn test_delete_by_row_ids() {
    let (_tmp_dir, storage) = create_test_storage().await;

    storage
        .upsert_with_vectors(&[test_chunk_row("file1:1", "file1", 1)], &[zero_vector()])
        .await
        .unwrap();
    storage
        .upsert_with_vectors(&[test_chunk_row("file1:2", "file1", 2)], &[zero_vector()])
        .await
        .unwrap();

    storage
        .delete_by_row_ids(&["file1:1".to_string()])
        .await
        .unwrap();

    let retrieved1 = storage.get_by_row_id("file1:1").await.unwrap();
    let retrieved2 = storage.get_by_row_id("file1:2").await.unwrap();

    assert!(retrieved1.is_none());
    assert!(retrieved2.is_some());
}

#[tokio::test]
async fn test_delete_by_catalog() {
    let (_tmp_dir, storage) = create_test_storage().await;

    storage
        .upsert_with_vectors(&[test_chunk_row("file1:1", "file1", 1)], &[zero_vector()])
        .await
        .unwrap();
    storage
        .upsert_with_vectors(&[test_chunk_row("file2:1", "file2", 1)], &[zero_vector()])
        .await
        .unwrap();

    let count = storage.delete_by_catalog("test-catalog").await.unwrap();
    assert_eq!(count, 2);

    let chunks = storage
        .get_chunks_for_label("test-catalog:main", None)
        .await
        .unwrap();
    assert_eq!(chunks.len(), 0);
}

#[tokio::test]
async fn test_truncate() {
    let (_tmp_dir, storage) = create_test_storage().await;

    storage
        .upsert_with_vectors(&[test_chunk_row("file1:1", "file1", 1)], &[zero_vector()])
        .await
        .unwrap();
    storage
        .upsert_with_vectors(&[test_chunk_row("file2:1", "file2", 1)], &[zero_vector()])
        .await
        .unwrap();

    storage.truncate().await.unwrap();

    let count = storage.table().count_rows(None).await.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn test_upsert_overwrites() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Insert initial row
    let mut row = test_chunk_row("file1:1", "file1", 1);
    row.text = "Original text".to_string();
    storage
        .upsert_with_vectors(&[row.clone()], &[zero_vector()])
        .await
        .unwrap();

    // Upsert with updated text
    row.text = "Updated text".to_string();
    storage
        .upsert_with_vectors(&[row.clone()], &[zero_vector()])
        .await
        .unwrap();

    let retrieved = storage.get_by_row_id("file1:1").await.unwrap().unwrap();
    assert_eq!(retrieved.text, "Updated text");

    // Verify only one row exists for this row_id
    let chunks = storage
        .get_chunks_for_label("test-catalog:main", Some(1))
        .await
        .unwrap();
    assert_eq!(chunks.len(), 1);
}

/// Helper to create a test chunk row with custom label.
fn test_chunk_row_with_label(file_id: &str, ordinal: i32, label_id: &str) -> ChunkRow {
    let row_id = format!("{}:{}", file_id, ordinal);
    ChunkRow {
        row_id,
        text: format!("Test content for {}", file_id),
        catalog: label_id
            .split(':')
            .next()
            .unwrap_or("test-catalog")
            .to_string(),
        active_label_ids: vec![label_id.to_string()],
        embedder_id: "test-embedder:v1".to_string(),
        chunker_id: "test-chunker:v1".to_string(),
        blob_id: "abc123".to_string(),
        content_hash: "def456".to_string(),
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
        breadcrumb: Some("test-package:test.ts:testFunction".to_string()),
        split_part_ordinal: None,
        split_part_count: None,
        file_complete: ordinal == 1,
    }
}

/// Test vector_search honors label filter.
///
/// Inserts rows with different vectors and labels, then verifies that
/// vector_search correctly filters by label.
#[tokio::test]
async fn test_vector_search_with_filter() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Create two distinct vectors:
    // v1: unit vector along axis 0 (padded with zeros)
    // v2: unit vector along axis 1 (padded with zeros)
    let mut v1 = vec![0.0f32; VECTOR_DIMENSION];
    v1[0] = 1.0;
    let mut v2 = vec![0.0f32; VECTOR_DIMENSION];
    v2[1] = 1.0;

    // Insert row with label "catalog-a:label-a"
    let row1 = test_chunk_row_with_label("file1", 1, "catalog-a:label-a");
    storage
        .upsert_with_vectors(std::slice::from_ref(&row1), std::slice::from_ref(&v1))
        .await
        .unwrap();

    // Insert row with label "catalog-b:label-b"
    let row2 = test_chunk_row_with_label("file2", 1, "catalog-b:label-b");
    storage
        .upsert_with_vectors(std::slice::from_ref(&row2), std::slice::from_ref(&v2))
        .await
        .unwrap();

    // Verify rows were inserted with correct labels
    let rows_a = storage
        .get_chunks_for_label("catalog-a:label-a", None)
        .await
        .unwrap();
    let rows_b = storage
        .get_chunks_for_label("catalog-b:label-b", None)
        .await
        .unwrap();
    assert_eq!(rows_a.len(), 1, "Should have 1 row for label-a");
    assert_eq!(rows_b.len(), 1, "Should have 1 row for label-b");

    // Search with query vector matching v1, filtered to label-a
    // Should return p1 (has label-a and best matching vector)
    let results = storage
        .vector_search(&v1, "catalog-a:label-a", 10)
        .await
        .unwrap();

    assert_eq!(results.len(), 1, "Should find 1 result for label-a");
    assert_eq!(results[0].chunk.row_id, "file1:1");

    // Search with query vector matching v1, filtered to label-b
    // Should return p2 (has label-b, even though vector doesn't match as well)
    let results = storage
        .vector_search(&v1, "catalog-b:label-b", 10)
        .await
        .unwrap();

    // This proves the filter works: we searched with v1 but got p2 because
    // the filter restricted us to label-b, which only p2 has
    assert_eq!(results.len(), 1, "Should find 1 result for label-b");
    assert_eq!(results[0].chunk.row_id, "file2:1");

    // Search with a non-existent label should return nothing
    let results = storage
        .vector_search(&v1, "nonexistent:label", 10)
        .await
        .unwrap();

    assert_eq!(
        results.len(),
        0,
        "Should find 0 results for nonexistent label"
    );
}

/// Test vector_search correctness with hand-crafted vectors.
///
/// This test catches the "dot product vs cosine on unnormalized vectors" class of bug
/// that structural tests would miss. We use unit vectors along specific axes and verify
/// that cosine similarity returns results in the expected order.
///
/// Uses smaller 4-dim vectors for simplicity (VECTOR_DIMENSION is 768 which is too large
/// for meaningful hand-crafted tests).
#[tokio::test]
async fn test_vector_search_correctness() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Create 10 unit vectors along different axes and directions
    // v0: [1, 0, 0, 0]  - points along axis 0
    // v1: [0, 1, 0, 0]  - points along axis 1
    // v2: [0, 0, 1, 0]  - points along axis 2
    // v3: [0, 0, 0, 1]  - points along axis 3
    // v4: [-1, 0, 0, 0] - opposite of v0
    // v5: [0, -1, 0, 0] - opposite of v1
    // v6: [0, 0, -1, 0] - opposite of v2
    // v7: [0, 0, 0, -1] - opposite of v3
    // v8: [0.707, 0.707, 0, 0] - 45° between axes 0 and 1 (normalized)
    // v9: [0.5, 0.5, 0.5, 0.5] - equally along all axes (normalized)
    let small_vectors: Vec<Vec<f32>> = vec![
        vec![1.0, 0.0, 0.0, 0.0],
        vec![0.0, 1.0, 0.0, 0.0],
        vec![0.0, 0.0, 1.0, 0.0],
        vec![0.0, 0.0, 0.0, 1.0],
        vec![-1.0, 0.0, 0.0, 0.0],
        vec![0.0, -1.0, 0.0, 0.0],
        vec![0.0, 0.0, -1.0, 0.0],
        vec![0.0, 0.0, 0.0, -1.0],
        vec![0.707, 0.707, 0.0, 0.0],
        vec![0.5, 0.5, 0.5, 0.5],
    ];

    // Pad to VECTOR_DIMENSION and insert rows
    for (i, small_vec) in small_vectors.iter().enumerate() {
        let file_id = format!("file{}", i);
        let row = test_chunk_row_with_label(&file_id, 1, "test:label");

        // Pad the small vector to VECTOR_DIMENSION with zeros
        let mut padded = small_vec.clone();
        padded.resize(VECTOR_DIMENSION, 0.0f32);

        storage
            .upsert_with_vectors(&[row], &[padded])
            .await
            .unwrap();
    }

    // Test 1: Query with [1, 0, 0, 0] - should rank v0 first (cosine = 1.0)
    let mut query = vec![1.0f32; VECTOR_DIMENSION];
    // Set first 4 dims to match test vector
    query[0] = 1.0;
    query[1] = 0.0;
    query[2] = 0.0;
    query[3] = 0.0;
    // Rest are 1.0 from initialization, reset to 0.0
    for item in query.iter_mut().skip(4) {
        *item = 0.0;
    }

    let results = storage
        .vector_search(&query, "test:label", 10)
        .await
        .unwrap();

    // file0:1 should be first (distance ~0, cosine similarity = 1)
    assert_eq!(
        results.first().map(|r| r.chunk.row_id.as_str()),
        Some("file0:1"),
        "file0:1 should be ranked first for query [1,0,0,0]"
    );

    // file4:1 should be last (distance ~2, cosine similarity = -1)
    assert_eq!(
        results.last().map(|r| r.chunk.row_id.as_str()),
        Some("file4:1"),
        "file4:1 should be ranked last for query [1,0,0,0]"
    );

    // Test 2: Query with [0.707, 0.707, 0, 0] - file8:1 should be first
    let mut query = vec![0.0f32; VECTOR_DIMENSION];
    query[0] = 0.707;
    query[1] = 0.707;

    let results = storage
        .vector_search(&query, "test:label", 10)
        .await
        .unwrap();

    // file8:1 should be first (exact match, cosine = 1)
    assert_eq!(
        results.first().map(|r| r.chunk.row_id.as_str()),
        Some("file8:1"),
        "file8:1 should be ranked first for query at 45° between axes 0 and 1"
    );

    // file0:1 and file1:1 should be in top 4 (both have cosine = 0.707 with the query)
    let top_4: Vec<&str> = results
        .iter()
        .take(4)
        .map(|r| r.chunk.row_id.as_str())
        .collect();
    assert!(
        top_4.contains(&"file0:1") && top_4.contains(&"file1:1"),
        "file0:1 and file1:1 should both be in top 4 for query at 45° between axes 0 and 1"
    );

    // Test 3: Query with [1, 1, 1, 1] - file9:1 should be first
    let mut query = vec![0.0f32; VECTOR_DIMENSION];
    query[0] = 1.0;
    query[1] = 1.0;
    query[2] = 1.0;
    query[3] = 1.0;

    let results = storage
        .vector_search(&query, "test:label", 10)
        .await
        .unwrap();

    // file9:1 should be first (all components equal, normalized)
    assert_eq!(
        results.first().map(|r| r.chunk.row_id.as_str()),
        Some("file9:1"),
        "file9:1 should be ranked first for query [1,1,1,1] (equal components)"
    );
}

// =============================================================================
// upsert_without_vectors and preservation invariants
// =============================================================================

/// Test that upsert_without_vectors creates rows with NULL vectors.
#[tokio::test]
async fn test_upsert_without_vectors() {
    let (_tmp_dir, storage) = create_test_storage().await;

    let row = test_chunk_row("file1:1", "file1", 1);
    storage.upsert_without_vectors(&[row]).await.unwrap();

    // Verify the row was created
    let retrieved = storage.get_by_row_id("file1:1").await.unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.row_id, "file1:1");

    // Verify we can get the sentinel status and it reports no vector
    let status = storage.get_sentinel_status("file1:1").await.unwrap();
    assert!(status.is_some());
    let status = status.unwrap();
    assert!(
        !status.has_vector,
        "FTS-only chunk should not have a vector"
    );
}

/// Test that upsert_without_vectors preserves existing vectors.
///
/// This is the key invariant: if a chunk already has a vector, an FTS-only
/// upsert should NOT clobber it with NULL.
#[tokio::test]
async fn test_upsert_without_vectors_preserves_vectors() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // First, insert with a NON-ZERO vector (zero vector causes issues with cosine similarity)
    let row = test_chunk_row("file1:1", "file1", 1);
    let mut vec = vec![0.0f32; VECTOR_DIMENSION];
    vec[0] = 1.0; // Non-zero vector for valid cosine similarity
    storage
        .upsert_with_vectors(std::slice::from_ref(&row), &[vec.clone()])
        .await
        .unwrap();

    // Verify it has a vector
    let status = storage
        .get_sentinel_status("file1:1")
        .await
        .unwrap()
        .unwrap();
    assert!(status.has_vector, "Should have vector after initial insert");

    // Now upsert without vectors (simulating FTS-only crawl)
    storage.upsert_without_vectors(&[row]).await.unwrap();

    // Verify the vector is preserved
    let status = storage
        .get_sentinel_status("file1:1")
        .await
        .unwrap()
        .unwrap();
    assert!(
        status.has_vector,
        "Vector should be preserved after upsert_without_vectors"
    );

    // Verify vector_search still works
    let results = storage
        .vector_search(&vec, "test-catalog:main", 10)
        .await
        .unwrap();
    assert_eq!(
        results.len(),
        1,
        "Should still find the chunk via vector search"
    );
}

/// Test that upsert_with_vectors preserves existing active_label_ids.
///
/// When upserting a row that already exists, the labels should be unioned,
/// not replaced.
#[tokio::test]
async fn test_upsert_preserves_active_label_ids() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Insert with label A
    let mut row = test_chunk_row("file1:1", "file1", 1);
    row.active_label_ids = vec!["catalog:label-a".to_string()];
    storage
        .upsert_with_vectors(&[row.clone()], &[zero_vector()])
        .await
        .unwrap();

    // Verify label A
    let retrieved = storage.get_by_row_id("file1:1").await.unwrap().unwrap();
    assert_eq!(retrieved.active_label_ids, vec!["catalog:label-a"]);

    // Upsert with label B (same row_id)
    row.active_label_ids = vec!["catalog:label-b".to_string()];
    storage
        .upsert_with_vectors(&[row.clone()], &[zero_vector()])
        .await
        .unwrap();

    // Verify both labels are present (union)
    let retrieved = storage.get_by_row_id("file1:1").await.unwrap().unwrap();
    assert_eq!(retrieved.active_label_ids.len(), 2);
    assert!(
        retrieved
            .active_label_ids
            .contains(&"catalog:label-a".to_string())
    );
    assert!(
        retrieved
            .active_label_ids
            .contains(&"catalog:label-b".to_string())
    );
}

/// Test that upsert_without_vectors preserves existing active_label_ids.
///
/// FTS-only crawl followed by vector crawl should preserve both labels.
#[tokio::test]
async fn test_upsert_without_vectors_preserves_active_label_ids() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Insert without vector, with label A (FTS-only crawl)
    let mut row = test_chunk_row("file1:1", "file1", 1);
    row.active_label_ids = vec!["catalog:label-a".to_string()];
    storage
        .upsert_without_vectors(&[row.clone()])
        .await
        .unwrap();

    // Verify label A and no vector
    let retrieved = storage.get_by_row_id("file1:1").await.unwrap().unwrap();
    assert_eq!(retrieved.active_label_ids, vec!["catalog:label-a"]);
    let status = storage
        .get_sentinel_status("file1:1")
        .await
        .unwrap()
        .unwrap();
    assert!(!status.has_vector);

    // Now do a vector crawl with label B
    row.active_label_ids = vec!["catalog:label-b".to_string()];
    storage
        .upsert_with_vectors(&[row.clone()], &[zero_vector()])
        .await
        .unwrap();

    // Verify both labels are present AND vector exists
    let retrieved = storage.get_by_row_id("file1:1").await.unwrap().unwrap();
    assert_eq!(retrieved.active_label_ids.len(), 2);
    assert!(
        retrieved
            .active_label_ids
            .contains(&"catalog:label-a".to_string())
    );
    assert!(
        retrieved
            .active_label_ids
            .contains(&"catalog:label-b".to_string())
    );
    let status = storage
        .get_sentinel_status("file1:1")
        .await
        .unwrap()
        .unwrap();
    assert!(status.has_vector);
}

/// Test that self-upsert is idempotent (labels don't duplicate).
#[tokio::test]
async fn test_upsert_idempotent_labels() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Insert with label A
    let mut row = test_chunk_row("file1:1", "file1", 1);
    row.active_label_ids = vec!["catalog:label-a".to_string()];
    storage
        .upsert_with_vectors(&[row.clone()], &[zero_vector()])
        .await
        .unwrap();

    // Upsert again with same label
    storage
        .upsert_with_vectors(&[row.clone()], &[zero_vector()])
        .await
        .unwrap();

    // Verify no duplicates
    let retrieved = storage.get_by_row_id("file1:1").await.unwrap().unwrap();
    assert_eq!(retrieved.active_label_ids, vec!["catalog:label-a"]);
}

/// Test get_sentinel_status returns None for nonexistent row.
#[tokio::test]
async fn test_get_sentinel_status_nonexistent() {
    let (_tmp_dir, storage) = create_test_storage().await;

    let status = storage.get_sentinel_status("nonexistent:1").await.unwrap();
    assert!(status.is_none());
}

/// Test get_sentinel_status correctly reports vector presence.
#[tokio::test]
async fn test_get_sentinel_status_vector_presence() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Insert with vector
    let row = test_chunk_row("file1:1", "file1", 1);
    storage
        .upsert_with_vectors(std::slice::from_ref(&row), &[zero_vector()])
        .await
        .unwrap();

    let status = storage
        .get_sentinel_status("file1:1")
        .await
        .unwrap()
        .unwrap();
    assert!(status.has_vector);
    assert!(status.row.file_complete);
}

/// Test that vector_search does not return rows with NULL vectors.
///
/// This is important for FTS-only crawls: chunks inserted without vectors
/// should not appear in vector search results.
#[tokio::test]
async fn test_vector_search_excludes_null_vectors() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Insert a chunk with a real vector (non-zero for valid cosine similarity)
    let row_with_vector = test_chunk_row_with_label("file1", 1, "test:label");
    let mut vec = vec![0.0f32; VECTOR_DIMENSION];
    vec[0] = 1.0; // Non-zero vector for valid cosine similarity
    storage
        .upsert_with_vectors(std::slice::from_ref(&row_with_vector), &[vec.clone()])
        .await
        .unwrap();

    // Insert a chunk WITHOUT a vector (FTS-only)
    let row_without_vector = test_chunk_row_with_label("file2", 1, "test:label");
    storage
        .upsert_without_vectors(std::slice::from_ref(&row_without_vector))
        .await
        .unwrap();

    // Verify both rows exist
    assert!(storage.get_by_row_id("file1:1").await.unwrap().is_some());
    assert!(storage.get_by_row_id("file2:1").await.unwrap().is_some());

    // Verify vector presence
    let status1 = storage
        .get_sentinel_status("file1:1")
        .await
        .unwrap()
        .unwrap();
    assert!(status1.has_vector);
    let status2 = storage
        .get_sentinel_status("file2:1")
        .await
        .unwrap()
        .unwrap();
    assert!(!status2.has_vector);

    // Vector search should only return the row with a vector
    let results = storage.vector_search(&vec, "test:label", 10).await.unwrap();

    // Should only return file1:1, not file2:1
    assert_eq!(results.len(), 1, "Should only return rows with vectors");
    assert_eq!(results[0].chunk.row_id, "file1:1");
}

// =========================================================================
// upsert_without_vectors_with_progress tests
// =========================================================================

/// Test that empty inputs produce no progress events and no errors.
#[tokio::test]
async fn test_upsert_without_vectors_with_progress_empty() {
    let (_tmp_dir, storage) = create_test_storage().await;

    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let callback = move |event: StorageProgressEvent| {
        events_clone.lock().unwrap().push(event);
    };

    storage
        .upsert_without_vectors_with_progress(&[], &[], callback)
        .await
        .unwrap();

    assert!(events.lock().unwrap().is_empty());
}

/// Test that the progress callback receives all three phase labels in order
/// with monotonically non-decreasing `completed` per phase.
#[tokio::test]
async fn test_upsert_without_vectors_with_progress_phases() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Create test rows (more than UPSERT_BATCH_SIZE to test batching)
    let num_rows = 2500; // > UPSERT_BATCH_SIZE (1000)
    let rows: Vec<ChunkRow> = (0..num_rows)
        .map(|i| test_chunk_row(&format!("file{}:1", i), &format!("file{}", i), 1))
        .collect();

    let sentinel_row_ids: Vec<String> = (0..num_rows).map(|i| format!("file{}:1", i)).collect();

    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let callback = move |event: StorageProgressEvent| {
        events_clone.lock().unwrap().push(event);
    };

    storage
        .upsert_without_vectors_with_progress(&rows, &sentinel_row_ids, callback)
        .await
        .unwrap();

    let events = events.lock().unwrap();

    // Verify we have events from both phases
    let upserting_events: Vec<_> = events
        .iter()
        .filter(|e| e.phase == "Upserting chunks")
        .collect();
    let marking_events: Vec<_> = events
        .iter()
        .filter(|e| e.phase == "Marking file sentinels")
        .collect();

    assert!(!upserting_events.is_empty(), "Should have upserting events");
    assert!(!marking_events.is_empty(), "Should have marking events");

    // Verify phase order: all upserting, then all marking
    let first_marking_idx = events
        .iter()
        .position(|e| e.phase == "Marking file sentinels")
        .unwrap();
    let last_upsert_idx = events
        .iter()
        .rposition(|e| e.phase == "Upserting chunks")
        .unwrap();
    assert!(
        last_upsert_idx < first_marking_idx,
        "Upserting should complete before marking"
    );

    // Verify monotonically non-decreasing `completed` within each phase
    fn check_monotonic(events: &[&StorageProgressEvent]) {
        let mut prev = 0;
        for event in events {
            assert!(
                event.completed >= prev,
                "Completed should be non-decreasing: got {} after {}",
                event.completed,
                prev
            );
            prev = event.completed;
        }
    }

    check_monotonic(&upserting_events);
    check_monotonic(&marking_events);

    // Verify final `completed == total` per phase
    assert_eq!(
        upserting_events.last().unwrap().completed,
        upserting_events.last().unwrap().total,
        "Upserting should complete all items"
    );
    assert_eq!(
        marking_events.last().unwrap().completed,
        marking_events.last().unwrap().total,
        "Marking should complete all items"
    );

    // Verify unit field matches documented values
    for event in &upserting_events {
        assert_eq!(event.unit, "chunks");
    }
    for event in &marking_events {
        assert_eq!(event.unit, "files");
    }
}

/// Test that the method correctly handles inputs larger than UPSERT_BATCH_SIZE
/// with multiple batches per phase.
#[tokio::test]
async fn test_upsert_without_vectors_with_progress_multi_batch() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Create 2500 rows, which should produce 3 batches per phase (1000 + 1000 + 500)
    let num_rows = 2500;
    let rows: Vec<ChunkRow> = (0..num_rows)
        .map(|i| test_chunk_row(&format!("file{}:1", i), &format!("file{}", i), 1))
        .collect();

    let sentinel_row_ids: Vec<String> = (0..num_rows).map(|i| format!("file{}:1", i)).collect();

    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let callback = move |event: StorageProgressEvent| {
        events_clone.lock().unwrap().push(event);
    };

    storage
        .upsert_without_vectors_with_progress(&rows, &sentinel_row_ids, callback)
        .await
        .unwrap();

    let events = events.lock().unwrap();

    // Should have 3 batches per phase = 6 events total (2 phases)
    // Phase A (upserting): 1000, 2000, 2500
    // Phase B (marking): 1000, 2000, 2500
    assert_eq!(
        events.len(),
        6,
        "Should have 6 events (2 phases x 3 batches)"
    );

    // Verify the batch sizes are correct
    let upserting_events: Vec<_> = events
        .iter()
        .filter(|e| e.phase == "Upserting chunks")
        .collect();
    assert_eq!(upserting_events.len(), 3, "Should have 3 upserting batches");
    assert_eq!(upserting_events[0].completed, 1000);
    assert_eq!(upserting_events[1].completed, 2000);
    assert_eq!(upserting_events[2].completed, 2500);
    assert_eq!(upserting_events[2].total, 2500);
}

/// Correctness test for `upsert_without_vectors_with_progress`.
///
/// Verifies that:
/// 1. Vectors are preserved (Phase A now upserts, does not clear)
/// 2. Rows are upserted correctly with the right text/active_label_ids
/// 3. Only complete files have their sentinel marked file_complete=true
/// 4. Partial files do NOT have their sentinel marked complete
#[tokio::test]
async fn test_upsert_without_vectors_with_progress_correctness() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // === Setup: Create two files ===
    // File A: complete (all 3 chunks)
    // File B: incomplete (only chunk 1 of 3)

    // Helper to create a chunk row with specific chunk_count
    fn make_row(file_id: &str, ordinal: i32, chunk_count: i32) -> ChunkRow {
        ChunkRow {
            row_id: format!("{}:{}", file_id, ordinal),
            text: format!("Content for {} chunk {}", file_id, ordinal),
            catalog: "test-catalog".to_string(),
            active_label_ids: vec!["test-catalog:main".to_string()],
            embedder_id: "test-embedder:v1".to_string(),
            chunker_id: "test-chunker:v1".to_string(),
            blob_id: "abc123".to_string(),
            content_hash: format!("hash-{}-{}", file_id, ordinal),
            file_id: file_id.to_string(),
            relative_path: format!("src/{}.ts", file_id),
            package_name: "test-package".to_string(),
            source_uri: format!("/path/to/{}.ts", file_id),
            chunk_ordinal: ordinal,
            chunk_count,
            start_line: ordinal * 10,
            end_line: ordinal * 10 + 9,
            symbol_name: Some(format!("func_{}", file_id)),
            chunk_type: "function".to_string(),
            chunk_kind: "content".to_string(),
            breadcrumb: Some(format!("test-package:{}.ts:func_{}", file_id, file_id)),
            split_part_ordinal: None,
            split_part_count: None,
            file_complete: false,
        }
    }

    // Pre-populate storage with file A's rows having vectors
    let file_a_rows: Vec<ChunkRow> = (1..=3).map(|i| make_row("fileA", i, 3)).collect();
    let vectors: Vec<Vec<f32>> = file_a_rows
        .iter()
        .map(|_| {
            let mut v = vec![0.0f32; VECTOR_DIMENSION];
            v[0] = 1.0; // Non-zero vector
            v
        })
        .collect();
    storage
        .upsert_with_vectors(&file_a_rows, &vectors)
        .await
        .unwrap();

    // Verify file A rows have vectors before FTS-only upsert
    for row in &file_a_rows {
        let status = storage
            .get_sentinel_status(&row.row_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            status.has_vector,
            "File A row {} should have vector initially",
            row.row_id
        );
    }

    // Create the input for FTS-only upsert:
    // File A: all 3 chunks (complete)
    // File B: only chunk 1 of 3 (incomplete)
    let mut rows_to_upsert: Vec<ChunkRow> = Vec::new();
    rows_to_upsert.extend((1..=3).map(|i| make_row("fileA", i, 3)));
    rows_to_upsert.push(make_row("fileB", 1, 3));

    // Sentinel list should only include file A (complete)
    let sentinel_row_ids = vec!["fileA:1".to_string()];

    // Run the upsert
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_clone = events.clone();
    let callback = move |event: StorageProgressEvent| {
        events_clone.lock().unwrap().push(event);
    };

    storage
        .upsert_without_vectors_with_progress(&rows_to_upsert, &sentinel_row_ids, callback)
        .await
        .unwrap();

    // === Verify correctness ===

    // 1. File A's rows should still have vectors (preserved, not cleared)
    for row in &file_a_rows {
        let status = storage
            .get_sentinel_status(&row.row_id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            status.has_vector,
            "File A row {} should still have vector after upsert_without_vectors (vectors are preserved)",
            row.row_id
        );
    }

    // 2. File A's rows should exist with correct text
    for i in 1..=3 {
        let row = storage
            .get_by_row_id(&format!("fileA:{}", i))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.text,
            format!("Content for fileA chunk {}", i),
            "Row fileA:{} should have correct text",
            i
        );
        assert!(
            row.active_label_ids
                .contains(&"test-catalog:main".to_string()),
            "Row fileA:{} should have the label",
            i
        );
    }

    // 3. File A's sentinel (fileA:1) should have file_complete = true
    let sentinel_a = storage.get_by_row_id("fileA:1").await.unwrap().unwrap();
    assert!(
        sentinel_a.file_complete,
        "File A sentinel should be marked complete"
    );

    // 4. File B's row should exist but NOT be complete
    let row_b = storage.get_by_row_id("fileB:1").await.unwrap().unwrap();
    assert_eq!(
        row_b.text, "Content for fileB chunk 1",
        "File B row should have correct text"
    );
    assert!(
        !row_b.file_complete,
        "File B sentinel should NOT be marked complete (partial file)"
    );

    // 5. Verify we got progress events from both phases
    let events = events.lock().unwrap();
    let phases: std::collections::HashSet<_> = events.iter().map(|e| e.phase).collect();
    assert!(
        phases.contains("Upserting chunks"),
        "Should have Phase A events"
    );
    assert!(
        phases.contains("Marking file sentinels"),
        "Should have Phase B events"
    );
}

/// Test that upsert_without_vectors_with_progress preserves vectors from peer labels.
///
/// When two labels share a row_id (same blob), and label A has vectors with
/// vector_complete=true, an FTS-only crawl via label B re-touches the row.
/// Label A's vectors must still be present after the crawl.
#[tokio::test]
async fn test_upsert_without_vectors_preserves_peer_label_vectors() {
    let (_tmp_dir, storage) = create_test_storage().await;

    // Helper to create a chunk row with specific label
    fn make_row_with_label(row_id: &str, file_id: &str, label_id: &str) -> ChunkRow {
        ChunkRow {
            row_id: row_id.to_string(),
            text: format!("Content for {}", row_id),
            catalog: "test-catalog".to_string(),
            active_label_ids: vec![label_id.to_string()],
            embedder_id: "test-embedder:v1".to_string(),
            chunker_id: "test-chunker:v1".to_string(),
            blob_id: "shared-blob".to_string(), // Same blob for both labels
            content_hash: "hash-shared".to_string(),
            file_id: file_id.to_string(),
            relative_path: "src/shared.ts".to_string(),
            package_name: "test-package".to_string(),
            source_uri: "/path/to/shared.ts".to_string(),
            chunk_ordinal: 1,
            chunk_count: 1,
            start_line: 1,
            end_line: 50,
            symbol_name: Some("sharedFunc".to_string()),
            chunk_type: "function".to_string(),
            chunk_kind: "content".to_string(),
            breadcrumb: Some("test-package:shared.ts:sharedFunc".to_string()),
            split_part_ordinal: None,
            split_part_count: None,
            file_complete: true, // Sentinel complete for label A
        }
    }

    // Label A: insert with vectors (vector_complete=true)
    let label_a = "test-catalog:label-a";
    let row_a = make_row_with_label("shared-file:1", "shared-file", label_a);
    let mut vec = vec![0.0f32; VECTOR_DIMENSION];
    vec[0] = 1.0; // Non-zero vector
    storage
        .upsert_with_vectors(std::slice::from_ref(&row_a), &[vec.clone()])
        .await
        .unwrap();

    // Verify label A has vectors
    let status_a = storage
        .get_sentinel_status("shared-file:1")
        .await
        .unwrap()
        .unwrap();
    assert!(status_a.has_vector, "Label A should have vector initially");
    assert!(
        status_a.row.file_complete,
        "Label A should be complete initially"
    );

    // Label B: FTS-only crawl touches the same row_id (same blob)
    let label_b = "test-catalog:label-b";
    let row_b = ChunkRow {
        active_label_ids: vec![label_b.to_string()],
        file_complete: true,
        ..row_a.clone()
    };

    // Run FTS-only upsert (simulating FTS-only crawl via label B)
    let sentinel_row_ids = vec!["shared-file:1".to_string()];
    let callback = |_event: StorageProgressEvent| {};
    storage
        .upsert_without_vectors_with_progress(&[row_b], &sentinel_row_ids, callback)
        .await
        .unwrap();

    // Verify label A's vectors are still present (not clobbered)
    let status_after = storage
        .get_sentinel_status("shared-file:1")
        .await
        .unwrap()
        .unwrap();
    assert!(
        status_after.has_vector,
        "Vectors should be preserved after FTS-only crawl via label B"
    );
    assert!(
        status_after.row.file_complete,
        "Sentinel should be marked complete"
    );

    // Verify the row now has both labels
    let row = storage
        .get_by_row_id("shared-file:1")
        .await
        .unwrap()
        .unwrap();
    assert!(
        row.active_label_ids.contains(&label_a.to_string()),
        "Should still have label A"
    );
    assert!(
        row.active_label_ids.contains(&label_b.to_string()),
        "Should have label B"
    );

    // Verify vector search still works (vectors weren't corrupted)
    let results = storage.vector_search(&vec, label_a, 10).await.unwrap();
    assert_eq!(
        results.len(),
        1,
        "Vector search should still find the chunk via label A"
    );
}
