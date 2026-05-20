//! Purpose: Handler for the `purge` command — delete all chunks for a catalog or for the entire database.
//! Edit here when: Modifying purge behavior, scope, or confirmation flow.
//! Do not edit here for: Storage delete operations (see `engine/storage/chunks/storage.rs`, `engine/storage/labels.rs`).

use crate::app::lock_progress::stderr_lock_progress;
use crate::app::number_format::format_count;
use crate::app::{Config, resolve_database_path};
use crate::engine::identifier;
use crate::engine::storage::{
    Database, acquire_catalog_lock, acquire_database_exclusive, acquire_database_shared,
};

/// Run purge command (delete all chunks from a catalog, or the entire database)
pub fn run_purge(
    config: &Config,
    catalog: Option<&str>,
    all: bool,
    _debug: bool,
) -> anyhow::Result<()> {
    // Resolve database path early (before any lock acquisition)
    let db_path = resolve_database_path(config)?;

    // Move error check to synchronous entry point (before lock acquisition)
    // No lock should be taken when arguments are invalid
    if !all && catalog.is_none() {
        return Err(anyhow::anyhow!(
            "Must specify either --catalog <name> or --all"
        ));
    }

    // Branch based on purge type for lock acquisition
    if all {
        // purge --all: acquire DatabaseLockExclusive
        let _guard = acquire_database_exclusive(&db_path, &stderr_lock_progress)?;
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(run_purge_all_async(&db_path))
    } else if let Some(catalog_name) = catalog {
        // purge --catalog X: validate catalog name, then acquire shared + catalog lock
        identifier::validate_catalog(catalog_name)?;

        let _db_guard = acquire_database_shared(&db_path, &stderr_lock_progress)?;
        let _catalog_guard = acquire_catalog_lock(&db_path, catalog_name, &stderr_lock_progress)?;

        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(run_purge_catalog_async(&db_path, catalog_name))
    } else {
        // Already handled above, but for exhaustiveness
        unreachable!("Either --all or --catalog must be specified")
    }
}

async fn run_purge_all_async(db_path: &std::path::Path) -> anyhow::Result<()> {
    let db = Database::open(db_path).await?;
    let chunk_storage = db.chunks_storage().await?;
    let label_storage = db.label_storage().await?;

    println!("🗑️  Purging entire database");

    // Truncate both tables (keeps monodex-meta.json and dataset structure)
    chunk_storage.truncate().await?;
    label_storage.truncate().await?;

    // Delete and recreate FTS folder (always recreate, even if absent)
    let fts_folder = db_path.join("fts");
    if fts_folder.exists() {
        std::fs::remove_dir_all(&fts_folder)?;
    }
    std::fs::create_dir_all(&fts_folder)?;

    println!("✅ Database purged successfully");
    Ok(())
}

