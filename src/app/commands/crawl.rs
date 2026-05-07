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
    enumerate_files, filter_files, format_selection_for_display, open_storage,
    print_narrowing_announcement, print_summary, print_warning_summary, run_fts_phase,
    run_label_cleanup, save_warning_state, update_final_metadata, write_in_progress_metadata,
};
use crate::app::crawl::types::{CrawlSourceMetadata, PhaseResults};
use crate::app::crawl::warning::create_warning_sink;
use crate::app::util::stderr_lock_progress;
use crate::app::{
    Config, load_warning_state, resolve_database_path, run_embed_upload_pipeline,
    run_upsert_without_vectors, validate_config_path,
};
use crate::engine::crawl_config::load_compiled_crawl_config;
use crate::engine::git_ops::{
    BlobSource, CommitBlobSource, WorkingDirBlobSource, resolve_commit_oid,
};
use crate::engine::identifier::LabelId;
use crate::engine::retrieval::RetrievalMethod;
use crate::engine::storage::{
    SOURCE_KIND_GIT_COMMIT, SOURCE_KIND_WORKING_DIRECTORY, acquire_catalog_lock,
    acquire_database_shared, read_selection,
};

/// Returns all retrieval methods (used when no explicit --retrieval is specified).
fn all_retrieval_methods() -> BTreeSet<RetrievalMethod> {
    let mut methods = BTreeSet::new();
    methods.insert(RetrievalMethod::Fts);
    methods.insert(RetrievalMethod::Vector);
    methods
}

/// Normalizes `Vec<RetrievalMethod>` to `BTreeSet<RetrievalMethod>`.
/// Empty vec means all methods; non-empty vec is deduplicated into a set.
fn normalize_retrieval(retrieval: Vec<RetrievalMethod>) -> BTreeSet<RetrievalMethod> {
    if retrieval.is_empty() {
        all_retrieval_methods()
    } else {
        // Set semantics: --retrieval vector --retrieval vector collapses to {Vector}
        retrieval.into_iter().collect()
    }
}

