//! Crawl pipeline phases.
//!
//! Purpose: Individual phases of the crawl pipeline, extracted for clarity and maintainability.
//! Edit here when: Modifying phase logic, adding new phases (e.g., FTS indexing), or changing phase ordering.
//! Do not edit here for: Crawl orchestration (see ../commands/crawl.rs), embed/upload pipeline (see pipeline.rs).

use anyhow::Result;
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use crate::app::format_duration;
use crate::engine::{
    TARGET_CHARS,
    chunker::{ChunkContext, chunk_content},
    crawl_config::CompiledCrawlConfig,
    git_ops::{BlobSource, FileEntry},
    identifier::LabelId,
    retrieval::RetrievalMethod,
    storage::{ChunkStorage, LabelMetadataRow, LabelStorage},
    warning::{CrawlWarning, WarningSink},
};

/// Opens the database and returns storage handles.
pub async fn open_storage(
    db_path: &std::path::Path,
    debug: bool,
) -> Result<(Arc<ChunkStorage>, Arc<LabelStorage>)> {
    let db = crate::engine::storage::Database::open(db_path).await?;
    if debug {
        println!("[DEBUG] Opened database at: {}", db_path.display());
    }
    let chunk_storage = Arc::new(db.chunks_storage().await?);
    let label_storage = Arc::new(db.label_storage().await?);
    if debug {
        println!("[DEBUG] Opened chunks and label_metadata tables");
    }
    Ok((chunk_storage, label_storage))
}

/// Writes in-progress label metadata before any work begins.
///
/// The `selection` parameter specifies which retrieval methods are in the new selection.
/// For each method in the selection, `<method>_source` is set to `source_value` and
/// `<method>_complete` is set to `false`. For methods not in the selection,
/// `<method>_source` is set to `NULL` and `<method>_complete` is set to `false`
/// (the `_complete` value is a don't-care when source is NULL, but must be written
/// since the column is non-nullable).
#[allow(clippy::too_many_arguments)]
pub async fn write_in_progress_metadata(
    label_storage: &LabelStorage,
    label_id: &LabelId,
    catalog_name: &str,
    label: &str,
    source_value: &str,
    source_kind: &str,
    selection: &BTreeSet<RetrievalMethod>,
    debug: bool,
) -> Result<()> {
    let metadata = LabelMetadataRow {
        label_id: label_id.to_string(),
        catalog: catalog_name.to_string(),
        label: label.to_string(),
        source_kind: source_kind.to_string(),
        vector_source: if selection.contains(&RetrievalMethod::Vector) {
            Some(source_value.to_string())
        } else {
            None
        },
        vector_complete: false,
        fts_source: if selection.contains(&RetrievalMethod::Fts) {
            Some(source_value.to_string())
        } else {
            None
        },
        fts_complete: false,
        updated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    };
    label_storage.upsert(&metadata).await?;
    if debug {
        println!("[DEBUG] Wrote in-progress label metadata: {}", label_id);
    }
    Ok(())
}

/// Enumerates files from the blob source.
pub fn enumerate_files(blob_source: &dyn BlobSource) -> Result<Vec<FileEntry>> {
    let files = blob_source.enumerate()?;
    println!("Found {} files", files.len());
    Ok(files)
}

/// Builds the package index from the blob source.
pub fn build_package_index(
    blob_source: &dyn BlobSource,
) -> Result<crate::engine::git_ops::PackageIndex> {
    println!("📦 Building package index...");
    let package_index = blob_source.build_package_index()?;
    println!("Package index built successfully");
    println!();
    Ok(package_index)
}

/// Filters files using the crawl configuration.
pub fn filter_files(files: Vec<FileEntry>, crawl_config: &CompiledCrawlConfig) -> Vec<FileEntry> {
    println!("📂 Filtering files...");
    let filtered: Vec<FileEntry> = files
        .into_iter()
        .filter(|f| crawl_config.should_crawl(&f.relative_path))
        .collect();
    println!("{} files to process after filtering", filtered.len());
    println!();
    filtered
}

// ============================================================================
// Phase 1: Classify files
// ============================================================================

/// Output from classifying files against existing chunks.
pub struct ClassifyOutput {
    /// Files that need to be chunked and indexed.
    pub new_files: Vec<FileEntry>,
    /// File IDs for files that already have chunks and need label added.
    pub existing_file_ids: HashSet<String>,
    /// Count of new files (for display).
    pub new_count: usize,
    /// Count of existing files (for display).
    pub existing_count: usize,
}

