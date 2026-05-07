//! FTS index management for Tantivy.
//!
//! Purpose: Open, create, and manage Tantivy indexes for full-text search.
//! Edit here when: Changing index open/create logic, adding new index operations.
//! Do not edit here for: Schema definitions (see schema.rs), tokenization (see tokenizer.rs).
//!
//! ## Index layout
//!
//! Each label has its own Tantivy index directory at:
//! `<db>/fts/<catalog>/<label>/`
//!
//! This directory contains:
//! - `meta.json`: Tantivy's index metadata
//! - Segment files: `*.idx`, `*.store`, `*.term`, `*.pos`, etc.
//! - `manifest.json`: Monodex's staleness manifest (managed by manifest.rs)

use anyhow::{Result, anyhow};
use std::path::{Path, PathBuf};
use tantivy::directory::MmapDirectory;
use tantivy::{Index, IndexSettings};

use crate::engine::fts::error::is_not_found_error;
use crate::engine::fts::manifest::{FtsManifest, ManifestRead, read_manifest, write_manifest};
use crate::engine::fts::schema::{FtsSchemaFields, fts_schema, get_fts_fields};
use crate::engine::fts::tokenizer::{FTS_TOKENIZER_NAME, MonodexFtsTokenizer};
use crate::engine::identifier::LabelId;

/// Heap budget for the FTS IndexWriter in bytes.
/// 200MB provides reasonable performance for typical workloads.
pub const FTS_HEAP_BUDGET_BYTES: usize = 200_000_000;

/// Wrapper around a Tantivy Index with schema field handles.
pub struct FtsIndex {
    /// The underlying Tantivy index.
    pub index: Index,
    /// Schema field handles for convenient access.
    pub fields: FtsSchemaFields,
    /// Path to the index directory.
    pub path: PathBuf,
}

impl FtsIndex {
    /// Open an existing FTS index or create a new one.
    ///
    /// This method implements a decision tree that handles:
    /// - Missing directory: create new index
    /// - Empty directory: create new index
    /// - Existing index: open and validate
    /// - Schema/tokenizer mismatch: rebuild from scratch
    /// - Corrupted state: error (do not silently rebuild)
    ///
    /// # Arguments
    /// * `db_path` - Path to the Monodex database root
    /// * `label_id` - The label identifier (determines index directory path)
    ///
    /// # Returns
    /// An `FtsIndex` wrapper with the index and field handles.
    pub fn open_or_create(db_path: &Path, label_id: &LabelId) -> Result<Self> {
        let index_dir = fts_index_dir(db_path, label_id);

        // Step 1: Read the manifest first
        let manifest_path = index_dir.join("manifest.json");
        let manifest_result = read_manifest(&manifest_path);

        // Handle manifest results that require action before opening Tantivy
        match &manifest_result {
            ManifestRead::IdMismatch { .. } => {
                // Delete the entire per-label FTS directory and rebuild
                if index_dir.exists() {
                    std::fs::remove_dir_all(&index_dir).map_err(|e| {
                        anyhow!("Failed to remove FTS directory for rebuild: {}", e)
                    })?;
                }
            }
            ManifestRead::Unreadable { error } => {
                // Check if Tantivy state exists
                if has_tantivy_state(&index_dir) {
                    return Err(anyhow!(
                        "FTS manifest at {} is unreadable but Tantivy state exists; database may be corrupt: {}",
                        manifest_path.display(),
                        error
                    ));
                }
                // No Tantivy state, treat as fresh (manifest write crashed before any Tantivy state)
            }
            _ => {}
        }

        // Step 2: Filesystem state check - open or create the index
        let schema = fts_schema();
        let index = if !index_dir.exists() {
            // Directory does not exist: create it and initialize a new index
            std::fs::create_dir_all(&index_dir)
                .map_err(|e| anyhow!("Failed to create FTS directory: {}", e))?;
            let directory = MmapDirectory::open(&index_dir)
                .map_err(|e| anyhow!("Failed to open MmapDirectory: {}", e))?;
            Index::create(directory, schema.clone(), IndexSettings::default())
                .map_err(|e| anyhow!("Failed to create Tantivy index: {}", e))?
        } else if !has_tantivy_state(&index_dir) {
            // Directory exists but is empty: initialize a new index
            let directory = MmapDirectory::open(&index_dir)
                .map_err(|e| anyhow!("Failed to open MmapDirectory: {}", e))?;
            Index::create(directory, schema.clone(), IndexSettings::default())
                .map_err(|e| anyhow!("Failed to create Tantivy index: {}", e))?
        } else {
            // Directory exists and contains Tantivy state: open it
            let directory = MmapDirectory::open(&index_dir)
                .map_err(|e| anyhow!("Failed to open MmapDirectory: {}", e))?;
            Index::open(directory)
                .map_err(|e| anyhow!("Failed to open existing Tantivy index: {}", e))?
        };

        // Step 3: Register the custom tokenizer
        index
            .tokenizers()
            .register(FTS_TOKENIZER_NAME, MonodexFtsTokenizer);

        let fields = get_fts_fields(&index.schema());

        Ok(FtsIndex {
            index,
            fields,
            path: index_dir,
        })
    }