/// Run crawl for a git commit label
#[allow(clippy::too_many_arguments)]
pub fn run_crawl_label(
    config: &Config,
    catalog_name: &str,
    label: &str,
    commit: &str,
    incremental_warnings: bool,
    retrieval: Vec<RetrievalMethod>,
    debug: bool,
) -> Result<()> {
    let selection = normalize_retrieval(retrieval);
    let total_start = std::time::Instant::now();
    println!("🔍 Starting label-aware crawl...");
    println!("Catalog: {}", catalog_name);
    println!(
        "Label: {} {}",
        label,
        format_selection_for_display(&selection)
    );

    // Get catalog config
    let catalog_config = config
        .catalogs
        .get(catalog_name)
        .ok_or_else(|| anyhow::anyhow!("Catalog '{}' not found in config", catalog_name))?;

    // Validate catalog path (must be absolute, no ~ or $VAR)
    let repo_path = validate_config_path("catalog path", &catalog_config.path)?;
    println!("Repository: {}", repo_path.display());
    println!("Type: {}", catalog_config.r#type);
    println!("Commit: {}", commit);
    println!();

    // Compute label_id (internal storage form)
    let label_id = LabelId::new(catalog_name, label).map_err(|e| anyhow::anyhow!("{}", e))?;

    // Load repo-specific crawl configuration
    let crawl_config = load_compiled_crawl_config(Some(&repo_path))?;
    println!("Loaded crawl configuration for repository");

    // Resolve database path (needed for warning state file location)
    let db_path = resolve_database_path(Some(config))?;
    println!("Database: {}", db_path.display());

    // Acquire locks before any catalog-scoped I/O
    // Order: DatabaseLockShared first, then CatalogLock
    let _db_guard = acquire_database_shared(&db_path, &stderr_lock_progress)?;
    let _catalog_guard = acquire_catalog_lock(&db_path, catalog_name, &stderr_lock_progress)?;

    // Load persisted chunking warning files (sticky by default)
    let prior_warning_files = load_warning_state(&db_path, catalog_name);
    if !prior_warning_files.is_empty() {
        println!(
            "Found {} files with prior chunking warnings",
            prior_warning_files.len()
        );
    }
    println!();

    // Resolve commit to full SHA before constructing the blob source
    println!("📦 Resolving commit...");
    let commit_oid = resolve_commit_oid(&repo_path, commit)?;
    println!("Resolved {} to {}", commit, &commit_oid[..12]);

    // Construct the blob source and metadata
    let blob_source = CommitBlobSource::new(repo_path.clone(), commit_oid.clone());
    let source_metadata = CrawlSourceMetadata {
        source_kind: SOURCE_KIND_GIT_COMMIT,
        source_value: commit_oid,
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_crawl_async(
        config,
        catalog_name,
        label,
        incremental_warnings,
        &repo_path,
        &label_id,
        &crawl_config,
        &prior_warning_files,
        &db_path,
        total_start,
        debug,
        &blob_source,
        source_metadata,
        selection,
    ))
}

/// Run crawl for working directory (indexes uncommitted changes)
#[allow(clippy::too_many_arguments)]
pub fn run_crawl_working_dir(
    config: &Config,
    catalog_name: &str,
    label: &str,
    incremental_warnings: bool,
    retrieval: Vec<RetrievalMethod>,
    debug: bool,
) -> Result<()> {
    let selection = normalize_retrieval(retrieval);
    let total_start = std::time::Instant::now();
    println!("🔍 Starting working directory crawl...");
    println!("Catalog: {}", catalog_name);
    println!(
        "Label: {} {}",
        label,
        format_selection_for_display(&selection)
    );

    // Get catalog config
    let catalog_config = config
        .catalogs
        .get(catalog_name)
        .ok_or_else(|| anyhow::anyhow!("Catalog '{}' not found in config", catalog_name))?;

    // Validate catalog path (must be absolute, no ~ or $VAR)
    let repo_path = validate_config_path("catalog path", &catalog_config.path)?;
    println!("Repository: {}", repo_path.display());
    println!("Type: {}", catalog_config.r#type);
    println!("Source: working directory (uncommitted changes)");
    println!();

    // Compute label_id (internal storage form)
    let label_id = LabelId::new(catalog_name, label).map_err(|e| anyhow::anyhow!("{}", e))?;

    // Load repo-specific crawl configuration
    let crawl_config = load_compiled_crawl_config(Some(&repo_path))?;
    println!("Loaded crawl configuration for repository");

    // Resolve database path (needed for warning state file location)
    let db_path = resolve_database_path(Some(config))?;
    println!("Database: {}", db_path.display());

    // Acquire locks before any catalog-scoped I/O
    // Order: DatabaseLockShared first, then CatalogLock
    let _db_guard = acquire_database_shared(&db_path, &stderr_lock_progress)?;
    let _catalog_guard = acquire_catalog_lock(&db_path, catalog_name, &stderr_lock_progress)?;

    // Load persisted chunking warning files (sticky by default)
    let prior_warning_files = load_warning_state(&db_path, catalog_name);
    if !prior_warning_files.is_empty() {
        println!(
            "Found {} files with prior chunking warnings",
            prior_warning_files.len()
        );
    }
    println!();

    // Construct the blob source and metadata
    let blob_source = WorkingDirBlobSource::new(repo_path.clone());
    let source_metadata = CrawlSourceMetadata {
        source_kind: SOURCE_KIND_WORKING_DIRECTORY,
        source_value: crate::engine::make_working_dir_source_sentinel(),
    };

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_crawl_async(
        config,
        catalog_name,
        label,
        incremental_warnings,
        &repo_path,
        &label_id,
        &crawl_config,
        &prior_warning_files,
        &db_path,
        total_start,
        debug,
        &blob_source,
        source_metadata,
        selection,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_crawl_async(
    config: &Config,
    catalog_name: &str,
    label: &str,
    incremental_warnings: bool,
    repo_path: &std::path::Path,
    label_id: &LabelId,
    crawl_config: &crate::engine::crawl_config::CompiledCrawlConfig,
    prior_warning_files: &HashSet<String>,
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
    print_narrowing_announcement(&previous_selection, &selection);

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
        prior_warning_files,
        incremental_warnings,
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
        classify_output.new_count,
        &warning_counter,
        &mut warning_sink,
    )?;

    // Initialize phase results with pessimistic defaults
    let mut phase_results = PhaseResults::new(&selection);

    // Track phase errors for proper error propagation (vector > reassignment > fts priority)
    let mut vector_phase_error: Option<anyhow::Error> = None;
    let mut label_reassignment_error: Option<anyhow::Error> = None;
    let mut fts_phase_error: Option<anyhow::Error> = None;

    // Phase 3: Embed-and-upsert (vector) OR upsert-without-vectors (FTS-only)
    let (pipeline_file_ids, pipeline_failures) = if vector_in_selection {
        // Vector path: embed and upload (handles both vector-only and {vector, fts} selections)
        match run_embed_upload_pipeline(
            chunking_output.chunks,
            Arc::clone(&chunk_storage),
            &config.embedding_model,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                eprintln!("  ❌ Vector phase failed: {}", e);
                phase_results.vector_succeeded = Some(false);
                vector_phase_error = Some(e);
                (HashSet::new(), crate::app::CrawlFailures::default())
            }
        }
    } else if fts_in_selection {
        // FTS-only path: upload without vectors (no ONNX initialization)
        match run_upsert_without_vectors(chunking_output.chunks, Arc::clone(&chunk_storage)).await {
            Ok(result) => result,
            Err(e) => {
                eprintln!("  ❌ Upsert phase failed: {}", e);
                phase_results.vector_succeeded = Some(false);
                vector_phase_error = Some(e);
                (HashSet::new(), crate::app::CrawlFailures::default())
            }
        }
    } else {
        // Empty selection - unreachable (rejected upstream)
        (HashSet::new(), crate::app::CrawlFailures::default())
    };

    // Mark vector phase as succeeded if it was in selection and no error was captured
    if vector_in_selection && vector_phase_error.is_none() {
        phase_results.vector_succeeded = Some(!pipeline_failures.has_failures());
    } else if fts_in_selection && vector_phase_error.is_none() {
        // FTS-only path: no vector phase, mark as succeeded (no failures possible)
        phase_results.vector_succeeded = None;
    }

    // Combine touched file IDs
    let mut all_touched_file_ids: HashSet<String> = classify_output.existing_file_ids.clone();
    all_touched_file_ids.extend(chunking_output.touched_file_ids);
    all_touched_file_ids.extend(pipeline_file_ids);

    let has_existing_file_failures = !label_add_output.failures.is_empty();
    let had_embed_failures = pipeline_failures.has_failures();

    // Phase 4: Label reassignment cleanup (conditional)
    // Skip cleanup if: per-chunk failures, embed failures, OR structural phase errors.
    // Structural errors (from run_embed_upload_pipeline or run_upsert_without_vectors)
    // are captured in vector_phase_error but weren't gating cleanup - this is the bug fix.
    if should_skip_label_cleanup(
        has_existing_file_failures,
        had_embed_failures,
        vector_phase_error.is_some(),
    ) {
        println!("🔶 Phase 4: SKIPPING label reassignment cleanup (crawl had failures)");
        println!("  This is intentional - cleanup should only run after successful crawls.");
        println!("  Run the crawl again to complete indexing and trigger cleanup.");
        phase_results.label_reassignment_succeeded = false;
    } else {
        match run_label_cleanup(&chunk_storage, label_id.as_str(), &all_touched_file_ids).await {
            Ok(_) => {
                phase_results.label_reassignment_succeeded = true;
            }
            Err(e) => {
                eprintln!("  ❌ Label cleanup failed: {}", e);
                phase_results.label_reassignment_succeeded = false;
                label_reassignment_error = Some(e);
            }
        }
    }
    println!();

    // Phase 5: FTS indexing (conditional on selection and prior success)
    if fts_in_selection && phase_results.label_reassignment_succeeded {
        match run_fts_phase(
            db_path,
            label_id,
            &chunk_storage,
            &mut warning_sink,
            is_commit_mode,
        )
        .await
        {
            Ok(()) => {
                phase_results.fts_succeeded = Some(true);
            }
            Err(e) => {
                eprintln!("  ❌ FTS indexing failed: {}", e);
                phase_results.fts_succeeded = Some(false);
                fts_phase_error = Some(e);
            }
        }
        println!();
    }

    // Phase: Update final label metadata (with per-method completion)
    // Always call this even if phases failed, so partial state is persisted
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

    // Phase: Print summary
    let had_failures = has_existing_file_failures
        || had_embed_failures
        || !phase_results.label_reassignment_succeeded
        || phase_results.fts_succeeded == Some(false);

    // Determine phase failure states for summary
    let vector_phase_failed = vector_phase_error.is_some();
    let fts_phase_failed = fts_phase_error.is_some();

    print_summary(
        total_start,
        classify_output.new_count,
        classify_output.existing_count,
        label_add_output.success_file_ids.len(),
        has_existing_file_failures || had_embed_failures,
        !phase_results.label_reassignment_succeeded,
        label_add_output.failures.len(),
        pipeline_failures.total(),
        vector_phase_failed,
        fts_phase_failed,
    );

    // Phase: Save warning state
    let warning_state_result = save_warning_state(
        db_path,
        catalog_name,
        &chunking_output.warning_files,
        prior_warning_files,
        incremental_warnings,
    );

    // Phase: Print warning summary
    print_warning_summary(&chunking_output.warning_files);

    // Return the most-specific captured error (vector > reassignment > fts priority)
    // An earlier-phase error is the root cause of any subsequent skipped work.
    // When returning a phase error, log any post-finalize errors to stderr.
    if let Some(e) = vector_phase_error {
        if let Err(ref we) = warning_state_result {
            eprintln!("  Warning: Failed to save warning state: {}", we);
        }
        if let Err(ref me) = final_metadata_result {
            eprintln!("  Warning: Failed to update label metadata: {}", me);
        }
        return Err(e);
    }
    if let Some(e) = label_reassignment_error {
        if let Err(ref we) = warning_state_result {
            eprintln!("  Warning: Failed to save warning state: {}", we);
        }
        if let Err(ref me) = final_metadata_result {
            eprintln!("  Warning: Failed to update label metadata: {}", me);
        }
        return Err(e);
    }
    if let Some(e) = fts_phase_error {
        if let Err(ref we) = warning_state_result {
            eprintln!("  Warning: Failed to save warning state: {}", we);
        }
        if let Err(ref me) = final_metadata_result {
            eprintln!("  Warning: Failed to update label metadata: {}", me);
        }
        return Err(e);
    }

    // No captured phase error: post-finalize errors propagate normally.
    if let Err(e) = final_metadata_result {
        return Err(e.context("Failed to update label metadata"));
    }
    if let Err(e) = warning_state_result {
        return Err(e.context("Failed to save warning state"));
    }

    // No captured phase error, but had per-chunk failures or other issues
    if had_failures {
        anyhow::bail!("Crawl completed with errors (see above). Label marked incomplete.");
    }

    Ok(())
}

/// Determines whether label cleanup should be skipped due to failures.
///
/// Label cleanup (Phase 4) should only run after fully successful crawls.
/// This predicate gates cleanup on:
/// - Per-chunk label-add failures (existing files that couldn't be updated)
/// - Per-chunk embed failures (individual chunks that failed to embed)
/// - Structural phase errors (pipeline failures that abort the entire phase)
///
/// # Arguments
/// * `has_existing_file_failures` - True if any existing-file label-add failed
/// * `had_embed_failures` - True if any per-chunk embedding failed
/// * `vector_phase_error_present` - True if a structural error occurred in Phase 3
///
/// # Returns
/// `true` if cleanup should be skipped, `false` if it should proceed.
fn should_skip_label_cleanup(
    has_existing_file_failures: bool,
    had_embed_failures: bool,
    vector_phase_error_present: bool,
) -> bool {
    has_existing_file_failures || had_embed_failures || vector_phase_error_present
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_skip_label_cleanup_no_failures() {
        // No failures of any kind: cleanup should run
        assert!(!should_skip_label_cleanup(false, false, false));
    }

    #[test]
    fn test_should_skip_label_cleanup_existing_file_failures() {
        // Existing-file failures only: skip cleanup
        assert!(should_skip_label_cleanup(true, false, false));
    }

    #[test]
    fn test_should_skip_label_cleanup_embed_failures() {
        // Per-chunk embed failures only: skip cleanup
        assert!(should_skip_label_cleanup(false, true, false));
    }

    #[test]
    fn test_should_skip_label_cleanup_structural_error() {
        // Structural vector_phase_error present: skip cleanup
        // This is the new case this jobsheet is fixing
        assert!(should_skip_label_cleanup(false, false, true));
    }

    #[test]
    fn test_should_skip_label_cleanup_multiple_failures() {
        // Multiple failure types: skip cleanup
        assert!(should_skip_label_cleanup(true, true, false));
        assert!(should_skip_label_cleanup(true, false, true));
        assert!(should_skip_label_cleanup(false, true, true));
        assert!(should_skip_label_cleanup(true, true, true));
    }
}
