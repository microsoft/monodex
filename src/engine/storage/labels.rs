//! Label metadata table operations for LanceDB storage.
//!
//! Purpose: Provide typed operations on the `label_metadata` table.
//!
//! Edit here when: Adding/modifying label metadata storage operations.
//! Do not edit here for: Row types (see rows.rs), chunk operations (see chunks.rs),
//!   database open logic (see database.rs).

use anyhow::{Result, anyhow};
use arrow_array::{
    ArrayRef, BooleanArray, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::SchemaRef;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::sync::Arc;

use crate::engine::storage::LabelMetadataRow;

/// Convert an iterator of LabelMetadataRows to a RecordBatch.
fn label_metadata_rows_to_record_batch<'a>(
    rows: impl IntoIterator<Item = &'a LabelMetadataRow>,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let rows: Vec<&LabelMetadataRow> = rows.into_iter().collect();

    let label_id: StringArray = rows.iter().map(|r| Some(r.label_id.as_str())).collect();
    let catalog: StringArray = rows.iter().map(|r| Some(r.catalog.as_str())).collect();
    let label: StringArray = rows.iter().map(|r| Some(r.label.as_str())).collect();
    let commit_oid: StringArray = rows.iter().map(|r| Some(r.commit_oid.as_str())).collect();
    let source_kind: StringArray = rows.iter().map(|r| Some(r.source_kind.as_str())).collect();
    let crawl_complete: BooleanArray = rows.iter().map(|r| Some(r.crawl_complete)).collect();
    let updated_at_unix_secs: Int64Array =
        rows.iter().map(|r| Some(r.updated_at_unix_secs)).collect();

    let columns: Vec<ArrayRef> = vec![
        Arc::new(label_id),
        Arc::new(catalog),
        Arc::new(label),
        Arc::new(commit_oid),
        Arc::new(source_kind),
        Arc::new(crawl_complete),
        Arc::new(updated_at_unix_secs),
    ];

    RecordBatch::try_new(schema, columns)
        .map_err(|e| anyhow!("Failed to create RecordBatch: {}", e))
}

