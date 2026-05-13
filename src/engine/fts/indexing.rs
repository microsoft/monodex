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
//! 3. Determine currently indexed row_ids by scanning Tantivy's term dictionary
//! 4. Compute diff: additions and removals
//! 5. Apply deletions and additions to Tantivy
//! 6. Commit and write updated manifest

use anyhow::Result;
use std::collections::BTreeSet;
use tantivy::doc;

use crate::engine::fts::index::FtsIndex;
use crate::engine::fts::manifest::{FtsManifest, reconcile_from_index};
use crate::engine::fts::tokenizer::tokenize_text;
use crate::engine::identifier::LabelId;
use crate::engine::storage::ChunkStorage;
/// Statistics from an FTS indexing operation.
pub struct FtsIndexingStats {
    /// Total number of live row_ids in the index after indexing.
    pub live_row_ids: usize,
    /// Number of new documents added during this indexing run.
    pub added: usize,
    /// Number of documents removed during this indexing run.
    pub removed: usize,
    /// Number of chunks skipped due to producing zero tokens.
    pub zero_token_skipped: usize,
    /// Row IDs of chunks that produced zero tokens (for diagnostics).
    pub zero_token_row_ids: Vec<String>,
}

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
/// * `is_commit_mode` - If true, wait for merging threads after commit
///
/// # Returns
/// Ok(FtsIndexingStats) on success with indexing statistics, or an error if indexing fails.
pub async fn index_chunks_for_fts(
    db_path: &std::path::Path,
    label_id: &LabelId,
    chunk_storage: &ChunkStorage,
    is_commit_mode: bool,
) -> Result<FtsIndexingStats> {
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
    let mut zero_token_row_ids: Vec<String> = Vec::new();

    for row_id in &additions {
        if let Some(chunk) = chunk_map.get(row_id) {
            // Tokenize to check for zero tokens
            let tokens = tokenize_text(&chunk.text);

            if tokens.is_empty() {
                // Track zero-token chunks for diagnostics, skip indexing
                zero_token_row_ids.push(row_id.clone());
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

    // Step 10: Write manifest with current compatibility metadata
    let manifest = FtsManifest::new();
    fts_index.write_manifest(&manifest)?;

    // Compute stats
    let added_count = successfully_added.len();
    let removed_count = removals.len();
    let zero_token_skipped = additions.len() - added_count;

    let final_indexed: BTreeSet<String> = currently_indexed
        .difference(&removals)
        .cloned()
        .chain(successfully_added)
        .collect();

    Ok(FtsIndexingStats {
        live_row_ids: final_indexed.len(),
        added: added_count,
        removed: removed_count,
        zero_token_skipped,
        zero_token_row_ids,
    })
}

/// Get the currently indexed row_ids from the FTS index.
///
/// Always derives the live row_id set from Tantivy's term dictionary.
/// The manifest stores only compatibility metadata (schema/tokenizer IDs);
/// it does not track row_ids.
fn get_currently_indexed_row_ids(fts_index: &FtsIndex) -> Result<BTreeSet<String>> {
    let reader = fts_index.reader()?;
    let searcher = reader.searcher();

    // Performance guardrail:
    // FTS term-dictionary scans are O(live_docs) per crawl. If profiling on a
    // real-scale catalog shows this scan dominating crawl time, prefer adding
    // periodic FTS commits during the crawl. Do not introduce a manifest-based
    // shortcut to skip the scan; the scan is the crawl correctness boundary.
    reconcile_from_index(&searcher, fts_index.fields.row_id)
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
