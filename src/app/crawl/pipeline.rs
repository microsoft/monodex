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
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::app::{
    CrawlFailures, EmbeddingModelConfig, chrono_timestamp, format_count, format_duration,
    format_eta, print_memory_warning, resolve_embedding_config,
};
use crate::engine::storage::{ChunkRow, ChunkStorage, StorageProgressEvent};
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
        format_count(total_chunks as u64),
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
                    format_count(current as u64),
                    format_count(total_chunks as u64),
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
        format_count(total_chunks as u64),
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
            format_count(failures.embedding_failures.len() as u64)
        );
        println!("      These files may not be searchable. Check logs above for details.");
    }
    println!();

    Ok((touched_file_ids, failures))
}

/// Run the FTS-only upsert pipeline (no embedding).
///
/// This is used for FTS-only crawls where we don't compute embeddings.
/// RAII guard for the progress reporter thread.
///
/// Dropping the guard closes the sender channel (causing the reporter's recv to return
/// Disconnected) and joins the reporter thread.
struct ProgressGuard {
    sender: Option<mpsc::Sender<StorageProgressEvent>>,
    handle: Option<JoinHandle<()>>,
}

impl ProgressGuard {
    fn new(sender: mpsc::Sender<StorageProgressEvent>, handle: JoinHandle<()>) -> Self {
        Self {
            sender: Some(sender),
            handle: Some(handle),
        }
    }

    /// Explicitly finish the reporter thread before the guard's natural drop.
    ///
    /// This ensures the progress reporter is fully joined before any subsequent
    /// output (e.g., "Storage complete") on the main thread, preventing stdout/stderr
    /// interleaving. On success, call this before printing final output; on error
    /// paths, the Drop impl still handles cleanup.
    fn finish(mut self) {
        // Drop sender first so the reporter thread's recv returns Disconnected.
        drop(self.sender.take());
        // Join the thread to ensure clean shutdown.
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        // Drop sender first so the reporter thread's recv returns Disconnected.
        drop(self.sender.take());
        // Join the thread to ensure clean shutdown.
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

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
        format_count(total_chunks as u64)
    );
    let start = std::time::Instant::now();

    // Convert chunks to rows
    let rows: Vec<ChunkRow> = all_chunks.iter().map(chunk_to_row).collect();

    // Build sentinel row IDs for completed files only.
    // A file is complete when all its chunks are present (uploaded_count == expected_count).
    // This maintains the invariant that file_complete=true means all chunks exist.
    use std::collections::HashMap;

    // Build expected count per file (all chunks of a file have the same chunk_count)
    let mut expected_count: HashMap<String, usize> = HashMap::new();
    for row in &rows {
        if !row.file_id.is_empty() {
            expected_count
                .entry(row.file_id.clone())
                .or_insert(row.chunk_count as usize);
        }
    }

    // Build uploaded count per file
    let mut uploaded_count: HashMap<String, usize> = HashMap::new();
    for row in &rows {
        if !row.file_id.is_empty() {
            *uploaded_count.entry(row.file_id.clone()).or_insert(0) += 1;
        }
    }

    // Debug assert the invariant: all files have complete chunk sets
    for file_id in expected_count.keys().chain(uploaded_count.keys()) {
        let expected = expected_count.get(file_id).copied().unwrap_or(0);
        let uploaded = uploaded_count.get(file_id).copied().unwrap_or(0);
        debug_assert_eq!(
            expected, uploaded,
            "File {} has incomplete chunks: expected {}, got {}",
            file_id, expected, uploaded
        );
    }

    // Build sentinel row IDs only for complete files
    let sentinel_row_ids: Vec<String> = expected_count
        .keys()
        .filter(|file_id| {
            let expected = expected_count.get(*file_id).copied().unwrap_or(0);
            let uploaded = uploaded_count.get(*file_id).copied().unwrap_or(0);
            expected == uploaded
        })
        .map(|file_id| format!("{}:1", file_id))
        .collect();

    // Set up progress reporter thread
    let (tx, rx): (
        mpsc::Sender<StorageProgressEvent>,
        mpsc::Receiver<StorageProgressEvent>,
    ) = mpsc::channel();

