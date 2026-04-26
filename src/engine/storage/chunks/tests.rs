//! Purpose: Test suite for chunks storage operations.
//! Edit here when: Adding or modifying ChunkStorage tests.
//! Do not edit here for: Production storage code — edit chunks.rs.

use super::*;
use crate::engine::schema::chunks_schema;
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

    (tmp_dir, ChunkStorage::new(Arc::new(table)))
}

fn test_chunk_row(point_id: &str, file_id: &str, ordinal: i32) -> ChunkRow {
    ChunkRow {
        point_id: point_id.to_string(),
        text: format!("Test content for {}", point_id),
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

    let retrieved = storage.get_by_point_id("file1:1").await.unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.point_id, "file1:1");
    assert_eq!(retrieved.text, row.text);
}

#[tokio::test]
async fn test_get_nonexistent() {
    let (_tmp_dir, storage) = create_test_storage().await;

    let retrieved = storage.get_by_point_id("nonexistent:1").await.unwrap();
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

    let retrieved = storage.get_by_point_id("file1:1").await.unwrap().unwrap();
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
async fn test_delete_by_point_ids() {
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
        .delete_by_point_ids(&["file1:1".to_string()])
        .await
        .unwrap();

    let retrieved1 = storage.get_by_point_id("file1:1").await.unwrap();
    let retrieved2 = storage.get_by_point_id("file1:2").await.unwrap();

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

    let count = storage.table.count_rows(None).await.unwrap();
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

    let retrieved = storage.get_by_point_id("file1:1").await.unwrap().unwrap();
    assert_eq!(retrieved.text, "Updated text");

    // Verify only one row exists for this point_id
    let chunks = storage
        .get_chunks_for_label("test-catalog:main", Some(1))
        .await
        .unwrap();
    assert_eq!(chunks.len(), 1);
}

/// Helper to create a test chunk row with custom label.
fn test_chunk_row_with_label(file_id: &str, ordinal: i32, label_id: &str) -> ChunkRow {
    let point_id = format!("{}:{}", file_id, ordinal);
    ChunkRow {
        point_id,
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
    assert_eq!(results[0].chunk.point_id, "file1:1");

    // Search with query vector matching v1, filtered to label-b
    // Should return p2 (has label-b, even though vector doesn't match as well)
    let results = storage
        .vector_search(&v1, "catalog-b:label-b", 10)
        .await
        .unwrap();

    // This proves the filter works: we searched with v1 but got p2 because
    // the filter restricted us to label-b, which only p2 has
    assert_eq!(results.len(), 1, "Should find 1 result for label-b");
    assert_eq!(results[0].chunk.point_id, "file2:1");

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
        results.first().map(|r| r.chunk.point_id.as_str()),
        Some("file0:1"),
        "file0:1 should be ranked first for query [1,0,0,0]"
    );

    // file4:1 should be last (distance ~2, cosine similarity = -1)
    assert_eq!(
        results.last().map(|r| r.chunk.point_id.as_str()),
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
        results.first().map(|r| r.chunk.point_id.as_str()),
        Some("file8:1"),
        "file8:1 should be ranked first for query at 45° between axes 0 and 1"
    );

    // file0:1 and file1:1 should be in top 4 (both have cosine = 0.707 with the query)
    let top_4: Vec<&str> = results
        .iter()
        .take(4)
        .map(|r| r.chunk.point_id.as_str())
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
        results.first().map(|r| r.chunk.point_id.as_str()),
        Some("file9:1"),
        "file9:1 should be ranked first for query [1,1,1,1] (equal components)"
    );
}
