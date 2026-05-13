//! Search output rendering infrastructure.
//!
//! Purpose: Unified rendering for search results, warnings, and metadata.
//! Edit here when: Changing search output format, adding warning types, modifying preamble.
//! Do not edit here for: Retrieval dispatch (see commands/search.rs), fusion algorithm (see engine/fusion.rs).
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

use super::util::format_source_pointer;

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
    let breadcrumb_report = super::util::format_chunk_report(
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
mod tests {
    use super::*;
    use crate::engine::fusion::{FusedHit, RankedContribution};

    fn make_fused_hit(row_id: &str, rrf_score: f32, methods: &[RetrievalMethod]) -> FusedHit {
        let contributors: Vec<RankedContribution> = methods
            .iter()
            .enumerate()
            .map(|(i, &method)| RankedContribution {
                method,
                rank: i + 1,
                backend_score: Some(0.5),
            })
            .collect();
        FusedHit {
            row_id: row_id.to_string(),
            rrf_score,
            contributors,
        }
    }

    fn make_chunk(row_id: &str, file_id: &str) -> ChunkRow {
        ChunkRow {
            row_id: row_id.to_string(),
            text: "line1\nline2\nline3\nline4".to_string(),
            catalog: "test-catalog".to_string(),
            active_label_ids: vec!["test-catalog:main".to_string()],
            embedder_id: "test".to_string(),
            chunker_id: "test".to_string(),
            blob_id: "test".to_string(),
            content_hash: "test".to_string(),
            file_id: file_id.to_string(),
            relative_path: "test.ts".to_string(),
            package_name: "test".to_string(),
            source_uri: "test.ts".to_string(),
            chunk_ordinal: 1,
            chunk_count: 1,
            start_line: 1,
            end_line: 10,
            symbol_name: None,
            chunk_type: "function".to_string(),
            chunk_kind: "content".to_string(),
            breadcrumb: Some("test:func".to_string()),
            split_part_ordinal: None,
            split_part_count: None,
            file_complete: true,
        }
    }

    #[test]
    fn test_build_provenance_marker() {
        let hit_fts = make_fused_hit("a:1", 0.5, &[RetrievalMethod::Fts]);
        assert_eq!(build_provenance_marker(&hit_fts.contributors), "f");

        let hit_vector = make_fused_hit("a:1", 0.5, &[RetrievalMethod::Vector]);
        assert_eq!(build_provenance_marker(&hit_vector.contributors), "v");

        let hit_hybrid =
            make_fused_hit("a:1", 0.5, &[RetrievalMethod::Fts, RetrievalMethod::Vector]);
        assert_eq!(build_provenance_marker(&hit_hybrid.contributors), "f+v");
    }

    #[test]
    fn test_decide_end_marker() {
        // Zero results -> NoResults
        assert_eq!(decide_end_marker(0, 10, &[]), EndMarker::NoResults);
        assert_eq!(decide_end_marker(0, 10, &[false]), EndMarker::NoResults);

        // Results = limit -> None
        assert_eq!(decide_end_marker(10, 10, &[false]), EndMarker::None);
        assert_eq!(decide_end_marker(10, 10, &[true]), EndMarker::None);

        // Results < limit, no saturation -> Sentinel
        assert_eq!(decide_end_marker(5, 10, &[false]), EndMarker::Sentinel);
        assert_eq!(
            decide_end_marker(5, 10, &[false, false]),
            EndMarker::Sentinel
        );

        // Results < limit, any saturation -> None
        assert_eq!(decide_end_marker(5, 10, &[true]), EndMarker::None);
        assert_eq!(decide_end_marker(5, 10, &[false, true]), EndMarker::None);
    }

    #[test]
    fn test_render_single_method_fts() {
        let hit = make_fused_hit("abc123:1", 0.5, &[RetrievalMethod::Fts]);
        let chunk = make_chunk("abc123:1", "abc123");

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts only".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![RenderedResult {
                fused_hit: hit,
                chunk,
                leading_inline_warnings: vec![],
            }],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::None,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("Catalog: my-catalog / Label: main / Searching: fts only"));
        assert!(output.contains("abc123:1 [f] test:func"));
        assert!(output.contains("> line1"));
    }

    #[test]
    fn test_render_single_method_vector() {
        let hit = make_fused_hit("abc123:1", 0.5, &[RetrievalMethod::Vector]);
        let chunk = make_chunk("abc123:1", "abc123");

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "vector only".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![RenderedResult {
                fused_hit: hit,
                chunk,
                leading_inline_warnings: vec![],
            }],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::None,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("abc123:1 [v] test:func"));
    }

    #[test]
    fn test_render_hybrid_marker() {
        let hit = make_fused_hit(
            "abc123:1",
            0.0323,
            &[RetrievalMethod::Fts, RetrievalMethod::Vector],
        );
        let chunk = make_chunk("abc123:1", "abc123");

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts, vector".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![RenderedResult {
                fused_hit: hit,
                chunk,
                leading_inline_warnings: vec![],
            }],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::None,
            mode: SearchMode::Hybrid,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("abc123:1 [f+v] test:func"));
    }

    #[test]
    fn test_render_debug_hybrid() {
        let mut hit = make_fused_hit(
            "abc123:1",
            0.0323,
            &[RetrievalMethod::Fts, RetrievalMethod::Vector],
        );
        // Set specific scores for debug output
        hit.contributors[0].backend_score = Some(1.754); // FTS BM25
        hit.contributors[1].backend_score = Some(0.234); // Vector distance
        let chunk = make_chunk("abc123:1", "abc123");

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts, vector".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![RenderedResult {
                fused_hit: hit,
                chunk,
                leading_inline_warnings: vec![],
            }],
            trailing_inline_warnings: vec![],
            debug: true,
            end_marker: EndMarker::None,
            mode: SearchMode::Hybrid,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        // Check debug line format and precision
        assert!(output.contains("Debug: rrf=0.0323, fts_bm25=1.754, vector_distance=0.234"));
    }

    #[test]
    fn test_render_debug_single_method_fts() {
        let mut hit = make_fused_hit("abc123:1", 0.5, &[RetrievalMethod::Fts]);
        hit.contributors[0].backend_score = Some(1.754);
        let chunk = make_chunk("abc123:1", "abc123");

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts only".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![RenderedResult {
                fused_hit: hit,
                chunk,
                leading_inline_warnings: vec![],
            }],
            trailing_inline_warnings: vec![],
            debug: true,
            end_marker: EndMarker::None,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        // Single-method should NOT have rrf=, only the method-local score
        assert!(output.contains("Debug: fts_bm25=1.754"));
        assert!(!output.contains("rrf="));
    }

    #[test]
    fn test_render_debug_single_method_vector() {
        let mut hit = make_fused_hit("abc123:1", 0.5, &[RetrievalMethod::Vector]);
        hit.contributors[0].backend_score = Some(0.234);
        let chunk = make_chunk("abc123:1", "abc123");

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "vector only".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![RenderedResult {
                fused_hit: hit,
                chunk,
                leading_inline_warnings: vec![],
            }],
            trailing_inline_warnings: vec![],
            debug: true,
            end_marker: EndMarker::None,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("Debug: vector_distance=0.234"));
        assert!(!output.contains("rrf="));
    }

    #[test]
    fn test_render_debug_hybrid_fts_only_contributor() {
        // Hybrid mode with a [f]-only contributor should still show rrf=
        let mut hit = make_fused_hit("abc123:1", 0.0164, &[RetrievalMethod::Fts]);
        hit.contributors[0].backend_score = Some(1.718);
        let chunk = make_chunk("abc123:1", "abc123");

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts, vector".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![RenderedResult {
                fused_hit: hit,
                chunk,
                leading_inline_warnings: vec![],
            }],
            trailing_inline_warnings: vec![],
            debug: true,
            end_marker: EndMarker::None,
            mode: SearchMode::Hybrid,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        // Should show rrf= even with single contributor because mode is Hybrid
        assert!(output.contains("Debug: rrf=0.0164, fts_bm25=1.718"));
        assert!(!output.contains("vector_distance="));
    }

    #[test]
    fn test_render_debug_hybrid_vector_only_contributor() {
        // Hybrid mode with a [v]-only contributor should still show rrf=
        let mut hit = make_fused_hit("abc123:1", 0.0164, &[RetrievalMethod::Vector]);
        hit.contributors[0].backend_score = Some(0.234);
        let chunk = make_chunk("abc123:1", "abc123");

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts, vector".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![RenderedResult {
                fused_hit: hit,
                chunk,
                leading_inline_warnings: vec![],
            }],
            trailing_inline_warnings: vec![],
            debug: true,
            end_marker: EndMarker::None,
            mode: SearchMode::Hybrid,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        // Should show rrf= even with single contributor because mode is Hybrid
        assert!(output.contains("Debug: rrf=0.0164, vector_distance=0.234"));
        assert!(!output.contains("fts_bm25="));
    }

    #[test]
    fn test_render_end_marker_sentinel() {
        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts only".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::Sentinel,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("End of results"));
    }

    #[test]
    fn test_render_end_marker_no_results() {
        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts only".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::NoResults,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("No results."));
    }

    #[test]
    fn test_render_warning_incomplete_method() {
        let warning = SearchWarning::IncompleteMethod {
            method: RetrievalMethod::Fts,
            label: "main".to_string(),
            source_pointer: "--commit abc123".to_string(),
        };

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts only".to_string(),
            },
            pre_result_warnings: vec![warning],
            results: vec![],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::NoResults,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        // Assert the exact pinned template lines
        let lines: Vec<&str> = output.lines().collect();
        assert!(lines.contains(&"⚠️  fts state for label main is incomplete; results may be missing entries indexed since the last successful crawl."));
        assert!(lines.contains(
            &"   To complete: monodex crawl --label main --commit abc123 --retrieval fts"
        ));
    }

    #[test]
    fn test_render_warning_fts_no_index_no_fallback() {
        let warning = SearchWarning::FtsNoIndexNoFallback {
            label: "my-catalog:main".to_string(),
            source_pointer: "--commit abc123".to_string(),
        };

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts only".to_string(),
            },
            pre_result_warnings: vec![warning],
            results: vec![],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::NoResults,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains(
            "⚠️  FTS state for label my-catalog:main is missing on disk; re-crawl to rebuild"
        ));
    }

    #[test]
    fn test_render_warning_fts_no_index_degrade() {
        let warning = SearchWarning::FtsNoIndexDegrade {
            label: "my-catalog:main".to_string(),
            source_pointer: "--commit abc123".to_string(),
        };

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts, vector".to_string(),
            },
            pre_result_warnings: vec![warning],
            results: vec![],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::NoResults,
            mode: SearchMode::Hybrid,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("⚠️  FTS state for label my-catalog:main is missing on disk; falling back to vector-only"));
    }

    #[test]
    fn test_render_warning_stale_hydration() {
        let warning = SearchWarning::StaleHydration {
            row_id: "abc123:1".to_string(),
        };

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts only".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![],
            trailing_inline_warnings: vec![warning],
            debug: false,
            end_marker: EndMarker::NoResults,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains(
            "⚠️  Chunk abc123:1 in FTS index but not in LanceDB (stale state), skipping"
        ));
    }

    #[test]
    fn test_render_warning_ordering() {
        // Multiple warnings should emit in input order
        let warnings = vec![
            SearchWarning::IncompleteMethod {
                method: RetrievalMethod::Fts,
                label: "main".to_string(),
                source_pointer: "--commit abc".to_string(),
            },
            SearchWarning::FtsNoIndexDegrade {
                label: "test:main".to_string(),
                source_pointer: "--commit abc".to_string(),
            },
        ];

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts, vector".to_string(),
            },
            pre_result_warnings: warnings,
            results: vec![],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::NoResults,
            mode: SearchMode::Hybrid,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        // Incomplete method warning should come before NoIndex warning
        let incomplete_pos = output.find("incomplete").unwrap();
        let noindex_pos = output.find("missing on disk").unwrap();
        assert!(incomplete_pos < noindex_pos);
    }

    #[test]
    fn test_render_trailing_warnings_with_no_results() {
        // When all hydration fails, trailing warnings emit before No results
        let warning = SearchWarning::StaleHydration {
            row_id: "abc123:1".to_string(),
        };

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts only".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![],
            trailing_inline_warnings: vec![warning],
            debug: false,
            end_marker: EndMarker::NoResults,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        // Stale-hydration warning should come before "No results."
        let stale_pos = output.find("stale state").unwrap();
        let no_results_pos = output.find("No results.").unwrap();
        assert!(stale_pos < no_results_pos);
    }

    #[test]
    fn test_render_leading_inline_warnings() {
        let warning = SearchWarning::StaleHydration {
            row_id: "abc123:0".to_string(),
        };
        let hit = make_fused_hit("abc123:1", 0.5, &[RetrievalMethod::Fts]);
        let chunk = make_chunk("abc123:1", "abc123");

        let model = SearchRenderModel {
            preamble: Preamble {
                catalog: "my-catalog".to_string(),
                label: "main".to_string(),
                searching: "fts only".to_string(),
            },
            pre_result_warnings: vec![],
            results: vec![RenderedResult {
                fused_hit: hit,
                chunk,
                leading_inline_warnings: vec![warning],
            }],
            trailing_inline_warnings: vec![],
            debug: false,
            end_marker: EndMarker::None,
            mode: SearchMode::SingleMethod,
        };

        let mut output = Vec::new();
        render(&mut output, &model).unwrap();
        let output = String::from_utf8(output).unwrap();

        // Warning should come before the result line
        let warning_pos = output.find("stale state").unwrap();
        let result_pos = output.find("abc123:1 [f]").unwrap();
        assert!(warning_pos < result_pos);
    }

    #[test]
    fn test_translate_decision_warnings() {
        use crate::engine::storage::LabelMetadataRow;

        let metadata = LabelMetadataRow {
            label_id: "test-catalog:main".to_string(),
            catalog: "test-catalog".to_string(),
            label: "main".to_string(),
            source_kind: "git-commit".to_string(),
            vector_source: Some("abc123".to_string()),
            vector_complete: true,
            fts_source: Some("abc123".to_string()),
            fts_complete: true,
            updated_at_unix_secs: 0,
        };

        let warnings = vec![DecisionWarning::IncompleteMethod {
            method: RetrievalMethod::Fts,
        }];

        let search_warnings = translate_decision_warnings(warnings, &metadata);

        assert_eq!(search_warnings.len(), 1);
        match &search_warnings[0] {
            SearchWarning::IncompleteMethod {
                method,
                label,
                source_pointer,
            } => {
                assert_eq!(*method, RetrievalMethod::Fts);
                assert_eq!(label, "main");
                assert_eq!(source_pointer, "--commit abc123");
            }
            _ => panic!("Expected IncompleteMethod warning"),
        }
    }

    // =========================================================================
    // hydrate_ranked_hits tests
    // =========================================================================

    #[test]
    fn test_hydrate_no_stale_rows() {
        // All row_ids hydrate. Result count = input count (or limit, whichever is smaller).
        // Trailing warnings empty.
        let fused_hits = vec![
            make_fused_hit("row_a", 0.1, &[RetrievalMethod::Fts]),
            make_fused_hit("row_b", 0.09, &[RetrievalMethod::Vector]),
            make_fused_hit("row_c", 0.08, &[RetrievalMethod::Fts]),
        ];
        let chunks = vec![
            ("row_a".to_string(), make_chunk("row_a", "file1")),
            ("row_b".to_string(), make_chunk("row_b", "file2")),
            ("row_c".to_string(), make_chunk("row_c", "file3")),
        ]
        .into_iter()
        .collect();

        let (results, trailing) = hydrate_ranked_hits(fused_hits, &chunks, 10);

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].fused_hit.row_id, "row_a");
        assert_eq!(results[1].fused_hit.row_id, "row_b");
        assert_eq!(results[2].fused_hit.row_id, "row_c");
        assert!(results[0].leading_inline_warnings.is_empty());
        assert!(results[1].leading_inline_warnings.is_empty());
        assert!(results[2].leading_inline_warnings.is_empty());
        assert!(trailing.is_empty());
    }

    #[test]
    fn test_hydrate_stale_before_first_valid() {
        // Stale before first valid: fused order [stale_a, valid_b].
        // Assert results[0] is b and its leading_inline_warnings contains the warning for a.
        // Trailing empty.
        let fused_hits = vec![
            make_fused_hit("stale_a", 0.1, &[RetrievalMethod::Fts]),
            make_fused_hit("valid_b", 0.09, &[RetrievalMethod::Vector]),
        ];
        let chunks = vec![("valid_b".to_string(), make_chunk("valid_b", "file2"))]
            .into_iter()
            .collect();

        let (results, trailing) = hydrate_ranked_hits(fused_hits, &chunks, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fused_hit.row_id, "valid_b");
        assert_eq!(results[0].leading_inline_warnings.len(), 1);
        match &results[0].leading_inline_warnings[0] {
            SearchWarning::StaleHydration { row_id } => assert_eq!(row_id, "stale_a"),
            _ => panic!("Expected StaleHydration warning"),
        }
        assert!(trailing.is_empty());
    }

    #[test]
    fn test_hydrate_stale_between_two_valid() {
        // Stale between two valid rows: fused order [valid_b, stale_c, valid_d].
        // Assert results[0] is b with empty leading; results[1] is d with leading
        // containing the warning for c. Trailing empty.
        let fused_hits = vec![
            make_fused_hit("valid_b", 0.1, &[RetrievalMethod::Fts]),
            make_fused_hit("stale_c", 0.09, &[RetrievalMethod::Vector]),
            make_fused_hit("valid_d", 0.08, &[RetrievalMethod::Fts]),
        ];
        let chunks = vec![
            ("valid_b".to_string(), make_chunk("valid_b", "file1")),
            ("valid_d".to_string(), make_chunk("valid_d", "file3")),
        ]
        .into_iter()
        .collect();

        let (results, trailing) = hydrate_ranked_hits(fused_hits, &chunks, 10);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].fused_hit.row_id, "valid_b");
        assert!(results[0].leading_inline_warnings.is_empty());
        assert_eq!(results[1].fused_hit.row_id, "valid_d");
        assert_eq!(results[1].leading_inline_warnings.len(), 1);
        match &results[1].leading_inline_warnings[0] {
            SearchWarning::StaleHydration { row_id } => assert_eq!(row_id, "stale_c"),
            _ => panic!("Expected StaleHydration warning"),
        }
        assert!(trailing.is_empty());
    }

    #[test]
    fn test_hydrate_stale_after_last_valid() {
        // Stale after last valid: fused order [valid_b, stale_c].
        // Assert results[0] is b with empty leading; trailing contains the warning for c.
        let fused_hits = vec![
            make_fused_hit("valid_b", 0.1, &[RetrievalMethod::Fts]),
            make_fused_hit("stale_c", 0.09, &[RetrievalMethod::Vector]),
        ];
        let chunks = vec![("valid_b".to_string(), make_chunk("valid_b", "file1"))]
            .into_iter()
            .collect();

        let (results, trailing) = hydrate_ranked_hits(fused_hits, &chunks, 10);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fused_hit.row_id, "valid_b");
        assert!(results[0].leading_inline_warnings.is_empty());
        assert_eq!(trailing.len(), 1);
        match &trailing[0] {
            SearchWarning::StaleHydration { row_id } => assert_eq!(row_id, "stale_c"),
            _ => panic!("Expected StaleHydration warning"),
        }
    }

    #[test]
    fn test_hydrate_all_stale() {
        // All stale (no hydratable rows): fused order all stale.
        // Assert results empty; trailing contains a warning per stale row, in order.
        let fused_hits = vec![
            make_fused_hit("stale_a", 0.1, &[RetrievalMethod::Fts]),
            make_fused_hit("stale_b", 0.09, &[RetrievalMethod::Vector]),
        ];
        let chunks: HashMap<String, ChunkRow> = HashMap::new();

        let (results, trailing) = hydrate_ranked_hits(fused_hits, &chunks, 10);

        assert!(results.is_empty());
        assert_eq!(trailing.len(), 2);
        match &trailing[0] {
            SearchWarning::StaleHydration { row_id } => assert_eq!(row_id, "stale_a"),
            _ => panic!("Expected StaleHydration warning for stale_a"),
        }
        match &trailing[1] {
            SearchWarning::StaleHydration { row_id } => assert_eq!(row_id, "stale_b"),
            _ => panic!("Expected StaleHydration warning for stale_b"),
        }
    }

    #[test]
    fn test_hydrate_limit_truncation_with_stale_after_limit() {
        // Limit-truncation with stale after limit: fused order [valid_a, valid_b, valid_c, stale_d]
        // with limit = 2. Assert results.len() == 2; the warning for d does NOT appear
        // (the helper stopped before iterating it). Trailing empty.
        let fused_hits = vec![
            make_fused_hit("valid_a", 0.1, &[RetrievalMethod::Fts]),
            make_fused_hit("valid_b", 0.09, &[RetrievalMethod::Vector]),
            make_fused_hit("valid_c", 0.08, &[RetrievalMethod::Fts]),
            make_fused_hit("stale_d", 0.07, &[RetrievalMethod::Vector]),
        ];
        let chunks = vec![
            ("valid_a".to_string(), make_chunk("valid_a", "file1")),
            ("valid_b".to_string(), make_chunk("valid_b", "file2")),
            ("valid_c".to_string(), make_chunk("valid_c", "file3")),
        ]
        .into_iter()
        .collect();

        let (results, trailing) = hydrate_ranked_hits(fused_hits, &chunks, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].fused_hit.row_id, "valid_a");
        assert_eq!(results[1].fused_hit.row_id, "valid_b");
        assert!(trailing.is_empty());
    }

    #[test]
    fn test_render_fts_stale_warning_exact_copy() {
        let warning = SearchWarning::FtsStale {
            catalog: "my-catalog".to_string(),
            label: "main".to_string(),
            source_pointer: "--commit abc123".to_string(),
        };

        let mut output = Vec::new();
        render_warning(&mut output, &warning).unwrap();
        let output = String::from_utf8(output).unwrap();

        let expected = "⚠️  FTS index for my-catalog:main was built against an older Monodex version and cannot be queried safely.\n   Re-crawl with: monodex crawl --catalog my-catalog --label main --commit abc123\n";
        assert_eq!(output, expected);
    }

    #[test]
    fn test_render_fts_manifest_unreadable_warning_exact_copy() {
        let warning = SearchWarning::FtsManifestUnreadable {
            catalog: "my-catalog".to_string(),
            label: "main".to_string(),
        };

        let mut output = Vec::new();
        render_warning(&mut output, &warning).unwrap();
        let output = String::from_utf8(output).unwrap();

        let expected = "⚠️  FTS index for my-catalog:main is in an inconsistent state (manifest unreadable).\n   Re-crawling may resolve this; if it does not, run `monodex init-db --delete-everything` and re-crawl.\n";
        assert_eq!(output, expected);
    }
}
