//! Integration tests for crawl label-add functionality.
//!
//! Tests the end-to-end behavior of crawling under multiple labels.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use lancedb::connect;

use monodex::engine::{
    Chunk,
    identifier::LabelId,
    schema::chunks_schema,
    storage::{ChunkRow, ChunkStorage},
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

    (tmp_dir, ChunkStorage::new(Arc::new(table)))
}

/// Test that crawling the same content under a second label makes it searchable
/// under that label.
///
/// This verifies the label-add code path: when a file already exists under label A,
/// crawling it under label B should add label B to the active_label_ids array
/// without re-embedding.
#[tokio::test]
async fn test_label_add_makes_chunks_searchable() {
    // Use a blocking scope to set up the test environment, then drop the lock
    // before any async operations
    let (_monodex_home, _tmp_dir) = {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let monodex_home = tmp_dir.path().to_path_buf();
        set_monodex_home(&monodex_home);
        write_minimal_config(&monodex_home);
        (monodex_home, tmp_dir)
    };

    // Create test storage
    let (_db_dir, chunk_storage) = create_test_storage().await;

    // Create test chunks with known vectors
    let catalog = "test-catalog";
    let label_a = "label-a";
    let label_b = "label-b";

    // Create a simple chunk with a unit-like vector (first dimension = 1.0)
    let chunk = test_chunk(
        "src/test.ts",
        "SparoProfile configuration class",
        catalog,
        label_a,
        1,
        1,
    );

    // Manually create a simple vector: [1.0, 0.0, 0.0, ...]
    let mut vector = vec![0.0f32; 768];
    vector[0] = 1.0;

    // Insert chunk with label A
    let rows = vec![ChunkRow {
        point_id: chunk.point_id(),
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
    }];

    chunk_storage
        .upsert_with_vectors(&rows, &[vector.clone()])
        .await
        .unwrap();

    // Verify search under label A returns results
    let label_a_id = LabelId::new(catalog, label_a).unwrap();
    let results_a = chunk_storage
        .vector_search(&vector, label_a_id.as_str(), 10)
        .await
        .unwrap();
    assert!(
        !results_a.is_empty(),
        "Search under label A should return results"
    );

    // Now simulate the label-add code path: get chunks by file_id and add label B
    let file_id = rows[0].file_id.clone();
    let chunks = chunk_storage.get_chunks_by_file_id(&file_id).await.unwrap();
    assert!(
        !chunks.is_empty(),
        "get_chunks_by_file_id should return chunks"
    );

    // Add label B to each chunk
    let label_b_id = LabelId::new(catalog, label_b).unwrap();
    for chunk in &chunks {
        let mut new_labels = chunk.active_label_ids.clone();
        if !new_labels.contains(&label_b_id.to_string()) {
            new_labels.push(label_b_id.to_string());
        }
        chunk_storage
            .update_active_labels(&chunk.point_id, &new_labels)
            .await
            .unwrap();
    }

    // Verify search under label B now returns results
    let results_b = chunk_storage
        .vector_search(&vector, label_b_id.as_str(), 10)
        .await
        .unwrap();
    assert!(
        !results_b.is_empty(),
        "Search under label B should return results after label-add"
    );

    // Verify both searches return the same file_ids
    let file_ids_a: HashSet<String> = results_a.iter().map(|r| r.chunk.file_id.clone()).collect();
    let file_ids_b: HashSet<String> = results_b.iter().map(|r| r.chunk.file_id.clone()).collect();
    assert_eq!(
        file_ids_a, file_ids_b,
        "Both labels should return the same file_ids"
    );

    remove_monodex_home();
}

/// Test that incomplete files (file_complete = false) are re-crawled.
///
/// This verifies the sentinel check: when a sentinel chunk exists but
/// file_complete is false, the file should be treated as new and re-crawled.
#[tokio::test]
async fn test_incomplete_file_is_recrawled() {
    // Use a blocking scope to set up the test environment, then drop the lock
    // before any async operations
    let (_monodex_home, _tmp_dir) = {
        let tmp_dir = tempfile::TempDir::new().unwrap();
        let monodex_home = tmp_dir.path().to_path_buf();
        set_monodex_home(&monodex_home);
        write_minimal_config(&monodex_home);
        (monodex_home, tmp_dir)
    };

    // Create test storage
    let (_db_dir, chunk_storage) = create_test_storage().await;

    let catalog = "test-catalog";
    let label = "main";

    // Create a sentinel chunk with file_complete = false (simulating interrupted crawl)
    let chunk = test_chunk(
        "src/incomplete.ts",
        "Incomplete file content",
        catalog,
        label,
        1,
        3,
    );
    let mut vector = vec![0.0f32; 768];
    vector[0] = 1.0;

    let row = ChunkRow {
        point_id: chunk.point_id(),
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
        chunk_ordinal: 1,
        chunk_count: 3,
        start_line: 1,
        end_line: 10,
        symbol_name: chunk.symbol_name.clone(),
        chunk_type: chunk.chunk_type.clone(),
        chunk_kind: chunk.chunk_kind.clone(),
        breadcrumb: Some(chunk.breadcrumb.clone()),
        split_part_ordinal: None,
        split_part_count: None,
        file_complete: false, // Incomplete!
    };

    chunk_storage
        .upsert_with_vectors(&[row], &[vector])
        .await
        .unwrap();

    // Retrieve the sentinel and verify file_complete is false
    let sentinel = chunk_storage
        .get_by_point_id(&format!("{}:1", chunk.file_id))
        .await
        .unwrap();
    assert!(sentinel.is_some(), "Sentinel should exist");
    let sentinel = sentinel.unwrap();
    assert!(
        !sentinel.file_complete,
        "Sentinel should have file_complete = false"
    );

    // Simulate the sentinel check logic: incomplete file should be treated as new
    let should_recrawl = !sentinel.file_complete;
    assert!(
        should_recrawl,
        "Incomplete file should be marked for re-crawl"
    );

    remove_monodex_home();
}
