//! Handler for the `search` command.
//!
//! Edit here when: Modifying search output or result formatting.
//! Do not edit here for: Vector search logic (see `engine/storage/chunks.rs`).

use crate::app::{Config, resolve_database_path, resolve_label_context, sanitize_for_terminal};
use crate::engine::{ParallelEmbedder, storage::Database};

pub fn run_search(
    config: &Config,
    text: &str,
    limit: usize,
    label: Option<&str>,
    catalog: Option<&str>,
    _debug: bool,
) -> anyhow::Result<()> {
    // Resolve label context from explicit flags or default context
    let (label_id, catalog_name, label) = resolve_label_context(label, catalog)?;

    // Open database (handshake validates monodex-meta.json)
    let db_path = resolve_database_path(Some(config))?;
    let rt = tokio::runtime::Runtime::new()?;
    let results = rt.block_on(async {
        let db = Database::open(&db_path).await?;
        let chunk_storage = db.chunks_storage().await?;

        // Generate embedding for query
        let embedder = ParallelEmbedder::new()?;
        let embedding = embedder.encode(text, 0)?;

        // Query LanceDB with label filter
        chunk_storage
            .vector_search(&embedding, label_id.as_str(), limit)
            .await
    })?;

    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label);
    println!();

    // Display results as blurbs
    for result in &results {
        let chunk = &result.chunk;

        // Line 1: file_id:chunk_ordinal  distance  breadcrumb [chunk_kind] (part N/M)
        // E.1: Sanitize breadcrumb to prevent terminal injection
        let breadcrumb = sanitize_for_terminal(chunk.breadcrumb.as_deref().unwrap_or("unknown"));

        // Build the report form with chunk_kind and split metadata
        let mut report = breadcrumb.clone();
        if let (Some(ordinal), Some(count)) = (chunk.split_part_ordinal, chunk.split_part_count) {
            report = format!("{} (part {}/{})", report, ordinal, count);
        }
        if chunk.chunk_kind != "content" {
            report = format!("{} [{}]", report, chunk.chunk_kind);
        }

        println!(
            "{}:{}  dist={:.3}  {}",
            chunk.file_id, chunk.chunk_ordinal, result.distance, report
        );

        // Lines 2-4: first 3 lines of code (quoted with >)
        for line in chunk.text.lines().take(3) {
            println!("> {}", line);
        }

        // Blank line between results
        println!();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::clear_tool_home_cache;
    use serial_test::serial;
    use tempfile::TempDir;

    use crate::app::commands::test_helpers::{
        create_test_db_with_chunks, remove_monodex_home, set_monodex_home, test_chunk_row,
        test_label_metadata_row, write_minimal_config,
    };

    #[test]
    #[serial(monodex_home)]
    fn test_search_missing_database() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config but no database
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        let config = crate::app::config::load_config(&config_path).unwrap();
        let result = run_search(
            &config,
            "test query",
            10,
            Some("main"),
            Some("test-catalog"),
            false,
        );

        let err = result.unwrap_err().to_string();
        // Should mention missing database and init-db
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
    fn test_search_missing_label_context() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Create database with chunks (use valid hex file IDs)
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(
                &db_path,
                vec![test_chunk_row(
                    "aaaabbbbcccc1111:1",
                    "aaaabbbbcccc1111",
                    1,
                    "test-catalog:main",
                )],
                vec![test_label_metadata_row("test-catalog:main")],
            )
            .await;
        });

        let config = crate::app::config::load_config(&config_path).unwrap();

        // Search without providing catalog or label, and no default context
        let result = run_search(&config, "test query", 10, None, None, false);

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No context set"),
            "Error should mention missing context: {}",
            err
        );

        remove_monodex_home();
    }
}
