//! Handler for the `purge` command.
//!
//! Edit here when: Modifying purge behavior (delete catalog or entire database).
//! Do not edit here for: Storage delete operations (see `engine/storage/chunks.rs`, `engine/storage/labels.rs`).

use crate::app::{Config, resolve_database_path};
use crate::engine::storage::Database;

/// Run purge command (delete all chunks from a catalog, or the entire database)
pub fn run_purge(
    config: &Config,
    catalog: Option<&str>,
    all: bool,
    _debug: bool,
) -> anyhow::Result<()> {
    // Open database (handshake validates monodex-meta.json)
    let db_path = resolve_database_path(Some(config))?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_purge_async(&db_path, catalog, all))
}

async fn run_purge_async(
    db_path: &std::path::Path,
    catalog: Option<&str>,
    all: bool,
) -> anyhow::Result<()> {
    let db = Database::open(db_path).await?;
    let chunk_storage = db.chunks_storage().await?;
    let label_storage = db.label_storage().await?;

    if all {
        println!("🗑️  Purging entire database");

        // Truncate both tables (keeps monodex-meta.json and dataset structure)
        chunk_storage.truncate().await?;
        label_storage.truncate().await?;

        println!("✅ Database purged successfully");
    } else if let Some(catalog_name) = catalog {
        println!("🗑️  Purging catalog: {}", catalog_name);

        // Delete chunks and label metadata for this catalog
        let chunks_deleted = chunk_storage.delete_by_catalog(catalog_name).await?;
        let labels_deleted = label_storage.delete_by_catalog(catalog_name).await?;

        println!(
            "✅ Catalog purged successfully ({} chunks, {} labels deleted)",
            chunks_deleted, labels_deleted
        );
    } else {
        return Err(anyhow::anyhow!(
            "Must specify either --catalog <name> or --all"
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::storage::{Database as StorageDatabase, META_FILE};
    use crate::paths::clear_tool_home_cache;
    use serial_test::serial;
    use tempfile::TempDir;

    use crate::app::commands::test_helpers::{
        create_test_db_with_chunks, remove_monodex_home, set_monodex_home,
        test_chunk_row_with_catalog, test_label_metadata_row_with_parts, write_minimal_config,
    };

    #[test]
    #[serial(monodex_home)]
    fn test_purge_missing_database() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config but no database
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        let config = crate::app::config::load_config(&config_path).unwrap();
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

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_purge_neither_catalog_nor_all() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Create database
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(&db_path, vec![], vec![]).await;
        });

        let config = crate::app::config::load_config(&config_path).unwrap();
        let result = run_purge(&config, None, false, false);

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("Must specify either --catalog"),
            "Error should mention missing options: {}",
            err
        );

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_purge_all_truncates_tables() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
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

        let config = crate::app::config::load_config(&config_path).unwrap();
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

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_purge_catalog_deletes_only_that_catalog() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
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

        let config = crate::app::config::load_config(&config_path).unwrap();
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

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_purge_nonexistent_catalog_succeeds() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
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

        let config = crate::app::config::load_config(&config_path).unwrap();
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

        remove_monodex_home();
    }
}
