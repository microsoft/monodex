//! FTS staleness manifest for incremental indexing.
//!
//! Purpose: Track which row_ids are indexed in Tantivy to enable cheap diff-based updates.
//! Edit here when: Changing manifest format, adding new versioning fields.
//! Do not edit here for: Tantivy schema (see schema.rs), indexing logic (see indexing.rs).
//!
//! ## Design
//!
//! The manifest serves two purposes:
//!
//! 1. **Fast diff computation**: When re-crawling a label, we can cheaply determine which
//!    chunks are new (add to Tantivy) vs removed (delete from Tantivy) by comparing
//!    LanceDB's current row_ids against the manifest's stored set.
//!
//! 2. **Version invalidation**: When the FTS schema or tokenizer changes, the stored
//!    `FTS_SCHEMA_ID` and `FTS_TOKENIZER_ID` will mismatch current constants, triggering
//!    a full rebuild.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

use crate::engine::util::{FTS_SCHEMA_ID, FTS_TOKENIZER_ID};

/// The result of reading a manifest file.
///
/// This enum distinguishes between different failure modes so callers can
/// dispatch appropriately (rebuild, error, or treat as fresh).
#[derive(Debug)]
pub enum ManifestRead {
    /// Manifest file does not exist.
    Missing,
    /// Manifest exists, parses, and IDs match current constants.
    Present(FtsManifest),
    /// Manifest exists and parses, but schema or tokenizer ID mismatches.
    IdMismatch {
        found_schema_id: String,
        found_tokenizer_id: String,
    },
    /// Manifest exists but cannot be parsed (truncated, corrupted JSON).
    Unreadable { error: String },
}

/// The manifest contents stored at `<db>/fts/<catalog>/<label>/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FtsManifest {
    /// Schema version identifier. Must match `FTS_SCHEMA_ID` for the manifest to be valid.
    pub fts_schema_id: String,
    /// Tokenizer version identifier. Must match `FTS_TOKENIZER_ID` for the manifest to be valid.
    pub fts_tokenizer_id: String,
    /// Sorted list of row_ids currently indexed in Tantivy.
    pub row_ids: Vec<String>,
}

impl FtsManifest {
    /// Create a new manifest with the current schema/tokenizer IDs and an empty row_id set.
    pub fn new() -> Self {
        Self {
            fts_schema_id: FTS_SCHEMA_ID.to_string(),
            fts_tokenizer_id: FTS_TOKENIZER_ID.to_string(),
            row_ids: Vec::new(),
        }
    }

    /// Create a manifest with the given row_ids.
    pub fn with_row_ids(row_ids: BTreeSet<String>) -> Self {
        let mut manifest = Self::new();
        manifest.row_ids = row_ids.into_iter().collect();
        manifest
    }

    /// Get the row_ids as a BTreeSet for efficient set operations.
    pub fn row_ids_set(&self) -> BTreeSet<String> {
        self.row_ids.iter().cloned().collect()
    }
}

impl Default for FtsManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// Read the manifest from disk.
///
/// Returns a `ManifestRead` enum that callers must dispatch on:
/// - `Missing`: No manifest exists; treat as empty index or check Tantivy state
/// - `Present(m)`: Valid manifest with matching IDs; use `m.row_ids` as the indexed set
/// - `IdMismatch { .. }`: IDs don't match; trigger a rebuild
/// - `Unreadable { .. }`: Corrupted manifest; check if Tantivy state exists to decide error vs rebuild
pub fn read_manifest(path: &Path) -> ManifestRead {
    if !path.exists() {
        return ManifestRead::Missing;
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            return ManifestRead::Unreadable {
                error: format!("Failed to read manifest: {}", e),
            };
        }
    };

    let manifest: FtsManifest = match serde_json::from_str(&content) {
        Ok(m) => m,
        Err(e) => {
            return ManifestRead::Unreadable {
                error: format!("Failed to parse manifest: {}", e),
            };
        }
    };

    // Check ID match
    if manifest.fts_schema_id != FTS_SCHEMA_ID || manifest.fts_tokenizer_id != FTS_TOKENIZER_ID {
        return ManifestRead::IdMismatch {
            found_schema_id: manifest.fts_schema_id,
            found_tokenizer_id: manifest.fts_tokenizer_id,
        };
    }

    ManifestRead::Present(manifest)
}

/// Write the manifest to disk.
///
/// Writes as pretty-printed JSON. No atomic rename; a crash mid-write leaves
/// a truncated file that will be detected as `Unreadable` on next read.
pub fn write_manifest(path: &Path, manifest: &FtsManifest) -> Result<()> {
    let content = serde_json::to_string_pretty(manifest)
        .map_err(|e| anyhow!("Failed to serialize manifest: {}", e))?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("Failed to create manifest directory: {}", e))?;
    }

    std::fs::write(path, content).map_err(|e| anyhow!("Failed to write manifest: {}", e))?;

    Ok(())
}

