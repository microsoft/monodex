//! Handler for the `crawl` command.
//!
//! Purpose: Crawl a repository and index chunks into LanceDB.
//! Edit here when: Modifying crawl entry points, label creation, or storage interactions.
//! Do not edit here for: Embed/upload pipeline (see ../crawl/pipeline.rs), crawl types (see ../crawl/types.rs),
//!                       crawl phases (see ../crawl/phases.rs).

use anyhow::Result;
use std::cell::Cell;
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use crate::app::crawl::phases::{
    add_label_to_existing_files, build_package_index, chunk_new_files, classify_files,
    enumerate_files, filter_files, open_storage, print_narrowing_announcement, print_summary,
    print_warning_summary, run_fts_phase, run_label_cleanup, update_final_metadata,
    write_in_progress_metadata,
};
use crate::app::crawl::preamble::{CrawlInput, CrawlPreamble, prepare_crawl_preamble};
use crate::app::crawl::types::{CrawlSourceMetadata, PhaseResults};
use crate::app::crawl::warning::create_warning_sink;
use crate::app::{Config, run_embed_upload_pipeline, run_upsert_without_vectors};
use crate::engine::git_ops::{BlobSource, CommitBlobSource, WorkingDirBlobSource};
use crate::engine::identifier::LabelId;
use crate::engine::retrieval::RetrievalMethod;
use crate::engine::storage::{SOURCE_KIND_GIT_COMMIT, read_selection};

/// Report from the post-chunking phases, used by print_summary.
///
/// This struct captures phase outcomes that the summary renderer needs,
/// distinct from PhaseResults which is used for metadata persistence.
/// The separation avoids conflating structural phase failures with
/// per-chunk embed failures (vector_succeeded = Some(false) covers both).
#[derive(Default)]
struct PostChunkingReport {
    /// Total per-chunk embedding/upload failures from the pipeline.
    pipeline_failures_total: usize,
    /// True if any per-chunk embedding failed.
    had_embed_failures: bool,
    /// True if the chunk-write phase failed structurally (embed/upload or FTS-only upsert).
    chunk_write_failed: bool,
    /// True if the FTS indexing phase failed structurally.
    fts_phase_failed: bool,
}

/// Run crawl for a git commit label
pub fn run_crawl_label(
    config: &Config,
    catalog_name: &str,
    label: &str,
    commit: &str,
    retrieval: Vec<RetrievalMethod>,
    debug: bool,
) -> Result<()> {
    let preamble = prepare_crawl_preamble(
        config,
        catalog_name,
        label,
        retrieval,
        CrawlInput::Commit { commit },
    )?;

    let CrawlPreamble {
        selection,
        total_start,
        repo_path,
        label_id,
        crawl_config,
        db_path,
        source_metadata,
        _db_guard,
        _catalog_guard,
    } = preamble;

    let blob_source = CommitBlobSource::new(&repo_path, source_metadata.source_value.clone())?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_crawl_async(
        config,
        catalog_name,
        label,
        &repo_path,
        &label_id,
        &crawl_config,
        &db_path,
        total_start,
        debug,
        &blob_source,
        source_metadata,
        selection,
    ))
}

/// Run crawl for working directory (indexes uncommitted changes)
pub fn run_crawl_working_dir(
    config: &Config,
    catalog_name: &str,
    label: &str,
    retrieval: Vec<RetrievalMethod>,
    debug: bool,
) -> Result<()> {
    let preamble = prepare_crawl_preamble(
        config,
        catalog_name,
        label,
        retrieval,
        CrawlInput::WorkingDir,
    )?;

    let CrawlPreamble {
        selection,
        total_start,
        repo_path,
        label_id,
        crawl_config,
        db_path,
        source_metadata,
        _db_guard,
        _catalog_guard,
    } = preamble;

    let blob_source = WorkingDirBlobSource::new(repo_path.clone());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_crawl_async(
        config,
        catalog_name,
        label,
        &repo_path,
        &label_id,
        &crawl_config,
        &db_path,
        total_start,
        debug,
        &blob_source,
        source_metadata,
        selection,
    ))
}

