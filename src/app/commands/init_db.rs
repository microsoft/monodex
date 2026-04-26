//! Database initialization command.
//!
//! Purpose: Create a new monodex database directory with LanceDB tables.
//! Edit here when: Changing init-db behavior, error messages, or initialization logic.
//! Do not edit here for: Database open logic (see engine/storage/database.rs),
//!   schema definitions (see engine/schema.rs), config loading (app/config.rs).

use anyhow::{Result, anyhow, bail};
use std::fs::{self, File, OpenOptions};
use std::path::Path;

use crate::app::config::{Config, resolve_database_path};
use crate::engine::schema::{
    CHUNKS_TABLE, LABEL_METADATA_TABLE, chunks_schema, label_metadata_schema,
};
use crate::engine::storage::{Database, META_FILE, MetaFile, err_schema_mismatch};
use crate::paths;

// ============================================================================
// Error message helpers
// These produce load-bearing user-facing strings. Tests assert exact matches.
// ============================================================================

/// Format the "parent missing" error with the database path.
fn err_parent_missing(db_path: &Path) -> String {
    format!(
        "Cannot create database at {}: parent directory does not exist.",
        db_path.display()
    )
}

/// Format the "not a monodex database" error with the database path.
fn err_not_monodex_db(db_path: &Path) -> String {
    format!(
        "Path {} exists but is not a monodex database.",
        db_path.display()
    )
}

/// Format the "partial state" error with the database path.
fn err_partial_state(db_path: &Path) -> String {
    format!(
        "Path {} appears to be a partially-initialized or corrupted monodex database. Manual cleanup required.",
        db_path.display()
    )
}

/// Format the "already initialized" log message with the database path and schema version.
fn log_already_initialized(db_path: &Path, schema_version: u32) -> String {
    format!(
        "Database at {} is already initialized (monodex_schema_version {}); skipping.",
        db_path.display(),
        schema_version
    )
}

// ============================================================================
// Lock guard for init-db
// ============================================================================

/// Guard that holds the lockfile handle. Drop releases the OS-level lock.
/// The lockfile itself is NOT removed - it is permanent database state.
struct LockGuard {
    file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs4::fs_std::FileExt::unlock(&self.file);
    }
}

// ============================================================================
// Command entry point
// ============================================================================

/// Run the init-db command.
///
/// Creates a new monodex database at the configured path, or verifies an existing
/// database is valid. The database contains LanceDB tables for chunks and label metadata.
///
/// Note: Config must be loaded by the caller (main.rs) and passed in.
/// This ensures the --config flag is respected uniformly across all commands.
pub fn run_init_db(config: &Config) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow!("Failed to create tokio runtime: {}", e))?;
    rt.block_on(init_db_inner(config))
}

