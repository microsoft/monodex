//! Typed row structs for LanceDB tables.
//!
//! Purpose: Define plain-Rust row types that the rest of the engine deals in.
//! The storage module handles conversion to/from Arrow RecordBatches internally.
//!
//! Edit here when: Adding/removing/modifying fields in ChunkRow or LabelMetadataRow.
//! Do not edit here for: Arrow schema definitions (see schema.rs), storage operations (see chunks.rs, labels.rs).

use anyhow::{Result, anyhow};

use crate::engine::identifier::{LabelId, validate_catalog};

/// A row in the `chunks` table.
///
/// This struct represents the Rust view of a chunk row. The `vector` column is stored
/// separately in LanceDB and not included in this struct.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkRow {
    // Primary key
    pub point_id: String,

    // Content
    pub text: String,

    // Label membership
    pub catalog: String,
    pub active_label_ids: Vec<String>,

    // Implementation identity
    pub embedder_id: String,
    pub chunker_id: String,

    // Provenance
    pub blob_id: String,
    pub content_hash: String,

    // File identity
    pub file_id: String,

    // Path context
    pub relative_path: String,
    pub package_name: String,
    pub source_uri: String,

    // Chunk metadata
    pub chunk_ordinal: i32,
    pub chunk_count: i32,
    pub start_line: i32,
    pub end_line: i32,

    // Semantic context
    pub symbol_name: Option<String>,
    pub chunk_type: String,
    pub chunk_kind: String,
    pub breadcrumb: Option<String>,

    // Split metadata
    pub split_part_ordinal: Option<i32>,
    pub split_part_count: Option<i32>,

    // Sentinel
    pub file_complete: bool,
}

impl ChunkRow {
    /// Validates all identifier fields.
    ///
    /// This is the boundary where storage-form data enters the application.
    /// Any malformed data is a hard error - we do not log-and-skip.
    pub fn validate(&self) -> Result<()> {
        // Validate catalog
        validate_catalog(&self.catalog)
            .map_err(|e| anyhow!("Invalid catalog in stored row '{}': {}", self.catalog, e))?;

        // Validate each active_label_id using the canonical storage-form parser
        for label_id in &self.active_label_ids {
            LabelId::parse(label_id)
                .map_err(|e| anyhow!("Invalid label_id in stored row active_label_ids: {}", e))?;
        }

        // Validate that active_label_ids is non-empty (a chunk with no labels is garbage)
        if self.active_label_ids.is_empty() {
            return Err(anyhow!(
                "ChunkRow has empty active_label_ids - a chunk must belong to at least one label"
            ));
        }

        // Validate chunk_ordinal bounds (must be >= 1 and <= chunk_count)
        // Order matters: validate >= 1 before any `as usize` cast to avoid negative i32
        // becoming a huge usize.
        if self.chunk_ordinal < 1 {
            return Err(anyhow!(
                "ChunkRow has invalid chunk_ordinal {}: must be >= 1",
                self.chunk_ordinal
            ));
        }
        if self.chunk_ordinal > self.chunk_count {
            return Err(anyhow!(
                "ChunkRow has invalid chunk_ordinal {} > chunk_count {}",
                self.chunk_ordinal,
                self.chunk_count
            ));
        }

        // Validate point_id matches computed value from file_id and chunk_ordinal
        let expected_point_id =
            crate::engine::util::compute_point_id(&self.file_id, self.chunk_ordinal as usize);
        if self.point_id != expected_point_id {
            return Err(anyhow!(
                "ChunkRow point_id '{}' does not match expected '{}' for file_id '{}' and chunk_ordinal {}",
                self.point_id,
                expected_point_id,
                self.file_id,
                self.chunk_ordinal
            ));
        }

        Ok(())
    }
}

/// A row in the `label_metadata` table.
///
/// This struct represents the Rust view of label metadata. There is no vector column
/// because label metadata is not searched by similarity.
#[derive(Debug, Clone, PartialEq)]
pub struct LabelMetadataRow {
    // Primary key
    pub label_id: String,

    // Catalog context
    pub catalog: String,
    pub label: String,