    let reporter_handle = thread::spawn(move || {
        let mut state = ReporterState {
            last_printed_at: None,
            last_printed_phase: None,
        };

        // First event: blocking recv
        let first_event = match rx.recv() {
            Ok(event) => event,
            Err(_) => return, // Channel disconnected immediately
        };

        // Collect batch: first event + all drained events (FIFO order)
        let mut batch = vec![first_event];
        while let Ok(event) = rx.try_recv() {
            batch.push(event);
        }

        // Decide and print
        let to_print = decide_prints(&batch, &mut state, Instant::now());
        for event in to_print {
            print_progress_event(&event);
        }

        // Main loop: timeout-based for cadence gating
        loop {
            match rx.recv_timeout(Duration::from_secs(10)) {
                Ok(event) => {
                    // Collect batch: first event + all drained events (FIFO order)
                    let mut batch = vec![event];
                    while let Ok(newer) = rx.try_recv() {
                        batch.push(newer);
                    }

                    // Decide which events to print
                    let to_print = decide_prints(&batch, &mut state, Instant::now());

                    // Print the selected events
                    for event in to_print {
                        print_progress_event(&event);
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Timeout is just a wakeup; cadence is handled in decide_prints
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // Sender dropped, exit cleanly. Do not print a final event;
                    // the main thread is responsible for the "Storage complete" line.
                    break;
                }
            }
        }
    });

    // Clone sender for the callback; the guard owns the original sender
    // and will drop it (triggering Disconnected) when the guard drops.
    let callback_tx = tx.clone();

    // RAII guard ensures thread cleanup on all exit paths
    let guard = ProgressGuard::new(tx, reporter_handle);

    // Run the combined storage operation with progress reporting
    chunk_storage
        .upsert_without_vectors_with_progress(&rows, &sentinel_row_ids, move |event| {
            let _ = callback_tx.send(event);
        })
        .await?;

    // Join the reporter thread before printing to stdout, to avoid interleaving
    // stderr (progress) with stdout (completion message).
    guard.finish();

    let elapsed = start.elapsed();
    let rate = total_chunks as f64 / elapsed.as_secs_f64().max(0.001);
    println!(
        "  Storage complete: {} chunks in {} ({:.1} chunks/sec)",
        format_count(total_chunks as u64),
        format_duration(elapsed.as_secs_f64()),
        rate
    );
    println!();

    Ok((touched_file_ids, failures))
}

/// State tracked by the progress reporter across event batches.
struct ReporterState {
    /// When the last event was printed (for cadence gating).
    last_printed_at: Option<Instant>,
    /// Which phase was last printed (for transition detection).
    last_printed_phase: Option<&'static str>,
}

/// Decide which events from a received batch should be printed.
///
/// Walks `events` in FIFO order. An event prints if:
/// - the phase differs from `state.last_printed_phase` (phase transition;
///   `None` counts as a transition), OR
/// - `event.completed == event.total` (100% tick).
///
/// After the walk, if nothing was printed and `state.last_printed_at`
/// elapsed >= 10s (or is `None`), the final event in the batch prints
/// as the cadence tick.
///
/// An event that satisfies multiple conditions prints exactly once.
/// `state` is mutated to reflect the last print.
fn decide_prints(
    events: &[StorageProgressEvent],
    state: &mut ReporterState,
    now: Instant,
) -> Vec<StorageProgressEvent> {
    if events.is_empty() {
        return Vec::new();
    }

    let mut to_print = Vec::new();

    for event in events {
        let phase_changed = state.last_printed_phase != Some(event.phase);
        let is_complete = event.completed == event.total;

        if phase_changed || is_complete {
            to_print.push(event.clone());
            state.last_printed_phase = Some(event.phase);
            state.last_printed_at = Some(now);
        }
    }

    // If nothing triggered, check cadence
    if to_print.is_empty() {
        let cadence_elapsed = state
            .last_printed_at
            .map(|t| now.duration_since(t) >= Duration::from_secs(10))
            .unwrap_or(true);

        if cadence_elapsed {
            // Print the final event only
            let last = events.last().unwrap();
            to_print.push(last.clone());
            state.last_printed_phase = Some(last.phase);
            state.last_printed_at = Some(now);
        }
    }

    to_print
}

/// Print a progress event line to stderr.
fn print_progress_event(event: &StorageProgressEvent) {
    let percentage = if event.total > 0 {
        (event.completed as f64 / event.total as f64 * 100.0) as usize
    } else {
        0
    };
    eprintln!(
        "[{}] {}: {}/{} {} ({}%)",
        chrono_timestamp(),
        event.phase,
        format_count(event.completed as u64),
        format_count(event.total as u64),
        event.unit,
        percentage
    );
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
        format_count(count as u64)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(phase: &'static str, completed: usize, total: usize) -> StorageProgressEvent {
        StorageProgressEvent {
            phase,
            completed,
            total,
            unit: "chunks",
        }
    }

    #[test]
    fn test_first_batch_single_event_prints() {
        let events = vec![make_event("Clearing vectors", 100, 1000)];
        let mut state = ReporterState {
            last_printed_at: None,
            last_printed_phase: None,
        };
        let now = Instant::now();

        let to_print = decide_prints(&events, &mut state, now);

        assert_eq!(to_print.len(), 1);
        assert_eq!(to_print[0].phase, "Clearing vectors");
    }

    #[test]
    fn test_batch_prints_phase_transitions_and_complete_ticks() {
        // Batch: [A:1000/1000, B:500/1000, B:1000/1000, C:100/1000]
        // State: last_printed_phase = Some("A"), recent last_printed_at
        // Should print: A 100%, B transition, B 100%, C transition
        let events = vec![
            make_event("A", 1000, 1000),
            make_event("B", 500, 1000),
            make_event("B", 1000, 1000),
            make_event("C", 100, 1000),
        ];
        let mut state = ReporterState {
            last_printed_at: Some(Instant::now()),
            last_printed_phase: Some("A"),
        };
        let now = Instant::now();

        let to_print = decide_prints(&events, &mut state, now);

        assert_eq!(to_print.len(), 4);
        assert_eq!(to_print[0].phase, "A"); // 100% tick
        assert_eq!(to_print[0].completed, 1000);
        assert_eq!(to_print[1].phase, "B"); // transition
        assert_eq!(to_print[1].completed, 500);
        assert_eq!(to_print[2].phase, "B"); // 100% tick
        assert_eq!(to_print[2].completed, 1000);
        assert_eq!(to_print[3].phase, "C"); // transition
        assert_eq!(to_print[3].completed, 100);
    }

    #[test]
    fn test_transition_and_complete_same_event_prints_once() {
        // Event that is both phase transition and 100% tick
        let events = vec![make_event("B", 1000, 1000)];
        let mut state = ReporterState {
            last_printed_at: Some(Instant::now()),
            last_printed_phase: Some("A"),
        };
        let now = Instant::now();

        let to_print = decide_prints(&events, &mut state, now);

        assert_eq!(to_print.len(), 1);
        assert_eq!(to_print[0].phase, "B");
        assert_eq!(to_print[0].completed, 1000);
    }

    #[test]
    fn test_cadence_elapsed_prints_final_event() {
        // No transition or 100%, but cadence elapsed
        let events = vec![make_event("A", 500, 1000)];
        let mut state = ReporterState {
            last_printed_at: Some(Instant::now() - Duration::from_secs(15)),
            last_printed_phase: Some("A"),
        };
        let now = Instant::now();

        let to_print = decide_prints(&events, &mut state, now);

        assert_eq!(to_print.len(), 1);
        assert_eq!(to_print[0].phase, "A");
        assert_eq!(to_print[0].completed, 500);
    }

    #[test]
    fn test_no_cadence_no_transition_no_complete_prints_nothing() {
        let events = vec![make_event("A", 500, 1000)];
        let mut state = ReporterState {
            last_printed_at: Some(Instant::now()),
            last_printed_phase: Some("A"),
        };
        let now = Instant::now();

        let to_print = decide_prints(&events, &mut state, now);

        assert!(to_print.is_empty());
    }
}
