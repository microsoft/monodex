//! Handler for the `view` command.
//!
//! Edit here when: Modifying view output or chunk selector parsing.
//! Do not edit here for: Chunk retrieval (see `engine/storage/chunks.rs`).

use std::collections::HashSet;

use crate::app::{Config, resolve_database_path, resolve_label_context, sanitize_for_terminal};
use crate::engine::storage::{ChunkRow, Database};

/// Parsed selector for file-based chunk queries
#[derive(Debug, Clone)]
enum ChunkSelector {
    /// All chunks in the file
    All,
    /// Single chunk at position N (1-indexed)
    Single(usize),
    /// Range from start to end (inclusive, 1-indexed)
    Range(usize, usize),
    /// Range from start to the end of file
    ToEnd(usize),
}

/// Parse file ID with optional selector
///
/// Formats:
/// - `700a4ba232fe9ddc` - all chunks in file
/// - `700a4ba232fe9ddc:3` - chunk 3
/// - `700a4ba232fe9ddc:2-3` - chunks 2 through 3
/// - `700a4ba232fe9ddc:3-end` - chunk 3 through the last chunk
fn parse_file_id_with_selector(s: &str) -> anyhow::Result<(String, ChunkSelector)> {
    let s = s.trim();

    // Check for selector suffix
    if let Some(colon_pos) = s.find(':') {
        let file_id = s[..colon_pos].to_string();
        let selector = &s[colon_pos + 1..];

        // Validate file_id is 16 hex chars
        if file_id.len() != 16 || !file_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                file_id
            ));
        }

        // Parse selector
        if selector == "end" {
            // Invalid: ":end" without start
            Err(anyhow::anyhow!(
                "Invalid selector ':end'. Use ':N-end' format."
            ))
        } else if let Some(start_str) = selector.strip_suffix("-end") {
            // :N-end format
            let start: usize = start_str
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if start < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    start
                ));
            }
            Ok((file_id, ChunkSelector::ToEnd(start)))
        } else if selector.contains('-') {
            // :N-M format
            let parts: Vec<&str> = selector.split('-').collect();
            if parts.len() != 2 {
                return Err(anyhow::anyhow!(
                    "Invalid selector '{}'. Expected ':N-M' format.",
                    selector
                ));
            }
            let start: usize = parts[0]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid start chunk in selector '{}'", selector))?;
            let end: usize = parts[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid end chunk in selector '{}'", selector))?;
            if start < 1 || end < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}:{}",
                    start,
                    end
                ));
            }
            if start > end {
                return Err(anyhow::anyhow!("Start chunk {} > end chunk {}", start, end));
            }
            Ok((file_id, ChunkSelector::Range(start, end)))
        } else {
            // :N format (single chunk)
            let chunk_num: usize = selector
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if chunk_num < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    chunk_num
                ));
            }
            Ok((file_id, ChunkSelector::Single(chunk_num)))
        }
    } else {
        // No selector - validate file_id and return All selector
        if s.len() != 16 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                s
            ));
        }
        Ok((s.to_string(), ChunkSelector::All))
    }
}

