//! Search output rendering infrastructure.
//!
//! Purpose: Unified rendering for search results, warnings, and metadata.
//! Edit here when: Changing search output format, adding warning types, modifying preamble, source-pointer formatting for warning remediation strings.
//! Do not edit here for: Retrieval dispatch (see commands/search.rs), fusion algorithm (see engine/fusion.rs), chunk display (see app/chunk_display.rs).
//!
//! ## Output ordering rule
//!
//! The renderer emits output in a fixed order:
//! 1. Preamble (Catalog/Label/Searching line)
//! 2. Decision-time warnings (incomplete-method warnings), in RetrievalMethod enum order
//! 3. Search-time pre-result warnings (FTS NoIndex degradation under hybrid)
//! 4. Result block (each result preceded by its leading_inline_warnings)
//! 5. Trailing inline warnings (stale-hydration warnings after last result or when zero results)
//! 6. End marker (No results or End of results)

use std::io::{self, Write};

use crate::engine::{
    retrieval::RetrievalMethod,
    storage::ChunkRow,
    warning::DecisionWarning,
    {fusion::FusedHit, storage::LabelMetadataRow},
};

use std::collections::HashMap;

use super::chunk_display::format_chunk_report;

// =============================================================================
// Source pointer formatting (moved from util.rs)
// =============================================================================

/// Format source pointer for remediation messages.
///
/// Produces a `--commit <oid>` or `--working-dir` argument string suitable for
/// suggested crawl commands in error/warning messages.
pub fn format_source_pointer(row: &crate::engine::storage::LabelMetadataRow) -> String {
    use crate::engine::storage::{SOURCE_KIND_GIT_COMMIT, SOURCE_KIND_WORKING_DIRECTORY};

    match row.source_kind.as_str() {
        SOURCE_KIND_GIT_COMMIT => row
            .vector_source
            .as_ref()
            .or(row.fts_source.as_ref())
            .map(|s| format!("--commit {}", s))
            .unwrap_or_else(|| "--commit <commit>".to_string()),
        SOURCE_KIND_WORKING_DIRECTORY => "--working-dir".to_string(),
        _ => "[source]".to_string(),
    }
}

// =============================================================================
// Warning types
// =============================================================================

/// Search-time warnings, emitted through the renderer.
///
/// These carry pre-formatted strings (source_pointer already resolved)
/// so the renderer only does template substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchWarning {
    /// A method in the persistent selection has not completed indexing.
    IncompleteMethod {
        method: RetrievalMethod,
        label: String,
        source_pointer: String,
    },
    /// FTS state is missing on disk, no fallback available (FTS-only path).
    FtsNoIndexNoFallback {
        label: String,
        source_pointer: String,
    },
    /// FTS state is missing on disk, falling back to vector-only (hybrid path).
    FtsNoIndexDegrade {
        label: String,
        source_pointer: String,
    },
    /// FTS index is stale (manifest mismatch) after upgrade.
    FtsStale {
        catalog: String,
        label: String,
        source_pointer: String,
    },
    /// FTS index manifest is unreadable (corrupted).
    FtsManifestUnreadable { catalog: String, label: String },
    /// A chunk in the FTS index was not found in LanceDB (stale state).
    StaleHydration { row_id: String },
}

// =============================================================================
// Render model types
// =============================================================================

/// End-of-results marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndMarker {
    /// Print "End of results" (non-empty result set, shorter than limit, genuinely exhausted)
    Sentinel,
    /// Print "No results." (zero results)
    NoResults,
    /// No extra output (limit satisfied or candidate window saturated)
    None,
}

/// Search mode, used to determine debug output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Single-method search (FTS-only or vector-only)
    SingleMethod,
    /// Hybrid search (RRF fusion of multiple methods)
    Hybrid,
}

/// Preamble for search output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preamble {
    pub catalog: String,
    pub label: String,
    /// "fts only" | "vector only" | "fts, vector"
    pub searching: String,
}