/// Parse a RecordBatch row into a LabelMetadataRow.
///
/// Validates all identifier fields.
fn parse_label_metadata_row(batch: &RecordBatch, row_idx: usize) -> Result<LabelMetadataRow> {
    let label_id = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("label_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let catalog = batch
        .column(1)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("catalog column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let label = batch
        .column(2)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("label column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let commit_oid = batch
        .column(3)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("commit_oid column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let source_kind = batch
        .column(4)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("source_kind column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let crawl_complete = batch
        .column(5)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| anyhow!("crawl_complete column is not a BooleanArray"))?
        .value(row_idx);

    let updated_at_unix_secs = batch
        .column(6)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow!("updated_at_unix_secs column is not an Int64Array"))?
        .value(row_idx);

    let row = LabelMetadataRow {
        label_id,
        catalog,
        label,
        commit_oid,
        source_kind,
        crawl_complete,
        updated_at_unix_secs,
    };

    row.validate()?;
    Ok(row)
}

/// Label metadata storage operations for LanceDB.
pub struct LabelStorage {
    table: Arc<lancedb::table::Table>,
}

impl LabelStorage {
    /// Create a new LabelStorage wrapping a table reference.
    pub fn new(table: Arc<lancedb::table::Table>) -> Self {
        Self { table }
    }

    /// Upsert a single label metadata row by label_id.
    pub async fn upsert(&self, row: &LabelMetadataRow) -> Result<()> {
        // Validate row before writing
        row.validate()?;

        let schema = self.table.schema().await?;
        let batch = label_metadata_rows_to_record_batch(std::iter::once(row), schema.clone())?;

        // Use merge_insert for proper upsert semantics
        let reader = RecordBatchIterator::new(std::iter::once(Ok(batch)), schema);
        let mut builder = self.table.merge_insert(&["label_id"]);
        builder
            .when_matched_update_all(None)
            .when_not_matched_insert_all();
        builder.execute(Box::new(reader)).await?;

        Ok(())
    }

    /// Look up a single label metadata row by label_id.
    ///
    /// Returns None if the label doesn't exist.
    pub async fn get_by_label_id(&self, label_id: &str) -> Result<Option<LabelMetadataRow>> {
        let predicate = format!("label_id = '{}'", label_id);

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to query label metadata: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        for batch in &batches {
            if batch.num_rows() > 0 {
                let row = parse_label_metadata_row(batch, 0)?;
                return Ok(Some(row));
            }
        }

        Ok(None)
    }

    /// List all label metadata rows for a given catalog.
    ///
    /// Used by label-reassignment discovery.
    pub async fn list_for_catalog(&self, catalog: &str) -> Result<Vec<LabelMetadataRow>> {
        let predicate = format!("catalog = '{}'", catalog);

        let results = self
            .table
            .query()
            .only_if(&predicate)
            .execute()
            .await
            .map_err(|e| anyhow!("Failed to list label metadata for catalog: {}", e))?;

        let batches = results
            .try_collect::<Vec<_>>()
            .await
            .map_err(|e| anyhow!("Failed to collect query results: {}", e))?;

        let mut rows: Vec<LabelMetadataRow> = Vec::new();
        for batch in &batches {
            for i in 0..batch.num_rows() {
                rows.push(parse_label_metadata_row(batch, i)?);
            }
        }

        Ok(rows)
    }

    /// Delete a single label metadata row by label_id.
    pub async fn delete_by_label_id(&self, label_id: &str) -> Result<()> {
        let predicate = format!("label_id = '{}'", label_id);

        self.table
            .delete(&predicate)
            .await
            .map_err(|e| anyhow!("Failed to delete label metadata: {}", e))?;

        Ok(())
    }

    /// Delete all label metadata rows for a given catalog, returning the count deleted.
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
            .map_err(|e| anyhow!("Failed to delete label metadata by catalog: {}", e))?;

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
            .map_err(|e| anyhow!("Failed to truncate label_metadata table: {}", e))?;

        Ok(())
    }

    /// Get the table reference.
    pub fn table(&self) -> Arc<lancedb::table::Table> {
        self.table.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::schema::label_metadata_schema;
    use lancedb::connect;
    use tempfile::TempDir;

    async fn create_test_storage() -> (TempDir, LabelStorage) {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test_db");

        let db = connect(db_path.to_str().unwrap())
            .execute()
            .await
            .expect("Failed to create database");

        let schema = label_metadata_schema();
        let table = db
            .create_empty_table("label_metadata", schema)
            .execute()
            .await
            .expect("Failed to create table");

        (tmp_dir, LabelStorage::new(Arc::new(table)))
    }

    fn test_label_metadata_row(label: &str) -> LabelMetadataRow {
        LabelMetadataRow {
            label_id: format!("test-catalog:{}", label),
            catalog: "test-catalog".to_string(),
            label: label.to_string(),
            commit_oid: "abc123def456".to_string(),
            source_kind: "git-commit".to_string(),
            crawl_complete: true,
            updated_at_unix_secs: 1700000000,
        }
    }

    #[tokio::test]
    async fn test_upsert_and_get() {
        let (_tmp_dir, storage) = create_test_storage().await;

        let row = test_label_metadata_row("main");
        storage.upsert(&row).await.unwrap();

        let retrieved = storage.get_by_label_id("test-catalog:main").await.unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.label_id, "test-catalog:main");
        assert_eq!(retrieved.catalog, row.catalog);
    }

    #[tokio::test]
    async fn test_get_nonexistent() {
        let (_tmp_dir, storage) = create_test_storage().await;

        let retrieved = storage
            .get_by_label_id("test-catalog:nonexistent")
            .await
            .unwrap();
        assert!(retrieved.is_none());
    }

    #[tokio::test]
    async fn test_list_for_catalog() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&test_label_metadata_row("main"))
            .await
            .unwrap();
        storage
            .upsert(&test_label_metadata_row("feature"))
            .await
            .unwrap();

        // Insert a label for a different catalog
        let other_row = LabelMetadataRow {
            label_id: "other-catalog:main".to_string(),
            catalog: "other-catalog".to_string(),
            label: "main".to_string(),
            commit_oid: "xyz".to_string(),
            source_kind: "git-commit".to_string(),
            crawl_complete: true,
            updated_at_unix_secs: 1700000000,
        };
        storage.upsert(&other_row).await.unwrap();

        let rows = storage.list_for_catalog("test-catalog").await.unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn test_delete_by_label_id() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&test_label_metadata_row("main"))
            .await
            .unwrap();
        storage
            .upsert(&test_label_metadata_row("feature"))
            .await
            .unwrap();

        storage
            .delete_by_label_id("test-catalog:main")
            .await
            .unwrap();

        let retrieved = storage.get_by_label_id("test-catalog:main").await.unwrap();
        assert!(retrieved.is_none());

        let feature = storage
            .get_by_label_id("test-catalog:feature")
            .await
            .unwrap();
        assert!(feature.is_some());
    }

    #[tokio::test]
    async fn test_delete_by_catalog() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&test_label_metadata_row("main"))
            .await
            .unwrap();
        storage
            .upsert(&test_label_metadata_row("feature"))
            .await
            .unwrap();

        let count = storage.delete_by_catalog("test-catalog").await.unwrap();
        assert_eq!(count, 2);

        let rows = storage.list_for_catalog("test-catalog").await.unwrap();
        assert_eq!(rows.len(), 0);
    }

    #[tokio::test]
    async fn test_truncate() {
        let (_tmp_dir, storage) = create_test_storage().await;

        storage
            .upsert(&test_label_metadata_row("main"))
            .await
            .unwrap();
        storage
            .upsert(&test_label_metadata_row("feature"))
            .await
            .unwrap();

        storage.truncate().await.unwrap();

        let count = storage.table.count_rows(None).await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_upsert_overwrites() {
        let (_tmp_dir, storage) = create_test_storage().await;

        // Insert initial row
        let mut row = test_label_metadata_row("main");
        row.crawl_complete = false;
        storage.upsert(&row).await.unwrap();

        // Upsert with updated crawl_complete
        row.crawl_complete = true;
        storage.upsert(&row).await.unwrap();

        let retrieved = storage
            .get_by_label_id("test-catalog:main")
            .await
            .unwrap()
            .unwrap();
        assert!(retrieved.crawl_complete);

        // Verify only one row exists
        let rows = storage.list_for_catalog("test-catalog").await.unwrap();
        assert_eq!(rows.len(), 1);
    }
}