async fn init_db_inner(config: &Config) -> Result<()> {
    // Step 1: Resolve database path from config
    let db_path = resolve_database_path(Some(config))?;

    // Step 2: Validate parent directory exists (with exception for default-db)
    // This must happen BEFORE any directory creation.
    validate_parent_directory(&db_path)?;

    // Step 3: Create the database root directory (if it doesn't exist)
    // For default-db, we can create tool_home if needed. For custom paths, parent must exist.
    let tool_home = paths::tool_home()?;
    let default_db_path = tool_home.join("default-db");

    if db_path == default_db_path {
        // default-db: ensure tool_home exists, then create default-db if needed
        fs::create_dir_all(&tool_home)?;
        if !db_path.exists() {
            fs::create_dir(&db_path)?;
        }
    } else if !db_path.exists() {
        // Custom path: parent must exist (validated above), create only db_path
        fs::create_dir(&db_path)?;
    }

    // Step 4: Acquire exclusive lock
    // The lockfile is permanent and never removed. The OS-level lock is released on Drop.
    let lock_path = db_path.join(".monodex.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    fs4::fs_std::FileExt::lock_exclusive(&lock_file)?;

    // Hold the lock for the duration of init. Drop releases the OS-level lock.
    // Do NOT remove the lockfile - it is expected database state.
    let _guard = LockGuard { file: lock_file };

    // Step 5: Check if already initialized (under the lock)
    if let Some(meta) = check_existing_database(&db_path)? {
        println!(
            "{}",
            log_already_initialized(&db_path, meta.monodex_schema_version)
        );
        return Ok(());
    }

    // Step 6: Create the database
    create_database(&db_path).await?;

    println!("Created monodex database at {}", db_path.display());
    Ok(())
}

/// Check if a database already exists at the given path.
/// Returns Some(MetaFile) if valid database exists, None if path doesn't exist.
/// Returns error if path exists but is not a valid database.
fn check_existing_database(db_path: &Path) -> Result<Option<MetaFile>> {
    if !db_path.exists() {
        return Ok(None);
    }

    // Check if it's a valid monodex database
    let meta_path = db_path.join(META_FILE);
    let chunks_path = db_path.join(format!("{}.lance", CHUNKS_TABLE));
    let labels_path = db_path.join(format!("{}.lance", LABEL_METADATA_TABLE));

    if meta_path.exists() {
        // Try to load meta file
        let meta = match Database::load_meta(&meta_path) {
            Ok(m) => m,
            Err(_) => {
                // Corrupted meta file
                bail!(err_partial_state(db_path));
            }
        };

        // Check schema version
        if meta.monodex_schema_version != crate::engine::schema::MONODEX_SCHEMA_VERSION {
            bail!(err_schema_mismatch(
                meta.monodex_schema_version,
                crate::engine::schema::MONODEX_SCHEMA_VERSION
            ));
        }

        // Check that table directories exist
        if !chunks_path.exists() || !labels_path.exists() {
            bail!(err_partial_state(db_path));
        }

        Ok(Some(meta))
    } else {
        // Check for partial state (tables exist but no meta)
        if chunks_path.exists() || labels_path.exists() {
            bail!(err_partial_state(db_path));
        }

        // Check if directory is empty (ignoring lockfile)
        let is_empty = db_path
            .read_dir()
            .map(|mut entries| {
                entries.all(|e| {
                    e.ok()
                        .map(|e| e.file_name() == ".monodex.lock")
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        if is_empty {
            // Empty directory (or only lockfile), treat as non-existent
            Ok(None)
        } else {
            // Non-empty without meta file or tables
            bail!(err_not_monodex_db(db_path));
        }
    }
}

/// Validate that the parent directory exists, with exception for default-db.
fn validate_parent_directory(db_path: &Path) -> Result<()> {
    // Special case: if the path is exactly the default-db path under tool_home,
    // we can create tool_home itself.
    let tool_home = paths::tool_home()?;
    let default_db_path = tool_home.join("default-db");

    if db_path == default_db_path {
        // default-db: tool_home will be created by create_database
        return Ok(());
    }

    if let Some(parent) = db_path.parent()
        && !parent.exists()
    {
        bail!(err_parent_missing(db_path));
    }

    Ok(())
}

/// Create the database directory and initialize LanceDB tables.
async fn create_database(db_path: &Path) -> Result<()> {
    // Open LanceDB connection
    let conn = lancedb::connect(db_path.to_str().unwrap())
        .execute()
        .await
        .map_err(|e| anyhow!("Failed to create LanceDB database: {}", e))?;

    // Create chunks table
    conn.create_empty_table(CHUNKS_TABLE, chunks_schema())
        .execute()
        .await
        .map_err(|e| anyhow!("Failed to create chunks table: {}", e))?;

    // Create label_metadata table
    conn.create_empty_table(LABEL_METADATA_TABLE, label_metadata_schema())
        .execute()
        .await
        .map_err(|e| anyhow!("Failed to create label_metadata table: {}", e))?;

    // Write meta file using shared implementation (with fsync)
    let meta = MetaFile::new();
    let meta_path = db_path.join(META_FILE);
    Database::write_meta(&meta_path, &meta)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::commands::test_helpers::{
        remove_monodex_home, set_monodex_home, write_minimal_config,
    };
    use crate::app::config::load_config;
    use crate::paths::clear_tool_home_cache;
    use serial_test::serial;
    use std::io::Write;
    use tempfile::TempDir;

    /// Helper to create a config file with a custom database path.
    fn write_config_with_db_path(config_path: &Path, db_path: &str) {
        let mut file = File::create(config_path).unwrap();
        writeln!(
            file,
            r#"{{
  "catalogs": {{}},
  "database": {{ "path": "{}" }}
}}"#,
            db_path
        )
        .unwrap();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_happy_path_creates_database() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        // Set MONODEX_HOME to temp directory
        set_monodex_home(temp_dir.path());

        // Create minimal config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Load config (simulating main.rs behavior)
        let config = load_config(&config_path).expect("Config should load");

        // Run init-db
        let result = run_init_db(&config);

        // Should succeed
        assert!(result.is_ok(), "init-db should succeed: {:?}", result.err());

        // Verify structure
        let db_path = temp_dir.path().join("default-db");
        assert!(db_path.exists(), "Database directory should exist");
        assert!(
            db_path.join(META_FILE).exists(),
            "monodex-meta.json should exist"
        );

        // Cleanup env
        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_idempotent_second_run_succeeds() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Load config (simulating main.rs behavior)
        let config = load_config(&config_path).expect("Config should load");

        // First run
        let result1 = run_init_db(&config);
        assert!(result1.is_ok(), "First init-db should succeed");

        // Second run
        clear_tool_home_cache(); // Clear cache for second run
        let result2 = run_init_db(&config);
        assert!(result2.is_ok(), "Second init-db should succeed");

        // Verify database still valid
        let db_path = temp_dir.path().join("default-db");
        assert!(db_path.join(META_FILE).exists());

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_parent_missing_non_default_db() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Use an absolute path whose parent definitely doesn't exist
        let db_path_str = "/nonexistent-xyz-12345/db";
        let config_path = temp_dir.path().join("config.json");
        write_config_with_db_path(&config_path, db_path_str);

        // Load config (simulating main.rs behavior)
        let config = load_config(&config_path).expect("Config should load");

        let result = run_init_db(&config);
        let err = result.unwrap_err();

        // Exact match on error message
        let expected_db_path = std::path::PathBuf::from(db_path_str);
        assert_eq!(err.to_string(), err_parent_missing(&expected_db_path));

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_path_exists_but_not_monodex_database() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create a directory with a stray file (not a monodex database)
        let db_path = temp_dir.path().join("my-db");
        fs::create_dir_all(&db_path).unwrap();
        File::create(db_path.join("stray-file.txt"))
            .unwrap()
            .write_all(b"not a monodex database")
            .unwrap();

        let config_path = temp_dir.path().join("config.json");
        write_config_with_db_path(&config_path, db_path.to_str().unwrap());

        // Load config (simulating main.rs behavior)
        let config = load_config(&config_path).expect("Config should load");

        let result = run_init_db(&config);
        let err = result.unwrap_err();

        // Exact match on error message
        assert_eq!(err.to_string(), err_not_monodex_db(&db_path));

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_corrupt_meta_file() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // First, create a valid database
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Load config (simulating main.rs behavior)
        let config = load_config(&config_path).expect("Config should load");

        let result = run_init_db(&config);
        assert!(result.is_ok(), "Initial init-db should succeed");

        // Corrupt the meta file
        let db_path = temp_dir.path().join("default-db");
        let meta_path = db_path.join(META_FILE);
        let mut file = File::create(&meta_path).unwrap();
        file.write_all(b"this is not valid json").unwrap();

        // Try to run init-db again
        clear_tool_home_cache(); // Clear cache for second run
        let result = run_init_db(&config);
        let err = result.unwrap_err();

        // Exact match on error message
        assert_eq!(err.to_string(), err_partial_state(&db_path));

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_schema_version_mismatch() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // First, create a valid database
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        let config = load_config(&config_path).expect("Config should load");
        let result = run_init_db(&config);
        assert!(result.is_ok(), "Initial init-db should succeed");

        // Modify the meta file to have a different schema version
        let db_path = temp_dir.path().join("default-db");
        let meta_path = db_path.join(META_FILE);
        let mut meta = Database::load_meta(&meta_path).unwrap();
        meta.monodex_schema_version = 99;
        Database::write_meta(&meta_path, &meta).unwrap();

        // Try to run init-db again
        clear_tool_home_cache();
        let result = run_init_db(&config);
        let err = result.unwrap_err();

        // Should get schema mismatch error
        assert!(err.to_string().contains("Schema mismatch"));
        assert!(err.to_string().contains("version 99"));

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_meta_exists_tables_missing() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create database directory with meta file but no tables
        let db_path = temp_dir.path().join("default-db");
        fs::create_dir_all(&db_path).unwrap();
        let meta = MetaFile::new();
        let meta_path = db_path.join(META_FILE);
        Database::write_meta(&meta_path, &meta).unwrap();

        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        let config = load_config(&config_path).expect("Config should load");
        let result = run_init_db(&config);
        let err = result.unwrap_err();

        // Should get partial state error
        assert_eq!(err.to_string(), err_partial_state(&db_path));

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_tables_exist_meta_missing() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create database directory with tables but no meta
        let db_path = temp_dir.path().join("default-db");
        fs::create_dir_all(&db_path).unwrap();
        fs::create_dir_all(db_path.join("chunks.lance")).unwrap();
        fs::create_dir_all(db_path.join("label_metadata.lance")).unwrap();

        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        let config = load_config(&config_path).expect("Config should load");
        let result = run_init_db(&config);
        let err = result.unwrap_err();

        // Should get partial state error
        assert_eq!(err.to_string(), err_partial_state(&db_path));

        remove_monodex_home();
    }
}
