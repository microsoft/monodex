//! Chunk-row persistence and query operations against the chunks table.
//!
//! Purpose: Provide typed operations on the `chunks` table for storage,
//!   retrieval, vector search, and label membership management.
//!
//! Edit here when: Adding/modifying chunk storage operations, vector search logic,
//!   or label membership updates.
//! Do not edit here for: Row types (see rows.rs), label metadata operations (see labels.rs),
//!   database open logic (see database.rs), Arrow encoding/decoding (see arrow_encoding.rs).
//! Size note: 735 production lines. Chunk-table read and write operations against a shared ChunkStorage handle; a write/read split would fragment the file's central edit intent (operations against the chunks table). Revisit at 835.

use anyhow::{Result, anyhow};
use arrow_array::{Array, FixedSizeListArray, RecordBatchIterator};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use super::arrow_encoding::{
    VectorPolicy, chunk_rows_to_record_batch, extract_distance, parse_chunk_row,
};
use crate::engine::storage::arrow;
use crate::engine::storage::locks::acquire_commit_mutex;
use crate::engine::storage::predicate::{
    array_contains_str, eq_str, in_quoted_strs, quoted_str_array,
};
use crate::engine::storage::{ChunkRow, ScoredChunkRow};

/// Batch size for upsert operations. Storage-layer internal detail.
const UPSERT_BATCH_SIZE: usize = 1000;

/// Progress event emitted during storage operations.
///
/// Used by the crawl pipeline to report progress to the user during
/// long-running FTS-only upsert operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageProgressEvent {
    /// Phase label suitable for direct display, e.g. "Clearing vectors", "Upserting chunks", "Marking file sentinels".
    pub phase: &'static str,
    /// Items completed so far in this phase.
    pub completed: usize,
    /// Total items expected in this phase.
    pub total: usize,
    /// Unit label for the items, suitable for direct display, e.g. "chunks" or "files".
    pub unit: &'static str,
}

/// Status of a sentinel row, including vector presence.
///
/// Used by the crawl pipeline to determine whether a file can skip
/// embedding (fast path) or needs to be re-embedded.
#[derive(Debug)]
pub struct SentinelStatus {
    /// The sentinel row (chunk_ordinal = 1, file_complete = true)
    pub row: ChunkRow,
    /// Whether the vector column is non-NULL for this row.
    /// When true, all chunks of this file have vectors (invariant).
    pub has_vector: bool,
}

/// Helper to merge active_label_ids with existing rows.
///
/// For each incoming row, if an existing row with the same row_id exists,
/// union the active_label_ids. This preserves cross-label membership.
fn merge_active_label_ids(rows: &[ChunkRow], existing_rows: &[ChunkRow]) -> Vec<ChunkRow> {
    let existing_map: std::collections::HashMap<&str, &ChunkRow> = existing_rows
        .iter()
        .map(|r| (r.row_id.as_str(), r))
        .collect();

    rows.iter()
        .map(|row| {
            if let Some(existing) = existing_map.get(row.row_id.as_str()) {
                let mut merged_labels: BTreeSet<String> =
                    existing.active_label_ids.iter().cloned().collect();
                merged_labels.extend(row.active_label_ids.iter().cloned());
                let mut merged = row.clone();
                merged.active_label_ids = merged_labels.into_iter().collect();
                merged
            } else {
                row.clone()
            }
        })
        .collect()
}

/// Chunk storage operations for LanceDB.
pub struct ChunkStorage {
    table: Arc<lancedb::table::Table>,
    db_path: PathBuf,
}

impl ChunkStorage {
    /// Create a new ChunkStorage wrapping a table reference.
    pub fn new(table: Arc<lancedb::table::Table>, db_path: PathBuf) -> Self {
        Self { table, db_path }
    }

