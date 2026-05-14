//! Purpose: Handler for the `search` command — resolve label context, dispatch retrieval methods, fuse results, and build the render model.
//! Edit here when: Changing search orchestration, label-context resolution, retrieval dispatch, RRF fusion, or render-model construction.
//! Do not edit here for: Search output formatting or the `>`-prefixed line shape (see `app/search.rs`), vector search logic (see `engine/storage/chunks/mod.rs`), embedding (see `engine/parallel_embedder.rs`), FTS search (see `engine/fts/search.rs`).

use std::io::Write;

use crate::app::{
    Config, format_source_pointer, resolve_database_path, resolve_label_context,
    search::{self, EndMarker, Preamble, SearchRenderModel, SearchWarning},
};
use crate::engine::storage::ChunkRow;
use crate::engine::{
    ParallelConfig, ParallelEmbedder, RetrievalMethod,
    fts::{FtsSearchOutcome, fts_search},
    fusion::{FusedHit, MethodHit, RankedContribution, fuse},
    retrieval::format_selection,
    search_decision::{Decision, decide},
    storage::{Database, ScoredChunkRow},
};
use anyhow::anyhow;
use std::collections::{BTreeSet, HashMap};

// =============================================================================
// Collection types
// =============================================================================

/// The result of collecting hits from a single retrieval method.
///
/// Used by both single-method and hybrid paths.
pub struct CollectedMethod {
    pub method: RetrievalMethod,
    /// Hits in rank order (best first). Position implies rank (1-indexed).
    pub hits: Vec<MethodHit>,
    /// Whether the backend returned at least `limit` candidates.
    /// Used for end-of-results sentinel decision.
    pub saturated: bool,
}

/// Outcome of collecting FTS results, handling FTS-specific error cases.
pub enum FtsCollectOutcome {
    Collected(CollectedMethod),
    NoIndex,
    Stale {
        reason: crate::engine::fts::FtsStaleReason,
    },
    ParseError(String),
}

/// Adapt vector search results to `CollectedMethod`.
///
/// Discards the `ChunkRow` payload from `ScoredChunkRow` (keeping only `row_id`
/// and distance for `MethodHit`). The orchestration layer's bulk hydration step
/// re-fetches chunk data later. This is intentional: it keeps engine-API handling uniform between FTS and vector paths.
pub fn collect_vector(results: Vec<ScoredChunkRow>, limit: usize) -> CollectedMethod {
    let saturated = results.len() >= limit;
    let hits: Vec<MethodHit> = results
        .into_iter()
        .map(|r| MethodHit {
            row_id: r.chunk.row_id,
            backend_score: Some(r.distance),
        })
        .collect();
    CollectedMethod {
        method: RetrievalMethod::Vector,
        hits,
        saturated,
    }
}

/// Adapt FTS search results to `FtsCollectOutcome`.
///
/// Wraps `fts_search` and dispatches on `FtsSearchOutcome`. `Found(hits)` adapts
/// to `CollectedMethod`; `NoIndex` and `ParseError` propagate as their own variants.
pub async fn collect_fts(
    db_path: &std::path::Path,
    label_id: &crate::engine::identifier::LabelId,
    query: &str,
    limit: usize,
) -> anyhow::Result<FtsCollectOutcome> {
    let outcome = fts_search(db_path, label_id, query, limit)?;
    match outcome {
        FtsSearchOutcome::Found(hits) => {
            let saturated = hits.len() >= limit;
            let method_hits: Vec<MethodHit> = hits
                .into_iter()
                .map(|h| MethodHit {
                    row_id: h.row_id,
                    backend_score: Some(h.score),
                })
                .collect();
            Ok(FtsCollectOutcome::Collected(CollectedMethod {
                method: RetrievalMethod::Fts,
                hits: method_hits,
                saturated,
            }))
        }
        FtsSearchOutcome::NoIndex => Ok(FtsCollectOutcome::NoIndex),
        FtsSearchOutcome::Stale { reason } => Ok(FtsCollectOutcome::Stale { reason }),
        FtsSearchOutcome::ParseError(msg) => Ok(FtsCollectOutcome::ParseError(msg)),
    }
}