pub fn run_view(
    config: &Config,
    id_specs: &[String],
    label: Option<&str>,
    catalog: Option<&str>,
    show_full_paths: bool,
    chunks_only: bool,
    _debug: bool,
) -> anyhow::Result<()> {
    if id_specs.is_empty() {
        return Err(anyhow::anyhow!(
            "No IDs provided. Use --id <file_id>[:<selector>]"
        ));
    }

    // Resolve label context from explicit flag or default context
    let (label_id, catalog_name, label) = resolve_label_context(label, catalog)?;

    // Parse all file IDs with selectors
    let mut requests: Vec<(String, ChunkSelector)> = Vec::new();
    for spec in id_specs {
        let (file_id, selector) = parse_file_id_with_selector(spec)?;
        requests.push((file_id, selector));
    }

    // Open database (handshake validates monodex-meta.json)
    let db_path = resolve_database_path(Some(config))?;
    let rt = tokio::runtime::Runtime::new()?;
    let all_results: Vec<(String, ChunkSelector, Vec<ChunkRow>)> = rt.block_on(async {
        let db = Database::open(&db_path).await?;
        let chunk_storage = db.chunks_storage().await?;

        let mut results: Vec<(String, ChunkSelector, Vec<ChunkRow>)> = Vec::new();

        for (file_id, selector) in requests {
            let chunks = chunk_storage
                .get_chunks_by_file_id_with_label(&file_id, label_id.as_str())
                .await?;

            // Filter by selector
            let filtered: Vec<ChunkRow> = match &selector {
                ChunkSelector::All => chunks,
                ChunkSelector::Single(n) => chunks
                    .into_iter()
                    .filter(|c| c.chunk_ordinal as usize == *n)
                    .collect(),
                ChunkSelector::Range(start, end) => chunks
                    .into_iter()
                    .filter(|c| {
                        c.chunk_ordinal as usize >= *start && c.chunk_ordinal as usize <= *end
                    })
                    .collect(),
                ChunkSelector::ToEnd(start) => chunks
                    .into_iter()
                    .filter(|c| c.chunk_ordinal as usize >= *start)
                    .collect(),
            };

            results.push((file_id, selector, filtered));
        }

        anyhow::Ok(results)
    })?;

    if !chunks_only {
        println!("Catalog: {}", catalog_name);
        println!("Label: {}", label);
        println!();
    }

    // Collect unique catalogs for preamble
    if !chunks_only {
        let catalogs: HashSet<&str> = all_results
            .iter()
            .flat_map(|(_, _, results)| results.iter().map(|r| r.catalog.as_str()))
            .collect();

        if !catalogs.is_empty() {
            println!("Catalogs:");
            for cat in catalogs {
                if let Some(cat_config) = config.catalogs.get(cat) {
                    // E.1: Sanitize catalog name and path
                    println!("- {}", sanitize_for_terminal(cat));
                    println!(
                        "  Catalog path: {}",
                        sanitize_for_terminal(&cat_config.path)
                    );
                }
            }
            println!();
        }
    }

    // Display results
    for (file_id, selector, results) in &all_results {
        if results.is_empty() {
            // No chunks found
            let selector_str = match selector {
                ChunkSelector::All => String::new(),
                ChunkSelector::Single(n) => format!(":{}", n),
                ChunkSelector::Range(start, end) => format!(":{}-{}", start, end),
                ChunkSelector::ToEnd(start) => format!(":{}-end", start),
            };
            println!("{}{} ERROR: CHUNK NOT FOUND", file_id, selector_str);
            continue;
        }

        for result in results {
            // E.1: Sanitize output fields to prevent terminal injection
            let breadcrumb =
                sanitize_for_terminal(result.breadcrumb.as_deref().unwrap_or("unknown"));
            let chunk_count = result.chunk_count;
            let chunk_ordinal = result.chunk_ordinal;

            // Build the report form with chunk_kind and split metadata
            let mut report = breadcrumb.clone();
            if let (Some(ordinal), Some(count)) =
                (result.split_part_ordinal, result.split_part_count)
            {
                report = format!("{} (part {}/{})", report, ordinal, count);
            }
            if result.chunk_kind != "content" {
                report = format!("{} [{}]", report, result.chunk_kind);
            }

            // Header line: <file_id>:<chunk_ordinal> (<n>/<total>) <breadcrumb> [kind] (part N/M)
            println!(
                "{}:{} ({}/{}) {}",
                file_id, chunk_ordinal, chunk_ordinal, chunk_count, report
            );

            // Source line (non-grammar format per spec §8.6)
            println!(
                "Source: ({}) {}",
                sanitize_for_terminal(&result.catalog),
                sanitize_for_terminal(&result.relative_path)
            );

            // Full path (optional)
            if show_full_paths {
                println!("Full path: {}", sanitize_for_terminal(&result.source_uri));
            }

            // Lines and type
            println!("Lines: {}-{}", result.start_line, result.end_line);
            println!("Type: {}", sanitize_for_terminal(&result.chunk_type));

            // Content
            println!();
            for line in result.text.lines() {
                println!("> {}", line);
            }

            println!();
        }
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

    // =========================================================================
    // parse_file_id_with_selector tests
    // =========================================================================

    #[test]
    fn test_parse_file_id_all_chunks() {
        let (file_id, selector) = parse_file_id_with_selector("abcd1234efab5678").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::All));
    }

    #[test]
    fn test_parse_file_id_single_chunk() {
        let (file_id, selector) = parse_file_id_with_selector("abcd1234efab5678:3").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::Single(3)));
    }

    #[test]
    fn test_parse_file_id_range() {
        let (file_id, selector) = parse_file_id_with_selector("abcd1234efab5678:2-4").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::Range(2, 4)));
    }

    #[test]
    fn test_parse_file_id_to_end() {
        let (file_id, selector) = parse_file_id_with_selector("abcd1234efab5678:3-end").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::ToEnd(3)));
    }

    #[test]
    fn test_parse_file_id_invalid_file_id() {
        let result = parse_file_id_with_selector("invalid");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid file ID"));
    }

    #[test]
    fn test_parse_file_id_invalid_selector() {
        let result = parse_file_id_with_selector("abcd1234efab5678:abc");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid chunk number")
        );
    }

    #[test]
    fn test_parse_file_id_end_without_start() {
        let result = parse_file_id_with_selector("abcd1234efab5678:end");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid selector ':end'")
        );
    }

    #[test]
    fn test_parse_file_id_zero_chunk_number() {
        let result = parse_file_id_with_selector("abcd1234efab5678:0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("1-indexed"));
    }

    #[test]
    fn test_parse_file_id_reversed_range() {
        let result = parse_file_id_with_selector("abcd1234efab5678:5-2");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Start chunk 5 > end chunk 2")
        );
    }

    // =========================================================================
    // run_view tests
    // =========================================================================

    #[test]
    #[serial(monodex_home)]
    fn test_view_missing_database() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config but no database
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        let config = crate::app::config::load_config(&config_path).unwrap();
        let result = run_view(
            &config,
            &["abcd1234efab5678".to_string()],
            Some("main"),
            Some("test-catalog"),
            false,
            false,
            false,
        );

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
    fn test_view_no_ids_provided() {
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
        let result = run_view(&config, &[], None, None, false, false, false);

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No IDs provided"),
            "Error should mention no IDs: {}",
            err
        );

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_view_chunk_not_found() {
        clear_tool_home_cache();
        let temp_dir = TempDir::new().unwrap();

        set_monodex_home(temp_dir.path());

        // Create config
        let config_path = temp_dir.path().join("config.json");
        write_minimal_config(&config_path);

        // Create database with one chunk using valid hex file IDs
        let db_path = temp_dir.path().join("default-db");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            create_test_db_with_chunks(
                &db_path,
                // Use hex-only file IDs (16 chars)
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

        // View a different file ID that doesn't exist (valid hex, but not in DB)
        let result = run_view(
            &config,
            &["aaaabbbbcccc2222".to_string()],
            Some("main"),
            Some("test-catalog"),
            false,
            false,
            false,
        );

        // Should succeed but output "CHUNK NOT FOUND"
        assert!(
            result.is_ok(),
            "View should succeed even for non-existent chunks: {:?}",
            result.err()
        );

        remove_monodex_home();
    }

    #[test]
    #[serial(monodex_home)]
    fn test_view_missing_label_context() {
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

        // View without providing catalog or label, and no default context
        let result = run_view(
            &config,
            &["aaaabbbbcccc1111".to_string()],
            None,
            None,
            false,
            false,
            false,
        );

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No context set"),
            "Error should mention missing context: {}",
            err
        );

        remove_monodex_home();
    }
}