/// Run the post-chunking phases: embed/upload, label cleanup, and FTS indexing.
///
/// This helper owns the chunk-write phase, the label-reassignment phase, and the FTS phase.
/// It early-returns on the first phase error, which automatically enforces the error
/// priority order: vector > reassignment > FTS. A chunk-write failure returns before
/// label cleanup or FTS can run.
///
/// Phase outcomes are written to `phase_results` and `report` before any early return,
/// so partial state survives for the caller's metadata update.
#[allow(clippy::too_many_arguments)]
async fn run_post_chunking_phases(
    chunks: Vec<crate::engine::Chunk>,
    chunking_touched_file_ids: HashSet<String>,
    chunk_storage: Arc<crate::engine::storage::ChunkStorage>,
    config: &Config,
    vector_in_selection: bool,
    fts_in_selection: bool,
    existing_file_ids: &HashSet<String>,
    has_existing_file_failures: bool,
    db_path: &std::path::Path,
    label_id: &LabelId,
    is_commit_mode: bool,
    debug: bool,
    phase_results: &mut PhaseResults,
    report: &mut PostChunkingReport,
) -> Result<()> {
    // Embed-and-upsert (vector) OR upsert-without-vectors (FTS-only)
    let (pipeline_file_ids, pipeline_failures) = if vector_in_selection {
        // Vector path: embed and upload
        match run_embed_upload_pipeline(chunks, Arc::clone(&chunk_storage), &config.embedding_model)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                eprintln!("  ❌ Vector phase failed: {}", e);
                phase_results.vector_succeeded = Some(false);
                report.chunk_write_failed = true;
                return Err(e);
            }
        }
    } else if fts_in_selection {
        // FTS-only path: upload without vectors
        match run_upsert_without_vectors(chunks, Arc::clone(&chunk_storage)).await {
            Ok(result) => result,
            Err(e) => {
                eprintln!("  ❌ Upsert phase failed: {}", e);
                // Do NOT set vector_succeeded - vector is not in selection
                report.chunk_write_failed = true;
                return Err(e);
            }
        }
    } else {
        // Empty selection - unreachable (rejected upstream)
        (HashSet::new(), crate::app::CrawlFailures::default())
    };

    // Mark vector phase as succeeded if it was in selection
    if vector_in_selection {
        phase_results.vector_succeeded = Some(!pipeline_failures.has_failures());
    }

    // Track report fields
    report.pipeline_failures_total = pipeline_failures.total();
    report.had_embed_failures = pipeline_failures.has_failures();

    // Combine touched file IDs
    let mut all_touched_file_ids: HashSet<String> = existing_file_ids.clone();
    all_touched_file_ids.extend(chunking_touched_file_ids);
    all_touched_file_ids.extend(pipeline_file_ids);

    // Label reassignment cleanup (conditional)
    // Skip cleanup if per-chunk failures or embed failures occurred.
    if should_skip_label_cleanup(has_existing_file_failures, report.had_embed_failures) {
        println!("🔶 SKIPPING label reassignment cleanup (crawl had failures)");
        println!("  This is intentional - cleanup should only run after successful crawls.");
        println!("  Run the crawl again to complete indexing and trigger cleanup.");
        phase_results.label_reassignment_succeeded = false;
        // A cleanup skip is not an error - return Ok and let the caller handle the failure flag
        return Ok(());
    }

    match run_label_cleanup(&chunk_storage, label_id.as_str(), &all_touched_file_ids).await {
        Ok(_) => {
            phase_results.label_reassignment_succeeded = true;
        }
        Err(e) => {
            eprintln!("  ❌ Label cleanup failed: {}", e);
            phase_results.label_reassignment_succeeded = false;
            return Err(e);
        }
    }
    println!();

    // FTS indexing (conditional on selection and prior success)
    if fts_in_selection && phase_results.label_reassignment_succeeded {
        match run_fts_phase(db_path, label_id, &chunk_storage, is_commit_mode, debug).await {
            Ok(()) => {
                phase_results.fts_succeeded = Some(true);
            }
            Err(e) => {
                eprintln!("  ❌ FTS indexing failed: {}", e);
                phase_results.fts_succeeded = Some(false);
                report.fts_phase_failed = true;
                return Err(e);
            }
        }
        println!();
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_crawl_async(
    config: &Config,
    catalog_name: &str,
    label: &str,
    repo_path: &std::path::Path,
    label_id: &LabelId,
    crawl_config: &crate::engine::crawl_config::CompiledCrawlConfig,
    db_path: &std::path::Path,
    total_start: std::time::Instant,
    debug: bool,
    blob_source: &dyn BlobSource,
    source_metadata: CrawlSourceMetadata,
    selection: BTreeSet<RetrievalMethod>,
) -> Result<()> {
    // Create warning counter and sink for in-flight warnings
    let warning_counter = Cell::new(0usize);
    let mut warning_sink = create_warning_sink(&warning_counter);

    // Determine if this is commit mode (for FTS merging behavior)
    let is_commit_mode = source_metadata.source_kind == SOURCE_KIND_GIT_COMMIT;

    // Phase: Open database and get storage handles
    let (chunk_storage, label_storage) = open_storage(db_path, debug).await?;

    // Read previous selection (if any) for narrowing announcement
    let previous_metadata = label_storage.get_by_label_id(label_id.as_str()).await?;
    let previous_selection = previous_metadata
        .as_ref()
        .map(read_selection)
        .unwrap_or_default();

    // Phase: Write in-progress metadata before any work begins
    write_in_progress_metadata(
        &label_storage,
        label_id,
        catalog_name,
        label,
        &source_metadata.source_value,
        source_metadata.source_kind,
        &selection,
        debug,
    )
    .await?;

    // Print narrowing announcement if this crawl narrows the selection
    print_narrowing_announcement(&mut std::io::stdout(), &previous_selection, &selection);

    // Determine retrieval method presence early (used for fast-path predicate)
    let vector_in_selection = selection.contains(&RetrievalMethod::Vector);
    let fts_in_selection = selection.contains(&RetrievalMethod::Fts);

    // Phase: Enumerate files from the blob source
    let files = enumerate_files(blob_source)?;

    // Phase: Build package index
    let package_index = build_package_index(blob_source)?;

    // Phase: Filter files using crawl config
    let files_to_process = filter_files(files, crawl_config);

    // Phase: Classify files against existing chunks
    let classify_output = classify_files(
        &files_to_process,
        &chunk_storage,
        catalog_name,
        vector_in_selection,
        &mut warning_sink,
    )
    .await?;

    // Phase: Add label to existing files
    let label_add_output =
        add_label_to_existing_files(&classify_output.existing_file_ids, &chunk_storage, label_id)
            .await?;

    // Phase: Chunk new files (runs whenever any method is in selection)
    let chunking_output = chunk_new_files(
        &classify_output.new_files,
        blob_source,
        &package_index,
        crawl_config,
        catalog_name,
        label_id,
        repo_path,
        vector_in_selection,
        &warning_counter,
        &mut warning_sink,
    )?;

    // Initialize phase results with pessimistic defaults
    let mut phase_results = PhaseResults::new(&selection);
    let mut report = PostChunkingReport::default();

    let has_existing_file_failures = !label_add_output.failures.is_empty();

    // Destructure chunking_output so warning_files survives the helper call.
    let crate::app::crawl::phases::ChunkingOutput {
        chunks,
        touched_file_ids,
        warning_files,
    } = chunking_output;

    // Run the post-chunking phases. The helper short-circuits on the first error,
    // which automatically enforces the priority order vector > reassignment > FTS:
    // a chunk-write failure returns before label cleanup or FTS run, etc.
    // Final metadata MUST run regardless of the helper's outcome, because partial
    // state has to be persisted for resume (see docs/design/crawl.md step 7).
    // Do NOT use `?` on phase_run_result before update_final_metadata.
    let phase_run_result = run_post_chunking_phases(
        chunks,
        touched_file_ids,
        Arc::clone(&chunk_storage),
        config,
        vector_in_selection,
        fts_in_selection,
        &classify_output.existing_file_ids,
        has_existing_file_failures,
        db_path,
        label_id,
        is_commit_mode,
        debug,
        &mut phase_results,
        &mut report,
    )
    .await;

    let final_metadata_result = update_final_metadata(
        &label_storage,
        label_id,
        catalog_name,
        label,
        &source_metadata.source_value,
        source_metadata.source_kind,
        &selection,
        &phase_results,
    )
    .await;

    print_summary(
        total_start,
        classify_output.new_files.len(),
        classify_output.existing_file_ids.len(),
        label_add_output.success_file_ids.len(),
        has_existing_file_failures || report.had_embed_failures,
        !phase_results.label_reassignment_succeeded,
        label_add_output.failures.len(),
        report.pipeline_failures_total,
        report.chunk_write_failed,
        report.fts_phase_failed,
    );

    print_warning_summary(&warning_files);

    // Three-branch return, matching current behavior:
    if let Err(e) = phase_run_result {
        if let Err(ref me) = final_metadata_result {
            eprintln!("  Warning: Failed to update label metadata: {}", me);
        }
        return Err(e);
    }

    if let Err(e) = final_metadata_result {
        return Err(e.context("Failed to update label metadata"));
    }

    let had_failures = has_existing_file_failures
        || report.had_embed_failures
        || !phase_results.label_reassignment_succeeded
        || phase_results.fts_succeeded == Some(false);

    if had_failures {
        anyhow::bail!("Crawl completed with errors (see above). Label marked incomplete.");
    }

    Ok(())
}

/// Determines whether label cleanup should be skipped due to failures.
///
/// Label cleanup should only run after fully successful crawls.
/// This predicate gates cleanup on:
/// - Per-chunk label-add failures (existing files that couldn't be updated)
/// - Per-chunk embed failures (individual chunks that failed to embed)
///
/// # Arguments
/// * `has_existing_file_failures` - True if any existing-file label-add failed
/// * `had_embed_failures` - True if any per-chunk embedding failed
///
/// # Returns
/// `true` if cleanup should be skipped, `false` if it should proceed.
fn should_skip_label_cleanup(has_existing_file_failures: bool, had_embed_failures: bool) -> bool {
    has_existing_file_failures || had_embed_failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_skip_label_cleanup_no_failures() {
        // No failures of any kind: cleanup should run
        assert!(!should_skip_label_cleanup(false, false));
    }

    #[test]
    fn test_should_skip_label_cleanup_existing_file_failures() {
        // Existing-file failures only: skip cleanup
        assert!(should_skip_label_cleanup(true, false));
    }

    #[test]
    fn test_should_skip_label_cleanup_embed_failures() {
        // Per-chunk embed failures only: skip cleanup
        assert!(should_skip_label_cleanup(false, true));
    }

    #[test]
    fn test_should_skip_label_cleanup_multiple_failures() {
        // Multiple failure types: skip cleanup
        assert!(should_skip_label_cleanup(true, true));
    }
}
