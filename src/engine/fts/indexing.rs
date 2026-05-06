//! FTS indexing operations.
//!
//! Purpose: Index chunks from LanceDB into Tantivy for full-text search.
//! Edit here when: Changing indexing logic, handling zero-token chunks, manifest reconciliation.
//! Do not edit here for: Schema definitions (see schema.rs), search logic (see search.rs).
//!
//! ## Indexing flow
//!
//! 1. Open or create the FTS index for the label
//! 2. Read all chunks for the label from LanceDB
//! 3. Determine currently indexed row_ids (from manifest or by scanning Tantivy)
//! 4. Compute diff: additions and removals
//! 5. Apply deletions and additions to Tantivy
//! 6. Commit and write updated manifest

use anyhow::Result;
use std::collections::BTreeSet;
use tantivy::TantivyDocument;
use tantivy::doc;
use tantivy::schema::Value;

use crate::engine::fts::index::FtsIndex;
use crate::engine::fts::manifest::{FtsManifest, ManifestRead};
use crate::engine::fts::tokenizer::tokenize_text;
use crate::engine::identifier::LabelId;
use crate::engine::storage::ChunkStorage;
use crate::engine::warning::{CrawlWarning, WarningSink};

/// Index chunks for a label into Tantivy.
///
/// This is the main entry point for FTS indexing during a crawl. It:
/// 1. Opens or creates the FTS index
/// 2. Reads all chunks for the label from LanceDB
/// 3. Computes the diff between LanceDB and Tantivy state
/// 4. Applies additions and deletions
/// 5. Commits and writes the updated manifest
///
/// # Arguments
/// * `db_path` - Path to the Monodex database root
/// * `label_id` - The label to index
/// * `chunk_storage` - ChunkStorage instance for reading LanceDB chunks
/// * `warnings` - Warning sink for emitting FTS-specific warnings
/// * `is_commit_mode` - If true, wait for merging threads after commit
///
/// # Returns
/// Ok(()) on success, or an error if indexing fails.
pub async fn index_chunks_for_fts(
    db_path: &std::path::Path,
    label_id: &LabelId,
    chunk_storage: &ChunkStorage,
    warnings: WarningSink<'_>,
    is_commit_mode: bool,
) -> Result<()> {
    // Step 1: Open or create the FTS index
    let fts_index = FtsIndex::open_or_create(db_path, label_id)?;

    // Step 2: Open a writer
    let mut writer = fts_index.writer()?;

    // Step 3: Read all chunks for this label from LanceDB
    let chunks = chunk_storage
        .get_chunks_for_label(label_id.as_ref(), None)
        .await?;

    // Extract the row_ids from LanceDB chunks
    let lancedb_row_ids: BTreeSet<String> = chunks.iter().map(|c| c.row_id.clone()).collect();

    // Step 4: Determine currently indexed row_ids
    let currently_indexed = get_currently_indexed_row_ids(&fts_index)?;

    // Step 5: Compute diff
    let additions: BTreeSet<String> = lancedb_row_ids
        .difference(&currently_indexed)
        .cloned()
        .collect();
    let removals: BTreeSet<String> = currently_indexed
        .difference(&lancedb_row_ids)
        .cloned()
        .collect();

    // Build a map for quick chunk lookup
    let chunk_map: std::collections::HashMap<String, &crate::engine::storage::ChunkRow> =
        chunks.iter().map(|c| (c.row_id.clone(), c)).collect();

    // Step 6: Apply removals
    for row_id in &removals {
        let term = tantivy::Term::from_field_text(fts_index.fields.row_id, row_id);
        writer.delete_term(term);
    }

    // Step 7: Apply additions, tracking which were successfully added
    let mut successfully_added: BTreeSet<String> = BTreeSet::new();

    for row_id in &additions {
        if let Some(chunk) = chunk_map.get(row_id) {
            // Tokenize to check for zero tokens
            let tokens = tokenize_text(&chunk.text);

            if tokens.is_empty() {
                // Emit warning and skip
                warnings(CrawlWarning::FtsZeroTokens {
                    row_id: row_id.clone(),
                });
                continue;
            }

            // Build and add the document
            let doc = doc!(
                fts_index.fields.row_id => row_id.clone(),
                fts_index.fields.text => chunk.text.clone(),
            );

            writer.add_document(doc)?;
            successfully_added.insert(row_id.clone());
        }
    }

    // Step 8: Commit
    writer.commit()?;

    // Step 9: Wait for merging threads if in commit mode
    if is_commit_mode {
        writer.wait_merging_threads()?;
    }

    // Step 10: Compute post-commit indexed set and write manifest
    // The manifest contains: currently_indexed - removals + successfully_added
    let final_indexed: BTreeSet<String> = currently_indexed
        .difference(&removals)
        .cloned()
        .chain(successfully_added)
        .collect();

    let manifest = FtsManifest::with_row_ids(final_indexed);
    fts_index.write_manifest(&manifest)?;

    Ok(())
}