/// Build a degenerate `FusedHit` for single-method results.
///
/// Single-method results become `FusedHit`s with exactly one contributor.
/// The RRF score is set to 0.0 (unused for single-method ordering).
fn make_single_method_fused_hit(hit: MethodHit, method: RetrievalMethod) -> FusedHit {
    FusedHit {
        row_id: hit.row_id,
        rrf_score: 0.0,
        contributors: vec![RankedContribution {
            method,
            rank: 1, // Rank doesn't matter for single-method display
            backend_score: hit.backend_score,
        }],
    }
}

// =============================================================================
// Main search entry point
// =============================================================================

#[allow(clippy::too_many_arguments)]
pub fn run_search<W: Write>(
    writer: &mut W,
    config: &Config,
    text: &str,
    limit: usize,
    label: Option<&str>,
    catalog: Option<&str>,
    retrieval: Option<BTreeSet<RetrievalMethod>>,
    debug: bool,
) -> anyhow::Result<()> {
    // Resolve label context from explicit flags or default context
    let (label_id, catalog_name, label) = resolve_label_context(&config.paths, label, catalog)?;

    // Open database (handshake validates monodex-meta.json)
    let db_path = resolve_database_path(config)?;
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

        // Step 2: Call decide() to get decision
        let decision = decide(&label_metadata, retrieval.clone());

        // Step 3: Translate decision warnings to search warnings
        let decision_warnings = match &decision {
            Decision::SingleMethod { decision_warnings, .. } => decision_warnings.clone(),
            Decision::Hybrid { decision_warnings, .. } => decision_warnings.clone(),
            Decision::Error(_) => vec![],
        };
        let pre_result_warnings = search::translate_decision_warnings(
            decision_warnings,
            &label_metadata,
        );

        // Step 4: Compute candidate_limit = max(user_limit, 50)
        let candidate_limit = std::cmp::max(limit, 50);

        // Step 5: Build preamble
        let active_methods = match &decision {
            Decision::SingleMethod { method, .. } => {
                let mut set = BTreeSet::new();
                set.insert(*method);
                set
            }
            Decision::Hybrid { methods, .. } => methods.clone(),
            Decision::Error(_) => BTreeSet::new(),
        };
        let searching = format_selection(&active_methods);
        let preamble = Preamble {
            catalog: catalog_name.clone(),
            label: label.to_string(),
            searching,
        };

        // Step 6: Dispatch based on decision
        match decision {
            Decision::Error(err) => {
                // Render preamble first, then error
                let model = SearchRenderModel {
                    preamble,
                    pre_result_warnings: vec![],
                    results: vec![],
                    trailing_inline_warnings: vec![],
                    debug,
                    end_marker: EndMarker::None,
                    mode: search::SearchMode::SingleMethod, // Error path emits no results; default is safe
                };
                search::render(writer, &model)?;

                // Format error message
                let error_msg = format_decision_error(&err, &label_metadata, &label, debug);
                return Err(anyhow!("{}", error_msg));
            }
            Decision::SingleMethod { method, .. } => {
                run_single_method_search(
                    writer,
                    &db,
                    &db_path,
                    text,
                    limit,
                    candidate_limit,
                    &label_id,
                    &label_metadata,
                    method,
                    preamble,
                    pre_result_warnings,
                    debug,
                ).await?;
            }
            Decision::Hybrid { methods, .. } => {
                run_hybrid_search(
                    writer,
                    &db,
                    &db_path,
                    text,
                    limit,
                    candidate_limit,
                    &label_id,
                    &label_metadata,
                    methods,
                    preamble,
                    pre_result_warnings,
                    debug,
                ).await?;
            }
        }

        Ok(())
    })
}

