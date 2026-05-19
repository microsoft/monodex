//! Purpose: Provide typed operations on the `label_metadata` table.
//! Edit here when: Adding/modifying label metadata storage operations.
//! Do not edit here for: Row types (see rows.rs), chunk operations (see chunks/storage.rs), database open logic (see database.rs).

use super::arrow;
use anyhow::{Result, anyhow};
use arrow_array::{
    ArrayRef, BooleanArray, Int64Array, RecordBatch, RecordBatchIterator, StringArray,
};
use arrow_schema::SchemaRef;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::engine::retrieval::RetrievalMethod;
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
    let label_id = arrow::read_required_string(batch, row_idx, "label_id")?;
    let catalog = arrow::read_required_string(batch, row_idx, "catalog")?;
    let label = arrow::read_required_string(batch, row_idx, "label")?;
    let source_kind = arrow::read_required_string(batch, row_idx, "source_kind")?;
    let vector_source = arrow::read_nullable_string(batch, row_idx, "vector_source")?;
    let fts_source = arrow::read_nullable_string(batch, row_idx, "fts_source")?;
    let vector_complete = arrow::read_required_bool(batch, row_idx, "vector_complete")?;
    let fts_complete = arrow::read_required_bool(batch, row_idx, "fts_complete")?;
    let updated_at_unix_secs = arrow::read_required_i64(batch, row_idx, "updated_at_unix_secs")?;

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

/// Read the retrieval selection from a label metadata row.
///
/// The selection is derived from which `<method>_source` columns are non-NULL.
/// A method is in the selection iff its source column is Some.
/// This is the single source of truth for retrieval selection.
pub fn read_selection(row: &LabelMetadataRow) -> BTreeSet<RetrievalMethod> {
    let mut selection = BTreeSet::new();
    if row.vector_source.is_some() {
        selection.insert(RetrievalMethod::Vector);
    }
    if row.fts_source.is_some() {
        selection.insert(RetrievalMethod::Fts);
    }
    selection
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
        arrow::collect_first_row(&self.table, &predicate, "label metadata", |batch, i| {
            parse_label_metadata_row(batch, i)
        })
        .await
    }

    /// List all label metadata rows for a given catalog.
    ///
    /// Used by label-reassignment discovery.
    pub async fn list_for_catalog(&self, catalog: &str) -> Result<Vec<LabelMetadataRow>> {
        let predicate = eq_str("catalog", catalog);
        arrow::collect_rows(
            &self.table,
            &predicate,
            "label metadata for catalog",
            parse_label_metadata_row,
        )
        .await
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

    #[test]
    fn test_read_selection_both_methods() {
        let row = test_label_metadata_row("main");
        let selection = read_selection(&row);
        assert_eq!(selection.len(), 2);
        assert!(selection.contains(&RetrievalMethod::Fts));
        assert!(selection.contains(&RetrievalMethod::Vector));
    }

    #[test]
    fn test_read_selection_vector_only() {
        let row = LabelMetadataRow {
            label_id: "test-catalog:main".to_string(),
            catalog: "test-catalog".to_string(),
            label: "main".to_string(),
            source_kind: SOURCE_KIND_GIT_COMMIT.to_string(),
            vector_source: Some("abc123".to_string()),
            vector_complete: true,
            fts_source: None, // FTS not in selection
            fts_complete: false,
            updated_at_unix_secs: 1700000000,
        };
        let selection = read_selection(&row);
        assert_eq!(selection.len(), 1);
        assert!(selection.contains(&RetrievalMethod::Vector));
        assert!(!selection.contains(&RetrievalMethod::Fts));
    }

    #[test]
    fn test_read_selection_fts_only() {
        let row = LabelMetadataRow {
            label_id: "test-catalog:main".to_string(),
            catalog: "test-catalog".to_string(),
            label: "main".to_string(),
            source_kind: SOURCE_KIND_GIT_COMMIT.to_string(),
            vector_source: None, // Vector not in selection
            vector_complete: false,
            fts_source: Some("abc123".to_string()),
            fts_complete: true,
            updated_at_unix_secs: 1700000000,
        };
        let selection = read_selection(&row);
        assert_eq!(selection.len(), 1);
        assert!(selection.contains(&RetrievalMethod::Fts));
        assert!(!selection.contains(&RetrievalMethod::Vector));
    }

    #[test]
    fn test_read_selection_empty() {
        let row = LabelMetadataRow {
            label_id: "test-catalog:main".to_string(),
            catalog: "test-catalog".to_string(),
            label: "main".to_string(),
            source_kind: SOURCE_KIND_GIT_COMMIT.to_string(),
            vector_source: None,
            vector_complete: false,
            fts_source: None,
            fts_complete: false,
            updated_at_unix_secs: 1700000000,
        };
        let selection = read_selection(&row);
        assert!(selection.is_empty());
    }
}