    /// Upsert a batch of chunk rows with their embedding vectors by row_id.
    ///
    /// This is the primary method for writing chunks during crawl, where we have
    /// both the row data and the computed embedding vectors.
    ///
    /// Matched rows are updated in place (same row_id implies same content
    /// by construction, since file_id already incorporates blob_id + path +
    /// embedder + chunker).
    ///
    /// **Preserves existing active_label_ids**: When a row already exists, the
    /// incoming `active_label_ids` is unioned with the existing labels, never
    /// replaced. This ensures cross-label chunk sharing works correctly.
    ///
    /// Batching is handled internally; callers may pass any number of rows.
    pub async fn upsert_with_vectors(&self, rows: &[ChunkRow], vectors: &[Vec<f32>]) -> Result<()> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

        if rows.is_empty() {
            return Ok(());
        }

        if rows.len() != vectors.len() {
            return Err(anyhow!(
                "Rows and vectors count mismatch: {} vs {}",
                rows.len(),
                vectors.len()
            ));
        }

        // Validate all rows before writing
        for row in rows {
            row.validate()?;
        }

        self.upsert_chunks_inner(rows, VectorPolicy::With(vectors), None)
            .await
    }

    /// Upsert a batch of chunk rows without embedding vectors.
    ///
    /// This is used for FTS-only crawls where we don't compute embeddings.
    /// The vector column is omitted from the merge_insert, which preserves
    /// any existing vectors on matched rows.
    ///
    /// **Preserves existing vectors at the storage layer**: Matched rows keep
    /// their existing vector values because the vector column is not included
    /// in the merge batch. Callers responsible for maintaining the per-file
    /// vector-presence invariant must clear vectors separately using
    /// `null_vectors_for_row_ids` when the situation requires it.
    ///
    /// **Preserves existing active_label_ids**: When a row already exists, the
    /// incoming `active_label_ids` is unioned with the existing labels, never
    /// replaced. This ensures cross-label chunk sharing works correctly.
    ///
    /// Batching is handled internally; callers may pass any number of rows.
    pub async fn upsert_without_vectors(&self, rows: &[ChunkRow]) -> Result<()> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

        if rows.is_empty() {
            return Ok(());
        }

        // Validate all rows before writing
        for row in rows {
            row.validate()?;
        }

        self.upsert_chunks_inner(rows, VectorPolicy::Without, None)
            .await
    }

    /// Upsert chunks without vectors with progress reporting.
    ///
    /// This is the combined FTS-only upsert operation that:
    /// 1. Upserts the chunks without vectors (preserving any existing vectors)
    /// 2. Marks file sentinels as complete
    ///
    /// Vectors are preserved to avoid clobbering peer labels that share the same
    /// blob. The invariant that all chunks of a file have consistent vector presence
    /// will be maintained by structural separation in a future release: a vector
    /// crawl will process all chunks of a file atomically, and an FTS-only crawl
    /// does not touch vectors.
    ///
    /// Both phases run under a single commit mutex acquisition, which is
    /// correct because FTS-only crawls hold the per-catalog writer lock and
    /// other writers against this catalog are already serialized.
    ///
    /// Progress is reported via the callback after each batch in each phase.
    ///
    /// # Arguments
    /// * `rows` - Chunk rows to upsert
    /// * `sentinel_row_ids` - Row IDs of sentinel rows to mark complete (format: "{file_id}:1")
    /// * `on_progress` - Callback invoked with progress events
    pub async fn upsert_without_vectors_with_progress(
        &self,
        rows: &[ChunkRow],
        sentinel_row_ids: &[String],
        on_progress: impl Fn(StorageProgressEvent) + Send + Sync,
    ) -> Result<()> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

        if rows.is_empty() && sentinel_row_ids.is_empty() {
            return Ok(());
        }

        // Validate all rows before writing
        for row in rows {
            row.validate()?;
        }

        let total_sentinels = sentinel_row_ids.len();

        // Phase A: Upsert chunks without vectors
        if !rows.is_empty() {
            self.upsert_chunks_inner(rows, VectorPolicy::Without, Some(&on_progress))
                .await?;
        }

        // Phase B: Mark file sentinels as complete
        if !sentinel_row_ids.is_empty() {
            let sentinel_strs: Vec<&str> = sentinel_row_ids.iter().map(|s| s.as_str()).collect();

            for batch_start in (0..sentinel_strs.len()).step_by(UPSERT_BATCH_SIZE) {
                let batch_end = std::cmp::min(batch_start + UPSERT_BATCH_SIZE, sentinel_strs.len());
                let batch_ids = &sentinel_strs[batch_start..batch_end];

                let predicate = in_quoted_strs("row_id", batch_ids);
                self.table
                    .update()
                    .only_if(&predicate)
                    .column("file_complete", "true")
                    .execute()
                    .await
                    .map_err(|e| anyhow!("Failed to update file_complete: {}", e))?;

                on_progress(StorageProgressEvent {
                    phase: "Marking file sentinels",
                    completed: batch_end,
                    total: total_sentinels,
                    unit: "files",
                });
            }
        }

        Ok(())
    }

    /// Internal helper for upserting chunks with configurable vector handling.
    ///
    /// Assumes the commit mutex is already held by the caller.
    /// Performs per-batch active_label_ids preservation to avoid unbounded IN(...) predicates.
    async fn upsert_chunks_inner(
        &self,
        rows: &[ChunkRow],
        vectors: VectorPolicy<'_>,
        on_progress: Option<&(dyn Fn(StorageProgressEvent) + Send + Sync)>,
    ) -> Result<()> {
        let schema = self.table.schema().await?;

        // Process in batches, fetching existing rows per-batch to keep IN(...) predicate bounded
        for batch_start in (0..rows.len()).step_by(UPSERT_BATCH_SIZE) {
            let batch_end = std::cmp::min(batch_start + UPSERT_BATCH_SIZE, rows.len());
            let batch_rows = &rows[batch_start..batch_end];

            // Fetch existing rows for this batch only (bounded to UPSERT_BATCH_SIZE)
            let row_ids: Vec<&str> = batch_rows.iter().map(|r| r.row_id.as_str()).collect();
            let existing_rows = self.get_by_row_ids_inner(&row_ids).await?;
            let merged_rows = merge_active_label_ids(batch_rows, &existing_rows);

            // Build the record batch based on vector policy
            let batch = match &vectors {
                VectorPolicy::With(all_vectors) => {
                    let batch_vectors = &all_vectors[batch_start..batch_end];
                    chunk_rows_to_record_batch(
                        &merged_rows,
                        VectorPolicy::With(batch_vectors),
                        schema.clone(),
                    )?
                }
                VectorPolicy::Without => {
                    chunk_rows_to_record_batch(&merged_rows, VectorPolicy::Without, schema.clone())?
                }
            };

            // Use merge_insert for proper upsert semantics
            let batch_schema = batch.schema();
            let reader = RecordBatchIterator::new(std::iter::once(Ok(batch)), batch_schema);
            let mut builder = self.table.merge_insert(&["row_id"]);
            builder
                .when_matched_update_all(None)
                .when_not_matched_insert_all();
            builder.execute(Box::new(reader)).await?;

            // Report progress if callback provided
            if let Some(cb) = on_progress {
                cb(StorageProgressEvent {
                    phase: "Upserting chunks",
                    completed: batch_end,
                    total: rows.len(),
                    unit: "chunks",
                });
            }
        }

        Ok(())
    }

    /// Get sentinel status for a file.
    ///
    /// Returns None if the sentinel row doesn't exist.
    /// Returns the sentinel row plus whether it has a vector.
    ///
    /// This is used by the crawl pipeline to determine the fast-path eligibility:
    /// - If vector is in selection: skip if sentinel exists, file_complete=true, AND has_vector
    /// - If vector is not in selection: skip if sentinel exists AND file_complete=true
    pub async fn get_sentinel_status(
        &self,
        sentinel_row_id: &str,
    ) -> Result<Option<SentinelStatus>> {
        // Query including the vector column to check for NULL
        let predicate = eq_str("row_id", sentinel_row_id);

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query sentinel: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect sentinel query: {}", e))?;

        for batch in &batches {
            if batch.num_rows() > 0 {
                let row = parse_chunk_row(batch, 0)?;

                // Check if vector column is non-NULL
                let has_vector = {
                    let vector_col = batch.column_by_name("vector");
                    match vector_col {
                        Some(col) => {
                            let list_array = col
                                .as_any()
                                .downcast_ref::<FixedSizeListArray>()
                                .ok_or_else(|| {
                                    anyhow!("vector column is not a FixedSizeListArray")
                                })?;
                            !list_array.is_null(0)
                        }
                        None => false,
                    }
                };

                return Ok(Some(SentinelStatus { row, has_vector }));
            }
        }

        Ok(None)
    }

    /// Internal helper to fetch multiple rows by row_id (no mutex acquisition).
    async fn get_by_row_ids_inner(&self, row_ids: &[&str]) -> Result<Vec<ChunkRow>> {
        if row_ids.is_empty() {
            return Ok(Vec::new());
        }

        let predicate = in_quoted_strs("row_id", row_ids);
        arrow::collect_rows(&self.table, &predicate, "chunks by row_ids", |batch, i| {
            parse_chunk_row(batch, i)
        })
        .await
    }

    /// Look up a single chunk by row_id.
    ///
    /// Returns None if the chunk doesn't exist.
    pub async fn get_by_row_id(&self, row_id: &str) -> Result<Option<ChunkRow>> {
        let predicate = eq_str("row_id", row_id);
        arrow::collect_first_row(&self.table, &predicate, "chunk by row_id", |batch, i| {
            parse_chunk_row(batch, i)
        })
        .await
    }

    /// Return all chunks for a given file_id where active_label_ids contains
    /// the given label, sorted by chunk_ordinal.
    ///
    /// Validates each row.
    pub async fn get_chunks_by_file_id_with_label(
        &self,
        file_id: &str,
        label_id: &str,
    ) -> Result<Vec<ChunkRow>> {
        let predicate = format!(
            "{} AND {}",
            eq_str("file_id", file_id),
            array_contains_str("active_label_ids", label_id)
        );

        let mut rows =
            arrow::collect_rows(&self.table, &predicate, "chunks by file_id", |batch, i| {
                parse_chunk_row(batch, i)
            })
            .await?;

        // Sort by chunk_ordinal
        rows.sort_by_key(|r| r.chunk_ordinal);

        Ok(rows)
    }

    /// Return all chunks for a given file_id, sorted by chunk_ordinal.
    ///
    /// Does not filter by label; used for label-add operations.
    /// Validates each row.
    pub async fn get_chunks_by_file_id(&self, file_id: &str) -> Result<Vec<ChunkRow>> {
        let predicate = eq_str("file_id", file_id);

        let mut rows =
            arrow::collect_rows(&self.table, &predicate, "chunks by file_id", |batch, i| {
                parse_chunk_row(batch, i)
            })
            .await?;

        // Sort by chunk_ordinal
        rows.sort_by_key(|r| r.chunk_ordinal);

        Ok(rows)
    }

    /// Vector search: given a query vector, a label filter, and a limit,
    /// return the top-N chunks by cosine distance that belong to the label.
    ///
    /// Brute-force scan; no ANN index.
    pub async fn vector_search(
        &self,
        query_vector: &[f32],
        label_id: &str,
        limit: usize,
    ) -> Result<Vec<ScoredChunkRow>> {
        use lancedb::DistanceType;
        use lancedb::query::{ExecutableQuery, QueryBase};

        let predicate = array_contains_str("active_label_ids", label_id);

        let results = self
            .table
            .query()
            .nearest_to(query_vector)
            .map_err(|e| anyhow!("Failed to set query vector: {}", e))?
            .distance_type(DistanceType::Cosine)
            .only_if(&predicate)
            .limit(limit)
            .execute()
            .await
            .map_err(|e| anyhow!("Vector search failed: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect search results: {}", e))?;

        let mut scored_rows: Vec<ScoredChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                let chunk = parse_chunk_row(batch, i)?;
                let distance = extract_distance(batch, i)?;
                scored_rows.push(ScoredChunkRow { chunk, distance });
            }
        }

        Ok(scored_rows)
    }

    /// Return all chunks for a given label, with optional chunk_ordinal filter.
    ///
    /// In-memory Vec, not a streaming iterator.
    /// Used by sentinel scans and label reassignment.
    pub async fn get_chunks_for_label(
        &self,
        label_id: &str,
        chunk_ordinal: Option<i32>,
    ) -> Result<Vec<ChunkRow>> {
        let predicate = match chunk_ordinal {
            Some(ordinal) => format!(
                "{} AND chunk_ordinal = {}",
                array_contains_str("active_label_ids", label_id),
                ordinal
            ),
            None => array_contains_str("active_label_ids", label_id),
        };

        arrow::collect_rows(&self.table, &predicate, "chunks for label", |batch, i| {
            parse_chunk_row(batch, i)
        })
        .await
    }

    /// Get chunks by a list of row_ids for a specific label.
    ///
    /// This is used by FTS search to hydrate chunk data from LanceDB after
    /// getting row_ids from Tantivy search results. The returned chunks are
    /// in arbitrary order (caller re-orders to match FTS hit order).
    ///
    /// # Arguments
    /// * `label_id` - The label to filter by (must be in active_label_ids)
    /// * `row_ids` - List of row_ids to fetch
    ///
    /// # Returns
    /// Chunks matching the predicate, in arbitrary order.
    pub async fn get_chunks_by_row_ids_for_label(
        &self,
        label_id: &str,
        row_ids: &[String],
    ) -> Result<Vec<ChunkRow>> {
        if row_ids.is_empty() {
            return Ok(Vec::new());
        }

        let vals: Vec<&str> = row_ids.iter().map(|s| s.as_str()).collect();
        let predicate = format!(
            "{} AND {}",
            array_contains_str("active_label_ids", label_id),
            in_quoted_strs("row_id", &vals)
        );

        arrow::collect_rows(&self.table, &predicate, "chunks by row_ids", |batch, i| {
            parse_chunk_row(batch, i)
        })
        .await
    }

    /// Update the active_label_ids array of a single chunk.
    ///
    /// Uses LanceDB's update() with SQL expression for in-place modification.
    pub async fn update_active_labels(&self, row_id: &str, new_labels: &[String]) -> Result<()> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;
        self.update_active_labels_inner(row_id, new_labels).await
    }

    /// Internal helper for update_active_labels (no mutex acquisition).
    async fn update_active_labels_inner(&self, row_id: &str, new_labels: &[String]) -> Result<()> {
        // Reject empty label list - a chunk must belong to at least one label.
        // Callers should use delete_by_row_ids to remove chunks, not clear their labels.
        if new_labels.is_empty() {
            return Err(anyhow!(
                "Cannot update active_label_ids to empty list - a chunk must belong to at least one label"
            ));
        }

        // Build SQL array literal like "['label1', 'label2']"
        let labels_sql =
            quoted_str_array(&new_labels.iter().map(|s| s.as_str()).collect::<Vec<_>>());

        let predicate = eq_str("row_id", row_id);

        self.table
            .update()
            .only_if(&predicate)
            .column("active_label_ids", &labels_sql)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to update active_label_ids: {}", e))?;

        Ok(())
    }

    /// Update the file_complete boolean of a single chunk (sentinel marker).
    ///
    /// Uses LanceDB's update() with SQL boolean literal (true/false).
    pub async fn update_file_complete(&self, row_id: &str, complete: bool) -> Result<()> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

        let predicate = eq_str("row_id", row_id);
        let value = if complete { "true" } else { "false" };

        self.table
            .update()
            .only_if(&predicate)
            .column("file_complete", value)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to update file_complete: {}", e))?;

        Ok(())
    }

    /// Set the vector column to NULL for multiple chunks.
    ///
    /// This is a storage primitive used by tests pinning the no-vector-write invariant.
    /// No production caller exists today; the FTS-only crawl path preserves vectors
    /// rather than clearing them.
    ///
    /// Batching is handled internally using the same UPSERT_BATCH_SIZE as upsert operations.
    /// The commit mutex is acquired once at function entry, then released on drop.
    pub async fn null_vectors_for_row_ids(&self, row_ids: &[&str]) -> Result<()> {
        if row_ids.is_empty() {
            return Ok(());
        }

        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

        // Process in batches to keep the IN(...) predicate bounded
        for batch_start in (0..row_ids.len()).step_by(UPSERT_BATCH_SIZE) {
            let batch_end = std::cmp::min(batch_start + UPSERT_BATCH_SIZE, row_ids.len());
            let batch_ids = &row_ids[batch_start..batch_end];

            self.null_vectors_batch(batch_ids).await?;
        }

        Ok(())
    }

    /// Batch-delete chunks by a list of row_ids.
    pub async fn delete_by_row_ids(&self, row_ids: &[String]) -> Result<()> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;
        self.delete_by_row_ids_inner(row_ids).await
    }

    /// Internal helper for null_vectors_for_row_ids (no mutex acquisition).
    ///
    /// Nulls the vector column for one batch of row_ids. The caller must hold the commit mutex.
    async fn null_vectors_batch(&self, batch_ids: &[&str]) -> Result<()> {
        let predicate = in_quoted_strs("row_id", batch_ids);
        self.table
            .update()
            .only_if(&predicate)
            .column("vector", "null")
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to null vectors: {}", e))?;
        Ok(())
    }

    /// Internal helper for delete_by_row_ids (no mutex acquisition).
    async fn delete_by_row_ids_inner(&self, row_ids: &[String]) -> Result<()> {
        if row_ids.is_empty() {
            return Ok(());
        }

        let vals: Vec<&str> = row_ids.iter().map(|s| s.as_str()).collect();
        let predicate = in_quoted_strs("row_id", &vals);

        self.table
            .delete(&predicate)
            .await
            .map_err(|e| anyhow!("Failed to delete chunks: {}", e))?;

        Ok(())
    }

    /// Delete all chunks matching a given catalog, returning the count deleted.
    pub async fn delete_by_catalog(&self, catalog: &str) -> Result<u64> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

        let predicate = eq_str("catalog", catalog);

        // Use predicate-scoped count to avoid race with cross-catalog writes
        let count = self
            .table
            .count_rows(Some(predicate.clone()))
            .await
            .map_err(|e| anyhow!("Failed to count rows for catalog: {}", e))?;

        self.table
            .delete(&predicate)
            .await
            .map_err(|e| anyhow!("Failed to delete chunks by catalog: {}", e))?;

        Ok(count as u64)
    }

    /// Remove a label from chunks where it's in active_label_ids, excluding specified files.
    ///
    /// This scans all chunks with the label and removes the label from active_label_ids.
    /// If active_label_ids becomes empty, the chunk is deleted.
    ///
    /// Returns the count of chunks processed.
    pub async fn remove_label_from_chunks(
        &self,
        label_id: &str,
        exclude_file_ids: &HashSet<String>,
    ) -> Result<u64> {
        // Acquire commit mutex once for the entire operation
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

        let mut processed: u64 = 0;

        // Get all chunks with this label
        let chunks = self.get_chunks_for_label(label_id, None).await?;

        for chunk in chunks {
            // Skip if this file was touched in the current crawl
            if exclude_file_ids.contains(&chunk.file_id) {
                continue;
            }

            // Remove label from active_label_ids
            let mut new_labels = chunk.active_label_ids.clone();
            new_labels.retain(|l| l != label_id);

            if new_labels.is_empty() {
                // Delete the chunk using inner helper (no mutex re-acquisition)
                self.delete_by_row_ids_inner(std::slice::from_ref(&chunk.row_id))
                    .await?;
            } else {
                // Update active_label_ids using inner helper (no mutex re-acquisition)
                self.update_active_labels_inner(&chunk.row_id, &new_labels)
                    .await?;
            }

            processed += 1;
        }

        Ok(processed)
    }

    /// Truncate the table (empty all rows, preserve schema).
    pub async fn truncate(&self) -> Result<()> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

        self.table
            .delete("true")
            .await
            .map_err(|e| anyhow!("Failed to truncate chunks table: {}", e))?;

        Ok(())
    }

    /// Get the table reference.
    pub fn table(&self) -> Arc<lancedb::table::Table> {
        self.table.clone()
    }
}
