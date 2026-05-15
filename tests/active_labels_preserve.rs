//! Purpose: Integration tests for active_label_ids preservation during upserts.
//! Edit here when: Adding or modifying tests for the active_label_ids preservation invariant.
//! Do not edit here for: Production storage code (see `engine/storage/chunks/`); other storage tests (see `tests/label_add.rs`).

mod common;

use monodex::engine::{Chunk, identifier::LabelId};

fn test_chunk_with_labels(
    path: &str,
    text: &str,
    catalog: &str,
    active_label_ids: Vec<String>,
) -> Chunk {
    let file_id = format!("test-{}", path.replace('/', "-"));
    Chunk {
        text: text.to_string(),
        catalog: catalog.to_string(),
        active_label_ids,
        embedder_id: "test-embedder:v1".to_string(),
        chunker_id: "test-chunker:v1".to_string(),
        blob_id: "abc123".to_string(),
        content_hash: format!("hash-{}", text.len()),
        file_id: file_id.clone(),
        relative_path: path.to_string(),
        package_name: "test-package".to_string(),
        source_uri: format!("/path/to/{}", path),
        chunk_ordinal: 1,
        chunk_count: 1,
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

/// Test that upsert_with_vectors merges active_label_ids instead of replacing.
///
/// This verifies the active_label_ids preservation invariant:
/// when upserting a row that already exists, the incoming active_label_ids
/// should be unioned with the existing labels, not replace them.
///
/// Scenario (vector-only path):
/// 1. Insert a chunk with active_label_ids=[A] via upsert_with_vectors
/// 2. Upsert the same chunk with active_label_ids=[B] via upsert_with_vectors
/// 3. Verify the chunk now has active_label_ids containing both A and B
#[tokio::test]
async fn test_active_label_ids_preserved_vector_path() {
    let (_db_dir, chunk_storage) = common::create_test_storage().await;

    let catalog = "test-catalog";
    let label_a = "label-a";
    let label_b = "label-b";
    let label_a_id = format!("{}:{}", catalog, label_a);
    let label_b_id = format!("{}:{}", catalog, label_b);

    // Create a test chunk with label A
    let chunk_a = test_chunk_with_labels(
        "src/test.ts",
        "getUserProfile configuration",
        catalog,
        vec![label_a_id.clone()],
    );
    let row_a = common::chunk_to_row(&chunk_a);

    // Create a simple vector
    let mut vector = vec![0.0f32; 768];
    vector[0] = 1.0;

    // Step 1: Insert with label A
    chunk_storage
        .upsert_with_vectors(std::slice::from_ref(&row_a), &[vector.clone()])
        .await
        .unwrap();

    // Step 2: Upsert with label B (same row_id, different label)
    let chunk_b = Chunk {
        active_label_ids: vec![label_b_id.clone()],
        ..chunk_a.clone()
    };
    let row_b = common::chunk_to_row(&chunk_b);

    chunk_storage
        .upsert_with_vectors(std::slice::from_ref(&row_b), &[vector.clone()])
        .await
        .unwrap();

    // Step 3: Verify both labels are present
    let stored = chunk_storage.get_by_row_id(&row_a.row_id).await.unwrap();
    assert!(stored.is_some(), "Chunk should exist");
    let stored = stored.unwrap();

    assert!(
        stored.active_label_ids.contains(&label_a_id),
        "Should contain label A"
    );
    assert!(
        stored.active_label_ids.contains(&label_b_id),
        "Should contain label B"
    );
    assert_eq!(
        stored.active_label_ids.len(),
        2,
        "Should have exactly 2 labels (no duplicates)"
    );
}

/// Test that active_label_ids is preserved across FTS-then-vector upserts.
///
/// Scenario (FTS-then-vector path):
/// 1. Insert a chunk with active_label_ids=[A] via upsert_without_vectors (FTS-only)
/// 2. Upsert the same chunk with active_label_ids=[B] via upsert_with_vectors
/// 3. Verify the chunk has both labels AND the vector works
#[tokio::test]
async fn test_active_label_ids_preserved_fts_then_vector() {
    let (_db_dir, chunk_storage) = common::create_test_storage().await;

    let catalog = "test-catalog";
    let label_a = "label-a";
    let label_b = "label-b";
    let label_a_id = format!("{}:{}", catalog, label_a);
    let label_b_id = format!("{}:{}", catalog, label_b);

    // Create a test chunk with label A
    let chunk_a = test_chunk_with_labels(
        "src/test.ts",
        "getUserProfile configuration",
        catalog,
        vec![label_a_id.clone()],
    );
    let row_a = common::chunk_to_row(&chunk_a);

    // Step 1: Insert with label A via FTS-only path (no vector)
    chunk_storage
        .upsert_without_vectors(std::slice::from_ref(&row_a))
        .await
        .unwrap();

    // Step 2: Upsert with label B via vector path
    let chunk_b = Chunk {
        active_label_ids: vec![label_b_id.clone()],
        ..chunk_a.clone()
    };
    let row_b = common::chunk_to_row(&chunk_b);

    let mut vector = vec![0.0f32; 768];
    vector[0] = 1.0;

    chunk_storage
        .upsert_with_vectors(std::slice::from_ref(&row_b), &[vector.clone()])
        .await
        .unwrap();

    // Step 3: Verify both labels are present
    let stored = chunk_storage.get_by_row_id(&row_a.row_id).await.unwrap();
    assert!(stored.is_some(), "Chunk should exist");
    let stored = stored.unwrap();

    assert!(
        stored.active_label_ids.contains(&label_a_id),
        "Should contain label A (from FTS-only crawl)"
    );
    assert!(
        stored.active_label_ids.contains(&label_b_id),
        "Should contain label B (from vector crawl)"
    );
    assert_eq!(
        stored.active_label_ids.len(),
        2,
        "Should have exactly 2 labels (no duplicates)"
    );

    // Step 4: Verify vector search works for both labels
    let label_a_label_id = LabelId::new(catalog, label_a).unwrap();
    let results_a = chunk_storage
        .vector_search(&vector, label_a_label_id.as_str(), 10)
        .await
        .unwrap();
    assert_eq!(results_a.len(), 1, "Should find chunk via label A");

    let label_b_label_id = LabelId::new(catalog, label_b).unwrap();
    let results_b = chunk_storage
        .vector_search(&vector, label_b_label_id.as_str(), 10)
        .await
        .unwrap();
    assert_eq!(results_b.len(), 1, "Should find chunk via label B");
}

/// Test that self-upsert is idempotent (no duplicate labels).
///
/// Scenario:
/// 1. Insert a chunk with active_label_ids=[A]
/// 2. Upsert the same chunk with active_label_ids=[A] again
/// 3. Verify active_label_ids still has exactly [A], not [A, A]
#[tokio::test]
async fn test_active_label_ids_self_upsert_idempotent() {
    let (_db_dir, chunk_storage) = common::create_test_storage().await;

    let catalog = "test-catalog";
    let label_a = "label-a";
    let label_a_id = format!("{}:{}", catalog, label_a);

    // Create a test chunk with label A
    let chunk = test_chunk_with_labels(
        "src/test.ts",
        "getUserProfile configuration",
        catalog,
        vec![label_a_id.clone()],
    );
    let row = common::chunk_to_row(&chunk);

    let mut vector = vec![0.0f32; 768];
    vector[0] = 1.0;

    // Step 1: Insert with label A
    chunk_storage
        .upsert_with_vectors(std::slice::from_ref(&row), &[vector.clone()])
        .await
        .unwrap();

    // Step 2: Upsert the exact same thing again
    chunk_storage
        .upsert_with_vectors(std::slice::from_ref(&row), &[vector.clone()])
        .await
        .unwrap();

    // Step 3: Verify no duplicates
    let stored = chunk_storage.get_by_row_id(&row.row_id).await.unwrap();
    assert!(stored.is_some(), "Chunk should exist");
    let stored = stored.unwrap();

    assert_eq!(
        stored.active_label_ids.len(),
        1,
        "Should have exactly 1 label (no duplicates from self-upsert)"
    );
    assert!(
        stored.active_label_ids.contains(&label_a_id),
        "Should contain label A"
    );
}
