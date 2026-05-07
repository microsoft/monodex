//! Purpose: Handler for the `search` command — dispatch to retrieval methods, format results.
//! Edit here when: Modifying search output, result formatting, or the `>`-prefixed line shape.
//! Do not edit here for: Vector search logic (see `engine/storage/chunks/mod.rs`), embedding (see `engine/parallel_embedder.rs`), FTS search (see `engine/fts/search.rs`).

use crate::app::{
    Config, format_chunk_report, format_source_pointer, resolve_database_path,
    resolve_label_context,
};
use crate::engine::{
    ParallelEmbedder, RetrievalMethod,
    fts::{FtsSearchOutcome, fts_search},
    storage::Database,
};
use anyhow::anyhow;
use std::collections::BTreeSet;

pub fn run_search(
    config: &Config,
    text: &str,
    limit: usize,
    label: Option<&str>,
    catalog: Option<&str>,
    retrieval: Option<BTreeSet<RetrievalMethod>>,
    _debug: bool,
) -> anyhow::Result<()> {
    // Resolve label context from explicit flags or default context
    let (label_id, catalog_name, label) = resolve_label_context(label, catalog)?;

    // Open database (handshake validates monodex-meta.json)
    let db_path = resolve_database_path(Some(config))?;
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let db = Database::open(&db_path).await?;
        let label_storage = db.label_storage().await?;

        // Step 1: Read label metadata to get selection
        let label_metadata = label_storage
            .get_by_label_id(&label_id)
            .await?
            .ok_or_else(|| {
                anyhow!(
                    "Label {}/{} has no crawl metadata. Run `monodex crawl --catalog {} --label {} --commit <commit>` to create it.",
                    catalog_name, label, catalog_name, label
                )
            })?;

        // Step 2: Compute persistent selection
        let persistent_selection = crate::engine::storage::read_selection(&label_metadata);

        // Step 3: Compute requested methods (explicit flags or all in selection)
        let explicit_retrieval = retrieval.is_some();
        let requested_methods = retrieval.unwrap_or_else(|| persistent_selection.clone());

        // Step 4: Validate requested methods are in selection
        for method in &requested_methods {
            if !persistent_selection.contains(method) {
                let source_pointer = format_source_pointer(&label_metadata);
                return Err(anyhow!(
                    "Method {} is not in this label's retrieval selection. Re-run `monodex crawl --label {} {} --retrieval {}` to add it.",
                    method, label, source_pointer, method
                ));
            }
        }

        // Step 5: Compute active subset (filter out incomplete methods with warning)
        let mut active_subset = BTreeSet::new();
        for method in &requested_methods {
            let (source, complete) = match method {
                RetrievalMethod::Vector => {
                    (&label_metadata.vector_source, label_metadata.vector_complete)
                }
                RetrievalMethod::Fts => (&label_metadata.fts_source, label_metadata.fts_complete),
            };

            if source.is_none() {
                // Not in selection - should have been caught by validation above
                continue;
            }

            if !complete {
                // Emit yellow warning
                let source_pointer = format_source_pointer(&label_metadata);
                eprintln!(
                    "⚠️  {} indexing on this label is incomplete; results may be missing entries indexed since the last successful crawl.",
                    method
                );
                eprintln!(
                    "   To complete: monodex crawl --label {} {} --retrieval {}",
                    label, source_pointer, method
                );
                // If the user explicitly requested this method via --retrieval, proceed anyway
                if explicit_retrieval {
                    active_subset.insert(*method);
                }
            } else {
                active_subset.insert(*method);
            }
        }

        // Step 6: Print preamble before any decision-logic returns
        // This makes the retrieval-selection concept legible even when errors follow
        let searching_display = format_active_subset(&active_subset);
        println!(
            "Catalog: {} / Label: {} / Searching: {}",
            catalog_name, label, searching_display
        );
        println!();

        // Step 7: Apply search decision rules
        if active_subset.is_empty() {
            if persistent_selection.is_empty() {
                return Err(anyhow!(
                    "This label has no retrieval methods in its selection. Re-run `monodex crawl` to populate it."
                ));
            } else {
                let source_pointer = format_source_pointer(&label_metadata);
                return Err(anyhow!(
                    "All retrieval methods in this label's selection are incomplete (vector_complete = false, fts_complete = false).\nRe-run `monodex crawl --label {} {}` to complete indexing.",
                    label, source_pointer
                ));
            }
        }

        // Check for source disagreement (defensive - shouldn't happen through normal CLI flows)
        if active_subset.len() >= 2 {
            let vector_source = label_metadata.vector_source.as_ref();
            let fts_source = label_metadata.fts_source.as_ref();

            if let (Some(vs), Some(fs)) = (vector_source, fts_source)
                && vs != fs
            {
                let source_pointer = format_source_pointer(&label_metadata);
                return Err(anyhow!(
                    "This label's retrieval methods have inconsistent source state:\n  vector indexed against: {}\n  fts indexed against: {}\nRe-run `monodex crawl --label {} {}` to bring them back in sync.",
                    vs, fs, label, source_pointer
                ));
            }

            // PR1 stub error for multi-method search
            return Err(anyhow!(
                "Hybrid search across multiple retrieval methods is not yet implemented in this version.\nRe-run with --retrieval to pick a single method:\n  monodex search --text ... --retrieval fts\n  monodex search --text ... --retrieval vector"
            ));
        }

        // Step 8: Dispatch to single method
        let method = active_subset.iter().next().unwrap();

        match method {
            RetrievalMethod::Vector => {
                run_vector_search(&db, text, limit, &label_id).await?;
            }
            RetrievalMethod::Fts => {
                run_fts_search(&db_path, &db, text, limit, &label_id, label_metadata.fts_complete).await?;
            }
        }

        Ok(())
    })
}

