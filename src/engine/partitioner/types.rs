//! Purpose: Types and constants for partition-based code chunking — `PartitionedChunk`, `PartitionConfig`, `SplitResult`, `TARGET_CHARS`, `SMALL_CHUNK_CHARS`.
//! Edit here when: Adding or modifying chunk types, configuration, or result structures.
//! Do not edit here for: Debug logging (see `debug.rs`), scoring (see `scoring.rs`), split logic (see `split_search.rs`).

use std::fmt;

use super::debug::PartitionDebug;

/// Error returned when partitioning fails.
#[derive(Debug, Clone)]
pub struct PartitionError {
    /// Human-readable error message
    pub message: String,
}

impl fmt::Display for PartitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for PartitionError {}

/// Target chunk size in characters (same as runtime chunker's target_size)
pub const TARGET_CHARS: usize = 6000;

/// Threshold for "small" chunks in characters (roughly 20 lines × 50 chars)
pub const SMALL_CHUNK_CHARS: usize = 500;

/// Minimum chunk size as ratio of target (20%)
pub(super) const MIN_CHUNK_RATIO: f64 = 0.20;

/// Configuration for partition chunking
pub struct PartitionConfig {
    /// Target chunk size in characters (text only, breadcrumb is extra)
    pub target_size: usize,

    /// File name for breadcrumb prefix
    pub file_name: String,

    /// Package name for breadcrumb (e.g., "@rushstack/node-core-library")
    pub package_name: String,

    /// Debug logging for partitioning decisions
    pub debug: PartitionDebug,

    /// When false, disable fallback line-based splitting. Oversized chunks that
    /// cannot be split via AST will remain oversized. This is used by audit-chunks
    /// to measure AST-only chunking quality.
    pub allow_fallback: bool,
}

impl Default for PartitionConfig {
    fn default() -> Self {
        Self {
            target_size: 6000,
            file_name: "unknown.ts".to_string(),
            package_name: "unknown".to_string(),
            debug: PartitionDebug::default(),
            allow_fallback: true,
        }
    }
}

/// A chunk of code with breadcrumb context
#[derive(Debug, Clone)]
pub struct PartitionedChunk {
    /// Source URI (file path, issue reference, etc.)
    pub source_uri: String,

    /// Catalog name (for multi-source partitioning)
    pub catalog: String,

    /// Content hash (SHA256) for incremental sync
    pub content_hash: String,

    /// Breadcrumb path (e.g., "@rushstack/node-core-library:JsonFile.ts:JsonFile.load")
    pub breadcrumb: String,

    /// Source code text (including preceding comments)
    pub text: String,

    /// Starting line number (1-indexed)
    pub start_line: usize,

    /// Ending line number (inclusive)
    pub end_line: usize,

    /// Chunk type (function, class, method, etc.)
    pub chunk_type: String,

    /// Chunk kind (content, imports, changelog, config, fallback-split, degraded-ast-split)
    pub chunk_kind: String,

    /// Symbol name (if applicable)
    pub symbol_name: Option<String>,

    /// For split sections: which part this is (1-indexed)
    pub split_part_ordinal: Option<usize>,

    /// For split sections: total number of parts
    pub split_part_count: Option<usize>,
}

/// A line range representing a chunk-in-progress
#[derive(Debug, Clone)]
pub(super) struct ChunkRange {
    /// Starting line number (1-indexed, inclusive)
    pub start_line: usize,
    /// Ending line number (1-indexed, inclusive)
    pub end_line: usize,
    /// This chunk was created by a fallback split
    pub from_fallback: bool,
    /// This chunk was created by a degraded AST split
    pub from_degraded_ast_split: bool,
}

/// Result of attempting to split a chunk.
///
/// The algorithm distinguishes three outcomes:
/// 1. Good AST split (success) - semantically meaningful, respects min_chunk_size
/// 2. Degraded AST split (quality failure) - semantically meaningful but poor geometry
/// 3. Fallback split (algorithm failure) - no acceptable AST split found
///
/// Important: Fallback is NOT a heuristic choice. It is an explicit failure mode
/// indicating that the AST-based partitioner could not find any semantic structure
/// to use. It provides damage control for production but should trigger investigation.
pub(super) enum SplitResult {
    /// Successful AST split: semantically meaningful with good chunk geometry
    Split(usize),
    /// Degraded AST split: semantically meaningful but poor chunk geometry (tiny chunks)
    /// This is a quality failure, but still preferable to fallback in production.
    /// Marked in output with `:[degraded-ast-split]` breadcrumb suffix.
    DegradedSplit(usize),
    /// Fallback split: no acceptable AST split found, using line-based recovery.
    /// This is explicit failure of semantic partitioning, not a heuristic choice.
    /// Marked in output with `:[fallback-split]` breadcrumb suffix.
    Fallback(usize),
    /// Cannot split this chunk any further
    CannotSplit,
}
