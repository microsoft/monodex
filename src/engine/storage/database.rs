//! Database open and validation logic.
//!
//! Purpose: Open a LanceDB dataset, validate schema version, expose table handles.
//!
//! Edit here when: Changing database open logic, schema version validation, or table handle access.
//! Do not edit here for: Row types (see rows.rs), chunk operations (see chunks.rs), label operations (see labels.rs).

use anyhow::{Result, anyhow, bail};
use lancedb::connection::Connection;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::engine::schema::{CHUNKS_TABLE, LABEL_METADATA_TABLE, MONODEX_SCHEMA_VERSION};
use std::sync::Arc;

use super::{ChunkStorage, LANCEDB_CRATE_VERSION, LabelStorage};

/// Metadata file name in the database root.
pub const META_FILE: &str = "monodex-meta.json";

/// Represents a monodex database with typed table access.
pub struct Database {
    /// The LanceDB connection
    conn: Connection,
    /// Path to the database root directory
    path: PathBuf,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// Contents of `monodex-meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetaFile {
    /// Schema version, must match `MONODEX_SCHEMA_VERSION`.
    pub monodex_schema_version: u32,
    /// When the database was created (ISO 8601 timestamp).
    pub created_at: String,
    /// Version of the monodex binary that created the database.
    pub created_by_binary_version: String,
    /// Lance format version at creation time.
    pub lance_format_version: String,
}

impl MetaFile {
    /// Create a new meta file with current version info.
    pub fn new() -> Self {
        Self {
            monodex_schema_version: MONODEX_SCHEMA_VERSION,
            created_at: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .expect("RFC 3339 formatting is infallible"),
            created_by_binary_version: env!("CARGO_PKG_VERSION").to_string(),
            lance_format_version: LANCEDB_CRATE_VERSION.to_string(),
        }
    }
}

impl Default for MetaFile {
    fn default() -> Self {
        Self::new()
    }
}

/// Format the schema mismatch error message.
/// Used by both Database::open and init-db to ensure consistent wording.
pub fn err_schema_mismatch(db_version: u32, expected_version: u32) -> String {
    format!(
        "Schema mismatch: database has version {} but monodex expects version {}. \
         Run 'monodex upgrade-db' to migrate, or delete the database and run 'monodex init-db' to recreate it.",
        db_version, expected_version
    )
}

impl Database {
    /// Open an existing monodex database.
    ///
    /// Validates that:
    /// 1. The database directory exists
    /// 2. `monodex-meta.json` exists and is valid
    /// 3. The schema version matches `MONODEX_SCHEMA_VERSION`
    ///
    /// Returns an error if the database does not exist or is incompatible.
    /// Use `init_db()` to create a new database.
    pub async fn open(path: &Path) -> Result<Self> {
        // Check existence before canonicalize to avoid re-routing canonicalize errors to "no database"
        if !path.exists() {
            bail!(
                "No monodex database at '{}'. Run 'monodex init-db' to create it.",
                path.display()
            );
        }

        // Canonicalize after existence check
        let path = path.canonicalize().map_err(|e| {
            anyhow!(
                "Failed to resolve database path '{}': {}",
                path.display(),
                e
            )
        })?;

        // Check for meta file
        let meta_path = path.join(META_FILE);
        if !meta_path.exists() {
            bail!(
                "No monodex database at '{}'. Run 'monodex init-db' to create it.",
                path.display()
            );
        }

        // Load and validate meta file
        let meta = Self::load_meta(&meta_path)?;

        if meta.monodex_schema_version != MONODEX_SCHEMA_VERSION {
            bail!(
                "{}",
                err_schema_mismatch(meta.monodex_schema_version, MONODEX_SCHEMA_VERSION)
            );
        }

        // Check that table directories exist
        let chunks_path = path.join(format!("{}.lance", CHUNKS_TABLE));
        let labels_path = path.join(format!("{}.lance", LABEL_METADATA_TABLE));
        if !chunks_path.exists() || !labels_path.exists() {
            bail!(
                "Database at '{}' is missing table directories. Manual cleanup required.",
                path.display()
            );
        }

        // Open LanceDB connection
        let conn = lancedb::connect(path.to_str().unwrap())
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to open LanceDB database: {}", e))?;

        Ok(Self { conn, path })
    }