/// Format a decision error into a user-facing error message.
fn format_decision_error(
    err: &crate::engine::search_decision::DecisionError,
    metadata: &crate::engine::storage::LabelMetadataRow,
    label: &str,
    debug: bool,
) -> String {
    use crate::engine::search_decision::DecisionError;
    let source_pointer = format_source_pointer(metadata);

    match err {
        DecisionError::EmptySelection => {
            "This label has no retrieval methods in its selection. Re-run `monodex crawl` to populate it.".to_string()
        }
        DecisionError::AllInSelectionIncomplete { incomplete_methods } => {
            let methods_str: Vec<String> = incomplete_methods.iter().map(|m| format!("{}", m)).collect();
            let base_msg = format!(
                "All retrieval methods in this label's selection ({}) are incomplete.\nRe-run `monodex crawl --label {} {}` to complete indexing.",
                methods_str.join(", "),
                label,
                source_pointer
            );
            if debug {
                // Append schema details in debug mode
                let debug_details: Vec<String> = incomplete_methods
                    .iter()
                    .map(|m| format!("{}_complete = false", m))
                    .collect();
                format!("{} ({})", base_msg, debug_details.join(", "))
            } else {
                base_msg
            }
        }
        DecisionError::SourcesDisagree { vector_source, fts_source } => {
            format!(
                "This label's retrieval methods have inconsistent source state:\n  vector indexed against: {}\n  fts indexed against: {}\nRe-run `monodex crawl --label {} {}` to bring them back in sync.",
                vector_source, fts_source, label, source_pointer
            )
        }
        DecisionError::MethodNotInSelection { method } => {
            format!(
                "Method {} is not in this label's retrieval selection. Re-run `monodex crawl --label {} {} --retrieval {}` to add it.",
                method, label, source_pointer, method
            )
        }
        DecisionError::MethodsNotInSelection { methods } => {
            let methods_str: Vec<String> = methods.iter().map(|m| format!("{}", m)).collect();
            format!(
                "Methods {} are not in this label's retrieval selection. Re-run `monodex crawl --label {} {}` to add them.",
                methods_str.join(", "),
                label,
                source_pointer
            )
        }
    }
}