/// Classifies files as new or existing based on chunk presence.
pub async fn classify_files(
    files: &[FileEntry],
    chunk_storage: &ChunkStorage,
    prior_warning_files: &HashSet<String>,
    incremental_warnings: bool,
    catalog_name: &str,
    warnings: WarningSink<'_>,
) -> Result<ClassifyOutput> {
    println!("⚡ Phase 1: Checking existing chunks and collecting new files...");

    let mut new_files: Vec<FileEntry> = Vec::new();
    let mut existing_file_ids: HashSet<String> = HashSet::new();
    let mut new_count = 0;
    let mut existing_count = 0;

    for file_entry in files {
        // When incremental_warnings is false and file had prior warning, force reprocessing
        let force_reprocess =
            !incremental_warnings && prior_warning_files.contains(&file_entry.relative_path);

        let file_id = crate::engine::util::compute_file_id(
            crate::engine::util::EMBEDDER_ID,
            crate::engine::util::CHUNKER_ID,
            catalog_name,
            &file_entry.blob_id,
            &file_entry.relative_path,
        );

        if force_reprocess {
            // Treat as new file to re-chunk and re-index
            new_files.push(file_entry.clone());
            new_count += 1;
            continue;
        }

        // Check if sentinel exists and is complete
        let sentinel_row_id = format!("{}:1", file_id);
        match chunk_storage.get_by_row_id(&sentinel_row_id).await {
            Ok(Some(chunk)) => {
                // Check if file crawl was completed (file_complete == true)
                if !chunk.file_complete {
                    // Incomplete file - treat as new file to re-crawl
                    new_files.push(file_entry.clone());
                    new_count += 1;
                    continue;
                }
                // File already indexed - add to existing files list.
                // We do NOT short-circuit based on sentinel's active_label_ids because
                // partial label coverage is possible (some chunks may be missing the label).
                // The label-add phase will visit all chunks and update as needed.
                existing_file_ids.insert(file_id);
                existing_count += 1;
            }
            Ok(None) => {
                new_files.push(file_entry.clone());
                new_count += 1;
            }
            Err(e) => {
                warnings(CrawlWarning::SentinelReadFailed {
                    relative_path: file_entry.relative_path.clone(),
                    error: e.to_string(),
                });
                new_files.push(file_entry.clone());
                new_count += 1;
            }
        }
    }

    println!("  New files to index: {}", new_count);
    println!("  Existing files (label update only): {}", existing_count);
    println!();

    Ok(ClassifyOutput {
        new_files,
        existing_file_ids,
        new_count,
        existing_count,
    })
}

// ============================================================================
// Phase 2: Add label to existing files
// ============================================================================

/// Output from adding labels to existing files.
pub struct LabelAddOutput {
    /// File IDs that were successfully updated.
    pub success_file_ids: HashSet<String>,
    /// Error messages for files that failed.
    pub failures: Vec<String>,
}

/// Adds the current label to existing files' chunks.
pub async fn add_label_to_existing_files(
    existing_file_ids: &HashSet<String>,
    chunk_storage: &ChunkStorage,
    label_id: &LabelId,
) -> Result<LabelAddOutput> {
    let mut success_file_ids: HashSet<String> = HashSet::new();
    let mut failures: Vec<String> = Vec::new();

    if existing_file_ids.is_empty() {
        return Ok(LabelAddOutput {
            success_file_ids,
            failures,
        });
    }

    println!(
        "🏷️  Adding label to {} existing files...",
        existing_file_ids.len()
    );
    for file_id in existing_file_ids {
        // Get all chunks for this file and add the label
        match chunk_storage.get_chunks_by_file_id(file_id).await {
            Ok(chunks) => {
                let mut file_had_failures = false;
                for chunk in &chunks {
                    // Skip chunks that already have this label
                    if chunk.active_label_ids.contains(&label_id.to_string()) {
                        continue;
                    }
                    let new_labels = {
                        let mut labels = chunk.active_label_ids.clone();
                        labels.push(label_id.to_string());
                        labels
                    };
                    if let Err(e) = chunk_storage
                        .update_active_labels(&chunk.row_id, &new_labels)
                        .await
                    {
                        eprintln!("  ❌ Failed to add label to chunk {}: {}", chunk.row_id, e);
                        failures.push(format!("{}: {}", file_id, e));
                        file_had_failures = true;
                    }
                }
                if !file_had_failures {
                    success_file_ids.insert(file_id.clone());
                }
            }
            Err(e) => {
                eprintln!("  ❌ Failed to get chunks for file {}: {}", file_id, e);
                failures.push(format!("{}: {}", file_id, e));
            }
        }
    }
    println!("  Done.");
    if !failures.is_empty() {
        println!(
            "  ⚠️  Failed to add label to {} existing files",
            failures.len()
        );
    }
    println!();

    Ok(LabelAddOutput {
        success_file_ids,
        failures,
    })
}

