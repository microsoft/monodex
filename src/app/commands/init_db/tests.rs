//! Purpose: Test suite for init-db command behavior.
//! Edit here when: Adding or modifying tests for init-db command.
//! Do not edit here for: init-db implementation (see `run.rs`).

use super::run::*;
use crate::app::commands::test_fixtures::write_minimal_config;
use crate::app::config::load_config;
use crate::paths::Paths;
use std::fs;
use std::io::Write;
use std::path::Path;
use tempfile::TempDir;

use crate::engine::storage::{Database, META_FILE, MetaFile};

/// Helper to create a config file with a custom database path.
fn write_config_with_db_path(config_path: &Path, db_path: &str) {
    let mut file = std::fs::File::create(config_path).unwrap();
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
fn test_happy_path_creates_database() {
    let temp_dir = TempDir::new().unwrap();

    // Create minimal config
    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    // Load config (simulating main.rs behavior)
    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");

    // Run init-db
    let result = run_init_db(&config, false);

    // Should succeed
    assert!(result.is_ok(), "init-db should succeed: {:?}", result.err());

    // Verify structure
    let db_path = temp_dir.path().join("default-db");
    assert!(db_path.exists(), "Database folder should exist");
    assert!(
        db_path.join(META_FILE).exists(),
        "monodex-meta.json should exist"
    );

    // Verify locks folder was created
    assert!(db_path.join("locks").exists(), "locks folder should exist");

    // Verify fts folder was created
    assert!(db_path.join("fts").exists(), "fts folder should exist");
}

#[test]
fn test_idempotent_second_run_succeeds() {
    let temp_dir = TempDir::new().unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    // Load config (simulating main.rs behavior)
    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");

    // First run
    let result1 = run_init_db(&config, false);
    assert!(result1.is_ok(), "First init-db should succeed");

    // Second run
    let result2 = run_init_db(&config, false);
    assert!(result2.is_ok(), "Second init-db should succeed");

    // Verify database still valid
    let db_path = temp_dir.path().join("default-db");
    assert!(db_path.join(META_FILE).exists());
}

#[test]
fn test_parent_missing_non_default_db() {
    let temp_dir = TempDir::new().unwrap();

    // Use an absolute path whose parent definitely doesn't exist
    let db_path_str = "/nonexistent-xyz-12345/db";
    let config_path = temp_dir.path().join("monodex-config.json");
    write_config_with_db_path(&config_path, db_path_str);

    // Load config (simulating main.rs behavior)
    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");

    let result = run_init_db(&config, false);
    let err = result.unwrap_err();

    // Exact match on error message
    let expected_db_path = std::path::PathBuf::from(db_path_str);
    assert_eq!(err.to_string(), err_parent_missing(&expected_db_path));
}

#[test]
fn test_path_exists_but_not_monodex_database() {
    let temp_dir = TempDir::new().unwrap();

    // Create a folder with a stray file (not a monodex database)
    let db_path = temp_dir.path().join("my-db");
    fs::create_dir_all(&db_path).unwrap();
    std::fs::File::create(db_path.join("stray-file.txt"))
        .unwrap()
        .write_all(b"not a monodex database")
        .unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_config_with_db_path(&config_path, db_path.to_str().unwrap());

    // Load config (simulating main.rs behavior)
    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");

    let result = run_init_db(&config, false);
    let err = result.unwrap_err();

    // Exact match on error message
    assert_eq!(err.to_string(), err_not_monodex_db(&db_path));
}

#[test]
fn test_corrupt_meta_file() {
    let temp_dir = TempDir::new().unwrap();

    // First, create a valid database
    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    // Load config (simulating main.rs behavior)
    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");

    let result = run_init_db(&config, false);
    assert!(result.is_ok(), "Initial init-db should succeed");

    // Corrupt the meta file
    let db_path = temp_dir.path().join("default-db");
    let meta_path = db_path.join(META_FILE);
    let mut file = std::fs::File::create(&meta_path).unwrap();
    file.write_all(b"this is not valid json").unwrap();

    // Try to run init-db again
    let result = run_init_db(&config, false);
    let err = result.unwrap_err();

    // Exact match on error message
    assert_eq!(err.to_string(), err_partial_state(&db_path));
}

#[test]
fn test_schema_version_mismatch() {
    let temp_dir = TempDir::new().unwrap();

    // First, create a valid database
    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");
    let result = run_init_db(&config, false);
    assert!(result.is_ok(), "Initial init-db should succeed");

    // Modify the meta file to have a different schema version
    let db_path = temp_dir.path().join("default-db");
    let meta_path = db_path.join(META_FILE);
    let mut meta = Database::load_meta(&meta_path).unwrap();
    meta.monodex_schema_version = 99;
    Database::write_meta(&meta_path, &meta).unwrap();

    // Try to run init-db again
    let result = run_init_db(&config, false);
    let err = result.unwrap_err();

    // Should get schema mismatch error
    assert!(err.to_string().contains("Schema mismatch"));
    assert!(err.to_string().contains("version 99"));
}

#[test]
fn test_meta_exists_tables_missing() {
    let temp_dir = TempDir::new().unwrap();

    // Create database folder with meta file but no tables
    let db_path = temp_dir.path().join("default-db");
    fs::create_dir_all(&db_path).unwrap();
    let meta = MetaFile::new();
    let meta_path = db_path.join(META_FILE);
    Database::write_meta(&meta_path, &meta).unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");
    let result = run_init_db(&config, false);
    let err = result.unwrap_err();

    // Should get partial state error
    assert_eq!(err.to_string(), err_partial_state(&db_path));
}

#[test]
fn test_tables_exist_meta_missing() {
    let temp_dir = TempDir::new().unwrap();

    // Create database folder with tables but no meta
    let db_path = temp_dir.path().join("default-db");
    fs::create_dir_all(&db_path).unwrap();
    fs::create_dir_all(db_path.join("chunks.lance")).unwrap();
    fs::create_dir_all(db_path.join("label_metadata.lance")).unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");
    let result = run_init_db(&config, false);
    let err = result.unwrap_err();

    // Should get partial state error
    assert_eq!(err.to_string(), err_partial_state(&db_path));
}

#[test]
fn test_empty_directory_with_locks_dir_succeeds() {
    // Test that a folder containing only locks/ is treated as empty
    let temp_dir = TempDir::new().unwrap();

    // Create database folder with only locks/database.lock
    let db_path = temp_dir.path().join("default-db");
    fs::create_dir_all(db_path.join("locks")).unwrap();
    std::fs::File::create(db_path.join("locks/database.lock")).unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");
    let result = run_init_db(&config, false);

    // Should succeed - locks/ is treated as detritus
    assert!(
        result.is_ok(),
        "init-db should succeed with locks/ detritus: {:?}",
        result.err()
    );

    // Verify database was created
    assert!(db_path.join(META_FILE).exists());
}

#[test]
fn test_empty_directory_with_fts_dir_succeeds() {
    // Test that a folder containing only fts/ is treated as empty
    let temp_dir = TempDir::new().unwrap();

    // Create database folder with only fts/
    let db_path = temp_dir.path().join("default-db");
    fs::create_dir_all(db_path.join("fts")).unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");
    let result = run_init_db(&config, false);

    // Should succeed - fts/ is treated as detritus
    assert!(
        result.is_ok(),
        "init-db should succeed with fts/ detritus: {:?}",
        result.err()
    );

    // Verify database was created
    assert!(db_path.join(META_FILE).exists());
}

#[test]
fn test_pre_lock_check_tolerates_missing_meta_with_tables() {
    // Regression test for concurrent init-db race:
    // Pre-lock check should return Ok(None) for "tables exist, meta missing"
    // so the caller falls through to lock acquisition.
    // The strict check (used under lock) should error on this state.

    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("test-db");

    // Create the shape of a mid-init database: chunks.lance exists, no meta yet
    fs::create_dir_all(&db_path).unwrap();
    fs::create_dir_all(db_path.join("chunks.lance")).unwrap();

    // Pre-lock check should return Ok(None) (tolerant of transient state)
    let pre_lock_result = check_existing_database_pre_lock(&db_path);
    assert!(
        pre_lock_result.is_ok(),
        "pre-lock check should not error: {:?}",
        pre_lock_result.err()
    );
    assert!(
        pre_lock_result.unwrap().is_none(),
        "pre-lock check should return None for mid-init state"
    );

    // Strict check should error (partial state is corruption under lock)
    let strict_result = check_existing_database(&db_path);
    assert!(
        strict_result.is_err(),
        "strict check should error on partial state"
    );
    assert_eq!(
        strict_result.unwrap_err().to_string(),
        err_partial_state(&db_path)
    );
}

#[test]
fn test_delete_everything_with_existing_database() {
    // Test that --delete-everything wipes an existing database and recreates it
    let temp_dir = TempDir::new().unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");

    // First, create a valid database
    let result = run_init_db(&config, false);
    assert!(result.is_ok(), "Initial init-db should succeed");

    let db_path = temp_dir.path().join("default-db");
    assert!(db_path.join(META_FILE).exists());

    // Add some extra content to verify deletion
    fs::write(db_path.join("extra-file.txt"), "test content").unwrap();

    // Run init-db --delete-everything
    let result = run_init_db(&config, true);
    assert!(
        result.is_ok(),
        "init-db --delete-everything should succeed: {:?}",
        result.err()
    );

    // Verify database was recreated
    assert!(db_path.join(META_FILE).exists());
    // Verify extra content was deleted
    assert!(
        !db_path.join("extra-file.txt").exists(),
        "Extra file should be deleted"
    );
    // Verify locks folder still exists (not deleted)
    assert!(
        db_path.join("locks").exists(),
        "locks folder should be preserved"
    );
}

#[test]
fn test_delete_everything_with_nonexistent_database() {
    // Test that --delete-everything on a non-existent database prints a note but succeeds
    let temp_dir = TempDir::new().unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");

    // Run init-db --delete-everything on a fresh system
    let result = run_init_db(&config, true);
    assert!(
        result.is_ok(),
        "init-db --delete-everything should succeed on non-existent db: {:?}",
        result.err()
    );

    // Verify database was created
    let db_path = temp_dir.path().join("default-db");
    assert!(db_path.join(META_FILE).exists());
}

#[test]
fn test_delete_everything_with_current_version_database() {
    // Test that --delete-everything works even on a database with the current schema version
    let temp_dir = TempDir::new().unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");

    // Create a valid current-version database
    let result = run_init_db(&config, false);
    assert!(result.is_ok(), "Initial init-db should succeed");

    let db_path = temp_dir.path().join("default-db");

    // Run init-db --delete-everything (delete-and-recreate even though not strictly necessary)
    let result = run_init_db(&config, true);
    assert!(
        result.is_ok(),
        "init-db --delete-everything should succeed: {:?}",
        result.err()
    );

    // Verify database was recreated
    assert!(db_path.join(META_FILE).exists());
}

#[test]
fn test_delete_everything_with_v3_database() {
    // Test that --delete-everything works on a database with an older schema version (v3)
    // This verifies the delete path correctly wipes and recreates even when the schema
    // version doesn't match the current version.
    let temp_dir = TempDir::new().unwrap();

    let config_path = temp_dir.path().join("monodex-config.json");
    write_minimal_config(&config_path);

    let paths = Paths::for_test(temp_dir.path().into());
    let config = load_config(paths).expect("Config should load");

    // Create a database folder with a hand-written v3 meta file
    let db_path = temp_dir.path().join("default-db");
    fs::create_dir_all(&db_path).unwrap();

    // Write a v3 meta file (mock the v3 state without actually building v3 tables)
    let meta_content = r#"{
  "monodex_schema_version": 3
}"#;
    std::fs::write(db_path.join(META_FILE), meta_content).unwrap();

    // Also create minimal table folders so it looks like a real database
    fs::create_dir_all(db_path.join("chunks.lance")).unwrap();
    fs::create_dir_all(db_path.join("label_metadata.lance")).unwrap();

    // Run init-db --delete-everything
    let result = run_init_db(&config, true);
    assert!(
        result.is_ok(),
        "init-db --delete-everything should succeed on v3 database: {:?}",
        result.err()
    );

    // Verify the resulting meta file shows schema version 4 (current version)
    let meta_path = db_path.join(META_FILE);
    assert!(meta_path.exists(), "Meta file should exist");

    let meta_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    assert_eq!(
        meta_json["monodex_schema_version"].as_u64().unwrap(),
        crate::engine::schema::MONODEX_SCHEMA_VERSION as u64,
        "Schema version should be upgraded to current version"
    );
}
