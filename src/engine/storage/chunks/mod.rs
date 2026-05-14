//! Chunk table operations for LanceDB storage.
//!
//! Purpose: Provide typed operations on the `chunks` table.
//!
//! Edit here when: Adding/modifying chunk storage operations, vector search logic,
//!   or label membership updates.
//! Do not edit here for: Row types (see rows.rs), label metadata operations (see labels.rs),
//!   database open logic (see database.rs).

use anyhow::{Result, anyhow};
use arrow_array::{
    Array, ArrayRef, BooleanArray, FixedSizeListArray, Float32Array, Int32Array, ListArray,
    RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_buffer::{OffsetBuffer, ScalarBuffer};
use arrow_schema::{DataType, Field, SchemaRef};
use futures::TryStreamExt;
use lancedb::DistanceType;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use crate::engine::schema::VECTOR_DIMENSION;
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

/// Policy for handling the vector column during upsert.
///
/// This is a private implementation detail of the upsert methods.
enum VectorPolicy<'a> {
    /// Include vectors in the upsert batch.
    With(&'a [Vec<f32>]),
    /// Omit vectors from the upsert batch (preserves existing vectors on matched rows).
    Without,
}

/// Convert an iterator of ChunkRows with their vectors to a RecordBatch.
///
/// This is the primary function for writing chunks during crawl, where we have
/// both the row data and the computed embedding vectors.
fn chunk_rows_to_record_batch_with_vectors<'a>(
    rows: impl IntoIterator<Item = (&'a ChunkRow, &'a [f32])>,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let rows: Vec<(&ChunkRow, &[f32])> = rows.into_iter().collect();
    let n = rows.len();

    let row_id: StringArray = rows.iter().map(|(r, _)| Some(r.row_id.as_str())).collect();
    let text: StringArray = rows.iter().map(|(r, _)| Some(r.text.as_str())).collect();

    // Vector column with actual embedding values
    let vector_field = Field::new("item", DataType::Float32, true);
    let mut all_vector_values: Vec<f32> = Vec::with_capacity(VECTOR_DIMENSION * n);
    for (_, vector) in &rows {
        if vector.len() != VECTOR_DIMENSION {
            return Err(anyhow!(
                "Vector dimension mismatch: expected {}, got {}",
                VECTOR_DIMENSION,
                vector.len()
            ));
        }
        all_vector_values.extend_from_slice(vector);
    }
    let vector_values: Float32Array = all_vector_values.into();
    let vector: ArrayRef = Arc::new(FixedSizeListArray::new(
        Arc::new(vector_field),
        VECTOR_DIMENSION as i32,
        Arc::new(vector_values),
        None,
    ));

    let catalog: StringArray = rows.iter().map(|(r, _)| Some(r.catalog.as_str())).collect();

    // active_label_ids: List<Utf8>
    let active_label_ids = build_string_list_array(
        &rows
            .iter()
            .map(|(r, _)| r.active_label_ids.as_slice())
            .collect::<Vec<_>>(),
    );

    let embedder_id: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.embedder_id.as_str()))
        .collect();
    let chunker_id: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.chunker_id.as_str()))
        .collect();
    let blob_id: StringArray = rows.iter().map(|(r, _)| Some(r.blob_id.as_str())).collect();
    let content_hash: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.content_hash.as_str()))
        .collect();
    let file_id: StringArray = rows.iter().map(|(r, _)| Some(r.file_id.as_str())).collect();
    let relative_path: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.relative_path.as_str()))
        .collect();
    let package_name: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.package_name.as_str()))
        .collect();
    let source_uri: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.source_uri.as_str()))
        .collect();

    let chunk_ordinal: Int32Array = rows.iter().map(|(r, _)| Some(r.chunk_ordinal)).collect();
    let chunk_count: Int32Array = rows.iter().map(|(r, _)| Some(r.chunk_count)).collect();
    let start_line: Int32Array = rows.iter().map(|(r, _)| Some(r.start_line)).collect();
    let end_line: Int32Array = rows.iter().map(|(r, _)| Some(r.end_line)).collect();

    // Nullable string fields
    let symbol_name: StringArray = rows.iter().map(|(r, _)| r.symbol_name.as_deref()).collect();
    let chunk_type: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.chunk_type.as_str()))
        .collect();
    let chunk_kind: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.chunk_kind.as_str()))
        .collect();
    let breadcrumb: StringArray = rows.iter().map(|(r, _)| r.breadcrumb.as_deref()).collect();

    // Nullable int fields
    let split_part_ordinal: Int32Array = rows.iter().map(|(r, _)| r.split_part_ordinal).collect();
    let split_part_count: Int32Array = rows.iter().map(|(r, _)| r.split_part_count).collect();

    let file_complete: BooleanArray = rows.iter().map(|(r, _)| Some(r.file_complete)).collect();

    let columns: Vec<ArrayRef> = vec![
        Arc::new(row_id),
        Arc::new(text),
        vector,
        Arc::new(catalog),
        active_label_ids,
        Arc::new(embedder_id),
        Arc::new(chunker_id),
        Arc::new(blob_id),
        Arc::new(content_hash),
        Arc::new(file_id),
        Arc::new(relative_path),
        Arc::new(package_name),
        Arc::new(source_uri),
        Arc::new(chunk_ordinal),
        Arc::new(chunk_count),
        Arc::new(start_line),
        Arc::new(end_line),
        Arc::new(symbol_name),
        Arc::new(chunk_type),
        Arc::new(chunk_kind),
        Arc::new(breadcrumb),
        Arc::new(split_part_ordinal),
        Arc::new(split_part_count),
        Arc::new(file_complete),
    ];

    RecordBatch::try_new(schema, columns)
        .map_err(|e| anyhow!("Failed to create RecordBatch: {}", e))
}