/// Run a single-method search and render results.
#[allow(clippy::too_many_arguments)]
async fn run_single_method_search<W: Write>(
    writer: &mut W,
    db: &Database,
    db_path: &std::path::Path,
    text: &str,
    limit: usize,
    candidate_limit: usize,
    label_id: &str,
    label_metadata: &crate::engine::storage::LabelMetadataRow,
    method: RetrievalMethod,
    preamble: Preamble,
    pre_result_warnings: Vec<SearchWarning>,
    debug: bool,
) -> anyhow::Result<()> {
    let chunk_storage = db.chunks_storage().await?;

    // Collect from the appropriate backend
    let collected = match method {
        RetrievalMethod::Vector => {
            // Initialize embedder (only when vector is selected)
            // Use single worker for search; let it use all available cores for intra-op parallelism
            let intra_threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            let embedder = ParallelEmbedder::with_config(ParallelConfig {
                num_workers: 1,
                intra_threads,
            })?;
            let embedding = embedder.encode(text, 0)?;

            // Query LanceDB with candidate_limit
            let results = chunk_storage
                .vector_search(&embedding, label_id, candidate_limit)
                .await?;

            collect_vector(results, candidate_limit)
        }
        RetrievalMethod::Fts => {
            // Parse label_id into LabelId
            let parts: Vec<&str> = label_id.splitn(2, ':').collect();
            if parts.len() != 2 {
                return Err(anyhow!("Invalid label_id format: {}", label_id));
            }
            let label_id_struct = crate::engine::identifier::LabelId::new(parts[0], parts[1])?;

            let outcome = collect_fts(db_path, &label_id_struct, text, candidate_limit).await?;

            match outcome {
                FtsCollectOutcome::Collected(collected) => collected,
                FtsCollectOutcome::NoIndex => {
                    // Handle NoIndex case
                    let source_pointer = format_source_pointer(label_metadata);
                    let warning = if label_metadata.fts_complete {
                        // FTS was complete but directory is gone
                        SearchWarning::FtsNoIndexNoFallback {
                            label: preamble.label.clone(),
                            source_pointer,
                        }
                    } else {
                        // Incomplete - warning already emitted via decision_warnings
                        // Just render with no results
                        let model = SearchRenderModel {
                            preamble,
                            pre_result_warnings,
                            results: vec![],
                            trailing_inline_warnings: vec![],
                            debug,
                            end_marker: EndMarker::NoResults,
                            mode: search::SearchMode::SingleMethod,
                        };
                        search::render(writer, &model)?;
                        return Ok(());
                    };

                    // Render with warning and no results
                    let mut warnings = pre_result_warnings;
                    warnings.push(warning);
                    let model = SearchRenderModel {
                        preamble,
                        pre_result_warnings: warnings,
                        results: vec![],
                        trailing_inline_warnings: vec![],
                        debug,
                        end_marker: EndMarker::NoResults,
                        mode: search::SearchMode::SingleMethod,
                    };
                    search::render(writer, &model)?;
                    return Ok(());
                }
                FtsCollectOutcome::Stale { reason } => {
                    // Handle stale FTS index - emit warning and no results
                    use crate::engine::fts::FtsStaleReason;
                    let source_pointer = format_source_pointer(label_metadata);
                    let warning = match reason {
                        FtsStaleReason::IdMismatch | FtsStaleReason::MissingManifestWithState => {
                            SearchWarning::FtsStale {
                                catalog: preamble.catalog.clone(),
                                label: preamble.label.clone(),
                                source_pointer,
                            }
                        }
                        FtsStaleReason::UnreadableManifestWithState => {
                            SearchWarning::FtsManifestUnreadable {
                                catalog: preamble.catalog.clone(),
                                label: preamble.label.clone(),
                            }
                        }
                    };

                    // Render with warning and no results
                    let mut warnings = pre_result_warnings;
                    warnings.push(warning);
                    let model = SearchRenderModel {
                        preamble,
                        pre_result_warnings: warnings,
                        results: vec![],
                        trailing_inline_warnings: vec![],
                        debug,
                        end_marker: EndMarker::NoResults,
                        mode: search::SearchMode::SingleMethod,
                    };
                    search::render(writer, &model)?;
                    return Ok(());
                }
                FtsCollectOutcome::ParseError(msg) => {
                    return Err(anyhow!("Couldn't parse FTS query: {}", msg));
                }
            }
        }
    };

    // Build fused hits from collected results
    let fused_hits: Vec<FusedHit> = collected
        .hits
        .into_iter()
        .map(|hit| make_single_method_fused_hit(hit, method))
        .collect();

    // Hydrate chunks
    let row_ids: Vec<String> = fused_hits.iter().map(|h| h.row_id.clone()).collect();
    let chunks = chunk_storage
        .get_chunks_by_row_ids_for_label(label_id, &row_ids)
        .await?;

    // Build lookup map
    let chunk_map: HashMap<String, ChunkRow> =
        chunks.into_iter().map(|c| (c.row_id.clone(), c)).collect();

    // Hydrate fused hits with chunk data
    let (results, trailing_warnings) = search::hydrate_ranked_hits(fused_hits, &chunk_map, limit);

    // Decide end marker
    let saturations = &[collected.saturated];
    let end_marker = search::decide_end_marker(results.len(), limit, saturations);

    // Build render model
    let model = SearchRenderModel {
        preamble,
        pre_result_warnings,
        results,
        trailing_inline_warnings: trailing_warnings,
        debug,
        end_marker,
        mode: search::SearchMode::SingleMethod,
    };

    // Render
    search::render(writer, &model)?;

    Ok(())
}