    /// Open an existing FTS index for read-only access.
    ///
    /// Returns `Ok(None)` if the index directory does not exist or has no Tantivy state.
    /// This is the read-side answer to concurrent purge operations.
    ///
    /// Any `std::io::ErrorKind::NotFound` during the open path returns `Ok(None)`,
    /// not `Err`. Other Tantivy errors (mmap failures, corruption) remain `Err`.
    ///
    /// # Arguments
    /// * `db_path` - Path to the Monodex database root
    /// * `label_id` - The label identifier
    pub fn open_existing(db_path: &Path, label_id: &LabelId) -> Result<Option<Self>> {
        let index_dir = fts_index_dir(db_path, label_id);

        // Check if directory exists with Tantivy state
        if !has_tantivy_state(&index_dir) {
            return Ok(None);
        }

        // Try to open the index
        let directory = match MmapDirectory::open(&index_dir) {
            Ok(d) => d,
            Err(e) => {
                // Use typed error discrimination for NotFound
                if is_not_found_error(&tantivy::TantivyError::OpenDirectoryError(e.clone())) {
                    return Ok(None);
                }
                return Err(anyhow!("Failed to open MmapDirectory: {}", e));
            }
        };

        let index = match Index::open(directory) {
            Ok(i) => i,
            Err(e) => {
                // Use typed error discrimination for NotFound
                if is_not_found_error(&e) {
                    return Ok(None);
                }
                return Err(anyhow!("Failed to open existing Tantivy index: {}", e));
            }
        };

        // Register the custom tokenizer
        index
            .tokenizers()
            .register(FTS_TOKENIZER_NAME, MonodexFtsTokenizer);

        let fields = get_fts_fields(&index.schema());

        Ok(Some(FtsIndex {
            index,
            fields,
            path: index_dir,
        }))
    }

    /// Get an IndexWriter for document updates.
    ///
    /// The writer holds a lock on the index directory. Only one writer can exist
    /// at a time per index. Under our per-catalog lock discipline, this is
    /// guaranteed by the caller.
    pub fn writer(&self) -> Result<tantivy::IndexWriter> {
        self.index
            .writer(FTS_HEAP_BUDGET_BYTES)
            .map_err(|e| anyhow!("Failed to create IndexWriter: {}", e))
    }

    /// Get an IndexReader for queries.
    pub fn reader(&self) -> Result<tantivy::IndexReader> {
        self.index
            .reader()
            .map_err(|e| anyhow!("Failed to create IndexReader: {}", e))
    }

    /// Get the path to the manifest file for this index.
    pub fn manifest_path(&self) -> PathBuf {
        self.path.join("manifest.json")
    }

    /// Read the manifest for this index.
    ///
    /// Returns the manifest read result, handling the case where the manifest
    /// doesn't exist yet (new index).
    pub fn read_manifest(&self) -> ManifestRead {
        read_manifest(&self.manifest_path())
    }

    /// Write the manifest for this index.
    pub fn write_manifest(&self, manifest: &FtsManifest) -> Result<()> {
        write_manifest(&self.manifest_path(), manifest)
    }
}

/// Compute the FTS index directory path for a label.
///
/// The path is: `<db>/fts/<catalog>/<label>/`
pub fn fts_index_dir(db_path: &Path, label_id: &LabelId) -> PathBuf {
    db_path
        .join("fts")
        .join(label_id.catalog())
        .join(label_id.label())
}

/// Check if a directory contains Tantivy index state.
///
/// This is indicated by the presence of `meta.json` or any Tantivy segment files.
fn has_tantivy_state(dir: &Path) -> bool {
    if !dir.exists() {
        return false;
    }

    // Check for meta.json
    if dir.join("meta.json").exists() {
        return true;
    }

    // Check for any segment files
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.ends_with(".idx") || name.ends_with(".store") || name.ends_with(".term") {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::identifier::LabelId;
    use tempfile::TempDir;

    fn make_label_id(catalog: &str, label: &str) -> LabelId {
        LabelId::new(catalog, label).unwrap()
    }

    #[test]
    fn test_open_or_create_creates_new_index() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path();
        let label_id = make_label_id("test-catalog", "test-label");

        let _fts_index = FtsIndex::open_or_create(db_path, &label_id).unwrap();

        // Verify directory was created
        let expected_dir = fts_index_dir(db_path, &label_id);
        assert!(expected_dir.exists());

        // Verify meta.json exists (Tantivy creates it)
        assert!(expected_dir.join("meta.json").exists());
    }

    #[test]
    fn test_open_existing_returns_none_for_missing() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path();
        let label_id = make_label_id("test-catalog", "missing-label");

        let result = FtsIndex::open_existing(db_path, &label_id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_open_existing_opens_created_index() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path();
        let label_id = make_label_id("test-catalog", "test-label");

        // Create the index
        let _ = FtsIndex::open_or_create(db_path, &label_id).unwrap();

        // Open existing
        let fts_index = FtsIndex::open_existing(db_path, &label_id).unwrap();
        assert!(fts_index.is_some());
    }

    #[test]
    fn test_fts_index_dir_path() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path();
        let label_id = make_label_id("my-catalog", "my-label");

        let dir = fts_index_dir(db_path, &label_id);
        assert_eq!(dir, db_path.join("fts").join("my-catalog").join("my-label"));
    }

    #[test]
    fn test_writer_and_reader() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path();
        let label_id = make_label_id("test-catalog", "test-label");

        let fts_index = FtsIndex::open_or_create(db_path, &label_id).unwrap();

        // Should be able to create a writer
        let _writer = fts_index.writer().unwrap();

        // Should be able to create a reader
        let _reader = fts_index.reader().unwrap();
    }
}