/// A single rendered result with its associated warnings.
#[derive(Debug, Clone)]
pub struct RenderedResult {
    /// The fused hit (row_id, RRF score, contributors)
    pub fused_hit: FusedHit,
    /// The chunk data hydrated from LanceDB
    pub chunk: ChunkRow,
    /// Stale-hydration warnings for row_ids skipped between previous result and this one
    pub leading_inline_warnings: Vec<SearchWarning>,
}

/// The complete model for rendering search output.
///
/// The orchestrator builds this model, then passes it to `render()`.
/// The renderer is the only code that touches the output writer.
#[derive(Debug, Clone)]
pub struct SearchRenderModel {
    pub preamble: Preamble,
    /// Decision-time warnings + search-time pre-result warnings, in emission order
    pub pre_result_warnings: Vec<SearchWarning>,
    /// Results in fused-score-descending order
    pub results: Vec<RenderedResult>,
    /// Stale-hydration warnings collected after the last emitted result
    pub trailing_inline_warnings: Vec<SearchWarning>,
    /// Whether to show debug continuation lines
    pub debug: bool,
    /// End-of-results marker
    pub end_marker: EndMarker,
    /// Search mode (single-method vs hybrid)
    pub mode: SearchMode,
}

// =============================================================================
// Rendering functions
// =============================================================================

/// Render search output to a writer.
///
/// This is the single entry point for all search output. The orchestrator
/// builds a `SearchRenderModel` and passes it here; the renderer walks
/// the model in fixed order and emits to the writer.
pub fn render<W: Write>(writer: &mut W, model: &SearchRenderModel) -> io::Result<()> {
    // 1. Preamble
    writeln!(
        writer,
        "Catalog: {} / Label: {} / Searching: {}",
        model.preamble.catalog, model.preamble.label, model.preamble.searching
    )?;
    writeln!(writer)?;

    // 2. Pre-result warnings (decision-time + search-time pre-result)
    for warning in &model.pre_result_warnings {
        render_warning(writer, warning)?;
    }

    // 3. Result block
    for result in &model.results {
        // Emit leading inline warnings (stale-hydration) before this result
        for warning in &result.leading_inline_warnings {
            render_warning(writer, warning)?;
        }

        // Render result header line
        render_result_header(writer, result, model.debug)?;

        // Render debug continuation if enabled
        if model.debug {
            render_debug_continuation(writer, &result.fused_hit, model.mode)?;
        }

        // Render preview lines (first 3 lines of chunk text)
        render_preview_lines(writer, &result.chunk.text)?;

        // Blank line between results
        writeln!(writer)?;
    }

    // 4. Trailing inline warnings (stale-hydration after last result)
    for warning in &model.trailing_inline_warnings {
        render_warning(writer, warning)?;
    }

    // 5. End marker
    match model.end_marker {
        EndMarker::Sentinel => {
            writeln!(writer, "End of results")?;
            writeln!(writer)?;
        }
        EndMarker::NoResults => {
            writeln!(writer, "No results.")?;
            writeln!(writer)?;
        }
        EndMarker::None => {}
    }

    Ok(())
}

/// Render a single result header line.
///
/// Format: `{file_id}:{ord} [{marker}] {breadcrumb_report}`
/// Single-space delimiters, no score/distance columns.
fn render_result_header<W: Write>(
    writer: &mut W,
    result: &RenderedResult,
    _debug: bool,
) -> io::Result<()> {
    let chunk = &result.chunk;
    let hit = &result.fused_hit;

    // Build provenance marker
    let marker = build_provenance_marker(&hit.contributors);

    // Build breadcrumb report
    let breadcrumb_report = format_chunk_report(
        chunk.breadcrumb.as_deref(),
        chunk.split_part_ordinal.zip(chunk.split_part_count),
        &chunk.chunk_kind,
    );

    writeln!(
        writer,
        "{}:{} [{}] {}",
        chunk.file_id, chunk.chunk_ordinal, marker, breadcrumb_report
    )
}