    // Source info
    pub commit_oid: String,
    pub source_kind: String,

    // Crawl state
    pub crawl_complete: bool,
    pub updated_at_unix_secs: i64,
}

impl LabelMetadataRow {
    /// Validate all identifier fields in this metadata
    pub fn validate(&self) -> Result<()> {
        // Use LabelId::new as the canonical composition and validation path
        let expected_label_id = LabelId::new(&self.catalog, &self.label)
            .map_err(|e| anyhow!("Invalid identifier in stored metadata: {}", e))?;

        // Check label_id consistency (authoritative key)
        if self.label_id != expected_label_id.as_str() {
            return Err(anyhow!(
                "Label metadata has inconsistent label_id: expected '{}', got '{}'",
                expected_label_id.as_str(),
                self.label_id
            ));
        }

        Ok(())
    }
}

/// A chunk row with its similarity score from vector search.
#[derive(Debug, Clone)]
pub struct ScoredChunkRow {
    pub chunk: ChunkRow,
    /// Cosine distance from query vector. Smaller = more similar.
    pub distance: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_chunk_row() -> ChunkRow {
        ChunkRow {
            point_id: "abc123:1".to_string(),
            text: "some code".to_string(),
            catalog: "my-catalog".to_string(),
            active_label_ids: vec!["my-catalog:main".to_string()],
            embedder_id: "jina-embeddings-v2-base-code:v1".to_string(),
            chunker_id: "typescript-partitioner:v1".to_string(),
            blob_id: "abc123".to_string(),
            content_hash: "def456".to_string(),
            file_id: "abc123".to_string(),
            relative_path: "src/main.ts".to_string(),
            package_name: "my-package".to_string(),
            source_uri: "/path/to/src/main.ts".to_string(),
            chunk_ordinal: 1,
            chunk_count: 3,
            start_line: 1,
            end_line: 50,
            symbol_name: Some("main".to_string()),
            chunk_type: "function".to_string(),
            chunk_kind: "content".to_string(),
            breadcrumb: Some("my-package:main.ts:main".to_string()),
            split_part_ordinal: None,
            split_part_count: None,
            file_complete: true,
        }
    }

    #[test]
    fn test_chunk_row_validate_valid() {
        let row = valid_chunk_row();
        assert!(row.validate().is_ok());
    }

    #[test]
    fn test_chunk_row_validate_invalid_catalog() {
        let mut row = valid_chunk_row();
        row.catalog = "Invalid-Catalog".to_string(); // uppercase not allowed
        assert!(row.validate().is_err());
    }

    #[test]
    fn test_chunk_row_validate_empty_active_label_ids() {
        let mut row = valid_chunk_row();
        row.active_label_ids.clear();
        assert!(row.validate().is_err());
    }

    #[test]
    fn test_chunk_row_validate_invalid_active_label_id() {
        let mut row = valid_chunk_row();
        row.active_label_ids.push("invalid_label".to_string()); // missing colon
        assert!(row.validate().is_err());
    }

    fn valid_label_metadata_row() -> LabelMetadataRow {
        LabelMetadataRow {
            label_id: "my-catalog:main".to_string(),
            catalog: "my-catalog".to_string(),
            label: "main".to_string(),
            commit_oid: "abc123def456".to_string(),
            source_kind: "git-commit".to_string(),
            crawl_complete: true,
            updated_at_unix_secs: 1700000000,
        }
    }

    #[test]
    fn test_label_metadata_row_validate_valid() {
        let row = valid_label_metadata_row();
        assert!(row.validate().is_ok());
    }

    #[test]
    fn test_label_metadata_row_validate_inconsistent_label_id() {
        let mut row = valid_label_metadata_row();
        row.label_id = "my-catalog:other".to_string(); // doesn't match catalog+label
        assert!(row.validate().is_err());
    }

    #[test]
    fn test_label_metadata_row_validate_invalid_catalog() {
        let mut row = valid_label_metadata_row();
        row.catalog = "Invalid".to_string();
        row.label_id = "Invalid:main".to_string();
        assert!(row.validate().is_err());
    }
}