    /// Load and parse the meta file.
    pub fn load_meta(path: &Path) -> Result<MetaFile> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let meta: MetaFile = serde_json::from_reader(reader)
            .map_err(|e| anyhow!("Failed to parse {}: {}", path.display(), e))?;
        Ok(meta)
    }

    /// Write the meta file with fsync for durability.
    pub fn write_meta(path: &Path, meta: &MetaFile) -> Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, meta)
            .map_err(|e| anyhow!("Failed to write {}: {}", path.display(), e))?;
        writer.flush()?;
        writer
            .into_inner()
            .map_err(|e| anyhow!("Failed to flush {}: {}", path.display(), e))?
            .sync_all()?;

        // Sync the parent directory to ensure the file entry is durable
        #[cfg(unix)]
        {
            if let Some(parent) = path.parent() {
                let dir_file = File::open(parent)?;
                dir_file.sync_all()?;
            }
        }

        Ok(())
    }

    /// Get the database path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the LanceDB connection.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Get the chunks table name.
    pub fn chunks_table(&self) -> &'static str {
        CHUNKS_TABLE
    }

    /// Get the label metadata table name.
    pub fn label_metadata_table(&self) -> &'static str {
        LABEL_METADATA_TABLE
    }

    /// Open the chunks table and return a ChunkStorage wrapper.
    ///
    /// Returns an error if the table doesn't exist.
    pub async fn chunks_storage(&self) -> Result<ChunkStorage> {
        use crate::engine::storage::ChunkStorage;

        let table = self
            .conn
            .open_table(CHUNKS_TABLE)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to open chunks table: {}", e))?;

        Ok(ChunkStorage::new(Arc::new(table)))
    }

    /// Open the label_metadata table and return a LabelStorage wrapper.
    ///
    /// Returns an error if the table doesn't exist.
    pub async fn label_storage(&self) -> Result<LabelStorage> {
        use crate::engine::storage::LabelStorage;

        let table = self
            .conn
            .open_table(LABEL_METADATA_TABLE)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to open label_metadata table: {}", e))?;

        Ok(LabelStorage::new(Arc::new(table)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_meta_file_new() {
        let meta = MetaFile::new();
        assert_eq!(meta.monodex_schema_version, MONODEX_SCHEMA_VERSION);
        assert!(!meta.created_at.is_empty());
        assert!(!meta.created_by_binary_version.is_empty());
        assert!(!meta.lance_format_version.is_empty());
    }

    #[test]
    fn test_meta_file_roundtrip() {
        let meta = MetaFile::new();
        let tmp_dir = TempDir::new().unwrap();
        let meta_path = tmp_dir.path().join(META_FILE);

        Database::write_meta(&meta_path, &meta).unwrap();
        let loaded = Database::load_meta(&meta_path).unwrap();

        assert_eq!(loaded.monodex_schema_version, meta.monodex_schema_version);
        assert_eq!(loaded.created_at, meta.created_at);
        assert_eq!(
            loaded.created_by_binary_version,
            meta.created_by_binary_version
        );
        assert_eq!(loaded.lance_format_version, meta.lance_format_version);
    }

    #[tokio::test]
    async fn test_open_missing_database() {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("nonexistent");

        let result = Database::open(&db_path).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No monodex database"));
        assert!(err.contains("init-db"));
    }

    #[tokio::test]
    async fn test_open_missing_meta_file() {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("empty-db");
        std::fs::create_dir_all(&db_path).unwrap();

        let result = Database::open(&db_path).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("No monodex database"));
    }

    #[tokio::test]
    async fn test_open_schema_mismatch() {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("old-db");
        std::fs::create_dir_all(&db_path).unwrap();

        // Write meta with wrong schema version
        let mut meta = MetaFile::new();
        meta.monodex_schema_version = 99;
        Database::write_meta(&db_path.join(META_FILE), &meta).unwrap();

        let result = Database::open(&db_path).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Schema mismatch"));
        assert!(err.contains("version 99"));
    }
}