/// Get the currently indexed row_ids from the FTS index.
///
/// Uses the manifest fast path when available, falling back to scanning
/// Tantivy segments when the manifest is missing or invalid.
fn get_currently_indexed_row_ids(fts_index: &FtsIndex) -> Result<BTreeSet<String>> {
    match fts_index.read_manifest() {
        ManifestRead::Present(manifest) if !manifest.row_ids.is_empty() => {
            // Manifest fast path: trust the stored row_ids
            // Sanity check: verify count is approximately correct
            let reader = fts_index.reader()?;
            let searcher = reader.searcher();
            let num_docs = searcher.num_docs() as usize;

            // Tolerance for tombstoned docs: manifest count should be within 10x of num_docs
            // (tombstoned docs count in num_docs but not in manifest)
            if manifest.row_ids.len() > num_docs * 10 {
                // Sanity check failed, fall back to scanning
                return scan_tantivy_for_row_ids(fts_index);
            }

            Ok(manifest.row_ids_set())
        }
        _ => {
            // Missing, empty, IdMismatch, or Unreadable: scan Tantivy
            scan_tantivy_for_row_ids(fts_index)
        }
    }
}

/// Scan Tantivy segments to determine the currently indexed row_ids.
///
/// This is the fallback when the manifest is not available or fails validation.
fn scan_tantivy_for_row_ids(fts_index: &FtsIndex) -> Result<BTreeSet<String>> {
    let reader = fts_index.reader()?;
    let searcher = reader.searcher();

    let mut row_ids = BTreeSet::new();

    for segment_reader in searcher.segment_readers().iter() {
        let store_reader = segment_reader
            .get_store_reader(0)
            .map_err(|e| anyhow::anyhow!("Failed to get store reader: {}", e))?;

        // Get the alive bitset to skip tombstoned documents
        let alive_bitset = segment_reader.alive_bitset();

        for doc_id in 0..segment_reader.max_doc() {
            // Skip deleted documents
            let is_alive = if let Some(bitset) = alive_bitset {
                bitset.is_alive(doc_id)
            } else {
                true
            };

            if !is_alive {
                continue;
            }

            // Retrieve the stored document
            let doc: TantivyDocument = match store_reader.get(doc_id) {
                Ok(d) => d,
                Err(_) => continue, // Skip documents we can't read
            };

            // Extract the row_id field
            if let Some(value) = doc.get_first(fts_index.fields.row_id)
                && let Some(row_id) = value.as_str()
            {
                row_ids.insert(row_id.to_string());
            }
        }
    }

    Ok(row_ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Full integration tests require a real LanceDB setup which is complex.
    // The core logic is tested via unit tests in the manifest and index modules.
    // End-to-end tests for indexing are in the tests/ directory.

    #[test]
    fn test_tokenize_text_produces_tokens() {
        let tokens = tokenize_text("getUserProfile");
        assert!(tokens.contains(&"getuserprofile".to_string()));
        assert!(tokens.contains(&"get".to_string()));
        assert!(tokens.contains(&"user".to_string()));
        assert!(tokens.contains(&"profile".to_string()));
    }
}