/// Convert an iterator of ChunkRows without vectors to a RecordBatch.
///
/// This is used for FTS-only crawls where we don't have embedding vectors.
/// The vector column is omitted entirely, which preserves existing vectors
/// on matched rows (LanceDB's merge_insert only updates columns present in
/// the source batch).
fn chunk_rows_to_record_batch_without_vectors<'a>(
    rows: impl IntoIterator<Item = &'a ChunkRow>,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let rows: Vec<&ChunkRow> = rows.into_iter().collect();

    let row_id: StringArray = rows.iter().map(|r| Some(r.row_id.as_str())).collect();
    let text: StringArray = rows.iter().map(|r| Some(r.text.as_str())).collect();

    let catalog: StringArray = rows.iter().map(|r| Some(r.catalog.as_str())).collect();

    // active_label_ids: List<Utf8>
    let active_label_ids = build_string_list_array(
        &rows
            .iter()
            .map(|r| r.active_label_ids.as_slice())
            .collect::<Vec<_>>(),
    );

    let embedder_id: StringArray = rows.iter().map(|r| Some(r.embedder_id.as_str())).collect();
    let chunker_id: StringArray = rows.iter().map(|r| Some(r.chunker_id.as_str())).collect();
    let blob_id: StringArray = rows.iter().map(|r| Some(r.blob_id.as_str())).collect();
    let content_hash: StringArray = rows.iter().map(|r| Some(r.content_hash.as_str())).collect();
    let file_id: StringArray = rows.iter().map(|r| Some(r.file_id.as_str())).collect();
    let relative_path: StringArray = rows
        .iter()
        .map(|r| Some(r.relative_path.as_str()))
        .collect();
    let package_name: StringArray = rows.iter().map(|r| Some(r.package_name.as_str())).collect();
    let source_uri: StringArray = rows.iter().map(|r| Some(r.source_uri.as_str())).collect();

    let chunk_ordinal: Int32Array = rows.iter().map(|r| Some(r.chunk_ordinal)).collect();
    let chunk_count: Int32Array = rows.iter().map(|r| Some(r.chunk_count)).collect();
    let start_line: Int32Array = rows.iter().map(|r| Some(r.start_line)).collect();
    let end_line: Int32Array = rows.iter().map(|r| Some(r.end_line)).collect();

    // Nullable string fields
    let symbol_name: StringArray = rows.iter().map(|r| r.symbol_name.as_deref()).collect();
    let chunk_type: StringArray = rows.iter().map(|r| Some(r.chunk_type.as_str())).collect();
    let chunk_kind: StringArray = rows.iter().map(|r| Some(r.chunk_kind.as_str())).collect();
    let breadcrumb: StringArray = rows.iter().map(|r| r.breadcrumb.as_deref()).collect();

    // Nullable int fields
    let split_part_ordinal: Int32Array = rows.iter().map(|r| r.split_part_ordinal).collect();
    let split_part_count: Int32Array = rows.iter().map(|r| r.split_part_count).collect();

    let file_complete: BooleanArray = rows.iter().map(|r| Some(r.file_complete)).collect();

    // Note: vector column is intentionally omitted to preserve existing vectors.
    // We project the schema to exclude the vector column rather than creating
    // a new schema from scratch, to ensure field metadata matches.
    let columns: Vec<ArrayRef> = vec![
        Arc::new(row_id),
        Arc::new(text),
        // vector column omitted - this is the key difference
        Arc::new(catalog),
        active_label_ids,
        Arc::new(embedder_id),
        Arc::new(chunker_id),
        Arc::new(blob_id),
        Arc::new(content_hash),
        Arc::new(file_id),
        Arc::new(relative_path),
        Arc::new(package_name),
        Arc::new(source_uri),
        Arc::new(chunk_ordinal),
        Arc::new(chunk_count),
        Arc::new(start_line),
        Arc::new(end_line),
        Arc::new(symbol_name),
        Arc::new(chunk_type),
        Arc::new(chunk_kind),
        Arc::new(breadcrumb),
        Arc::new(split_part_ordinal),
        Arc::new(split_part_count),
        Arc::new(file_complete),
    ];

    // Project the schema to exclude the vector column
    let schema_without_vector: SchemaRef = Arc::new(
        schema
            .project(
                &schema
                    .fields()
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.name() != "vector")
                    .map(|(i, _)| i)
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| anyhow!("Failed to project schema: {}", e))?,
    );

    RecordBatch::try_new(schema_without_vector, columns)
        .map_err(|e| anyhow!("Failed to create RecordBatch: {}", e))
}