/// Format the active subset for the Searching: line.
///
/// Returns "fts only", "vector only", or "fts, vector" for multi-method.
/// The preamble is printed before decision-logic returns, so multi-method
/// cases (stub error in PR1) still show the preamble.
fn format_active_subset(active_subset: &BTreeSet<RetrievalMethod>) -> String {
    if active_subset.len() == 1 {
        let method = active_subset.iter().next().unwrap();
        match method {
            RetrievalMethod::Fts => "fts only".to_string(),
            RetrievalMethod::Vector => "vector only".to_string(),
        }
    } else {
        // Multi-method case - will be used by PR2 for RRF
        // Format as "fts + vector (RRF)" when that lands
        crate::engine::retrieval::format_selection(active_subset)
    }
}

/// Run vector search and display results.
async fn run_vector_search(
    db: &Database,
    text: &str,
    limit: usize,
    label_id: &str,
) -> anyhow::Result<()> {
    let chunk_storage = db.chunks_storage().await?;

    // Initialize embedder (only when vector is selected)
    let embedder = ParallelEmbedder::new()?;
    let embedding = embedder.encode(text, 0)?;

    // Query LanceDB with label filter
    let results = chunk_storage
        .vector_search(&embedding, label_id, limit)
        .await?;

    if results.is_empty() {
        println!("No results.");
        println!();
        return Ok(());
    }

    // Display results as blurbs
    for result in &results {
        let chunk = &result.chunk;

        // Line 1: file_id:chunk_ordinal  distance  breadcrumb [chunk_kind] (part N/M)
        let report = format_chunk_report(
            chunk.breadcrumb.as_deref(),
            chunk.split_part_ordinal.zip(chunk.split_part_count),
            &chunk.chunk_kind,
        );

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

/// Run FTS search and display results.
async fn run_fts_search(
    db_path: &std::path::Path,
    db: &Database,
    text: &str,
    limit: usize,
    label_id: &str,
    fts_complete: bool,
) -> anyhow::Result<()> {
    use crate::engine::identifier::LabelId;

    // Parse label_id into LabelId
    let parts: Vec<&str> = label_id.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(anyhow!("Invalid label_id format: {}", label_id));
    }
    let label_id_struct = LabelId::new(parts[0], parts[1])?;

    // Run FTS search
    let outcome = fts_search(db_path, &label_id_struct, text, limit).await?;

    match outcome {
        FtsSearchOutcome::Found(hits) => {
            if hits.is_empty() {
                println!("No results.");
                println!();
                return Ok(());
            }

            // Hydrate chunks from LanceDB
            let chunk_storage = db.chunks_storage().await?;
            let row_ids: Vec<String> = hits.iter().map(|h| h.row_id.clone()).collect();
            let chunks = chunk_storage
                .get_chunks_by_row_ids_for_label(label_id, &row_ids)
                .await?;

            // Build lookup map
            let chunk_map: std::collections::HashMap<String, _> =
                chunks.into_iter().map(|c| (c.row_id.clone(), c)).collect();

            // Display results in BM25 order
            for hit in &hits {
                match chunk_map.get(&hit.row_id) {
                    Some(chunk) => {
                        // Line 1: file_id:chunk_ordinal  score  breadcrumb [chunk_kind] (part N/M)
                        let report = format_chunk_report(
                            chunk.breadcrumb.as_deref(),
                            chunk.split_part_ordinal.zip(chunk.split_part_count),
                            &chunk.chunk_kind,
                        );

                        println!(
                            "{}:{}  score={:.3}  {}",
                            chunk.file_id, chunk.chunk_ordinal, hit.score, report
                        );

                        // Lines 2-4: first 3 lines of code (quoted with >)
                        for line in chunk.text.lines().take(3) {
                            println!("> {}", line);
                        }

                        // Blank line between results
                        println!();
                    }
                    None => {
                        // Stale FTS state - chunk was deleted but FTS hasn't caught up
                        eprintln!(
                            "⚠️  Chunk {} in FTS index but not in LanceDB (stale state), skipping",
                            hit.row_id
                        );
                    }
                }
            }
        }
        FtsSearchOutcome::NoIndex => {
            if fts_complete {
                // FTS directory was deleted out from under us
                eprintln!(
                    "⚠️  FTS state for label {} is missing on disk; re-crawl with --retrieval fts to rebuild",
                    label_id
                );
            }
            // If !fts_complete, the preprocessing warning already fired
            println!("No results.");
            println!();
        }
        FtsSearchOutcome::ParseError(msg) => {
            return Err(anyhow!("Couldn't parse FTS query: {}", msg));
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
            None,
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
        let result = run_search(&config, "test query", 10, None, None, None, false);

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No context set"),
            "Error should mention missing context: {}",
            err
        );

        remove_monodex_home();
    }

    #[test]
    fn test_format_active_subset() {
        let mut selection = BTreeSet::new();
        // Empty case (shouldn't happen in practice but test the helper)
        assert_eq!(format_active_subset(&selection), "no retrieval methods");

        selection.insert(RetrievalMethod::Fts);
        assert_eq!(format_active_subset(&selection), "fts only");

        selection.clear();
        selection.insert(RetrievalMethod::Vector);
        assert_eq!(format_active_subset(&selection), "vector only");

        selection.insert(RetrievalMethod::Fts);
        // Multi-method case (PR2 will use RRF format)
        assert_eq!(format_active_subset(&selection), "fts, vector");
    }
}
