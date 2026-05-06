//! Crawl-related types shared across command handlers.
//!
//! Purpose: Define crawl source types and failure tracking.
//! Edit here when: Adding new crawl source types or changing failure tracking fields.
//! Do not edit here for: Pipeline logic (see pipeline.rs), CLI handlers (see commands/crawl.rs), phase output types (see phases.rs).

/// Metadata about the crawl source that travels alongside a `BlobSource`.
///
/// This struct carries source identity (source_kind, source_value) as pure data.
/// It does NOT include behavior - that's what the `BlobSource` trait is for.
/// The separation ensures the merged crawl flow doesn't branch on source identity.
///
/// Use `SOURCE_KIND_GIT_COMMIT` and `SOURCE_KIND_WORKING_DIRECTORY` from
/// `crate::engine::storage` for the `source_kind` field.
///
/// The `source_value` field carries either a real commit OID (commit mode)
/// or a working-directory sentinel string (working-dir mode).
pub struct CrawlSourceMetadata {
    /// The source kind string: `"git-commit"` or `"working-directory"`.
    pub source_kind: &'static str,
    /// The resolved commit SHA for commit mode, or a working-directory sentinel
    /// for working-directory mode. The sentinel is per-crawl-unique so any two
    /// working-dir crawls compare unequal.
    pub source_value: String,
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
