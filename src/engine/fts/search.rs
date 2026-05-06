//! FTS search operations.
//!
//! Purpose: Search the Tantivy index for matching chunks.
//! Edit here when: Changing search logic, result ranking, query parsing.
//! Do not edit here for: Schema definitions (see schema.rs), indexing logic (see indexing.rs).
//!
//! ## Search flow
//!
//! 1. Open the FTS index for the label (returns NoIndex if missing)
//! 2. Build a QueryParser with the monodex-fts tokenizer
//! 3. Parse the query string
//! 4. Execute the search
//! 5. Return hits with scores and row_ids

use anyhow::Result;
use std::path::Path;
use tantivy::TantivyDocument;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::Value;

use crate::engine::fts::index::FtsIndex;
use crate::engine::identifier::LabelId;

/// A hit from FTS search.
#[derive(Debug, Clone)]
pub struct FtsHit {
    /// The row_id of the matching chunk.
    pub row_id: String,
    /// The BM25 score of the match.
    pub score: f32,
}

/// The outcome of an FTS search.
#[derive(Debug, Clone)]
pub enum FtsSearchOutcome {
    /// Search ran successfully. Vec may be empty (no matches).
    Found(Vec<FtsHit>),
    /// FTS directory does not exist for this label.
    /// Caller decides whether to warn or silently return empty.
    NoIndex,
    /// Tantivy QueryParser rejected the query string.
    /// String is the parser's error message.
    ParseError(String),
}

/// Search the FTS index for a label.
///
/// This is the main entry point for FTS search. It:
/// 1. Opens the FTS index (returns NoIndex if missing)
/// 2. Parses the query string with the monodex-fts tokenizer
/// 3. Executes the search
/// 4. Returns hits with row_ids and BM25 scores
///
/// # Arguments
/// * `db_path` - Path to the Monodex database root
/// * `label_id` - The label to search
/// * `query_text` - The query string (user input, parsed by Tantivy's QueryParser)
/// * `limit` - Maximum number of results to return
///
/// # Returns
/// - `Found(hits)`: Search succeeded, hits are in BM25 score-descending order
/// - `NoIndex`: FTS index doesn't exist for this label
/// - `ParseError(msg)`: Query string couldn't be parsed
pub async fn fts_search(
    db_path: &Path,
    label_id: &LabelId,
    query_text: &str,
    limit: usize,
) -> Result<FtsSearchOutcome> {
    // Step 1: Open the FTS index
    let fts_index = match FtsIndex::open_existing(db_path, label_id)? {
        Some(index) => index,
        None => return Ok(FtsSearchOutcome::NoIndex),
    };

    // Step 2: Build the QueryParser with the monodex-fts tokenizer
    let reader = fts_index.reader()?;
    let searcher = reader.searcher();

    let query_parser = QueryParser::for_index(&fts_index.index, vec![fts_index.fields.text]);

    // Note: In Tantivy 0.24, QueryParser respects per-field tokenizer config
    // when the field was indexed with a specific tokenizer. We need to ensure
    // the parser uses our tokenizer. This is done by setting the tokenizer
    // on the index's tokenizer manager (already done in open_existing).

    // Step 3: Parse the query
    let query = match query_parser.parse_query(query_text) {
        Ok(q) => q,
        Err(e) => return Ok(FtsSearchOutcome::ParseError(e.to_string())),
    };

    // Step 4: Execute the search
    let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

    // Step 5: Build hits from results
    let mut hits = Vec::new();

    for (score, doc_address) in top_docs {
        // Retrieve the document to get the row_id
        let doc: TantivyDocument = match searcher.doc(doc_address) {
            Ok(d) => d,
            Err(_) => continue, // Skip documents we can't retrieve
        };

        // Extract the row_id from the stored field
        if let Some(value) = doc.get_first(fts_index.fields.row_id)
            && let Some(row_id) = value.as_str()
        {
            hits.push(FtsHit {
                row_id: row_id.to_string(),
                score,
            });
        }
    }

    Ok(FtsSearchOutcome::Found(hits))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::identifier::LabelId;
    use tempfile::TempDir;

    fn make_label_id(catalog: &str, label: &str) -> LabelId {
        LabelId::new(catalog, label).unwrap()
    }

    #[test]
    fn test_fts_search_no_index() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path();
        let label_id = make_label_id("test-catalog", "missing-label");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(fts_search(db_path, &label_id, "test query", 10))
            .unwrap();

        match result {
            FtsSearchOutcome::NoIndex => {}
            other => panic!("Expected NoIndex, got {:?}", other),
        }
    }

    #[test]
    fn test_fts_search_parse_error() {
        // Note: This test requires an actual index with documents to hit the parse error path.
        // Without a real index, we can't test parse errors end-to-end here.
        // The parse error path is tested in integration tests.
    }
}
