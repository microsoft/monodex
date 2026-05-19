//! Arrow `RecordBatch` encoding and decoding for chunk rows.
//!
//! Purpose: Convert between ChunkRow structs and Arrow RecordBatch format for
//!   LanceDB storage operations.
//!
//! Edit here when: Changing how chunk rows are serialized to/from Arrow format,
//!   adding new columns to the chunks table schema, or modifying vector column handling.
//! Do not edit here for: Chunk storage operations (see storage.rs), Arrow readers
//!   for query results (see engine/storage/arrow.rs).

use anyhow::{Result, anyhow};
use arrow_array::{
    Array, ArrayRef, BooleanArray, FixedSizeListArray, Float32Array, Int32Array, ListArray,
    RecordBatch, StringArray,
};
use arrow_buffer::{OffsetBuffer, ScalarBuffer};
use arrow_schema::{DataType, Field, SchemaRef};
use std::sync::Arc;

use crate::engine::schema::VECTOR_DIMENSION;
use crate::engine::storage::ChunkRow;
use crate::engine::storage::arrow;

/// Policy for handling the vector column during upsert.
///
/// This is a private implementation detail of the upsert methods.
pub(super) enum VectorPolicy<'a> {
    /// Include vectors in the upsert batch.
    With(&'a [Vec<f32>]),
    /// Omit vectors from the upsert batch (preserves existing vectors on matched rows).
    Without,
}

/// Convert a slice of ChunkRows to a RecordBatch with configurable vector handling.
///
/// This is the unified function for building RecordBatch from ChunkRows,
/// handling both vector and non-vector upserts.
pub(super) fn chunk_rows_to_record_batch(
    rows: &[ChunkRow],
    vectors: VectorPolicy<'_>,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let row_id: StringArray = rows.iter().map(|r| Some(r.row_id.as_str())).collect();
    let text: StringArray = rows.iter().map(|r| Some(r.text.as_str())).collect();

    // Build common columns (everything except vector). The vector column, if present,
    // will be inserted at position 2 (between text and catalog).
    let mut columns: Vec<ArrayRef> = vec![
        Arc::new(row_id),
        Arc::new(text),
        // vector column placeholder - inserted at position 2 for VectorPolicy::With
        Arc::new(build_string_array(rows.iter().map(|r| r.catalog.as_str()))),
        build_string_list_array(
            &rows
                .iter()
                .map(|r| r.active_label_ids.as_slice())
                .collect::<Vec<_>>(),
        ),
        Arc::new(build_string_array(
            rows.iter().map(|r| r.embedder_id.as_str()),
        )),
        Arc::new(build_string_array(
            rows.iter().map(|r| r.chunker_id.as_str()),
        )),
        Arc::new(build_string_array(rows.iter().map(|r| r.blob_id.as_str()))),
        Arc::new(build_string_array(
            rows.iter().map(|r| r.content_hash.as_str()),
        )),
        Arc::new(build_string_array(rows.iter().map(|r| r.file_id.as_str()))),
        Arc::new(build_string_array(
            rows.iter().map(|r| r.relative_path.as_str()),
        )),
        Arc::new(build_string_array(
            rows.iter().map(|r| r.package_name.as_str()),
        )),
        Arc::new(build_string_array(
            rows.iter().map(|r| r.source_uri.as_str()),
        )),
        Arc::new(
            rows.iter()
                .map(|r| Some(r.chunk_ordinal))
                .collect::<Int32Array>(),
        ),
        Arc::new(
            rows.iter()
                .map(|r| Some(r.chunk_count))
                .collect::<Int32Array>(),
        ),
        Arc::new(
            rows.iter()
                .map(|r| Some(r.start_line))
                .collect::<Int32Array>(),
        ),
        Arc::new(
            rows.iter()
                .map(|r| Some(r.end_line))
                .collect::<Int32Array>(),
        ),
        Arc::new(
            rows.iter()
                .map(|r| r.symbol_name.as_deref())
                .collect::<StringArray>(),
        ),
        Arc::new(build_string_array(
            rows.iter().map(|r| r.chunk_type.as_str()),
        )),
        Arc::new(build_string_array(
            rows.iter().map(|r| r.chunk_kind.as_str()),
        )),
        Arc::new(
            rows.iter()
                .map(|r| r.breadcrumb.as_deref())
                .collect::<StringArray>(),
        ),
        Arc::new(
            rows.iter()
                .map(|r| r.split_part_ordinal)
                .collect::<Int32Array>(),
        ),
        Arc::new(
            rows.iter()
                .map(|r| r.split_part_count)
                .collect::<Int32Array>(),
        ),
        Arc::new(
            rows.iter()
                .map(|r| Some(r.file_complete))
                .collect::<BooleanArray>(),
        ),
    ];

    // Vector column handling depends on policy
    let final_schema = match vectors {
        VectorPolicy::With(all_vectors) => {
            // Validate vectors match rows count
            if all_vectors.len() != rows.len() {
                return Err(anyhow!(
                    "Rows and vectors count mismatch: {} vs {}",
                    rows.len(),
                    all_vectors.len()
                ));
            }

            // Validate each vector's dimension
            for vector in all_vectors {
                if vector.len() != VECTOR_DIMENSION {
                    return Err(anyhow!(
                        "Vector dimension mismatch: expected {}, got {}",
                        VECTOR_DIMENSION,
                        vector.len()
                    ));
                }
            }

            // Build vector column
            let mut all_vector_values: Vec<f32> = Vec::with_capacity(VECTOR_DIMENSION * rows.len());
            for vector in all_vectors {
                all_vector_values.extend_from_slice(vector);
            }
            let vector_values: Float32Array = all_vector_values.into();
            let vector_field = Field::new("item", DataType::Float32, true);
            let vector: ArrayRef = Arc::new(FixedSizeListArray::new(
                Arc::new(vector_field),
                VECTOR_DIMENSION as i32,
                Arc::new(vector_values),
                None,
            ));

            // Insert vector at position 2 (between text and catalog)
            columns.insert(2, vector);
            schema
        }
        VectorPolicy::Without => {
            // Project the schema to exclude the vector column
            Arc::new(
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
            )
        }
    };

    RecordBatch::try_new(final_schema, columns)
        .map_err(|e| anyhow!("Failed to create RecordBatch: {}", e))
}

