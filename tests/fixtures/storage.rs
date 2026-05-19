//! Purpose: Shared LanceDB storage fixtures for integration tests.
//! Edit here when: Adding or modifying storage setup helpers for integration tests.
//! Do not edit here for: Git-repo fixtures (see `git.rs`), test cases (see `tests/*.rs`).

use std::sync::Arc;

use lancedb::connect;

use monodex::engine::{
    Chunk,
    schema::chunks_schema,
    storage::{ChunkRow, ChunkStorage},
};

/// Create a temporary test storage with a LanceDB chunks table.
///
/// Returns a tuple of (TempDir, ChunkStorage). The TempDir must be kept
/// alive for the duration of the test to prevent the temporary directory
/// from being deleted.
#[allow(dead_code)]
pub async fn create_test_storage() -> (tempfile::TempDir, ChunkStorage) {
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

/// Convert a Chunk to a ChunkRow for storage.
///
/// This is a test helper that mirrors the production conversion in
/// `app/crawl/pipeline.rs`, which is not accessible from integration tests.
///
/// Note: This function is only used by `active_labels_preserve.rs`. Other
/// test files have their own inline ChunkRow construction patterns.
#[allow(dead_code)]
pub fn chunk_to_row(chunk: &Chunk) -> ChunkRow {
    ChunkRow {
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
    }
}