// ============================================================================
// Phase 3: Chunk new files
// ============================================================================

/// Output from the chunking phase.
pub struct ChunkingOutput {
    /// All chunks produced.
    pub chunks: Vec<crate::engine::Chunk>,
    /// File IDs that were touched during chunking.
    pub touched_file_ids: HashSet<String>,
    /// Files that had chunking warnings.
    pub warning_files: HashSet<String>,
}

/// Chunks new files and produces chunks for embedding.
#[allow(clippy::too_many_arguments)]
pub fn chunk_new_files(
    new_files: &[FileEntry],
    blob_source: &dyn BlobSource,
    package_index: &crate::engine::git_ops::PackageIndex,
    crawl_config: &CompiledCrawlConfig,
    catalog_name: &str,
    label_id: &LabelId,
    repo_path: &std::path::Path,
    new_count: usize,
    warning_counter: &std::cell::Cell<usize>,
    warnings: WarningSink<'_>,
) -> Result<ChunkingOutput> {
    let mut chunks: Vec<crate::engine::Chunk> = Vec::new();
    let mut touched_file_ids: HashSet<String> = HashSet::new();
    let mut warning_files: HashSet<String> = HashSet::new();

    if new_files.is_empty() {
        return Ok(ChunkingOutput {
            chunks,
            touched_file_ids,
            warning_files,
        });
    }

    println!("📦 Phase 2: Chunking {} new files...", new_count);

    for (idx, file_entry) in new_files.iter().enumerate() {
        print!(
            "\r  Processing file {}/{} ({:.0}%) | warnings: {}   ",
            idx + 1,
            new_count,
            ((idx + 1) as f64 / new_count as f64) * 100.0,
            warning_counter.get()
        );
        std::io::Write::flush(&mut std::io::stdout())?;

        let content = match blob_source.read_content(file_entry) {
            Ok(c) => c,
            Err(e) => {
                warnings(CrawlWarning::FileReadFailed {
                    relative_path: file_entry.relative_path.clone(),
                    error: e.to_string(),
                });
                continue;
            }
        };

        let content_str = match String::from_utf8(content) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let package_name = package_index
            .find_package_name(&file_entry.relative_path)
            .unwrap_or(catalog_name)
            .to_string();

        let ctx = ChunkContext {
            catalog: catalog_name.to_string(),
            label_id: label_id.to_string(),
            package_name,
            relative_path: file_entry.relative_path.clone(),
            blob_id: file_entry.blob_id.clone(),
            source_uri: format!("{}/{}", repo_path.display(), file_entry.relative_path),
        };

        let strategy = crawl_config.get_strategy(&file_entry.relative_path);
        match chunk_content(&content_str, &ctx, TARGET_CHARS, strategy) {
            Ok(file_chunks) => {
                // Detect fallback warning: chunk_kind == "fallback-split"
                let had_warning = file_chunks.iter().any(|c| c.chunk_kind == "fallback-split");
                if had_warning {
                    warning_files.insert(file_entry.relative_path.clone());
                    warnings(CrawlWarning::ChunkerFallbackSplit {
                        relative_path: file_entry.relative_path.clone(),
                    });
                }

                if !file_chunks.is_empty() {
                    touched_file_ids.insert(file_chunks[0].file_id.clone());
                }
                chunks.extend(file_chunks);
            }
            Err(e) => {
                warnings(CrawlWarning::ChunkingFailed {
                    relative_path: file_entry.relative_path.clone(),
                    error: e.to_string(),
                });
            }
        }
    }

    let total_chunks = chunks.len();
    println!("\n  Found {} chunks to embed", total_chunks);
    println!();

    Ok(ChunkingOutput {
        chunks,
        touched_file_ids,
        warning_files,
    })
}

