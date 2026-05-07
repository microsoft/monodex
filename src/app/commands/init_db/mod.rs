//! Database initialization command.
//!
//! Purpose: Create a new monodex database directory with LanceDB tables.
//! Edit here when: Changing init-db behavior, error messages, or initialization logic.
//! Do not edit here for: Database open logic (see engine/storage/database.rs),
//!   schema definitions (see engine/schema.rs), config loading (app/config.rs).

use anyhow::{Result, anyhow, bail};
use std::fs;
use std::path::Path;

use crate::app::config::{Config, resolve_database_path};
use crate::app::util::stderr_lock_progress;
use crate::engine::schema::{
    CHUNKS_TABLE, LABEL_METADATA_TABLE, chunks_schema, label_metadata_schema,
};
use crate::engine::storage::{
    Database, META_FILE, MetaFile, acquire_database_exclusive, err_schema_mismatch,
};
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
// Command entry point
// ============================================================================

/// Run the init-db command.
///
/// Creates a new monodex database at the configured path, or verifies an existing
/// database is valid. The database contains LanceDB tables for chunks and label metadata.
///
/// Note: Config must be loaded by the caller (main.rs) and passed in.
/// This ensures the --config flag is respected uniformly across all commands.
pub fn run_init_db(config: &Config, delete_everything: bool) -> Result<()> {
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
        fs::create_dir_all(&db_path)?;
    } else if !db_path.exists() {
        // Custom path: parent must exist (validated above), create only db_path
        // Use create_dir_all so concurrent init-db invocations both succeed
        fs::create_dir_all(&db_path)?;
    }

    // Step 4: Handle --delete-everything flag (separate branch with lock held through create)
    // This branch acquires the lock, deletes if needed, creates the database, and returns.
    // The lock guard stays in scope through create_database, preventing concurrent races.
    if delete_everything {
        let _guard = acquire_database_exclusive(&db_path, &stderr_lock_progress)?;

        if db_path.exists() {
            // Check if there's anything to delete
            let has_content = db_path
                .read_dir()
                .map(|mut entries| {
                    entries.any(|e| {
                        e.ok()
                            .map(|e| {
                                let name = e.file_name();
                                // Ignore locks directory - we hold a lock under it
                                name != "locks"
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

            if has_content {
                println!("Deleted contents of {}; reinitializing.", db_path.display());
                delete_database_contents(&db_path)?;
            } else {
                println!("Note: --delete-everything specified but no existing database to delete.");
            }
        }

        // Create the database while still holding the lock
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| anyhow!("Failed to create tokio runtime: {}", e))?;
        rt.block_on(create_database(&db_path))?;

        println!("Created monodex database at {}", db_path.display());
        return Ok(());
    }

    // Non-delete path: existing logic unchanged.

    // Step 5: Pre-lock existence check (short-circuit if already initialized)
    // Use the tolerant check that falls through to lock acquisition for transient mid-init states
    if let Some(meta) = check_existing_database_pre_lock(&db_path)? {
        println!(
            "{}",
            log_already_initialized(&db_path, meta.monodex_schema_version)
        );
        return Ok(());
    }

    // Step 6: Acquire exclusive database lock
    // The lock module creates <db>/locks/ and the lockfile lazily
    let _guard = acquire_database_exclusive(&db_path, &stderr_lock_progress)?;

    // Step 7: Under-lock recheck (double-checked init pattern)
    if let Some(meta) = check_existing_database(&db_path)? {
        println!(
            "{}",
            log_already_initialized(&db_path, meta.monodex_schema_version)
        );
        return Ok(());
    }

    // Step 8: Create the database
    // Note: create_empty_table calls do not acquire the commit mutex.
    // The database is being initialized for the first time under DatabaseLockExclusive,
    // with no possible concurrent writer; the commit mutex would be ceremonial.
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| anyhow!("Failed to create tokio runtime: {}", e))?;
    rt.block_on(create_database(&db_path))?;

    println!("Created monodex database at {}", db_path.display());
    Ok(())
}

/// Pre-lock existence check for init-db.
///
/// Returns Some(MetaFile) if a fully-valid initialized database exists.
/// Returns None if the path doesn't exist, is empty (ignoring lockfile/locks detritus),
/// or is in a transient mid-init state (tables exist but meta missing).
/// Returns error for terminal conditions that no concurrent writer could resolve:
/// corrupt meta, schema mismatch, meta present but tables missing, or non-empty non-monodex directory.
///
/// The pre-lock check is tolerant of the "tables exist, meta missing" state because another
/// process might be mid-init. The caller should acquire the exclusive lock and recheck with
/// the strict `check_existing_database`.
fn check_existing_database_pre_lock(db_path: &Path) -> Result<Option<MetaFile>> {
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
                // Corrupted meta file - terminal error
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
        // Pre-lock: this might be a concurrent init in progress, so fall through to lock
        if chunks_path.exists() || labels_path.exists() {
            return Ok(None);
        }

        // Check if directory is empty (ignoring lockfile, locks/, and fts/ detritus)
        let is_empty = db_path
            .read_dir()
            .map(|mut entries| {
                entries.all(|e| {
                    e.ok()
                        .map(|e| {
                            let name = e.file_name();
                            name == ".monodex.lock" || name == "locks" || name == "fts"
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        if is_empty {
            // Empty directory (or only lockfile/locks/fts), treat as non-existent
            Ok(None)
        } else {
            // Non-empty without meta file or tables
            bail!(err_not_monodex_db(db_path));
        }
    }
}

/// Strict existence check for init-db (used under the exclusive lock).
///
/// Returns Some(MetaFile) if a fully-valid initialized database exists.
/// Returns None if path doesn't exist or is an empty directory (ignoring lockfile/locks detritus).
/// Returns error if path exists but is not a valid database, including partial state.
///
/// This is the strict version used after acquiring the exclusive lock. Under the lock,
/// no other process can be mid-init, so any partial state is a real corruption.
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

        // Check if directory is empty (ignoring lockfile, locks/, and fts/ detritus)
        let is_empty = db_path
            .read_dir()
            .map(|mut entries| {
                entries.all(|e| {
                    e.ok()
                        .map(|e| {
                            let name = e.file_name();
                            name == ".monodex.lock" || name == "locks" || name == "fts"
                        })
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        if is_empty {
            // Empty directory (or only lockfile/locks/fts), treat as non-existent
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
        // default-db: tool_home will be created by run_init_db
        return Ok(());
    }

    if let Some(parent) = db_path.parent()
        && !parent.exists()
    {
        bail!(err_parent_missing(db_path));
    }

    Ok(())
}

/// Delete all contents of the database directory except the locks/ subdirectory.
///
/// This is used by `init-db --delete-everything` to wipe the database clean
/// while still holding the lock under locks/.
fn delete_database_contents(db_path: &Path) -> Result<()> {
    let entries: Vec<_> = db_path
        .read_dir()
        .map_err(|e| anyhow!("Failed to read database directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            // Don't delete the locks directory - we hold a lock under it
            name != "locks"
        })
        .collect();

    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path)
                .map_err(|e| anyhow!("Failed to remove directory {}: {}", path.display(), e))?;
        } else {
            fs::remove_file(&path)
                .map_err(|e| anyhow!("Failed to remove file {}: {}", path.display(), e))?;
        }
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

    // Create fts directory for Tantivy indexes (populated lazily per label)
    let fts_dir = db_path.join("fts");
    std::fs::create_dir_all(&fts_dir)
        .map_err(|e| anyhow!("Failed to create fts directory: {}", e))?;

    // Write meta file using shared implementation (with fsync)
    let meta = MetaFile::new();
    let meta_path = db_path.join(META_FILE);
    Database::write_meta(&meta_path, &meta)?;

    Ok(())
}

#[cfg(test)]
mod tests;
