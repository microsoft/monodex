//! In-flight crawl warning types.
//!
//! Purpose: Define warning events that can occur during a crawl and a sink abstraction for emitting them.
//! Edit here when: Adding new warning types, changing warning categorization.
//! Do not edit here for: Rendering warnings to stdout/stderr (see `app/crawl/warning.rs`), persisting warnings to disk (see `app/crawl/phases.rs`).

/// In-flight crawl warning. Surfaces immediately; not retained.
///
/// Distinct from `warnings-<catalog>.json` path-keyed bookkeeping
/// (chunker fallback-split tracking in chunk_new_files), which is a
/// separate flow and does not migrate.
#[derive(Debug, Clone)]
pub enum CrawlWarning {
    /// Chunker fell back to line-based splitting on a file.
    ChunkerFallbackSplit { relative_path: String },
    /// Reading a file's bytes failed.
    FileReadFailed {
        relative_path: String,
        error: String,
    },
    /// Chunking a file failed (post-read).
    ChunkingFailed {
        relative_path: String,
        error: String,
    },
    /// Sentinel-row read failed; file falls through to slow path.
    SentinelReadFailed {
        relative_path: String,
        error: String,
    },
}

/// Sink for in-flight crawl warnings.
///
/// This is a closure trait object that accepts warnings and renders them
/// immediately to the appropriate output stream. The closure is constructed
/// by the crawl orchestrator and passed to phase functions.
pub type WarningSink<'a> = &'a mut dyn FnMut(CrawlWarning);

// =============================================================================
// Decision warnings (search-time)
// =============================================================================

use crate::engine::retrieval::RetrievalMethod;

/// Warning emitted during search decision-rule evaluation.
///
/// These are structured warnings returned by the `decide` function.
/// The orchestrator translates them to `SearchWarning` instances with
/// pre-formatted source pointers for rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionWarning {
    /// A method in the persistent selection has not completed indexing.
    IncompleteMethod { method: RetrievalMethod },
}