/// Build a StringArray from an iterator of &str values (all non-null).
fn build_string_array<'a>(values: impl Iterator<Item = &'a str>) -> StringArray {
    values.map(Some).collect()
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
pub(super) fn parse_chunk_row(batch: &RecordBatch, row_idx: usize) -> Result<ChunkRow> {
    // "vector" is intentionally not read here; ChunkRow does not carry the embedding.

    let row_id = arrow::read_required_string(batch, row_idx, "row_id")?;
    let text = arrow::read_required_string(batch, row_idx, "text")?;
    let catalog = arrow::read_required_string(batch, row_idx, "catalog")?;
    let active_label_ids = arrow::read_string_list(batch, row_idx, "active_label_ids")?;
    let embedder_id = arrow::read_required_string(batch, row_idx, "embedder_id")?;
    let chunker_id = arrow::read_required_string(batch, row_idx, "chunker_id")?;
    let blob_id = arrow::read_required_string(batch, row_idx, "blob_id")?;
    let content_hash = arrow::read_required_string(batch, row_idx, "content_hash")?;
    let file_id = arrow::read_required_string(batch, row_idx, "file_id")?;
    let relative_path = arrow::read_required_string(batch, row_idx, "relative_path")?;
    let package_name = arrow::read_required_string(batch, row_idx, "package_name")?;
    let source_uri = arrow::read_required_string(batch, row_idx, "source_uri")?;
    let chunk_ordinal = arrow::read_required_i32(batch, row_idx, "chunk_ordinal")?;
    let chunk_count = arrow::read_required_i32(batch, row_idx, "chunk_count")?;
    let start_line = arrow::read_required_i32(batch, row_idx, "start_line")?;
    let end_line = arrow::read_required_i32(batch, row_idx, "end_line")?;
    let symbol_name = arrow::read_nullable_string(batch, row_idx, "symbol_name")?;
    let chunk_type = arrow::read_required_string(batch, row_idx, "chunk_type")?;
    let chunk_kind = arrow::read_required_string(batch, row_idx, "chunk_kind")?;
    let breadcrumb = arrow::read_nullable_string(batch, row_idx, "breadcrumb")?;
    let split_part_ordinal = arrow::read_nullable_i32(batch, row_idx, "split_part_ordinal")?;
    let split_part_count = arrow::read_nullable_i32(batch, row_idx, "split_part_count")?;
    let file_complete = arrow::read_required_bool(batch, row_idx, "file_complete")?;

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
pub(super) fn extract_distance(batch: &RecordBatch, row_idx: usize) -> Result<f32> {
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
