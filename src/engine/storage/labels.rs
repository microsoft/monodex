//! Purpose: Provide typed operations on the `label_metadata` table.
//! Edit here when: Adding/modifying label metadata storage operations.
//! Do not edit here for: Row types (see rows.rs), chunk operations (see chunks/mod.rs), database open logic (see database.rs).

use anyhow::{Result, anyhow};
use arrow_array::{
    Array, ArrayRef, BooleanArray, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::SchemaRef;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};
use std::path::PathBuf;
use std::sync::Arc;

use crate::engine::storage::LabelMetadataRow;
use crate::engine::storage::locks::acquire_commit_mutex;
use crate::engine::storage::predicate::eq_str;

/// Convert an iterator of LabelMetadataRows to a RecordBatch.
fn label_metadata_rows_to_record_batch<'a>(
    rows: impl IntoIterator<Item = &'a LabelMetadataRow>,
    schema: SchemaRef,
) -> Result<RecordBatch> {
    let rows: Vec<&LabelMetadataRow> = rows.into_iter().collect();

    let label_id: StringArray = rows.iter().map(|r| Some(r.label_id.as_str())).collect();
    let catalog: StringArray = rows.iter().map(|r| Some(r.catalog.as_str())).collect();
    let label: StringArray = rows.iter().map(|r| Some(r.label.as_str())).collect();
    let source_kind: StringArray = rows.iter().map(|r| Some(r.source_kind.as_str())).collect();
    let vector_source: StringArray = rows.iter().map(|r| r.vector_source.as_deref()).collect();
    let vector_complete: BooleanArray = rows.iter().map(|r| Some(r.vector_complete)).collect();
    let fts_source: StringArray = rows.iter().map(|r| r.fts_source.as_deref()).collect();
    let fts_complete: BooleanArray = rows.iter().map(|r| Some(r.fts_complete)).collect();
    let updated_at_unix_secs: Int64Array =
        rows.iter().map(|r| Some(r.updated_at_unix_secs)).collect();

    let columns: Vec<ArrayRef> = vec![
        Arc::new(label_id),
        Arc::new(catalog),
        Arc::new(label),
        Arc::new(source_kind),
        Arc::new(vector_source),
        Arc::new(vector_complete),
        Arc::new(fts_source),
        Arc::new(fts_complete),
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
        .column_by_name("label_id")
        .ok_or_else(|| anyhow!("label_id column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("label_id column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let catalog = batch
        .column_by_name("catalog")
        .ok_or_else(|| anyhow!("catalog column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("catalog column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let label = batch
        .column_by_name("label")
        .ok_or_else(|| anyhow!("label column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("label column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    let source_kind = batch
        .column_by_name("source_kind")
        .ok_or_else(|| anyhow!("source_kind column not found"))?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("source_kind column is not a StringArray"))?
        .value(row_idx)
        .to_string();

    // Helper to read nullable string column
    let read_nullable_string = |col_name: &str| -> Result<Option<String>> {
        let col = batch
            .column_by_name(col_name)
            .ok_or_else(|| anyhow!("{} column not found", col_name))?
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| anyhow!("{} column is not a StringArray", col_name))?;
        if col.is_null(row_idx) {
            Ok(None)
        } else {
            Ok(Some(col.value(row_idx).to_string()))
        }
    };

    let vector_source = read_nullable_string("vector_source")?;
    let fts_source = read_nullable_string("fts_source")?;

    let vector_complete = batch
        .column_by_name("vector_complete")
        .ok_or_else(|| anyhow!("vector_complete column not found"))?
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| anyhow!("vector_complete column is not a BooleanArray"))?
        .value(row_idx);

    let fts_complete = batch
        .column_by_name("fts_complete")
        .ok_or_else(|| anyhow!("fts_complete column not found"))?
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| anyhow!("fts_complete column is not a BooleanArray"))?
        .value(row_idx);

    let updated_at_unix_secs = batch
        .column_by_name("updated_at_unix_secs")
        .ok_or_else(|| anyhow!("updated_at_unix_secs column not found"))?
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow!("updated_at_unix_secs column is not an Int64Array"))?
        .value(row_idx);

    let row = LabelMetadataRow {
        label_id,
        catalog,
        label,
        source_kind,
        vector_source,
        vector_complete,
        fts_source,
        fts_complete,
        updated_at_unix_secs,
    };

    row.validate()?;
    Ok(row)
}

/// Label metadata storage operations for LanceDB.
pub struct LabelStorage {
    table: Arc<lancedb::table::Table>,
    db_path: PathBuf,
}

impl LabelStorage {
    /// Create a new LabelStorage wrapping a table reference.
    pub fn new(table: Arc<lancedb::table::Table>, db_path: PathBuf) -> Self {
        Self { table, db_path }
    }

    /// Upsert a single label metadata row by label_id.
    pub async fn upsert(&self, row: &LabelMetadataRow) -> Result<()> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

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
        let predicate = eq_str("label_id", label_id);

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
        let predicate = eq_str("catalog", catalog);

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
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

        let predicate = eq_str("label_id", label_id);

        self.table
            .delete(&predicate)
            .await
            .map_err(|e| anyhow!("Failed to delete label metadata: {}", e))?;

        Ok(())
    }

    /// Delete all label metadata rows for a given catalog, returning the count deleted.
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
            .map_err(|e| anyhow!("Failed to delete label metadata by catalog: {}", e))?;

        Ok(count as u64)
    }

    /// Truncate the table (empty all rows, preserve schema).
    pub async fn truncate(&self) -> Result<()> {
        let _commit_guard = acquire_commit_mutex(&self.db_path)?;

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
    use crate::engine::storage::SOURCE_KIND_GIT_COMMIT;
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

        // Pass db_path for commit mutex acquisition in write methods
        (tmp_dir, LabelStorage::new(Arc::new(table), db_path))
    }

    fn test_label_metadata_row(label: &str) -> LabelMetadataRow {
        LabelMetadataRow {
            label_id: format!("test-catalog:{}", label),
            catalog: "test-catalog".to_string(),
            label: label.to_string(),
            source_kind: SOURCE_KIND_GIT_COMMIT.to_string(),
            vector_source: Some("abc123def456".to_string()),
            vector_complete: true,
            fts_source: Some("abc123def456".to_string()),
            fts_complete: true,
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
            source_kind: SOURCE_KIND_GIT_COMMIT.to_string(),
            vector_source: Some("xyz".to_string()),
            vector_complete: true,
            fts_source: Some("xyz".to_string()),
            fts_complete: true,
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
        row.vector_complete = false;
        storage.upsert(&row).await.unwrap();

        // Upsert with updated vector_complete
        row.vector_complete = true;
        storage.upsert(&row).await.unwrap();

        let retrieved = storage
            .get_by_label_id("test-catalog:main")
            .await
            .unwrap()
            .unwrap();
        assert!(retrieved.vector_complete);

        // Verify only one row exists
        let rows = storage.list_for_catalog("test-catalog").await.unwrap();
        assert_eq!(rows.len(), 1);
    }
}