/// Runs label reassignment cleanup to remove stale chunks.
pub async fn run_label_cleanup(
    chunk_storage: &ChunkStorage,
    label_id: &str,
    all_touched_file_ids: &HashSet<String>,
) -> Result<u64> {
    println!("🧹 Phase 4: Label reassignment cleanup...");
    let processed = chunk_storage
        .remove_label_from_chunks(label_id, all_touched_file_ids)
        .await?;
    println!("  Processed {} chunks for label cleanup", processed);
    Ok(processed)
}

/// Updates the final label metadata.
pub async fn update_final_metadata(
    label_storage: &LabelStorage,
    label_id: &LabelId,
    catalog_name: &str,
    label: &str,
    source_value: &str,
    source_kind: &str,
    crawl_complete: bool,
) -> Result<()> {
    println!("📝 Updating label metadata...");
    let metadata = LabelMetadataRow {
        label_id: label_id.to_string(),
        catalog: catalog_name.to_string(),
        label: label.to_string(),
        source_kind: source_kind.to_string(),
        // Stage 6 will update these with proper per-method completion handling
        vector_source: Some(source_value.to_string()),
        vector_complete: crawl_complete,
        fts_source: Some(source_value.to_string()),
        fts_complete: crawl_complete,
        updated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    };

    label_storage.upsert(&metadata).await?;
    if crawl_complete {
        println!("  Label metadata saved.");
    } else {
        println!("  Label metadata saved (crawl_complete=false due to failures).");
    }
    println!();
    Ok(())
}

/// Prints the crawl summary.
#[allow(clippy::too_many_arguments)]
pub fn print_summary(
    total_start: std::time::Instant,
    new_count: usize,
    existing_count: usize,
    existing_success_count: usize,
    had_failures: bool,
    cleanup_failed: bool,
    existing_file_failures_count: usize,
    pipeline_failures_count: usize,
) {
    let total_elapsed = total_start.elapsed();
    if had_failures || cleanup_failed {
        println!("⚠️  Crawl completed with errors!");
        println!(
            "  Total time: {}",
            format_duration(total_elapsed.as_secs_f64())
        );
        println!("  New files indexed: {}", new_count);
        println!("  Existing files detected: {}", existing_count);
        println!(
            "  Existing files updated successfully: {}",
            existing_success_count
        );
        let total_failures = pipeline_failures_count + existing_file_failures_count;
        println!("  Total failures: {}", total_failures);
        if existing_file_failures_count > 0 {
            println!(
                "  - Existing file label-add failures: {}",
                existing_file_failures_count
            );
        }
        if cleanup_failed {
            println!("  - Label cleanup failed (crawl not marked complete)");
        }
        println!();
        println!("  This crawl is marked as incomplete. Re-run to complete indexing.");
    } else {
        println!("✅ Crawl complete!");
        println!(
            "  Total time: {}",
            format_duration(total_elapsed.as_secs_f64())
        );
        println!("  New files indexed: {}", new_count);
        println!("  Existing files detected: {}", existing_count);
        println!(
            "  Existing files updated successfully: {}",
            existing_success_count
        );
    }
}

/// Formats the retrieval selection for display in the crawl preamble.
///
/// Returns a string like "(fts, vector)", "(fts only, no vector)", "(vector only, no fts)",
/// or "(no retrieval methods)" for empty selection.
pub fn format_selection_for_display(selection: &BTreeSet<RetrievalMethod>) -> String {
    let has_vector = selection.contains(&RetrievalMethod::Vector);
    let has_fts = selection.contains(&RetrievalMethod::Fts);

    match (has_fts, has_vector) {
        (true, true) => "(fts, vector)".to_string(),
        (true, false) => "(fts only, no vector)".to_string(),
        (false, true) => "(vector only, no fts)".to_string(),
        (false, false) => "(no retrieval methods)".to_string(),
    }
}

/// Prints the selection-narrowing announcement if applicable.
///
/// This should be called after the previous label metadata has been read,
/// which happens after storage is opened. The announcement prints separately
/// from the main preamble (which prints before storage is open).
pub fn print_narrowing_announcement(
    previous_selection: &BTreeSet<RetrievalMethod>,
    new_selection: &BTreeSet<RetrievalMethod>,
) {
    // Only print if this is a strict narrowing (previous is a strict superset of new)
    if previous_selection.is_superset(new_selection) && previous_selection != new_selection {
        let has_fts = new_selection.contains(&RetrievalMethod::Fts);
        let has_vector = new_selection.contains(&RetrievalMethod::Vector);

        match (has_fts, has_vector) {
            (true, false) => {
                println!();
                println!("👉 This crawl narrows retrieval selection to fts only, no vector.");
                println!(
                    "   Any vector data from a previous crawl is preserved and will be reused"
                );
                println!("   if you re-add vector to the selection.");
            }
            (false, true) => {
                println!();
                println!("👉 This crawl narrows retrieval selection to vector only, no fts.");
                println!("   Any fts data from a previous crawl is preserved and will be reused");
                println!("   if you re-add fts to the selection.");
            }
            // Empty selection or other narrowing combinations are not expected in practice,
            // but if they occur, we don't print a misleading message.
            _ => {}
        }
    }
}

