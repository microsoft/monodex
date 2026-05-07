//! Embed and upload pipeline for crawl processing.
//!
//! Purpose: Coordinate parallel embedding and LanceDB storage writes.
//! Edit here when: Modifying how chunks are embedded and stored.
//! Do not edit here for: Storage operations (see engine/storage/), CLI handlers (see app/commands/).

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, unbounded};
use rayon::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::app::{
    CrawlFailures, EmbeddingModelConfig, chrono_timestamp, format_duration, format_eta,
    print_memory_warning, resolve_embedding_config,
};
use crate::engine::storage::{ChunkRow, ChunkStorage};
use crate::engine::{Chunk, ParallelEmbedder};

/// Run the embedding and storage pipeline with progress reporting.
///
/// Returns (touched_file_ids, failures) for the crawl.
///
/// # Errors
///
/// Returns an error immediately on any storage failure (disk full, corruption, etc.).
/// Embedding failures are tracked per-chunk and returned in CrawlFailures.
pub async fn run_embed_upload_pipeline(
    all_chunks: Vec<Chunk>,
    chunk_storage: Arc<ChunkStorage>,
    embedding_config: &EmbeddingModelConfig,
) -> Result<(HashSet<String>, CrawlFailures)> {
    let mut touched_file_ids: HashSet<String> = HashSet::new();
    let failures = CrawlFailures::default();

    if all_chunks.is_empty() {
        return Ok((touched_file_ids, failures));
    }

    // Track file IDs from chunks
    for chunk in &all_chunks {
        if !chunk.file_id.is_empty() {
            touched_file_ids.insert(chunk.file_id.clone());
        }
    }

    let total_chunks = all_chunks.len();

    // Resolve embedding config (handles "auto" and explicit values)
    let resolved = resolve_embedding_config(embedding_config);

    // Print memory warning before embedding
    print_memory_warning(&resolved);

    // Initialize parallel embedder with resolved config
    let embedder = ParallelEmbedder::with_config(crate::engine::ParallelConfig {
        num_workers: resolved.model_instances,
        intra_threads: resolved.threads_per_instance,
    })?;

    println!(
        "🔶 Phase 3: Embedding {} chunks with {} parallel sessions...",
        total_chunks,
        embedder.num_workers()
    );
    println!("  (Checkpoints every 60s - safe to CTRL+C)");
    let embed_start = std::time::Instant::now();

    /// Type alias for the embedding channel (reduces type complexity)
    type EmbedChannel = (Sender<(Chunk, Vec<f32>)>, Receiver<(Chunk, Vec<f32>)>);

    let (embed_tx, embed_rx): EmbedChannel = unbounded();
    let processed = Arc::new(AtomicUsize::new(0));
    let stop_flag = Arc::new(AtomicBool::new(false));
    let last_upload_time = Arc::new(tokio::sync::Mutex::new(std::time::Instant::now()));

    // Progress reporter thread
    let processed_clone = Arc::clone(&processed);
    let stop_clone = Arc::clone(&stop_flag);
    let last_print_time = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));
    let embed_start_for_thread = std::time::Instant::now();

    let progress_thread = std::thread::spawn(move || {
        while !stop_clone.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let mut last = last_print_time.lock().unwrap();
            if last.elapsed() >= std::time::Duration::from_secs(30) {
                let current = processed_clone.load(Ordering::Relaxed);
                let elapsed = embed_start_for_thread.elapsed();
                let rate = current as f64 / elapsed.as_secs_f64().max(0.001);
                let remaining = (total_chunks - current) as f64 / rate;
                let eta = format_eta(remaining);
                eprintln!(
                    "[{}] Embedded {}/{} ({:.0}%) - {:.1} chunks/sec - ETA: {}",
                    chrono_timestamp(),
                    current,
                    total_chunks,
                    (current as f64 / total_chunks as f64) * 100.0,
                    rate,
                    eta
                );
                *last = std::time::Instant::now();
            }
        }
    });

    // Failure tracking (shared between threads)
    let embedding_failures: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    // Storage writer task (async)
    let stop_writer = Arc::clone(&stop_flag);
    let last_upload_time_clone = Arc::clone(&last_upload_time);
    let chunk_storage_clone = Arc::clone(&chunk_storage);

    let writer_task = tokio::spawn(async move {
        let mut accumulated: Vec<(Chunk, Vec<f32>)> = Vec::new();
        let mut expected_count: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut uploaded_count: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        loop {
            let should_upload = {
                let mut last = last_upload_time_clone.lock().await;
                if last.elapsed() >= std::time::Duration::from_secs(60) {
                    *last = std::time::Instant::now();
                    true
                } else {
                    false
                }
            };

            // Drain embedding results
            while let Ok(embedded) = embed_rx.try_recv() {
                let file_id = embedded.0.file_id.clone();
                if let std::collections::hash_map::Entry::Vacant(e) =
                    expected_count.entry(file_id.clone())
                {
                    e.insert(embedded.0.chunk_count);
                }
                accumulated.push(embedded);
            }

            if should_upload && !accumulated.is_empty() {
                upload_and_mark_complete(
                    &accumulated,
                    &chunk_storage_clone,
                    &mut expected_count,
                    &mut uploaded_count,
                    "Uploading checkpoint",
                )
                .await?;
                accumulated.clear();
            }

            if stop_writer.load(Ordering::Relaxed) && embed_rx.is_empty() {
                // Final upload
                if !accumulated.is_empty() {
                    upload_and_mark_complete(
                        &accumulated,
                        &chunk_storage_clone,
                        &mut expected_count,
                        &mut uploaded_count,
                        "Final upload",
                    )
                    .await?;
                }
                break;
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        Ok::<(), anyhow::Error>(())
    });

    // Parallel embedding
    let processed_clone = Arc::clone(&processed);
    let embedding_failures_clone = Arc::clone(&embedding_failures);
    let num_workers = embedder.num_workers();

    all_chunks
        .into_par_iter()
        .enumerate()
        .for_each(|(idx, chunk)| {
            let worker_index = idx % num_workers;
            match embedder.encode(&chunk.text, worker_index) {
                Ok(embedding) => {
                    let _ = embed_tx.send((chunk, embedding));
                    processed_clone.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    eprintln!(
                        "\n[{}] ❌ Embedding failed for {}:{} - {}",
                        chrono_timestamp(),
                        chunk.relative_path,
                        chunk.chunk_ordinal,
                        e
                    );
                    let mut failures = embedding_failures_clone.lock().unwrap();
                    failures.push(format!(
                        "{}:{}: {}",
                        chunk.relative_path, chunk.chunk_ordinal, e
                    ));
                }
            }
        });

    // Signal completion
    stop_flag.store(true, Ordering::Relaxed);
    progress_thread.join().ok();
    writer_task.await??;

    let embed_elapsed = embed_start.elapsed();
    let rate = total_chunks as f64 / embed_elapsed.as_secs_f64().max(0.001);
    println!(
        "\n  Embedding complete: {} chunks in {} ({:.1} chunks/sec)",
        total_chunks,
        format_duration(embed_elapsed.as_secs_f64()),
        rate
    );

    // Collect failures
    let failures = CrawlFailures {
        embedding_failures: embedding_failures.lock().unwrap().clone(),
    };

    // Report failures
    if failures.has_failures() {
        println!();
        println!(
            "  ⚠️  Encountered {} embedding failures",
            failures.embedding_failures.len()
        );
        println!("      These files may not be searchable. Check logs above for details.");
    }
    println!();

    Ok((touched_file_ids, failures))
}

/// Run the FTS-only upsert pipeline (no embedding).
///
/// This is used for FTS-only crawls where we don't compute embeddings.
/// Chunks are written with NULL vectors. Before upserting, any existing vectors
/// on matched rows are cleared to maintain the per-file vector-presence invariant
/// (all chunks of a file must have the same vector presence when file_complete=true).
///
/// The storage primitive `upsert_without_vectors` preserves vectors at the row level;
/// this function clears vectors separately before calling it.
///
/// Returns (touched_file_ids, failures) for the crawl.
pub async fn run_upsert_without_vectors(
    all_chunks: Vec<Chunk>,
    chunk_storage: Arc<ChunkStorage>,
) -> Result<(HashSet<String>, CrawlFailures)> {
    let mut touched_file_ids: HashSet<String> = HashSet::new();
    let failures = CrawlFailures::default();

    if all_chunks.is_empty() {
        return Ok((touched_file_ids, failures));
    }

    // Track file IDs from chunks
    for chunk in &all_chunks {
        if !chunk.file_id.is_empty() {
            touched_file_ids.insert(chunk.file_id.clone());
        }
    }

    let total_chunks = all_chunks.len();

    println!(
        "🔶 Phase 3: Storing {} chunks (FTS-only, no embedding)...",
        total_chunks
    );
    let start = std::time::Instant::now();

    // Convert chunks to rows
    let rows: Vec<ChunkRow> = all_chunks.iter().map(chunk_to_row).collect();

    // Clear any existing vectors on these rows to maintain the per-file
    // vector-presence invariant. This handles the case where a previous
    // vector-phase crawl was interrupted mid-file, leaving some chunks with
    // vectors and others without. The FTS-only slow path must produce a
    // uniform NULL-vector state for the whole file.
    let row_ids: Vec<&str> = rows.iter().map(|r| r.row_id.as_str()).collect();
    chunk_storage.null_vectors_for_row_ids(&row_ids).await?;

    // Group by file_id for sentinel tracking
    let mut expected_count: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut uploaded_count: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    for row in &rows {
        if let std::collections::hash_map::Entry::Vacant(e) =
            expected_count.entry(row.file_id.clone())
        {
            e.insert(row.chunk_count as usize);
        }
    }

    // Upsert all chunks without vectors
    chunk_storage.upsert_without_vectors(&rows).await?;

    // Mark all chunks as uploaded
    for row in &rows {
        *uploaded_count.entry(row.file_id.clone()).or_insert(0) += 1;
    }

    // Flip sentinels for completed files
    let mut completed_files: Vec<String> = Vec::new();
    for (file_id, expected) in &expected_count {
        let uploaded = uploaded_count.get(file_id).copied().unwrap_or(0);
        if uploaded == *expected {
            completed_files.push(file_id.clone());
        }
    }

    // Set file_complete=true for sentinel rows
    for file_id in &completed_files {
        let sentinel_row_id = format!("{}:1", file_id);
        chunk_storage
            .update_file_complete(&sentinel_row_id, true)
            .await?;
    }

    let elapsed = start.elapsed();
    let rate = total_chunks as f64 / elapsed.as_secs_f64().max(0.001);
    println!(
        "  Storage complete: {} chunks in {} ({:.1} chunks/sec)",
        total_chunks,
        format_duration(elapsed.as_secs_f64()),
        rate
    );
    println!();

    Ok((touched_file_ids, failures))
}

/// Upload accumulated chunks and mark completed files.
///
/// This helper extracts the common upload logic used for both periodic checkpoints
/// and final flush.
async fn upload_and_mark_complete(
    accumulated: &[(Chunk, Vec<f32>)],
    chunk_storage: &ChunkStorage,
    expected_count: &mut std::collections::HashMap<String, usize>,
    uploaded_count: &mut std::collections::HashMap<String, usize>,
    log_message: &str,
) -> Result<()> {
    let count = accumulated.len();
    println!(
        "[{}] {} ({} chunks)...",
        chrono_timestamp(),
        log_message,
        count
    );

    // Convert chunks to ChunkRows and upsert
    let rows: Vec<ChunkRow> = accumulated
        .iter()
        .map(|(chunk, _)| chunk_to_row(chunk))
        .collect();
    let vectors: Vec<Vec<f32>> = accumulated
        .iter()
        .map(|(_, vector)| vector.clone())
        .collect();

    // Upsert chunks with vectors (storage handles batching internally)
    upsert_chunks_with_vectors(chunk_storage, &rows, &vectors).await?;

    // Track uploaded counts and mark files complete
    // Count chunks per file in this batch
    let mut files_in_batch: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (chunk, _) in accumulated {
        *files_in_batch.entry(chunk.file_id.clone()).or_insert(0) += 1;
    }
    for (file_id, cnt) in &files_in_batch {
        *uploaded_count.entry(file_id.clone()).or_insert(0) += cnt;
    }

    // Mark completed files
    for file_id in files_in_batch.keys() {
        let uploaded = uploaded_count.get(file_id).copied().unwrap_or(0);
        let expected = expected_count.get(file_id).copied().unwrap_or(0);
        if uploaded == expected && expected > 0 {
            // Find the sentinel chunk (ordinal 1) and mark it complete
            let row_id = format!("{}:{}", file_id, 1);
            chunk_storage.update_file_complete(&row_id, true).await?;
        }
    }

    Ok(())
}

/// Convert a Chunk to a ChunkRow for storage.
fn chunk_to_row(chunk: &Chunk) -> ChunkRow {
    ChunkRow {
        row_id: chunk.row_id(),
        text: chunk.text.clone(),
        catalog: chunk.catalog.clone(),
        active_label_ids: chunk.active_label_ids.clone(),
        embedder_id: chunk.embedder_id.clone(),
        chunker_id: chunk.chunker_id.clone(),
        blob_id: chunk.blob_id.clone(),
        content_hash: chunk.content_hash.clone(),
        file_id: chunk.file_id.clone(),
        relative_path: chunk.relative_path.clone(),
        package_name: chunk.package_name.clone(),
        source_uri: chunk.source_uri.clone(),
        chunk_ordinal: chunk.chunk_ordinal as i32,
        chunk_count: chunk.chunk_count as i32,
        start_line: chunk.start_line as i32,
        end_line: chunk.end_line as i32,
        symbol_name: chunk.symbol_name.clone(),
        chunk_type: chunk.chunk_type.clone(),
        chunk_kind: chunk.chunk_kind.clone(),
        breadcrumb: if chunk.breadcrumb.is_empty() {
            None
        } else {
            Some(chunk.breadcrumb.clone())
        },
        split_part_ordinal: chunk.split_part_ordinal.map(|n| n as i32),
        split_part_count: chunk.split_part_count.map(|n| n as i32),
        file_complete: false, // Initially false; set to true when all chunks uploaded
    }
}

/// Upsert chunks with their vectors to LanceDB.
///
/// This is a separate function because ChunkRow doesn't include the vector,
/// so we need to construct the RecordBatch with vectors separately.
async fn upsert_chunks_with_vectors(
    storage: &ChunkStorage,
    rows: &[ChunkRow],
    vectors: &[Vec<f32>],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }

    // Persist chunks and their embedding vectors through the storage layer.
    storage.upsert_with_vectors(rows, vectors).await
}