/// Build provenance marker from contributors.
///
/// Returns "f", "v", or "f+v" (alphabetical order).
fn build_provenance_marker(contributors: &[crate::engine::fusion::RankedContribution]) -> String {
    let has_fts = contributors
        .iter()
        .any(|c| c.method == RetrievalMethod::Fts);
    let has_vector = contributors
        .iter()
        .any(|c| c.method == RetrievalMethod::Vector);

    match (has_fts, has_vector) {
        (true, true) => "f+v".to_string(),
        (true, false) => "f".to_string(),
        (false, true) => "v".to_string(),
        (false, false) => "unknown".to_string(), // Should never happen
    }
}

/// Render debug continuation line.
///
/// Format: `Debug: rrf={:.4}, fts_bm25={:.3}, vector_distance={:.3}`
/// Only emits keys whose contributors are present. RRF is emitted only for hybrid mode.
fn render_debug_continuation<W: Write>(
    writer: &mut W,
    hit: &FusedHit,
    mode: SearchMode,
) -> io::Result<()> {
    let mut parts = Vec::new();

    // RRF score (only for hybrid mode)
    if mode == SearchMode::Hybrid {
        parts.push(format!("rrf={:.4}", hit.rrf_score));
    }

    // Per-method scores (in enum order: Fts before Vector)
    for method in [RetrievalMethod::Fts, RetrievalMethod::Vector] {
        if let Some(contrib) = hit.contributors.iter().find(|c| c.method == method) {
            let label = match method {
                RetrievalMethod::Fts => "fts_bm25",
                RetrievalMethod::Vector => "vector_distance",
            };
            if let Some(score) = contrib.backend_score {
                parts.push(format!("{}={:.3}", label, score));
            }
        }
    }

    if !parts.is_empty() {
        writeln!(writer, "Debug: {}", parts.join(", "))?;
    }

    Ok(())
}

/// Render preview lines from chunk text.
///
/// Emits up to 3 lines, each prefixed with "> ".
fn render_preview_lines<W: Write>(writer: &mut W, text: &str) -> io::Result<()> {
    for line in text.lines().take(3) {
        writeln!(writer, "> {}", line)?;
    }
    Ok(())
}

/// Render a warning to the writer.
fn render_warning<W: Write>(writer: &mut W, warning: &SearchWarning) -> io::Result<()> {
    match warning {
        SearchWarning::IncompleteMethod {
            method,
            label,
            source_pointer,
        } => {
            writeln!(
                writer,
                "⚠️  {} state for label {} is incomplete; results may be missing entries indexed since the last successful crawl.",
                method, label
            )?;
            writeln!(
                writer,
                "   To complete: monodex crawl --label {} {} --retrieval {}",
                label, source_pointer, method
            )?;
        }
        SearchWarning::FtsNoIndexNoFallback {
            label,
            source_pointer,
        } => {
            writeln!(
                writer,
                "⚠️  FTS state for label {} is missing on disk; re-crawl to rebuild.",
                label
            )?;
            writeln!(
                writer,
                "   To rebuild: monodex crawl --label {} {} --retrieval fts",
                label, source_pointer
            )?;
        }
        SearchWarning::FtsNoIndexDegrade {
            label,
            source_pointer,
        } => {
            writeln!(
                writer,
                "⚠️  FTS state for label {} is missing on disk; falling back to vector-only.",
                label
            )?;
            writeln!(
                writer,
                "   To rebuild: monodex crawl --label {} {} --retrieval fts",
                label, source_pointer
            )?;
        }
        SearchWarning::StaleHydration { row_id } => {
            writeln!(
                writer,
                "⚠️  Chunk {} in FTS index but not in LanceDB (stale state), skipping",
                row_id
            )?;
        }
        SearchWarning::FtsStale {
            catalog,
            label,
            source_pointer,
        } => {
            writeln!(
                writer,
                "⚠️  FTS index for {}:{} was built against an older Monodex version and cannot be queried safely.",
                catalog, label
            )?;
            writeln!(
                writer,
                "   Re-crawl with: monodex crawl --catalog {} --label {} {}",
                catalog, label, source_pointer
            )?;
        }
        SearchWarning::FtsManifestUnreadable { catalog, label } => {
            writeln!(
                writer,
                "⚠️  FTS index for {}:{} is in an inconsistent state (manifest unreadable).",
                catalog, label
            )?;
            writeln!(
                writer,
                "   Re-crawling may resolve this; if it does not, run `monodex init-db --delete-everything` and re-crawl."
            )?;
        }
    }
    Ok(())
}