/// Saves warning state to disk.
pub fn save_warning_state(
    db_path: &std::path::Path,
    catalog_name: &str,
    crawl_warning_files: &HashSet<String>,
    prior_warning_files: &HashSet<String>,
    incremental_warnings: bool,
) -> Result<Vec<String>> {
    let mut next_warning_files: HashSet<String> = HashSet::new();
    next_warning_files.extend(crawl_warning_files.iter().cloned());
    if incremental_warnings {
        next_warning_files.extend(prior_warning_files.iter().cloned());
    }
    let mut sorted_warning_files: Vec<String> = next_warning_files.iter().cloned().collect();
    sorted_warning_files.sort();
    crate::app::save_warning_state(db_path, catalog_name, &sorted_warning_files)?;
    Ok(sorted_warning_files)
}

/// Prints the warning summary.
pub fn print_warning_summary(crawl_warning_files: &HashSet<String>) {
    if crawl_warning_files.is_empty() {
        return;
    }
    let mut sorted: Vec<&String> = crawl_warning_files.iter().collect();
    sorted.sort();
    let plural = if sorted.len() == 1 { "file" } else { "files" };
    println!();
    println!("Chunking warnings in {} {}:", sorted.len(), plural);
    for file in sorted.iter().take(20) {
        println!("  - {}", file);
    }
    if sorted.len() > 20 {
        println!("  ... and {} more", sorted.len() - 20);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_selection(methods: &[RetrievalMethod]) -> BTreeSet<RetrievalMethod> {
        methods.iter().cloned().collect()
    }

    #[test]
    fn test_format_selection_both_methods() {
        let selection = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        assert_eq!(format_selection_for_display(&selection), "(fts, vector)");
    }

    #[test]
    fn test_format_selection_fts_only() {
        let selection = make_selection(&[RetrievalMethod::Fts]);
        assert_eq!(
            format_selection_for_display(&selection),
            "(fts only, no vector)"
        );
    }

    #[test]
    fn test_format_selection_vector_only() {
        let selection = make_selection(&[RetrievalMethod::Vector]);
        assert_eq!(
            format_selection_for_display(&selection),
            "(vector only, no fts)"
        );
    }

    #[test]
    fn test_format_selection_empty() {
        let selection: BTreeSet<RetrievalMethod> = BTreeSet::new();
        assert_eq!(
            format_selection_for_display(&selection),
            "(no retrieval methods)"
        );
    }

    #[test]
    fn test_narrowing_announcement_fts_only() {
        // Previous: both methods, New: fts only -> should print narrowing announcement
        let previous = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        let new = make_selection(&[RetrievalMethod::Fts]);
        // This test verifies the function doesn't panic; the actual output goes to stdout
        print_narrowing_announcement(&previous, &new);
    }

    #[test]
    fn test_narrowing_announcement_vector_only() {
        // Previous: both methods, New: vector only -> should print narrowing announcement
        let previous = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        let new = make_selection(&[RetrievalMethod::Vector]);
        print_narrowing_announcement(&previous, &new);
    }

    #[test]
    fn test_narrowing_announcement_no_narrowing_same() {
        // Previous and new are the same -> no announcement
        let previous = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        let new = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        print_narrowing_announcement(&previous, &new);
    }

    #[test]
    fn test_narrowing_announcement_no_narrowing_widening() {
        // Previous: fts only, New: both -> widening, not narrowing
        let previous = make_selection(&[RetrievalMethod::Fts]);
        let new = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        print_narrowing_announcement(&previous, &new);
    }

    #[test]
    fn test_narrowing_announcement_first_crawl() {
        // Previous: empty (first crawl), New: both -> no narrowing announcement
        let previous: BTreeSet<RetrievalMethod> = BTreeSet::new();
        let new = make_selection(&[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        print_narrowing_announcement(&previous, &new);
    }
}