async fn run_purge_catalog_async(
    db_path: &std::path::Path,
    catalog_name: &str,
) -> anyhow::Result<()> {
    let db = Database::open(db_path).await?;
    let chunk_storage = db.chunks_storage().await?;
    let label_storage = db.label_storage().await?;

    println!("🗑️  Purging catalog: {}", catalog_name);

    // Delete chunks and label metadata for this catalog
    let chunks_deleted = chunk_storage.delete_by_catalog(catalog_name).await?;
    let labels_deleted = label_storage.delete_by_catalog(catalog_name).await?;

    // Delete FTS folder for this catalog
    let fts_catalog_folder = db_path.join("fts").join(catalog_name);
    if fts_catalog_folder.exists() {
        std::fs::remove_dir_all(&fts_catalog_folder)?;
    }

    println!(
        "✅ Catalog purged successfully ({} chunks, {} labels deleted)",
        format_count(chunks_deleted),
        format_count(labels_deleted)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::storage::{Database as StorageDatabase, META_FILE};
    use tempfile::TempDir;

    use crate::app::commands::test_fixtures::{
        create_test_db_with_chunks, test_chunk_row_with_catalog,
        test_label_metadata_row_with_parts, write_minimal_config,
    };
    use crate::paths::Paths;

    #[test]
    fn test_purge_missing_database() {
        let temp_dir = TempDir::new().unwrap();

        // Create config but no database
        let config_path = temp_dir.path().join("monodex-config.json");
        write_minimal_config(&config_path);

        let paths = Paths::for_test(temp_dir.path().into());
        let config = crate::app::config::load_config(paths).unwrap();
        let result = run_purge(&config, Some("test-catalog"), false, false);

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No monodex database"),
            "Error should mention missing database: {}",
            err
        );
        assert!(
            err.contains("init-db"),
            "Error should mention init-db: {}",
            err
        );
    }

    #[test]
    fn test_purge_neither_catalog_nor_all() {
        let temp_dir = TempDir::new().unwrap();

        // Create config
        let config_path = temp_dir.path().join("monodex-config.json");
        write_minimal_config(&config_path);

        // Create database
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(&db_path, vec![], vec![]).await;
        });

        let paths = Paths::for_test(temp_dir.path().into());
        let config = crate::app::config::load_config(paths).unwrap();
        let result = run_purge(&config, None, false, false);

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Must specify either --catalog"),
            "Error should mention missing options: {}",
            err
        );
    }

    #[test]
    fn test_purge_all_truncates_tables() {
        let temp_dir = TempDir::new().unwrap();

        // Create config
        let config_path = temp_dir.path().join("monodex-config.json");
        write_minimal_config(&config_path);

        // Create database with chunks and labels
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(
                &db_path,
                vec![
                    test_chunk_row_with_catalog("file1:1", "file1", 1, "catalog1", "catalog1:main"),
                    test_chunk_row_with_catalog("file2:1", "file2", 1, "catalog2", "catalog2:main"),
                ],
                vec![
                    test_label_metadata_row_with_parts("catalog1", "main"),
                    test_label_metadata_row_with_parts("catalog2", "main"),
                ],
            )
            .await;
        });

        let paths = Paths::for_test(temp_dir.path().into());
        let config = crate::app::config::load_config(paths).unwrap();
        let result = run_purge(&config, None, true, false);

        assert!(
            result.is_ok(),
            "purge --all should succeed: {:?}",
            result.err()
        );

        // Verify tables are empty
        rt.block_on(async {
            let db = StorageDatabase::open(&db_path).await.unwrap();
            let chunk_storage = db.chunks_storage().await.unwrap();
            let label_storage = db.label_storage().await.unwrap();

            let chunk_count = chunk_storage.table().count_rows(None).await.unwrap();
            let label_count = label_storage.table().count_rows(None).await.unwrap();

            assert_eq!(chunk_count, 0, "Chunks table should be empty");
            assert_eq!(label_count, 0, "Labels table should be empty");
        });

        // Verify meta file still exists
        assert!(
            db_path.join(META_FILE).exists(),
            "Meta file should still exist"
        );
    }

    #[test]
    fn test_purge_catalog_deletes_only_that_catalog() {
        let temp_dir = TempDir::new().unwrap();

        // Create config
        let config_path = temp_dir.path().join("monodex-config.json");
        write_minimal_config(&config_path);

        // Create database with chunks from multiple catalogs
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(
                &db_path,
                vec![
                    test_chunk_row_with_catalog("file1:1", "file1", 1, "catalog1", "catalog1:main"),
                    test_chunk_row_with_catalog("file2:1", "file2", 1, "catalog1", "catalog1:main"),
                    test_chunk_row_with_catalog("file3:1", "file3", 1, "catalog2", "catalog2:main"),
                ],
                vec![
                    test_label_metadata_row_with_parts("catalog1", "main"),
                    test_label_metadata_row_with_parts("catalog2", "main"),
                ],
            )
            .await;
        });

        let paths = Paths::for_test(temp_dir.path().into());
        let config = crate::app::config::load_config(paths).unwrap();
        let result = run_purge(&config, Some("catalog1"), false, false);

        assert!(
            result.is_ok(),
            "purge catalog1 should succeed: {:?}",
            result.err()
        );

        // Verify only catalog1 was deleted
        rt.block_on(async {
            let db = StorageDatabase::open(&db_path).await.unwrap();
            let chunk_storage = db.chunks_storage().await.unwrap();
            let label_storage = db.label_storage().await.unwrap();

            let chunk_count = chunk_storage.table().count_rows(None).await.unwrap();
            let label_count = label_storage.table().count_rows(None).await.unwrap();

            assert_eq!(chunk_count, 1, "Only catalog2 chunks should remain");
            assert_eq!(label_count, 1, "Only catalog2 label should remain");
        });
    }

    #[test]
    fn test_purge_nonexistent_catalog_succeeds() {
        let temp_dir = TempDir::new().unwrap();

        // Create config
        let config_path = temp_dir.path().join("monodex-config.json");
        write_minimal_config(&config_path);

        // Create database with chunks
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(
                &db_path,
                vec![test_chunk_row_with_catalog(
                    "file1:1",
                    "file1",
                    1,
                    "catalog1",
                    "catalog1:main",
                )],
                vec![test_label_metadata_row_with_parts("catalog1", "main")],
            )
            .await;
        });

        let paths = Paths::for_test(temp_dir.path().into());
        let config = crate::app::config::load_config(paths).unwrap();
        let result = run_purge(&config, Some("nonexistent-catalog"), false, false);

        // Should succeed (deletes 0 rows)
        assert!(
            result.is_ok(),
            "purge nonexistent catalog should succeed: {:?}",
            result.err()
        );

        // Verify original data is still there
        rt.block_on(async {
            let db = StorageDatabase::open(&db_path).await.unwrap();
            let chunk_storage = db.chunks_storage().await.unwrap();

            let chunk_count = chunk_storage.table().count_rows(None).await.unwrap();
            assert_eq!(chunk_count, 1, "Original chunks should still exist");
        });
    }
}