/// Reconcile the live row_id set from Tantivy's index.
///
/// Walks segments via `searcher.segment_readers()` and uses `alive_bitset()` to
/// skip tombstoned docs. Returns the set of row_ids actually present in the index.
///
/// This is used when the manifest is missing or invalid, to derive the current
/// indexed set from Tantivy's on-disk state.
///
/// NotFound errors (from concurrent purge) are handled gracefully: returns an empty
/// or partial set, consistent with the function's contract for missing-segment cases.
pub fn reconcile_from_index(
    searcher: &tantivy::Searcher,
    row_id_field: tantivy::schema::Field,
) -> Result<BTreeSet<String>> {
    use tantivy::TantivyDocument;
    use tantivy::schema::Value;

    use crate::engine::fts::error::is_io_not_found;

    let mut row_ids = BTreeSet::new();

    for segment_reader in searcher.segment_readers().iter() {
        let store_reader = match segment_reader.get_store_reader(0) {
            Ok(r) => r,
            Err(e) => {
                // NotFound errors (concurrent purge) are handled gracefully
                if is_io_not_found(&e) {
                    // Return what we have so far; the segment is gone
                    return Ok(row_ids);
                }
                return Err(anyhow!("Failed to get store reader: {}", e));
            }
        };

        // Get the alive bitset to skip tombstoned documents
        let alive_bitset = segment_reader.alive_bitset();

        for doc_id in 0..segment_reader.max_doc() {
            // Skip deleted documents
            let is_alive = if let Some(bitset) = alive_bitset {
                bitset.is_alive(doc_id)
            } else {
                true
            };

            if !is_alive {
                continue;
            }

            // Retrieve the stored document
            // NotFound errors during doc retrieval are also handled gracefully
            let doc: TantivyDocument = match store_reader.get(doc_id) {
                Ok(d) => d,
                Err(e) => {
                    // Check if this is a NotFound-related error
                    // store_reader.get returns various error types; check the error string
                    // as a fallback since the exact type varies by Tantivy version
                    let err_str = e.to_string().to_lowercase();
                    if err_str.contains("not found") || err_str.contains("does not exist") {
                        return Ok(row_ids);
                    }
                    return Err(anyhow!("Failed to get document: {}", e));
                }
            };

            // Extract the row_id field
            if let Some(value) = doc.get_first(row_id_field)
                && let Some(row_id) = value.as_str()
            {
                row_ids.insert(row_id.to_string());
            }
        }
    }

    Ok(row_ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    #[test]
    fn test_manifest_new() {
        let manifest = FtsManifest::new();
        assert_eq!(manifest.fts_schema_id, FTS_SCHEMA_ID);
        assert_eq!(manifest.fts_tokenizer_id, FTS_TOKENIZER_ID);
        assert!(manifest.row_ids.is_empty());
    }

    #[test]
    fn test_manifest_with_row_ids() {
        let mut set = BTreeSet::new();
        set.insert("row1".to_string());
        set.insert("row2".to_string());

        let manifest = FtsManifest::with_row_ids(set);
        assert_eq!(manifest.row_ids, vec!["row1", "row2"]);
    }

    #[test]
    fn test_read_missing_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("manifest.json");

        match read_manifest(&path) {
            ManifestRead::Missing => {}
            other => panic!("Expected Missing, got {:?}", other),
        }
    }

    #[test]
    fn test_write_and_read_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("manifest.json");

        let mut set = BTreeSet::new();
        set.insert("abc:1".to_string());
        set.insert("abc:2".to_string());

        let manifest = FtsManifest::with_row_ids(set);
        write_manifest(&path, &manifest).unwrap();

        match read_manifest(&path) {
            ManifestRead::Present(m) => {
                assert_eq!(m.row_ids, vec!["abc:1", "abc:2"]);
            }
            other => panic!("Expected Present, got {:?}", other),
        }
    }

    #[test]
    fn test_manifest_id_mismatch() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("manifest.json");

        // Write a manifest with wrong IDs
        let bad_manifest = FtsManifest {
            fts_schema_id: "old-schema-id".to_string(),
            fts_tokenizer_id: FTS_TOKENIZER_ID.to_string(),
            row_ids: vec!["row1".to_string()],
        };
        write_manifest(&path, &bad_manifest).unwrap();

        match read_manifest(&path) {
            ManifestRead::IdMismatch {
                found_schema_id, ..
            } => {
                assert_eq!(found_schema_id, "old-schema-id");
            }
            other => panic!("Expected IdMismatch, got {:?}", other),
        }
    }

    #[test]
    fn test_unreadable_manifest() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.path().join("manifest.json");

        // Write garbage
        std::fs::write(&path, "not valid json").unwrap();

        match read_manifest(&path) {
            ManifestRead::Unreadable { .. } => {}
            other => panic!("Expected Unreadable, got {:?}", other),
        }
    }
}
