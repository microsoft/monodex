//! Purpose: Typed Arrow readers and query-collect helpers shared by storage submodules.
//! Edit here when: changing how storage code reads typed values from Arrow batches or collects LanceDB query batches into typed rows.
//! Do not edit here for: row type definitions (see rows.rs), SQL predicate builders (see predicate.rs), or table-specific operations (see chunks/storage.rs, labels.rs). For chunk-row Arrow encoding and decoding, see chunks/arrow_encoding.rs.

use anyhow::{Result, anyhow};
use arrow_array::{
    Array, BooleanArray, Int32Array, Int64Array, ListArray, RecordBatch, StringArray,
};
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

/// Read a required string column from a RecordBatch row.
///
/// Returns an error if the column is missing or not a StringArray.
/// Does not check for null values.
pub(super) fn read_required_string(
    batch: &RecordBatch,
    row_idx: usize,
    col_name: &str,
) -> Result<String> {
    let arr = batch
        .column_by_name(col_name)
        .ok_or_else(|| anyhow!("{} column not found", col_name))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("{} column is not a StringArray", col_name))?;
    Ok(arr.value(row_idx).to_string())
}

/// Read a nullable string column from a RecordBatch row.
///
/// Returns None if the value is null, otherwise returns Some(String).
/// Returns an error if the column is missing or not a StringArray.
pub(super) fn read_nullable_string(
    batch: &RecordBatch,
    row_idx: usize,
    col_name: &str,
) -> Result<Option<String>> {
    let arr = batch
        .column_by_name(col_name)
        .ok_or_else(|| anyhow!("{} column not found", col_name))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("{} column is not a StringArray", col_name))?;
    if arr.is_null(row_idx) {
        Ok(None)
    } else {
        Ok(Some(arr.value(row_idx).to_string()))
    }
}

/// Read a required i32 column from a RecordBatch row.
///
/// Returns an error if the column is missing or not an Int32Array.
pub(super) fn read_required_i32(
    batch: &RecordBatch,
    row_idx: usize,
    col_name: &str,
) -> Result<i32> {
    let arr = batch
        .column_by_name(col_name)
        .ok_or_else(|| anyhow!("{} column not found", col_name))?
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("{} column is not an Int32Array", col_name))?;
    Ok(arr.value(row_idx))
}

/// Read a nullable i32 column from a RecordBatch row.
///
/// Returns None if the value is null, otherwise returns Some(i32).
/// Returns an error if the column is missing or not an Int32Array.
pub(super) fn read_nullable_i32(
    batch: &RecordBatch,
    row_idx: usize,
    col_name: &str,
) -> Result<Option<i32>> {
    let arr = batch
        .column_by_name(col_name)
        .ok_or_else(|| anyhow!("{} column not found", col_name))?
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow!("{} column is not an Int32Array", col_name))?;
    if arr.is_null(row_idx) {
        Ok(None)
    } else {
        Ok(Some(arr.value(row_idx)))
    }
}

/// Read a required i64 column from a RecordBatch row.
///
/// Returns an error if the column is missing or not an Int64Array.
pub(super) fn read_required_i64(
    batch: &RecordBatch,
    row_idx: usize,
    col_name: &str,
) -> Result<i64> {
    let arr = batch
        .column_by_name(col_name)
        .ok_or_else(|| anyhow!("{} column not found", col_name))?
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow!("{} column is not an Int64Array", col_name))?;
    Ok(arr.value(row_idx))
}

/// Read a required bool column from a RecordBatch row.
///
/// Returns an error if the column is missing or not a BooleanArray.
pub(super) fn read_required_bool(
    batch: &RecordBatch,
    row_idx: usize,
    col_name: &str,
) -> Result<bool> {
    let arr = batch
        .column_by_name(col_name)
        .ok_or_else(|| anyhow!("{} column not found", col_name))?
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| anyhow!("{} column is not a BooleanArray", col_name))?;
    Ok(arr.value(row_idx))
}

/// Read a list-of-strings column from a RecordBatch row.
///
/// Returns an empty Vec if the value is null, otherwise returns all string values.
/// Returns an error if the column is missing, not a ListArray, or the inner array
/// is not a StringArray.
pub(super) fn read_string_list(
    batch: &RecordBatch,
    row_idx: usize,
    col_name: &str,
) -> Result<Vec<String>> {
    let list_array = batch
        .column_by_name(col_name)
        .ok_or_else(|| anyhow!("{} column not found", col_name))?
        .as_any()
        .downcast_ref::<ListArray>()
        .ok_or_else(|| anyhow!("{} column is not a ListArray", col_name))?;
    let list_value = list_array.value(row_idx);
    let string_array = list_value
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("{} inner array is not a StringArray", col_name))?;
    Ok((0..string_array.len())
        .map(|i| string_array.value(i).to_string())
        .collect())
}

/// Execute a query and collect all rows into a Vec.
///
/// Executes `table.query().only_if(predicate).execute()`, collects batches,
/// and calls the parse function for each row.
///
/// # Arguments
/// * `table` - The LanceDB table to query
/// * `predicate` - SQL predicate for filtering
/// * `query_label` - Label for error messages (e.g., "chunks by file_id")
/// * `parse` - Function to parse each row from a RecordBatch
///
/// # Returns
/// All parsed rows in the order LanceDB yielded them.
pub(super) async fn collect_rows<T, F>(
    table: &lancedb::table::Table,
    predicate: &str,
    query_label: &str,
    parse: F,
) -> Result<Vec<T>>
where
    F: Fn(&RecordBatch, usize) -> Result<T>,
{
    let results = table
        .query()
        .only_if(predicate)
        .execute()
        .await
        .map_err(|e| anyhow!("Failed to query {}: {}", query_label, e))?;

    let batches = results
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect {} results: {}", query_label, e))?;

    let mut rows: Vec<T> = Vec::new();
    for batch in &batches {
        for i in 0..batch.num_rows() {
            rows.push(parse(batch, i)?);
        }
    }

    Ok(rows)
}

/// Execute a query and return the first row, if any.
///
/// Executes `table.query().only_if(predicate).execute()`, collects batches,
/// and returns the first non-empty row parsed.
///
/// # Arguments
/// * `table` - The LanceDB table to query
/// * `predicate` - SQL predicate for filtering
/// * `query_label` - Label for error messages (e.g., "chunk by row_id")
/// * `parse` - Function to parse each row from a RecordBatch
///
/// # Returns
/// Some(parsed_row) if found, None if no rows match.
pub(super) async fn collect_first_row<T, F>(
    table: &lancedb::table::Table,
    predicate: &str,
    query_label: &str,
    parse: F,
) -> Result<Option<T>>
where
    F: Fn(&RecordBatch, usize) -> Result<T>,
{
    let results = table
        .query()
        .only_if(predicate)
        .execute()
        .await
        .map_err(|e| anyhow!("Failed to query {}: {}", query_label, e))?;

    let batches = results
        .try_collect::<Vec<_>>()
        .await
        .map_err(|e| anyhow!("Failed to collect {} results: {}", query_label, e))?;

    for batch in &batches {
        if batch.num_rows() > 0 {
            return Ok(Some(parse(batch, 0)?));
        }
    }

    Ok(None)
}