/// Run hybrid search across multiple retrieval methods.
///
/// Sequential orchestration: FTS first, then vector.
/// - FTS ParseError: hard error (fail-fast before embedder construction)
/// - FTS NoIndex: degrade to vector-only with warning
/// - Vector failure: hard error
/// - Empty results from one method: fusion proceeds with other method
#[allow(clippy::too_many_arguments)]
async fn run_hybrid_search<W: Write>(
    writer: &mut W,
    db: &Database,
    db_path: &std::path::Path,
    text: &str,
    limit: usize,
    candidate_limit: usize,
    label_id: &str,
    label_metadata: &crate::engine::storage::LabelMetadataRow,
    methods: BTreeSet<RetrievalMethod>,
    preamble: Preamble,
    pre_result_warnings: Vec<SearchWarning>,
    debug: bool,
) -> anyhow::Result<()> {
    let chunk_storage = db.chunks_storage().await?;

    // Step 1: Collect FTS results first (fail-fast for ParseError)
    let mut method_results: Vec<(RetrievalMethod, Vec<MethodHit>)> = Vec::new();
    let mut search_warnings = pre_result_warnings;
    let mut saturations: Vec<bool> = Vec::new();

    if methods.contains(&RetrievalMethod::Fts) {
        // Parse label_id into LabelId
        let parts: Vec<&str> = label_id.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(anyhow!("Invalid label_id format: {}", label_id));
        }
        let label_id_struct = crate::engine::identifier::LabelId::new(parts[0], parts[1])?;

        let outcome = collect_fts(db_path, &label_id_struct, text, candidate_limit).await?;

        match outcome {
            FtsCollectOutcome::Collected(collected) => {
                saturations.push(collected.saturated);
                method_results.push((RetrievalMethod::Fts, collected.hits));
            }
            FtsCollectOutcome::NoIndex => {
                // Check if this is incomplete or genuinely missing
                if !label_metadata.fts_complete {
                    // Incomplete - warning already emitted via decision_warnings
                    // Don't add to method_results, skip FTS
                } else {
                    // Complete but directory is gone - degrade with warning
                    let source_pointer = format_source_pointer(label_metadata);
                    search_warnings.push(SearchWarning::FtsNoIndexDegrade {
                        label: preamble.label.clone(),
                        source_pointer,
                    });
                    // Don't add to method_results, skip FTS
                }
            }
            FtsCollectOutcome::Stale { reason } => {
                // Stale FTS index - degrade to vector with warning
                use crate::engine::fts::FtsStaleReason;
                let source_pointer = format_source_pointer(label_metadata);
                let warning = match reason {
                    FtsStaleReason::IdMismatch | FtsStaleReason::MissingManifestWithState => {
                        SearchWarning::FtsStale {
                            catalog: preamble.catalog.clone(),
                            label: preamble.label.clone(),
                            source_pointer,
                        }
                    }
                    FtsStaleReason::UnreadableManifestWithState => {
                        SearchWarning::FtsManifestUnreadable {
                            catalog: preamble.catalog.clone(),
                            label: preamble.label.clone(),
                        }
                    }
                };
                search_warnings.push(warning);
                // Don't add to method_results, skip FTS
            }
            FtsCollectOutcome::ParseError(msg) => {
                // Hard error - fail fast before embedder construction
                return Err(anyhow!("Couldn't parse FTS query: {}", msg));
            }
        }
    }

    // Step 2: Collect vector results (only if FTS didn't hard-error)
    if methods.contains(&RetrievalMethod::Vector) {
        // Initialize embedder
        // Use single worker for search; let it use all available cores for intra-op parallelism
        let intra_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let embedder = ParallelEmbedder::with_config(ParallelConfig {
            num_workers: 1,
            intra_threads,
        })?;
        let embedding = embedder.encode(text, 0)?;

        // Query LanceDB with candidate_limit
        let results = chunk_storage
            .vector_search(&embedding, label_id, candidate_limit)
            .await?;

        let collected = collect_vector(results, candidate_limit);
        saturations.push(collected.saturated);
        method_results.push((RetrievalMethod::Vector, collected.hits));
    }

    // Step 3: Call fuse() to produce fused hits
    let fused_hits = fuse(method_results, candidate_limit);

    // Step 4: Hydrate chunks with fill-from-lower-candidates
    // Single bulk LanceDB fetch over all fused row_ids (up to candidate_limit)
    let row_ids: Vec<String> = fused_hits.iter().map(|h| h.row_id.clone()).collect();
    let chunks = chunk_storage
        .get_chunks_by_row_ids_for_label(label_id, &row_ids)
        .await?;

    // Build lookup map
    let chunk_map: HashMap<String, ChunkRow> =
        chunks.into_iter().map(|c| (c.row_id.clone(), c)).collect();

    // Hydrate fused hits with chunk data
    let (results, trailing_warnings) = search::hydrate_ranked_hits(fused_hits, &chunk_map, limit);

    // Step 5: Decide end marker
    // Saturations contains one entry per backend that actually ran and returned results
    let end_marker = search::decide_end_marker(results.len(), limit, &saturations);

    // Step 6: Build render model
    let model = SearchRenderModel {
        preamble,
        pre_result_warnings: search_warnings,
        results,
        trailing_inline_warnings: trailing_warnings,
        debug,
        end_marker,
        mode: search::SearchMode::Hybrid,
    };

    // Step 7: Render
    search::render(writer, &model)?;

    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    use crate::app::commands::test_helpers::{
        create_test_db_with_chunks, test_chunk_row, test_label_metadata_row, write_minimal_config,
    };
    use crate::paths::Paths;

    #[test]
    fn test_search_missing_database() {
        let temp_dir = TempDir::new().unwrap();

        // Create config but no database
        let config_path = temp_dir.path().join("monodex-config.json");
        write_minimal_config(&config_path);

        let paths = Paths::for_test(temp_dir.path().into());
        let config = crate::app::config::load_config(paths).unwrap();

        let mut output = Vec::new();
        let result = run_search(
            &mut output,
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
    }

    #[test]
    fn test_search_missing_label_context() {
        let temp_dir = TempDir::new().unwrap();

        // Create config
        let config_path = temp_dir.path().join("monodex-config.json");
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

        let paths = Paths::for_test(temp_dir.path().into());
        let config = crate::app::config::load_config(paths).unwrap();

        // Search without providing catalog or label, and no default context
        let mut output = Vec::new();
        let result = run_search(
            &mut output,
            &config,
            "test query",
            10,
            None,
            None,
            None,
            false,
        );

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("No context set"),
            "Error should mention missing context: {}",
            err
        );
    }

    // =========================================================================
    // format_decision_error tests
    // =========================================================================

    fn make_test_metadata() -> crate::engine::storage::LabelMetadataRow {
        crate::engine::storage::LabelMetadataRow {
            label_id: "test-catalog:main".to_string(),
            catalog: "test-catalog".to_string(),
            label: "main".to_string(),
            source_kind: "git-commit".to_string(),
            vector_source: Some("abc123".to_string()),
            vector_complete: true,
            fts_source: Some("abc123".to_string()),
            fts_complete: true,
            updated_at_unix_secs: 0,
        }
    }

    #[test]
    fn test_format_decision_error_empty_selection() {
        let metadata = make_test_metadata();
        let err = crate::engine::search_decision::DecisionError::EmptySelection;
        let result = format_decision_error(&err, &metadata, "main", false);
        assert_eq!(
            result,
            "This label has no retrieval methods in its selection. Re-run `monodex crawl` to populate it."
        );
    }

    #[test]
    fn test_format_decision_error_all_in_selection_incomplete_default() {
        use crate::engine::retrieval::RetrievalMethod;
        use std::collections::BTreeSet;

        let metadata = make_test_metadata();
        let mut incomplete_methods: BTreeSet<RetrievalMethod> = BTreeSet::new();
        incomplete_methods.insert(RetrievalMethod::Fts);
        incomplete_methods.insert(RetrievalMethod::Vector);

        let err = crate::engine::search_decision::DecisionError::AllInSelectionIncomplete {
            incomplete_methods,
        };
        let result = format_decision_error(&err, &metadata, "main", false);

        // Default form should NOT contain schema details
        assert!(result.contains(
            "All retrieval methods in this label's selection (fts, vector) are incomplete."
        ));
        assert!(
            result.contains(
                "Re-run `monodex crawl --label main --commit abc123` to complete indexing."
            )
        );
        assert!(!result.contains("_complete = false"));
    }

    #[test]
    fn test_format_decision_error_all_in_selection_incomplete_debug() {
        use crate::engine::retrieval::RetrievalMethod;
        use std::collections::BTreeSet;

        let metadata = make_test_metadata();
        let mut incomplete_methods: BTreeSet<RetrievalMethod> = BTreeSet::new();
        incomplete_methods.insert(RetrievalMethod::Fts);
        incomplete_methods.insert(RetrievalMethod::Vector);

        let err = crate::engine::search_decision::DecisionError::AllInSelectionIncomplete {
            incomplete_methods,
        };
        let result = format_decision_error(&err, &metadata, "main", true);

        // Debug form SHOULD contain schema details
        assert!(result.contains(
            "All retrieval methods in this label's selection (fts, vector) are incomplete."
        ));
        assert!(
            result.contains(
                "Re-run `monodex crawl --label main --commit abc123` to complete indexing."
            )
        );
        assert!(result.contains("(fts_complete = false, vector_complete = false)"));
    }

    #[test]
    fn test_format_decision_error_sources_disagree() {
        let metadata = make_test_metadata();
        let err = crate::engine::search_decision::DecisionError::SourcesDisagree {
            vector_source: "commit-a".to_string(),
            fts_source: "commit-b".to_string(),
        };
        let result = format_decision_error(&err, &metadata, "main", false);

        assert!(result.contains("vector indexed against: commit-a"));
        assert!(result.contains("fts indexed against: commit-b"));
        assert!(result.contains(
            "Re-run `monodex crawl --label main --commit abc123` to bring them back in sync."
        ));
    }

    #[test]
    fn test_format_decision_error_method_not_in_selection() {
        use crate::engine::retrieval::RetrievalMethod;

        let metadata = make_test_metadata();
        let err = crate::engine::search_decision::DecisionError::MethodNotInSelection {
            method: RetrievalMethod::Fts,
        };
        let result = format_decision_error(&err, &metadata, "main", false);

        assert!(result.contains("Method fts is not in this label's retrieval selection."));
        assert!(result.contains(
            "Re-run `monodex crawl --label main --commit abc123 --retrieval fts` to add it."
        ));
    }

    #[test]
    fn test_format_decision_error_methods_not_in_selection() {
        use crate::engine::retrieval::RetrievalMethod;
        use std::collections::BTreeSet;

        let metadata = make_test_metadata();
        let mut methods: BTreeSet<RetrievalMethod> = BTreeSet::new();
        methods.insert(RetrievalMethod::Fts);
        methods.insert(RetrievalMethod::Vector);

        let err = crate::engine::search_decision::DecisionError::MethodsNotInSelection { methods };
        let result = format_decision_error(&err, &metadata, "main", false);

        assert!(
            result.contains("Methods fts, vector are not in this label's retrieval selection.")
        );
        assert!(
            result.contains("Re-run `monodex crawl --label main --commit abc123` to add them.")
        );
    }
}