/// Build a List<Utf8> array from a slice of string slices.
fn build_string_list_array(values: &[&[String]]) -> ArrayRef {
    let mut offsets = Vec::with_capacity(values.len() + 1);
    offsets.push(0i32);

    let mut all_strings = Vec::new();
    for list in values {
        for s in *list {
            all_strings.push(Some(s.as_str()));
        }
        offsets.push(all_strings.len() as i32);
    }

    let inner: StringArray = all_strings.iter().copied().collect();
    let offset_buffer = OffsetBuffer::new(ScalarBuffer::from(offsets));

    Arc::new(
        ListArray::try_new(
            Arc::new(Field::new("item", DataType::Utf8, false)), // non-null items
            offset_buffer,
            Arc::new(inner),
            None,
        )
        .expect("Failed to create ListArray"),
    )
}

/// Parse a RecordBatch row into a ChunkRow.
///
/// Validates all identifier fields.
fn parse_chunk_row(batch: &RecordBatch, row_idx: usize) -> Result<ChunkRow> {
    let row_id = batch
        .column_by_name("row_id")
        .ok_or_else(|| anyhow!("row_id column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("row_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let text = batch
        .column_by_name("text")
        .ok_or_else(|| anyhow!("text column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("text column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    // "vector" is intentionally not read here; ChunkRow does not carry the embedding.

    let catalog = batch
        .column_by_name("catalog")
        .ok_or_else(|| anyhow!("catalog column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("catalog column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let active_label_ids = {
        let list_array = batch
            .column_by_name("active_label_ids")
            .ok_or_else(|| anyhow!("active_label_ids column not found"))?
            .as_any()
            .downcast_ref::<ListArray>()
            .ok_or_else(|| anyhow!("active_label_ids column is not a ListArray"))?;
        let list_value = list_array.value(row_idx);
        let string_array = list_value
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("active_label_ids inner array is not a StringArray"))?;
        (0..string_array.len())
            .map(|i| string_array.value(i).to_string())
            .collect()
    };

    let embedder_id = batch
        .column_by_name("embedder_id")
        .ok_or_else(|| anyhow!("embedder_id column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("embedder_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let chunker_id = batch
        .column_by_name("chunker_id")
        .ok_or_else(|| anyhow!("chunker_id column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("chunker_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let blob_id = batch
        .column_by_name("blob_id")
        .ok_or_else(|| anyhow!("blob_id column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("blob_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let content_hash = batch
        .column_by_name("content_hash")
        .ok_or_else(|| anyhow!("content_hash column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("content_hash column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let file_id = batch
        .column_by_name("file_id")
        .ok_or_else(|| anyhow!("file_id column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("file_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let relative_path = batch
        .column_by_name("relative_path")
        .ok_or_else(|| anyhow!("relative_path column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("relative_path column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let package_name = batch
        .column_by_name("package_name")
        .ok_or_else(|| anyhow!("package_name column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("package_name column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let source_uri = batch
        .column_by_name("source_uri")
        .ok_or_else(|| anyhow!("source_uri column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("source_uri column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let chunk_ordinal = batch
        .column_by_name("chunk_ordinal")
        .ok_or_else(|| anyhow!("chunk_ordinal column not found"))?
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("chunk_ordinal column is not an Int32Array"))?
        .value(row_idx);

    let chunk_count = batch
        .column_by_name("chunk_count")
        .ok_or_else(|| anyhow!("chunk_count column not found"))?
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("chunk_count column is not an Int32Array"))?
        .value(row_idx);

    let start_line = batch
        .column_by_name("start_line")
        .ok_or_else(|| anyhow!("start_line column not found"))?
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("start_line column is not an Int32Array"))?
        .value(row_idx);

    let end_line = batch
        .column_by_name("end_line")
        .ok_or_else(|| anyhow!("end_line column not found"))?
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("end_line column is not an Int32Array"))?
        .value(row_idx);

    // Nullable string fields
    let symbol_name = {
        let arr = batch
            .column_by_name("symbol_name")
            .ok_or_else(|| anyhow!("symbol_name column not found"))?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("symbol_name column is not a StringArray"))?;
        if arr.is_null(row_idx) {
            None
        } else {
            Some(arr.value(row_idx).to_string())
        }
    };

    let chunk_type = batch
        .column_by_name("chunk_type")
        .ok_or_else(|| anyhow!("chunk_type column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("chunk_type column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let chunk_kind = batch
        .column_by_name("chunk_kind")
        .ok_or_else(|| anyhow!("chunk_kind column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("chunk_kind column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let breadcrumb = {
        let arr = batch
            .column_by_name("breadcrumb")
            .ok_or_else(|| anyhow!("breadcrumb column not found"))?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("breadcrumb column is not a StringArray"))?;
        if arr.is_null(row_idx) {
            None
        } else {
            Some(arr.value(row_idx).to_string())
        }
    };

    // Nullable int fields
    let split_part_ordinal = {
        let arr = batch
            .column_by_name("split_part_ordinal")
            .ok_or_else(|| anyhow!("split_part_ordinal column not found"))?
            .as_any()
            .downcast_ref::<Int32Array>()
            .ok_or_else(|| anyhow!("split_part_ordinal column is not an Int32Array"))?;
        if arr.is_null(row_idx) {
            None
        } else {
            Some(arr.value(row_idx))
        }
    };

    let split_part_count = {
        let arr = batch
            .column_by_name("split_part_count")
            .ok_or_else(|| anyhow!("split_part_count column not found"))?
            .as_any()
            .downcast_ref::<Int32Array>()
            .ok_or_else(|| anyhow!("split_part_count column is not an Int32Array"))?;
        if arr.is_null(row_idx) {
            None
        } else {
            Some(arr.value(row_idx))
        }
    };

    let file_complete = batch
        .column_by_name("file_complete")
        .ok_or_else(|| anyhow!("file_complete column not found"))?
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| anyhow!("file_complete column is not a BooleanArray"))?
        .value(row_idx);

    let row = ChunkRow {
        row_id,
        text,
        catalog,
        active_label_ids,
        embedder_id,
        chunker_id,
        blob_id,
        content_hash,
        file_id,
        relative_path,
        package_name,
        source_uri,
        chunk_ordinal,
        chunk_count,
        start_line,
        end_line,
        symbol_name,
        chunk_type,
        chunk_kind,
        breadcrumb,
        split_part_ordinal,
        split_part_count,
        file_complete,
    };

    row.validate()?;
    Ok(row)
}

/// Extract the distance column from a vector search result.
fn extract_distance(batch: &RecordBatch, row_idx: usize) -> Result<f32> {
    // LanceDB returns distance as "_distance" column
    let distance_array = batch
        .column_by_name("_distance")
        .ok_or_else(|| anyhow!("Missing _distance column in vector search result"))?;

    let distances = distance_array
        .as_any()
        .downcast_ref::<Float32Array>()
        .ok_or_else(|| anyhow!("_distance column is not a Float32Array"))?;

    Ok(distances.value(row_idx))
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

        self.upsert_chunks_inner(rows, VectorPolicy::With(vectors))
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

        self.upsert_chunks_inner(rows, VectorPolicy::Without).await
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

        let total_rows = rows.len();
        let total_sentinels = sentinel_row_ids.len();

        // Phase A: Upsert chunks without vectors
        if !rows.is_empty() {
            let schema = self.table.schema().await?;

            for batch_start in (0..rows.len()).step_by(UPSERT_BATCH_SIZE) {
                let batch_end = std::cmp::min(batch_start + UPSERT_BATCH_SIZE, rows.len());
                let batch_rows = &rows[batch_start..batch_end];

                // Fetch existing rows for this batch to preserve active_label_ids
                let batch_row_ids: Vec<&str> =
                    batch_rows.iter().map(|r| r.row_id.as_str()).collect();
                let existing_rows = self.get_by_row_ids_inner(&batch_row_ids).await?;
                let merged_rows = merge_active_label_ids(batch_rows, &existing_rows);

                let batch =
                    chunk_rows_to_record_batch_without_vectors(merged_rows.iter(), schema.clone())?;

                let batch_schema = batch.schema();
                let reader = RecordBatchIterator::new(std::iter::once(Ok(batch)), batch_schema);
                let mut builder = self.table.merge_insert(&["row_id"]);
                builder
                    .when_matched_update_all(None)
                    .when_not_matched_insert_all();
                builder.execute(Box::new(reader)).await?;

                on_progress(StorageProgressEvent {
                    phase: "Upserting chunks",
                    completed: batch_end,
                    total: total_rows,
                    unit: "chunks",
                });
            }
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
                    let rows_with_vectors: Vec<(&ChunkRow, &[f32])> = merged_rows
                        .iter()
                        .zip(batch_vectors.iter().map(|v| v.as_slice()))
                        .collect();
                    chunk_rows_to_record_batch_with_vectors(
                        rows_with_vectors.into_iter(),
                        schema.clone(),
                    )?
                }
                VectorPolicy::Without => {
                    chunk_rows_to_record_batch_without_vectors(merged_rows.iter(), schema.clone())?
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

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunks by row_ids: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        let mut rows: Vec<ChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                rows.push(parse_chunk_row(batch, i)?);
            }
        }

        Ok(rows)
    }

    /// Look up a single chunk by row_id.
    ///
    /// Returns None if the chunk doesn't exist.
    pub async fn get_by_row_id(&self, row_id: &str) -> Result<Option<ChunkRow>> {
        let predicate = eq_str("row_id", row_id);

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunk by row_id: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        for batch in &batches {
            if batch.num_rows() > 0 {
                let row = parse_chunk_row(batch, 0)?;
                return Ok(Some(row));
            }
        }

        Ok(None)
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

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunks by file_id: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        let mut rows: Vec<ChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                rows.push(parse_chunk_row(batch, i)?);
            }
        }

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

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunks by file_id: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        let mut rows: Vec<ChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                rows.push(parse_chunk_row(batch, i)?);
            }
        }

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

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunks for label: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        let mut rows: Vec<ChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                rows.push(parse_chunk_row(batch, i)?);
            }
        }

        Ok(rows)
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

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunks by row_ids: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        let mut rows: Vec<ChunkRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                rows.push(parse_chunk_row(batch, i)?);
            }
        }

        Ok(rows)
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

#[cfg(test)]
mod tests;
