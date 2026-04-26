//! Handler for the `crawl` command.
//!
//! Purpose: Crawl a repository and index chunks into LanceDB.
//! Edit here when: Modifying crawl entry points, label creation, or storage interactions.
//! Do not edit here for: Embed/upload pipeline (see ../crawl/pipeline.rs), crawl types (see ../crawl/types.rs).

use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;

use crate::app::{
    Config, format_duration, load_warning_state, resolve_database_path, run_embed_upload_pipeline,
    save_warning_state, validate_config_path,
};
use crate::engine::{
    chunker::{ChunkContext, chunk_content},
    crawl_config::load_compiled_crawl_config,
    git_ops::{
        build_package_index_for_commit, build_package_index_for_working_dir, enumerate_commit_tree,
        enumerate_working_directory, read_blob_content, read_working_file_content,
        resolve_commit_oid,
    },
    identifier::LabelId,
    storage::{ChunkStorage, Database, LabelMetadataRow},
};

/// Run crawl for a git commit label
#[allow(clippy::too_many_arguments)]
pub fn run_crawl_label(
    config: &Config,
    catalog_name: &str,
    label: &str,
    commit: &str,
    incremental_warnings: bool,
    debug: bool,
) -> Result<()> {
    let total_start = std::time::Instant::now();
    println!("🔍 Starting label-aware crawl...");
    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label);

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

    // Load persisted chunking warning files (sticky by default)
    let prior_warning_files = load_warning_state(&db_path, catalog_name);
    if !prior_warning_files.is_empty() {
        println!(
            "Found {} files with prior chunking warnings",
            prior_warning_files.len()
        );
    }
    println!();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_crawl_label_async(
        config,
        catalog_name,
        label,
        commit,
        incremental_warnings,
        &repo_path,
        &label_id,
        &crawl_config,
        &prior_warning_files,
        &db_path,
        total_start,
        debug,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_crawl_label_async(
    config: &Config,
    catalog_name: &str,
    label: &str,
    commit: &str,
    incremental_warnings: bool,
    repo_path: &std::path::Path,
    label_id: &LabelId,
    crawl_config: &crate::engine::crawl_config::CompiledCrawlConfig,
    prior_warning_files: &HashSet<String>,
    db_path: &std::path::Path,
    total_start: std::time::Instant,
    debug: bool,
) -> Result<()> {
    // Open database and get storage handles
    let db = Database::open(db_path).await?;
    if debug {
        println!("[DEBUG] Opened database at: {}", db_path.display());
    }
    let chunk_storage = Arc::new(db.chunks_storage().await?);
    let label_storage = Arc::new(db.label_storage().await?);
    if debug {
        println!("[DEBUG] Opened chunks and label_metadata tables");
    }

    // Step 1: Resolve commit to full SHA and write in-progress metadata
    println!("📦 Resolving commit...");
    let commit_oid = resolve_commit_oid(repo_path, commit)?;
    println!("Resolved {} to {}", commit, &commit_oid[..12]);

    // Write in-progress metadata before any work begins
    let in_progress_metadata = LabelMetadataRow {
        label_id: label_id.to_string(),
        catalog: catalog_name.to_string(),
        label: label.to_string(),
        commit_oid: commit_oid.clone(),
        source_kind: "git-commit".to_string(),
        crawl_complete: false,
        updated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    };
    label_storage.upsert(&in_progress_metadata).await?;
    if debug {
        println!("[DEBUG] Wrote in-progress label metadata: {}", label_id);
    }

    let files = enumerate_commit_tree(repo_path, commit)?;
    println!("Found {} files in commit tree", files.len());

    // Step 2: Build package index for this commit
    println!("📦 Building package index...");
    let package_index = build_package_index_for_commit(repo_path, commit)?;
    println!("Package index built successfully");
    println!();

    // Step 3: Filter files using crawl config
    println!("📂 Filtering files...");
    let files_to_process: Vec<_> = files
        .iter()
        .filter(|f| crawl_config.should_crawl(&f.relative_path))
        .cloned()
        .collect();
    println!(
        "{} files to process after filtering",
        files_to_process.len()
    );
    println!();

    // Step 4: Process each file - check for existing chunks, then embed if needed
    println!("⚡ Phase 1: Checking existing chunks and collecting new files...");

    let mut new_files: Vec<(String, String)> = Vec::new(); // (relative_path, blob_id)
    let mut existing_files_needing_label: HashSet<String> = HashSet::new();
    let mut new_count = 0;
    let mut existing_count = 0;

    for file_entry in &files_to_process {
        // When incremental_warnings is false and file had prior warning, force reprocessing
        let force_reprocess =
            !incremental_warnings && prior_warning_files.contains(&file_entry.relative_path);

        let file_id = crate::engine::util::compute_file_id(
            crate::engine::util::EMBEDDER_ID,
            crate::engine::util::CHUNKER_ID,
            &file_entry.blob_id,
            &file_entry.relative_path,
        );

        if force_reprocess {
            // Treat as new file to re-chunk and re-index
            new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
            new_count += 1;
            continue;
        }

        // Check if sentinel exists and is complete
        let sentinel_point_id = format!("{}:1", file_id);
        match chunk_storage.get_by_point_id(&sentinel_point_id).await {
            Ok(Some(chunk)) => {
                // Check if file crawl was completed (file_complete == true)
                if !chunk.file_complete {
                    // Incomplete file - treat as new file to re-crawl
                    new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
                    new_count += 1;
                    continue;
                }
                // File already indexed - add to existing files list.
                // We do NOT short-circuit based on sentinel's active_label_ids because
                // partial label coverage is possible (some chunks may be missing the label).
                // The label-add phase will visit all chunks and update as needed.
                existing_files_needing_label.insert(file_id);
                existing_count += 1;
            }
            Ok(None) => {
                new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
                new_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "  ⚠️  Error checking sentinel for {}: {}",
                    file_entry.relative_path, e
                );
                new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
                new_count += 1;
            }
        }
    }

    println!("  New files to index: {}", new_count);
    println!("  Existing files (label update only): {}", existing_count);
    println!();

    // Step 5: Add label to existing files that need it
    let mut label_add_success_files: HashSet<String> = HashSet::new();
    let mut existing_file_label_add_failures: Vec<String> = Vec::new();
    if !existing_files_needing_label.is_empty() {
        println!(
            "🏷️  Adding label to {} existing files...",
            existing_files_needing_label.len()
        );
        for file_id in &existing_files_needing_label {
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
                            .update_active_labels(&chunk.point_id, &new_labels)
                            .await
                        {
                            eprintln!(
                                "  ❌ Failed to add label to chunk {}: {}",
                                chunk.point_id, e
                            );
                            existing_file_label_add_failures.push(format!("{}: {}", file_id, e));
                            file_had_failures = true;
                        }
                    }
                    if !file_had_failures {
                        label_add_success_files.insert(file_id.clone());
                    }
                }
                Err(e) => {
                    eprintln!("  ❌ Failed to get chunks for file {}: {}", file_id, e);
                    existing_file_label_add_failures.push(format!("{}: {}", file_id, e));
                }
            }
        }
        println!("  Done.");
        if !existing_file_label_add_failures.is_empty() {
            println!(
                "  ⚠️  Failed to add label to {} existing files",
                existing_file_label_add_failures.len()
            );
        }
        println!();
    }
    // existing_files now equals label_add_success_files since we no longer
    // short-circuit files that already have the label on the sentinel
    let existing_files = label_add_success_files;

    // Step 6: Index new files
    let mut all_chunks: Vec<crate::engine::Chunk> = Vec::new();
    let mut touched_file_ids: HashSet<String> = HashSet::new();
    let mut crawl_warning_files: HashSet<String> = HashSet::new();
    let mut warning_count: usize = 0;

    if !new_files.is_empty() {
        println!("📦 Phase 2: Chunking {} new files...", new_count);

        for (idx, (relative_path, blob_id)) in new_files.iter().enumerate() {
            print!(
                "\r  Processing file {}/{} ({:.0}%) | warnings: {}   ",
                idx + 1,
                new_count,
                ((idx + 1) as f64 / new_count as f64) * 100.0,
                warning_count
            );
            std::io::Write::flush(&mut std::io::stdout())?;

            let content = match read_blob_content(repo_path, blob_id) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "\n  ⚠️  Failed to read blob {} for {}: {}",
                        &blob_id[..8],
                        relative_path,
                        e
                    );
                    continue;
                }
            };

            let content_str = match String::from_utf8(content) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let package_name = package_index
                .find_package_name(relative_path)
                .unwrap_or(catalog_name)
                .to_string();

            let ctx = ChunkContext {
                catalog: catalog_name.to_string(),
                label_id: label_id.to_string(),
                package_name,
                relative_path: relative_path.clone(),
                blob_id: blob_id.clone(),
                source_uri: format!("{}/{}", repo_path.display(), relative_path),
            };

            let strategy = crawl_config.get_strategy(relative_path);
            match chunk_content(&content_str, &ctx, 6000, strategy) {
                Ok(chunks) => {
                    // Detect fallback warning: chunk_kind == "fallback-split"
                    let had_warning = chunks.iter().any(|c| c.chunk_kind == "fallback-split");
                    if had_warning {
                        warning_count += 1;
                        crawl_warning_files.insert(relative_path.clone());
                        println!();
                        println!("Warning: Couldn't find a splitpoint for {}", relative_path);
                    }

                    if !chunks.is_empty() {
                        touched_file_ids.insert(chunks[0].file_id.clone());
                    }
                    all_chunks.extend(chunks);
                }
                Err(e) => {
                    eprintln!("\n  ⚠️  Failed to chunk {}: {}", relative_path, e);
                }
            }
        }

        let total_chunks = all_chunks.len();
        println!("\n  Found {} chunks to embed", total_chunks);
        println!();
    }

    // Phase 3: Run the embed/upload pipeline
    let (pipeline_file_ids, pipeline_failures) = run_embed_upload_pipeline(
        all_chunks,
        Arc::clone(&chunk_storage),
        &config.embedding_model,
    )
    .await?;

    touched_file_ids.extend(pipeline_file_ids);

    let has_existing_file_failures = !existing_file_label_add_failures.is_empty();
    let had_failures = pipeline_failures.has_failures() || has_existing_file_failures;

    // Step 7: Label reassignment cleanup
    let mut cleanup_failed = false;
    if had_failures {
        println!("🧹 Phase 4: SKIPPING label reassignment cleanup (crawl had failures)");
        println!("  This is intentional - cleanup should only run after successful crawls.");
        println!("  Run the crawl again to complete indexing and trigger cleanup.");
    } else {
        println!("🧹 Phase 4: Label reassignment cleanup...");
        let all_touched: HashSet<String> =
            existing_files.union(&touched_file_ids).cloned().collect();

        match remove_label_from_chunks(&chunk_storage, label_id.as_str(), &all_touched).await {
            Ok(processed) => {
                println!("  Processed {} chunks for label cleanup", processed);
            }
            Err(e) => {
                eprintln!("  ❌ Label cleanup failed: {}", e);
                cleanup_failed = true;
            }
        }
    }
    println!();

    // Step 8: Update label metadata
    println!("📝 Updating label metadata...");
    let crawl_complete = !had_failures && !cleanup_failed;
    let metadata = LabelMetadataRow {
        label_id: label_id.to_string(),
        catalog: catalog_name.to_string(),
        label: label.to_string(),
        commit_oid: commit_oid.clone(),
        source_kind: "git-commit".to_string(),
        crawl_complete,
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

    // Summary
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
            existing_files.len()
        );
        let total_failures = pipeline_failures.total() + existing_file_label_add_failures.len();
        println!("  Total failures: {}", total_failures);
        if has_existing_file_failures {
            println!(
                "  - Existing file label-add failures: {}",
                existing_file_label_add_failures.len()
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
            existing_files.len()
        );
    }

    // Save warning state
    let mut next_warning_files: HashSet<String> = HashSet::new();
    next_warning_files.extend(crawl_warning_files.iter().cloned());
    if incremental_warnings {
        next_warning_files.extend(prior_warning_files.iter().cloned());
    }
    let mut sorted_warning_files: Vec<String> = next_warning_files.iter().cloned().collect();
    sorted_warning_files.sort();
    save_warning_state(db_path, catalog_name, &sorted_warning_files)?;

    // Warning summary
    if !crawl_warning_files.is_empty() {
        let mut sorted_summary: Vec<&String> = crawl_warning_files.iter().collect();
        sorted_summary.sort();
        let plural = if sorted_summary.len() == 1 {
            "file"
        } else {
            "files"
        };
        println!();
        println!("Chunking warnings in {} {}:", sorted_summary.len(), plural);
        for file in sorted_summary.iter().take(20) {
            println!("  - {}", file);
        }
        if sorted_summary.len() > 20 {
            println!("  ... and {} more", sorted_summary.len() - 20);
        }
    }

    if had_failures || cleanup_failed {
        anyhow::bail!("Crawl completed with errors (see above). Label marked incomplete.");
    }

    Ok(())
}

