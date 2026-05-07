//! Diagnose FTS tokenization and ranking for a single chunk.
//!
//! Purpose: Handler for the `debug-fts` command — print tokens for a chunk and
//!          optionally explain query ranking.
//! Edit here when: Changing FTS diagnostic output or error messages.
//! Do not edit here for: FTS engine logic (see engine/fts/), tokenizer (see engine/fts/tokenizer.rs).

use anyhow::{Result, anyhow};

use crate::app::{
    ChunkSelector, Config, format_source_pointer, parse_chunk_selector, resolve_database_path,
    resolve_label_context,
};
use crate::engine::fts::index::FtsIndex;
use crate::engine::fts::tokenizer::tokenize_text;
use crate::engine::storage::Database;
use crate::engine::util::compute_row_id;

/// Maximum number of tokens to display in output.
const MAX_TOKENS_DISPLAY: usize = 100;

/// Run the `debug-fts` command.
///
/// This command:
/// - Parses the chunk identifier (file_id:ordinal form)
/// - Resolves label context
/// - Retrieves the chunk from LanceDB
/// - Tokenizes the chunk text and displays the tokens
/// - Optionally explains query ranking if --query is provided
pub fn run_debug_fts(
    config: &Config,
    id: &str,
    label: Option<&str>,
    catalog: Option<&str>,
    query: Option<&str>,
    _debug: bool,
) -> Result<()> {
    // Parse the chunk identifier - must be single-ordinal form
    let (file_id, ordinal) = parse_chunk_id(id)?;

    // Resolve label context
    let (label_id, catalog_name, label_name) = resolve_label_context(label, catalog)?;

    // Resolve database path
    let db_path = resolve_database_path(Some(config))?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Open database
        let db = Database::open(&db_path).await?;
        let chunk_storage = db.chunks_storage().await?;
        let label_storage = db.label_storage().await?;

        // Compute row_id from file_id and ordinal
        let row_id = compute_row_id(&file_id, ordinal);

        // Verify the chunk exists and is in the label
        let chunk = chunk_storage.get_by_row_id(&row_id).await?;
        let chunk = chunk.ok_or_else(|| anyhow!("Chunk {} not found.", row_id))?;

        // Check that the chunk is active for this label
        let label_id_str = label_id.as_str();
        if !chunk.active_label_ids.iter().any(|l| l == label_id_str) {
            return Err(anyhow!(
                "Chunk {} is not in label {}/{}.",
                row_id,
                catalog_name,
                label_name
            ));
        }

        // Build breadcrumb for display
        let breadcrumb = chunk.breadcrumb.as_deref();
        let chunk_ref = format!("{}:{}", file_id, ordinal);

        // Tokenize the chunk text
        let tokens = tokenize_text(&chunk.text);

        // Print header
        println!("Catalog: {}", catalog_name);
        println!("Label: {}", label_name);
        println!(
            "Chunk: {} ({})",
            chunk_ref,
            breadcrumb.unwrap_or("unknown")
        );
        println!();

        // Print tokens
        if tokens.is_empty() {
            println!("No tokens (chunk text produced zero tokens after tokenization).");
        } else {
            println!("Tokens ({}):", tokens.len());
            print_tokens_wrapped(&tokens, MAX_TOKENS_DISPLAY);
        }

        // If --query is provided, explain ranking
        if let Some(query_text) = query {
            println!();
            println!("Query: \"{}\"", query_text);

            // Query tokens
            let query_tokens = tokenize_text(query_text);
            if query_tokens.is_empty() {
                println!("Query tokens: (none)");
            } else {
                println!("Query tokens: {}", query_tokens.join(", "));
            }

            // Open FTS index and explain
            match FtsIndex::open_existing(&db_path, &label_id)? {
                Some(fts_index) => {
                    explain_query(&fts_index, &row_id, query_text, &chunk.text)?;
                }
                None => {
                    // Load label metadata to format source pointer, with fallback
                    let label_metadata = label_storage.get_by_label_id(label_id.as_str()).await?;
                    let source_pointer = label_metadata
                        .as_ref()
                        .map(format_source_pointer)
                        .unwrap_or_else(|| "--commit <commit>".to_string());
                    println!();
                    println!(
                        "No FTS index for label {}/{}. Run `monodex crawl --label {} {} --retrieval fts` to build it.",
                        catalog_name, label_name, label_name, source_pointer
                    );
                }
            }
        }

        Ok(())
    })
}

/// Default line width for token wrapping when console width cannot be detected.
const DEFAULT_LINE_WIDTH: usize = 80;

