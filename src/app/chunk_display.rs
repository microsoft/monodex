//! Purpose: Chunk-display rendering: format a chunk record as a multi-line display block for search results and `view` output.
//! Edit here when: Adding or modifying chunk report formatting for search/view output.
//! Do not edit here for: Terminal sanitization (see `app/terminal_output.rs`), search orchestration (see `app/search.rs`).

use crate::app::terminal_output::sanitize_for_terminal;

/// Format a chunk's breadcrumb, split-part metadata, and chunk-kind decoration.
///
/// Used by search and view commands to produce consistent chunk report headers.
///
/// - `breadcrumb: None` renders as `"unknown"`
/// - The breadcrumb is sanitized internally for terminal safety
/// - `split_part: Some((ordinal, count))` appends ` (part {ordinal}/{count})`
/// - `chunk_kind != "content"` appends ` [{chunk_kind}]`
pub fn format_chunk_report(
    breadcrumb: Option<&str>,
    split_part: Option<(i32, i32)>,
    chunk_kind: &str,
) -> String {
    let breadcrumb = sanitize_for_terminal(breadcrumb.unwrap_or("unknown"));

    let mut report = breadcrumb;
    if let Some((ordinal, count)) = split_part {
        report = format!("{} (part {}/{})", report, ordinal, count);
    }
    if chunk_kind != "content" {
        report = format!("{} [{}]", report, chunk_kind);
    }

    report
}
