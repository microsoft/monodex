//! Crawl-related types shared across command handlers.
//!
//! Purpose: Define crawl source types and failure tracking.
//! Edit here when: Adding new crawl source types or changing failure tracking fields.
//! Do not edit here for: Pipeline logic (see pipeline.rs), CLI handlers (see commands/crawl.rs).

use std::collections::HashSet;

use crate::engine::git_ops::FileEntry;

/// Metadata about the crawl source that travels alongside a `BlobSource`.
///
/// This struct carries source identity (source_kind, commit_oid) as pure data.
/// It does NOT include behavior - that's what the `BlobSource` trait is for.
/// The separation ensures the merged crawl flow doesn't branch on source identity.
///
/// Use `SOURCE_KIND_GIT_COMMIT` and `SOURCE_KIND_WORKING_DIRECTORY` from
/// `crate::engine::storage` for the `source_kind` field.
pub struct CrawlSourceMetadata {
    /// The source kind string: `"git-commit"` or `"working-directory"`.
    pub source_kind: &'static str,
    /// The resolved commit SHA for commit mode, or empty string for working-directory mode.
    pub commit_oid: String,
}

/// Failure tracking for crawl pipeline.
///
/// With LanceDB storage, structural errors (disk full, dataset corruption) cause
/// immediate abort. Only embedding failures are tracked per-chunk, as these can
/// fail for tokenizer edge cases or model issues on specific content.
#[derive(Debug, Default)]
pub struct CrawlFailures {
    /// Chunks that failed to embed (per-chunk tokenizer/model issues)
    pub embedding_failures: Vec<String>,
}

impl CrawlFailures {
    pub fn total(&self) -> usize {
        self.embedding_failures.len()
    }

    pub fn has_failures(&self) -> bool {
        self.total() > 0
    }
}

// ============================================================================
// Phase output types
// ============================================================================

/// Output from classifying files against existing chunks.
pub struct ClassifyOutput {
    /// Files that need to be chunked and indexed.
    pub new_files: Vec<FileEntry>,
    /// File IDs for files that already have chunks and need label added.
    pub existing_file_ids: HashSet<String>,
    /// Count of new files (for display).
    pub new_count: usize,
    /// Count of existing files (for display).
    pub existing_count: usize,
}

/// Output from adding labels to existing files.
pub struct LabelAddOutput {
    /// File IDs that were successfully updated.
    pub success_file_ids: HashSet<String>,
    /// Error messages for files that failed.
    pub failures: Vec<String>,
}

/// Output from the chunking phase.
pub struct ChunkingOutput {
    /// All chunks produced.
    pub chunks: Vec<crate::engine::Chunk>,
    /// File IDs that were touched during chunking.
    pub touched_file_ids: HashSet<String>,
    /// Files that had chunking warnings.
    pub warning_files: HashSet<String>,
    /// Total warning count.
    pub warning_count: usize,
}