/// Remove a label from chunks where it's in active_label_ids, excluding specified files.
///
/// This scans all chunks with the label and removes the label from active_label_ids.
/// If active_label_ids becomes empty, the chunk is deleted.
async fn remove_label_from_chunks(
    chunk_storage: &ChunkStorage,
    label_id: &str,
    exclude_file_ids: &HashSet<String>,
) -> Result<u64> {
    let mut processed: u64 = 0;

    // Get all chunks with this label
    let chunks = chunk_storage.get_chunks_for_label(label_id, None).await?;

    for chunk in chunks {
        // Skip if this file was touched in the current crawl
        if exclude_file_ids.contains(&chunk.file_id) {
            continue;
        }

        // Remove label from active_label_ids
        let mut new_labels = chunk.active_label_ids.clone();
        new_labels.retain(|l| l != label_id);

        if new_labels.is_empty() {
            // Delete the chunk
            chunk_storage
                .delete_by_point_ids(std::slice::from_ref(&chunk.point_id))
                .await?;
        } else {
            // Update active_label_ids
            chunk_storage
                .update_active_labels(&chunk.point_id, &new_labels)
                .await?;
        }

        processed += 1;
    }

    Ok(processed)
}

/// Run crawl for working directory (indexes uncommitted changes)
#[allow(clippy::too_many_arguments)]
pub fn run_crawl_working_dir(
    config: &Config,
    catalog_name: &str,
    label: &str,
    incremental_warnings: bool,
    debug: bool,
) -> Result<()> {
    let total_start = std::time::Instant::now();
    println!("🔍 Starting working directory crawl...");
    println!("Catalog: {}", catalog_name);
    println!("Label: {}", label);

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

    // Load persisted chunking warning files (sticky by default)
    let prior_warning_files = load_warning_state(&db_path, catalog_name);
    if !prior_warning_files.is_empty() {
        println!(
            "Found {} files with prior chunking warnings",
            prior_warning_files.len()
        );
    }
    println!();

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_crawl_working_dir_async(
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
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_crawl_working_dir_async(
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
) -> Result<()> {
    // Open database and get storage handles
    let db = Database::open(db_path).await?;
    if debug {
        println!("[DEBUG] Opened database at: {}", db_path.display());
    }
    let chunk_storage = Arc::new(db.chunks_storage().await?);
    let label_storage = Arc::new(db.label_storage().await?);
    if debug {
        println!("[DEBUG] Opened chunks and label_metadata tables");
    }

    // Write in-progress metadata
    let in_progress_metadata = LabelMetadataRow {
        label_id: label_id.to_string(),
        catalog: catalog_name.to_string(),
        label: label.to_string(),
        commit_oid: "".to_string(), // No commit for working directory
        source_kind: "working-directory".to_string(),
        crawl_complete: false,
        updated_at_unix_secs: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64,
    };
    label_storage.upsert(&in_progress_metadata).await?;

    // Enumerate working directory files
    println!("📂 Enumerating working directory...");
    let files = enumerate_working_directory(repo_path)?;
    println!(
        "Found {} files in working directory (before crawl config filtering)",
        files.len()
    );
    println!();

    // Build package index from working directory
    println!("📦 Building package index...");
    let package_index = build_package_index_for_working_dir(repo_path)?;
    println!("Package index built successfully");
    println!();

    // Filter files using compiled crawl config
    println!("📂 Filtering files...");
    let files_to_process: Vec<_> = files
        .iter()
        .filter(|f| crawl_config.should_crawl(&f.relative_path))
        .cloned()
        .collect();
    println!(
        "{} files to process after filtering",
        files_to_process.len()
    );
    println!();

    // Check for existing chunks and collect new files
    println!("⚡ Phase 1: Checking existing chunks and collecting new files...");

    let mut new_files: Vec<(String, String)> = Vec::new(); // (relative_path, blob_id)
    let mut existing_files_needing_label: HashSet<String> = HashSet::new();
    let mut new_count = 0;
    let mut existing_count = 0;

    for file_entry in &files_to_process {
        // When incremental_warnings is false and file had prior warning, force reprocessing
        let force_reprocess =
            !incremental_warnings && prior_warning_files.contains(&file_entry.relative_path);

        let file_id = crate::engine::util::compute_file_id(
            crate::engine::util::EMBEDDER_ID,
            crate::engine::util::CHUNKER_ID,
            &file_entry.blob_id,
            &file_entry.relative_path,
        );

        if force_reprocess {
            // Treat as new file to re-chunk and re-index
            new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
            new_count += 1;
            continue;
        }

        let sentinel_point_id = format!("{}:1", file_id);
        match chunk_storage.get_by_point_id(&sentinel_point_id).await {
            Ok(Some(chunk)) => {
                // Check if file crawl was completed (file_complete == true)
                if !chunk.file_complete {
                    // Incomplete file - treat as new file to re-crawl
                    new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
                    new_count += 1;
                    continue;
                }
                // File already indexed - add to existing files list.
                // We do NOT short-circuit based on sentinel's active_label_ids because
                // partial label coverage is possible (some chunks may be missing the label).
                // The label-add phase will visit all chunks and update as needed.
                existing_files_needing_label.insert(file_id);
                existing_count += 1;
            }
            Ok(None) => {
                new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
                new_count += 1;
            }
            Err(e) => {
                eprintln!(
                    "  ⚠️  Error checking sentinel for {}: {}",
                    file_entry.relative_path, e
                );
                new_files.push((file_entry.relative_path.clone(), file_entry.blob_id.clone()));
                new_count += 1;
            }
        }
    }

    println!("  New files to index: {}", new_count);
    println!("  Existing files (label update only): {}", existing_count);
    println!();

    // Add label to existing files that need it
    let mut label_add_success_files: HashSet<String> = HashSet::new();
    let mut existing_file_label_add_failures: Vec<String> = Vec::new();
    if !existing_files_needing_label.is_empty() {
        println!(
            "🏷️  Adding label to {} existing files...",
            existing_files_needing_label.len()
        );
        for file_id in &existing_files_needing_label {
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
                            .update_active_labels(&chunk.point_id, &new_labels)
                            .await
                        {
                            eprintln!(
                                "  ❌ Failed to add label to chunk {}: {}",
                                chunk.point_id, e
                            );
                            existing_file_label_add_failures.push(format!("{}: {}", file_id, e));
                            file_had_failures = true;
                        }
                    }
                    if !file_had_failures {
                        label_add_success_files.insert(file_id.clone());
                    }
                }
                Err(e) => {
                    eprintln!("  ❌ Failed to get chunks for file {}: {}", file_id, e);
                    existing_file_label_add_failures.push(format!("{}: {}", file_id, e));
                }
            }
        }
        println!("  Done.");
        if !existing_file_label_add_failures.is_empty() {
            println!(
                "  ⚠️  Failed to add label to {} existing files",
                existing_file_label_add_failures.len()
            );
        }
        println!();
    }
    // existing_files now equals label_add_success_files since we no longer
    // short-circuit files that already have the label on the sentinel
    let existing_files = label_add_success_files;

    // Index new files
    let mut all_chunks: Vec<crate::engine::Chunk> = Vec::new();
    let mut touched_file_ids: HashSet<String> = HashSet::new();
    let mut crawl_warning_files: HashSet<String> = HashSet::new();
    let mut warning_count: usize = 0;

    if !new_files.is_empty() {
        println!("📦 Phase 2: Chunking {} new files...", new_count);

        for (idx, (relative_path, blob_id)) in new_files.iter().enumerate() {
            print!(
                "\r  Processing file {}/{} ({:.0}%) | warnings: {}   ",
                idx + 1,
                new_count,
                ((idx + 1) as f64 / new_count as f64) * 100.0,
                warning_count
            );
            std::io::Write::flush(&mut std::io::stdout())?;

            let content = match read_working_file_content(repo_path, relative_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("\n  ⚠️  Failed to read {}: {}", relative_path, e);
                    continue;
                }
            };

            let content_str = match String::from_utf8(content) {
                Ok(s) => s,
                Err(_) => continue,
            };

            let package_name = package_index
                .find_package_name(relative_path)
                .unwrap_or(catalog_name)
                .to_string();

            let ctx = ChunkContext {
                catalog: catalog_name.to_string(),
                label_id: label_id.to_string(),
                package_name,
                relative_path: relative_path.clone(),
                blob_id: blob_id.clone(),
                source_uri: format!("{}/{}", repo_path.display(), relative_path),
            };

            let strategy = crawl_config.get_strategy(relative_path);
            match chunk_content(&content_str, &ctx, 6000, strategy) {
                Ok(chunks) => {
                    // Detect fallback warning: chunk_kind == "fallback-split"
                    let had_warning = chunks.iter().any(|c| c.chunk_kind == "fallback-split");
                    if had_warning {
                        warning_count += 1;
                        crawl_warning_files.insert(relative_path.clone());
                        println!();
                        println!("Warning: Couldn't find a splitpoint for {}", relative_path);
                    }

                    if !chunks.is_empty() {
                        touched_file_ids.insert(chunks[0].file_id.clone());
                    }
                    all_chunks.extend(chunks);
                }
                Err(e) => {
                    eprintln!("\n  ⚠️  Failed to chunk {}: {}", relative_path, e);
                }
            }
        }

        let total_chunks = all_chunks.len();
        println!("\n  Found {} chunks to embed", total_chunks);
        println!();
    }

    // Phase 3: Run the embed/upload pipeline
    let (pipeline_file_ids, pipeline_failures) = run_embed_upload_pipeline(
        all_chunks,
        Arc::clone(&chunk_storage),
        &config.embedding_model,
    )
    .await?;

    touched_file_ids.extend(pipeline_file_ids);

    let has_existing_file_failures = !existing_file_label_add_failures.is_empty();
    let had_failures = pipeline_failures.has_failures() || has_existing_file_failures;

    // Step 7: Label reassignment cleanup
    let mut cleanup_failed = false;
    if had_failures {
        println!("🧹 Phase 4: SKIPPING label reassignment cleanup (crawl had failures)");
        println!("  This is intentional - cleanup should only run after successful crawls.");
        println!("  Run the crawl again to complete indexing and trigger cleanup.");
    } else {
        println!("🧹 Phase 4: Label reassignment cleanup...");
        let all_touched: HashSet<String> =
            existing_files.union(&touched_file_ids).cloned().collect();

        match remove_label_from_chunks(&chunk_storage, label_id.as_str(), &all_touched).await {
            Ok(processed) => println!("  Processed {} chunks for label cleanup", processed),
            Err(e) => {
                eprintln!("  ❌ Label cleanup failed: {}", e);
                cleanup_failed = true;
            }
        }
    }
    println!();

    // Update label metadata
    println!("📝 Updating label metadata...");
    let crawl_complete = !had_failures && !cleanup_failed;
    let metadata = LabelMetadataRow {
        label_id: label_id.to_string(),
        catalog: catalog_name.to_string(),
        label: label.to_string(),
        commit_oid: "".to_string(),
        source_kind: "working-directory".to_string(),
        crawl_complete,
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

    let total_elapsed = total_start.elapsed();
    if had_failures || cleanup_failed {
        println!("⚠️  Working directory crawl completed with errors!");
        println!(
            "  Total time: {}",
            format_duration(total_elapsed.as_secs_f64())
        );
        println!("  New files indexed: {}", new_count);
        println!("  Existing files detected: {}", existing_count);
        println!(
            "  Existing files updated successfully: {}",
            existing_files.len()
        );
        let total_failures = pipeline_failures.total() + existing_file_label_add_failures.len();
        println!("  Total failures: {}", total_failures);
        if has_existing_file_failures {
            println!(
                "  - Existing file label-add failures: {}",
                existing_file_label_add_failures.len()
            );
        }
        if cleanup_failed {
            println!("  - Label cleanup failed (crawl not marked complete)");
        }
        println!();
        println!("  This crawl is marked as incomplete. Re-run to complete indexing.");
    } else {
        println!("✅ Working directory crawl complete!");
        println!(
            "  Total time: {}",
            format_duration(total_elapsed.as_secs_f64())
        );
        println!("  New files indexed: {}", new_count);
        println!("  Existing files detected: {}", existing_count);
        println!(
            "  Existing files updated successfully: {}",
            existing_files.len()
        );
    }

    // Save warning state
    let mut next_warning_files: HashSet<String> = HashSet::new();
    next_warning_files.extend(crawl_warning_files.iter().cloned());
    if incremental_warnings {
        next_warning_files.extend(prior_warning_files.iter().cloned());
    }
    let mut sorted_warning_files: Vec<String> = next_warning_files.iter().cloned().collect();
    sorted_warning_files.sort();
    save_warning_state(db_path, catalog_name, &sorted_warning_files)?;

    // Warning summary
    if !crawl_warning_files.is_empty() {
        let mut sorted_summary: Vec<&String> = crawl_warning_files.iter().collect();
        sorted_summary.sort();
        let plural = if sorted_summary.len() == 1 {
            "file"
        } else {
            "files"
        };
        println!();
        println!("Chunking warnings in {} {}:", sorted_summary.len(), plural);
        for file in sorted_summary.iter().take(20) {
            println!("  - {}", file);
        }
        if sorted_summary.len() > 20 {
            println!("  ... and {} more", sorted_summary.len() - 20);
        }
    }

    if had_failures || cleanup_failed {
        anyhow::bail!("Crawl completed with errors (see above). Label marked incomplete.");
    }

    Ok(())
}
