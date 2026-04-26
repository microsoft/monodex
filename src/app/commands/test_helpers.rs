//! Shared test helpers for command tests.
//!
//! Purpose: Reduce duplication of test setup code across command handlers.
//! Edit here when: Adding new test utilities shared across commands.
//! Do not edit here for: Test logic specific to a single command.

use std::fs::{self, File};
use std::io::Write;

use lancedb::connect;

use crate::engine::schema::{VECTOR_DIMENSION, chunks_schema, label_metadata_schema};
use crate::engine::storage::{ChunkRow, Database, LabelMetadataRow, META_FILE, MetaFile};

/// Helper to safely set MONODEX_HOME.
pub fn set_monodex_home(path: &std::path::Path) {
    // SAFETY: Tests are serialized via #[serial_test::serial(monodex_home)] attribute.
    unsafe {
        std::env::set_var("MONODEX_HOME", path);
    }
}

/// Helper to safely remove MONODEX_HOME.
pub fn remove_monodex_home() {
    // SAFETY: Tests are serialized via #[serial_test::serial(monodex_home)] attribute.
    unsafe {
        std::env::remove_var("MONODEX_HOME");
    }
}

/// Helper to create a minimal config file.
pub fn write_minimal_config(config_path: &std::path::Path) {
    let mut file = File::create(config_path).unwrap();
    writeln!(
        file,
        r#"{{
  "catalogs": {{}}
}}"#
    )
    .unwrap();
}

/// Create a test database with chunks and label metadata.
pub async fn create_test_db_with_chunks(
    db_path: &std::path::Path,
    chunks: Vec<ChunkRow>,
    labels: Vec<LabelMetadataRow>,
) {
    // Create database directory
    fs::create_dir_all(db_path).unwrap();

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
    let meta_file = File::create(db_path.join(META_FILE)).unwrap();
    serde_json::to_writer_pretty(meta_file, &meta).unwrap();

    // Insert chunks if any
    if !chunks.is_empty() {
        let db = Database::open(db_path).await.unwrap();
        let chunk_storage = db.chunks_storage().await.unwrap();
        let zero_vectors: Vec<Vec<f32>> = chunks
            .iter()
            .map(|_| vec![0.0f32; VECTOR_DIMENSION])
            .collect();
        chunk_storage
            .upsert_with_vectors(&chunks, &zero_vectors)
            .await
            .unwrap();
    }

    // Insert labels if any
    if !labels.is_empty() {
        let db = Database::open(db_path).await.unwrap();
        let label_storage = db.label_storage().await.unwrap();
        for label in labels {
            label_storage.upsert(&label).await.unwrap();
        }
    }
}

/// Create a test chunk row with default catalog.
pub fn test_chunk_row(point_id: &str, file_id: &str, ordinal: i32, label_id: &str) -> ChunkRow {
    ChunkRow {
        point_id: point_id.to_string(),
        text: format!("Test content for chunk {} in file {}", ordinal, file_id),
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
        breadcrumb: Some(format!(
            "test-package:test.ts:testFunction-chunk{}",
            ordinal
        )),
        split_part_ordinal: None,
        split_part_count: None,
        file_complete: ordinal == 1,
    }
}

/// Create a test chunk row with explicit catalog.
pub fn test_chunk_row_with_catalog(
    point_id: &str,
    file_id: &str,
    ordinal: i32,
    catalog: &str,
    label_id: &str,
) -> ChunkRow {
    ChunkRow {
        point_id: point_id.to_string(),
        text: format!("Test content for chunk {}", ordinal),
        catalog: catalog.to_string(),
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

/// Create a test label metadata row from a label_id string.
pub fn test_label_metadata_row(label_id: &str) -> LabelMetadataRow {
    LabelMetadataRow {
        label_id: label_id.to_string(),
        catalog: label_id
            .split(':')
            .next()
            .unwrap_or("test-catalog")
            .to_string(),
        label: label_id
            .split(':')
            .next_back()
            .unwrap_or("main")
            .to_string(),
        commit_oid: "abc123def456".to_string(),
        source_kind: "git-commit".to_string(),
        crawl_complete: true,
        updated_at_unix_secs: 1700000000,
    }
}

/// Create a test label metadata row with explicit catalog and label.
pub fn test_label_metadata_row_with_parts(catalog: &str, label: &str) -> LabelMetadataRow {
    LabelMetadataRow {
        label_id: format!("{}:{}", catalog, label),
        catalog: catalog.to_string(),
        label: label.to_string(),
        commit_oid: "abc123def456".to_string(),
        source_kind: "git-commit".to_string(),
        crawl_complete: true,
        updated_at_unix_secs: 1700000000,
    }
}