// =============================================================================
// Helper functions
// =============================================================================

/// Compute the end marker based on result count and saturation.
///
/// Rule:
/// - Sentinel iff rendered_count > 0 && rendered_count < limit && all backends unsaturated
/// - NoResults iff rendered_count == 0
/// - None otherwise
pub fn decide_end_marker(rendered_count: usize, limit: usize, saturations: &[bool]) -> EndMarker {
    if rendered_count == 0 {
        EndMarker::NoResults
    } else if rendered_count < limit && saturations.iter().all(|&s| !s) {
        EndMarker::Sentinel
    } else {
        EndMarker::None
    }
}

/// Translate decision warnings to search warnings.
///
/// Calls `format_source_pointer` against metadata to produce the source_pointer field.
pub fn translate_decision_warnings(
    warnings: Vec<DecisionWarning>,
    metadata: &LabelMetadataRow,
) -> Vec<SearchWarning> {
    let source_pointer = format_source_pointer(metadata);

    warnings
        .into_iter()
        .map(|w| match w {
            DecisionWarning::IncompleteMethod { method } => SearchWarning::IncompleteMethod {
                method,
                label: metadata.label.clone(),
                source_pointer: source_pointer.clone(),
            },
        })
        .collect()
}

// =============================================================================
// Hydration helper
// =============================================================================

/// Hydrate ranked hits with chunk data, handling stale rows correctly.
///
/// Walks `fused_hits` in order, accumulating stale-row warnings in a pending buffer.
/// When a valid hit is found, the pending warnings are attached to that result's
/// `leading_inline_warnings`. This ensures warnings appear immediately before the
/// result that "displaced" the stale row.
///
/// Returns `(results, trailing_inline_warnings)`:
/// - `results`: up to `limit` successfully hydrated results
/// - `trailing_inline_warnings`: warnings for stale rows after the last valid result,
///   or when no valid results were found at all
pub fn hydrate_ranked_hits(
    fused_hits: Vec<FusedHit>,
    chunks_by_row_id: &HashMap<String, ChunkRow>,
    limit: usize,
) -> (Vec<RenderedResult>, Vec<SearchWarning>) {
    let mut results = Vec::with_capacity(limit.min(fused_hits.len()));
    let mut pending_warnings: Vec<SearchWarning> = Vec::new();

    for fused_hit in fused_hits.into_iter() {
        if results.len() >= limit {
            // We've reached the limit; stop iterating.
            // Any remaining stale rows are not our concern (they're beyond the limit).
            break;
        }

        match chunks_by_row_id.get(&fused_hit.row_id) {
            Some(chunk) => {
                // Valid hit: attach pending warnings to this result
                results.push(RenderedResult {
                    fused_hit,
                    chunk: chunk.clone(),
                    leading_inline_warnings: std::mem::take(&mut pending_warnings),
                });
            }
            None => {
                // Stale row: accumulate warning for the next valid result
                pending_warnings.push(SearchWarning::StaleHydration {
                    row_id: fused_hit.row_id,
                });
            }
        }
    }

    // Any remaining pending warnings are trailing (after the last valid result,
    // or when no valid results were found at all)
    (results, pending_warnings)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests;