/// Print tokens space-delimited with word wrapping.
///
/// Tokens are printed on lines indented with two spaces, wrapping at the console
/// width (or 80 characters if detection fails). At most `max_tokens` are displayed;
/// if more tokens exist, a trailing message indicates the count.
fn print_tokens_wrapped(tokens: &[String], max_tokens: usize) {
    let display_count = tokens.len().min(max_tokens);
    let display_tokens = &tokens[..display_count];

    // Try to get terminal width, fall back to default
    let line_width = terminal_size::terminal_size()
        .map(|(width, _)| width.0 as usize)
        .unwrap_or(DEFAULT_LINE_WIDTH);

    // Account for the 2-space indent on each line
    let max_line_content = line_width.saturating_sub(2);

    let mut current_line = String::new();

    for (i, token) in display_tokens.iter().enumerate() {
        // Check if adding this token would exceed the line width
        let potential_line = if current_line.is_empty() {
            token.clone()
        } else {
            format!("{} {}", current_line, token)
        };

        if potential_line.len() > max_line_content && !current_line.is_empty() {
            // Print current line and start a new one
            println!("  {}", current_line);
            current_line = token.clone();
        } else {
            current_line = potential_line;
        }

        // Print the last line if we're at the end
        if i == display_count - 1 && !current_line.is_empty() {
            println!("  {}", current_line);
        }
    }

    // If we truncated tokens, indicate how many more
    if tokens.len() > max_tokens {
        println!("  ... and {} more.", tokens.len() - max_tokens);
    }
}

/// Parse a chunk identifier in file_id:ordinal form.
///
/// Uses the shared `parse_chunk_selector` and rejects any form that is not
/// a single ordinal (i.e., rejects ranges, :N-end, and bare file_id).
fn parse_chunk_id(s: &str) -> Result<(String, usize)> {
    let (file_id, selector) = parse_chunk_selector(s)?;

    match selector {
        ChunkSelector::Single(ordinal) => Ok((file_id, ordinal)),
        ChunkSelector::Range(..) => Err(anyhow!(
            "Ranges are not supported. Use <file_id>:<ordinal> form (e.g. 700a4ba232fe9ddc:3)."
        )),
        ChunkSelector::ToEnd(..) => Err(anyhow!(
            "Range-to-end is not supported. Use <file_id>:<ordinal> form (e.g. 700a4ba232fe9ddc:3)."
        )),
        ChunkSelector::All => Err(anyhow!(
            "Missing ordinal. Use <file_id>:<ordinal> form (e.g. 700a4ba232fe9ddc:3)."
        )),
    }
}

/// Explain query ranking for a chunk.
///
/// Uses Tantivy's explain() to show BM25 scoring details.
fn explain_query(
    fts_index: &FtsIndex,
    row_id: &str,
    query_text: &str,
    _chunk_text: &str,
) -> Result<()> {
    println!();

    // Get a reader and searcher
    let reader = fts_index.reader()?;
    let searcher = reader.searcher();

    // Build a query to find the document by row_id
    let row_id_field = fts_index.fields.row_id;
    let text_field = fts_index.fields.text;

    // Find the document address by searching for the row_id
    use tantivy::collector::DocSetCollector;
    use tantivy::query::TermQuery;

    let row_id_term = tantivy::Term::from_field_text(row_id_field, row_id);
    let row_id_query = TermQuery::new(row_id_term, tantivy::schema::IndexRecordOption::Basic);

    let doc_addresses = searcher.search(&row_id_query, &DocSetCollector)?;

    if doc_addresses.is_empty() {
        println!(
            "Chunk is in LanceDB but not in the FTS index (possibly skipped during indexing for tokenizer reasons, or stale FTS state)."
        );
        return Ok(());
    }

    let doc_address = doc_addresses.into_iter().next().unwrap();

    // Parse the user query with our tokenizer-aware parser
    use tantivy::query::QueryParser;

    let query_parser = QueryParser::for_index(&fts_index.index, vec![text_field]);
    let user_query = match query_parser.parse_query(query_text) {
        Ok(q) => q,
        Err(e) => {
            println!("Couldn't parse query: {}", e);
            return Ok(());
        }
    };

    // Get explanation using Query::explain (not Searcher::explain)
    let explanation = user_query.explain(&searcher, doc_address)?;

    // Render the explanation using Tantivy's pretty JSON format
    println!("Explanation:");
    println!("{}", explanation.to_pretty_json());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_chunk_id_valid() {
        let (file_id, ordinal) = parse_chunk_id("700a4ba232fe9ddc:3").unwrap();
        assert_eq!(file_id, "700a4ba232fe9ddc");
        assert_eq!(ordinal, 3);
    }

    #[test]
    fn test_parse_chunk_id_rejects_range() {
        let result = parse_chunk_id("700a4ba232fe9ddc:2-4");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Ranges are not supported")
        );
    }

    #[test]
    fn test_parse_chunk_id_rejects_bare_file() {
        let result = parse_chunk_id("700a4ba232fe9ddc");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing ordinal"));
    }

    #[test]
    fn test_parse_chunk_id_rejects_invalid_file_id() {
        let result = parse_chunk_id("short:1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid file ID"));
    }

    #[test]
    fn test_parse_chunk_id_rejects_zero_ordinal() {
        let result = parse_chunk_id("700a4ba232fe9ddc:0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("1-indexed"));
    }
}
