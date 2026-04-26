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
use std::sync::Arc;

use crate::engine::schema::VECTOR_DIMENSION;
use crate::engine::storage::{ChunkRow, ScoredChunkRow};

/// Batch size for upsert operations. Storage-layer internal detail.
const UPSERT_BATCH_SIZE: usize = 1000;

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

    let point_id: StringArray = rows
        .iter()
        .map(|(r, _)| Some(r.point_id.as_str()))
        .collect();
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
        Arc::new(point_id),
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
    let point_id = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("point_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let text = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("text column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    // Skip vector column (column 2) - we don't need it for the row struct

    let catalog = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("catalog column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let active_label_ids = {
        let list_array = batch
            .column(4)
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
        .column(5)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("embedder_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let chunker_id = batch
        .column(6)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("chunker_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let blob_id = batch
        .column(7)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("blob_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let content_hash = batch
        .column(8)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("content_hash column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let file_id = batch
        .column(9)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("file_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let relative_path = batch
        .column(10)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("relative_path column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let package_name = batch
        .column(11)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("package_name column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let source_uri = batch
        .column(12)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("source_uri column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let chunk_ordinal = batch
        .column(13)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("chunk_ordinal column is not an Int32Array"))?
        .value(row_idx);

    let chunk_count = batch
        .column(14)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("chunk_count column is not an Int32Array"))?
        .value(row_idx);

    let start_line = batch
        .column(15)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("start_line column is not an Int32Array"))?
        .value(row_idx);

    let end_line = batch
        .column(16)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("end_line column is not an Int32Array"))?
        .value(row_idx);

    // Nullable string fields
    let symbol_name = {
        let arr = batch
            .column(17)
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
        .column(18)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("chunk_type column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let chunk_kind = batch
        .column(19)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("chunk_kind column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let breadcrumb = {
        let arr = batch
            .column(20)
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
            .column(21)
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
            .column(22)
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
        .column(23)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| anyhow!("file_complete column is not a BooleanArray"))?
        .value(row_idx);

    let row = ChunkRow {
        point_id,
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

/// Chunk storage operations for LanceDB.
pub struct ChunkStorage {
    table: Arc<lancedb::table::Table>,
}

impl ChunkStorage {
    /// Create a new ChunkStorage wrapping a table reference.
    pub fn new(table: Arc<lancedb::table::Table>) -> Self {
        Self { table }
    }
    /// Upsert a batch of chunk rows with their embedding vectors by point_id.
    ///
    /// This is the primary method for writing chunks during crawl, where we have
    /// both the row data and the computed embedding vectors.
    ///
    /// Matched rows are updated in place (same point_id implies same content
    /// by construction, since file_id already incorporates blob_id + path +
    /// embedder + chunker).
    ///
    /// Batching is handled internally; callers may pass any number of rows.
    pub async fn upsert_with_vectors(&self, rows: &[ChunkRow], vectors: &[Vec<f32>]) -> Result<()> {
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

        let schema = self.table.schema().await?;

        // Process in batches internally
        for (batch_rows, batch_vectors) in rows
            .chunks(UPSERT_BATCH_SIZE)
            .zip(vectors.chunks(UPSERT_BATCH_SIZE))
        {
            let rows_with_vectors: Vec<(&ChunkRow, &[f32])> = batch_rows
                .iter()
                .zip(batch_vectors.iter().map(|v| v.as_slice()))
                .collect();
            let batch = chunk_rows_to_record_batch_with_vectors(
                rows_with_vectors.into_iter(),
                schema.clone(),
            )?;

            // Use merge_insert for proper upsert semantics
            let reader = RecordBatchIterator::new(std::iter::once(Ok(batch)), schema.clone());
            let mut builder = self.table.merge_insert(&["point_id"]);
            builder
                .when_matched_update_all(None)
                .when_not_matched_insert_all();
            builder.execute(Box::new(reader)).await?;
        }

        Ok(())
    }

    /// Look up a single chunk by point_id.
    ///
    /// Returns None if the chunk doesn't exist.
    pub async fn get_by_point_id(&self, point_id: &str) -> Result<Option<ChunkRow>> {
        let predicate = format!("point_id = '{}'", point_id);

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query chunk by point_id: {}", e))?;

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
            "file_id = '{}' AND array_contains(active_label_ids, '{}')",
            file_id, label_id
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
        let predicate = format!("file_id = '{}'", file_id);

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
        let predicate = format!("array_contains(active_label_ids, '{}')", label_id);

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
                "array_contains(active_label_ids, '{}') AND chunk_ordinal = {}",
                label_id, ordinal
            ),
            None => format!("array_contains(active_label_ids, '{}')", label_id),
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

    /// Update the active_label_ids array of a single chunk.
    ///
    /// Uses LanceDB's update() with SQL expression for in-place modification.
    pub async fn update_active_labels(&self, point_id: &str, new_labels: &[String]) -> Result<()> {
        // Reject empty label list - a chunk must belong to at least one label.
        // Callers should use delete_by_point_ids to remove chunks, not clear their labels.
        if new_labels.is_empty() {
            return Err(anyhow!(
                "Cannot update active_label_ids to empty list - a chunk must belong to at least one label"
            ));
        }

        // Build SQL array literal like "['label1', 'label2']"
        let quoted: Vec<String> = new_labels.iter().map(|l| format!("'{}'", l)).collect();
        let labels_sql = format!("[{}]", quoted.join(", "));

        let predicate = format!("point_id = '{}'", point_id);

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
    pub async fn update_file_complete(&self, point_id: &str, complete: bool) -> Result<()> {
        let predicate = format!("point_id = '{}'", point_id);
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

    /// Batch-delete chunks by a list of point_ids.
    pub async fn delete_by_point_ids(&self, point_ids: &[String]) -> Result<()> {
        if point_ids.is_empty() {
            return Ok(());
        }

        // Build IN clause predicate
        let quoted: Vec<String> = point_ids.iter().map(|id| format!("'{}'", id)).collect();
        let predicate = format!("point_id IN ({})", quoted.join(", "));

        self.table
            .delete(&predicate)
            .await
            .map_err(|e| anyhow!("Failed to delete chunks: {}", e))?;

        Ok(())
    }

    /// Delete all chunks matching a given catalog, returning the count deleted.
    pub async fn delete_by_catalog(&self, catalog: &str) -> Result<u64> {
        let predicate = format!("catalog = '{}'", catalog);

        let count_before = self
            .table
            .count_rows(None)
            .await
            .map_err(|e| anyhow!("Failed to count rows before delete: {}", e))?;

        self.table
            .delete(&predicate)
            .await
            .map_err(|e| anyhow!("Failed to delete chunks by catalog: {}", e))?;

        let count_after = self
            .table
            .count_rows(None)
            .await
            .map_err(|e| anyhow!("Failed to count rows after delete: {}", e))?;

        Ok(count_before.saturating_sub(count_after) as u64)
    }

    /// Truncate the table (empty all rows, preserve schema).
    pub async fn truncate(&self) -> Result<()> {
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
