//! Arrow schemas for LanceDB tables.
//!
//! Purpose: Define the canonical schema for monodex database tables.
//! Edit here when: Adding/removing/modifying columns in the chunks or label_metadata tables.
//! Do not edit here for: Storage operations (see engine/storage/), CLI handlers (see app/commands/).
//!
//! ## Schema version
//!
//! The `MONODEX_SCHEMA_VERSION` constant tracks breaking schema changes. When columns
//! are added, removed, or have their types changed, this version must be incremented
//! and a future `monodex upgrade-db` command will handle migrations.
//!
//! ## Vector dimension
//!
//! The 768-dimension vector column is a schema-bound constant. It matches the output
//! of the current embedding model (jina-embeddings-v2-base-code). Changing this
//! dimension requires:
//! 1. Incrementing `MONODEX_SCHEMA_VERSION`
//! 2. Implementing migration in `upgrade-db`
//! 3. Updating the embedder configuration
//!
//! Do NOT make this runtime-configurable. The dimension is part of the schema contract.

use arrow_schema::{DataType, Field, Schema, SchemaRef};
use std::sync::Arc;

/// Current schema version. Increment on breaking schema changes.
pub const MONODEX_SCHEMA_VERSION: u32 = 1;

/// Vector dimension for the embedding model (jina-embeddings-v2-base-code).
pub const VECTOR_DIMENSION: usize = 768;

/// Table name for code chunks.
pub const CHUNKS_TABLE: &str = "chunks";

/// Table name for label metadata.
pub const LABEL_METADATA_TABLE: &str = "label_metadata";

/// Returns the Arrow schema for the `chunks` table.
///
/// Column ordering follows a logical grouping:
/// 1. Primary key (point_id)
/// 2. Content (text, vector)
/// 3. Label membership (catalog, active_label_ids)
/// 4. Implementation identity
/// 5. Provenance
/// 6. File identity
/// 7. Path context
/// 8. Chunk metadata
/// 9. Semantic context
/// 10. Split metadata
/// 11. Sentinel flag
pub fn chunks_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        // Primary key
        Field::new("point_id", DataType::Utf8, false),
        // Content
        Field::new("text", DataType::Utf8, false),
        Field::new(
            "vector",
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                VECTOR_DIMENSION as i32,
            ),
            false, // vectors are mandatory: every chunk has an embedding
        ),
        // Label membership
        Field::new("catalog", DataType::Utf8, false),
        Field::new(
            "active_label_ids",
            DataType::List(Arc::new(Field::new("item", DataType::Utf8, false))),
            false, // non-nullable; a chunk must belong to at least one label
        ),
        // Implementation identity
        Field::new("embedder_id", DataType::Utf8, false),
        Field::new("chunker_id", DataType::Utf8, false),
        // Provenance
        Field::new("blob_id", DataType::Utf8, false),
        Field::new("content_hash", DataType::Utf8, false),
        // File identity
        Field::new("file_id", DataType::Utf8, false),
        // Path context
        Field::new("relative_path", DataType::Utf8, false),
        Field::new("package_name", DataType::Utf8, false),
        Field::new("source_uri", DataType::Utf8, false),
        // Chunk metadata
        Field::new("chunk_ordinal", DataType::Int32, false),
        Field::new("chunk_count", DataType::Int32, false),
        Field::new("start_line", DataType::Int32, false),
        Field::new("end_line", DataType::Int32, false),
        // Semantic context
        Field::new("symbol_name", DataType::Utf8, true), // nullable
        Field::new("chunk_type", DataType::Utf8, false),
        Field::new("chunk_kind", DataType::Utf8, false),
        Field::new("breadcrumb", DataType::Utf8, true), // nullable
        // Split metadata (for oversized sections split into parts)
        Field::new("split_part_ordinal", DataType::Int32, true), // nullable
        Field::new("split_part_count", DataType::Int32, true),   // nullable
        // Sentinel for incremental crawl
        Field::new("file_complete", DataType::Boolean, false),
    ]))
}

/// Returns the Arrow schema for the `label_metadata` table.
///
/// The `label_id` is the primary key: `"{catalog}:{label}"`. This table has no vector
/// column because label metadata is not searched by similarity.
pub fn label_metadata_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        // Primary key
        Field::new("label_id", DataType::Utf8, false),
        // Catalog context
        Field::new("catalog", DataType::Utf8, false),
        Field::new("label", DataType::Utf8, false),
        // Source info
        Field::new("commit_oid", DataType::Utf8, false),
        Field::new("source_kind", DataType::Utf8, false),
        // Crawl state
        Field::new("crawl_complete", DataType::Boolean, false),
        Field::new("updated_at_unix_secs", DataType::Int64, false),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunks_schema_constructible() {
        let schema = chunks_schema();

        // Verify expected column count
        assert_eq!(
            schema.fields().len(),
            24,
            "chunks table should have 24 columns"
        );

        // Verify primary key column exists and is non-nullable
        let point_id_field = schema.field_with_name("point_id").unwrap();
        assert_eq!(point_id_field.data_type(), &DataType::Utf8);
        assert!(!point_id_field.is_nullable());

        // Verify vector column has correct dimension
        let vector_field = schema.field_with_name("vector").unwrap();
        match vector_field.data_type() {
            DataType::FixedSizeList(_, dim) => {
                assert_eq!(*dim, VECTOR_DIMENSION as i32);
            }
            _ => panic!("vector field should be FixedSizeList"),
        }

        // Verify active_label_ids is List<Utf8>
        let labels_field = schema.field_with_name("active_label_ids").unwrap();
        match labels_field.data_type() {
            DataType::List(inner) => {
                assert_eq!(inner.data_type(), &DataType::Utf8);
            }
            _ => panic!("active_label_ids field should be List<Utf8>"),
        }
    }

    #[test]
    fn test_label_metadata_schema_constructible() {
        let schema = label_metadata_schema();

        // Verify expected column count
        assert_eq!(
            schema.fields().len(),
            7,
            "label_metadata table should have 7 columns"
        );

        // Verify primary key column exists and is non-nullable
        let label_id_field = schema.field_with_name("label_id").unwrap();
        assert_eq!(label_id_field.data_type(), &DataType::Utf8);
        assert!(!label_id_field.is_nullable());

        // Verify no vector column
        assert!(schema.field_with_name("vector").is_err());
    }

    #[test]
    fn test_schema_version_is_one() {
        // This test documents the initial version. It will need updating
        // when the schema evolves.
        assert_eq!(MONODEX_SCHEMA_VERSION, 1);
    }

    #[test]
    fn test_table_name_constants() {
        assert_eq!(CHUNKS_TABLE, "chunks");
        assert_eq!(LABEL_METADATA_TABLE, "label_metadata");
    }
}
