//! Purpose: Reusable LanceDB indexing engine — chunking, embedding, storage, identifiers, breadcrumbs, and Git-aware enumeration.
//! Edit here when: Adding a new top-level engine submodule or convenience re-export.
//! Do not edit here for: App-level concerns such as CLI, config, commands (see `app/`); details inside individual engine submodules.

mod breadcrumb;
mod chunker;
mod crawl_config;
pub mod fts;
mod fusion;
mod git_ops;
pub mod identifier;
pub mod identity;
mod markdown_partitioner;
mod parallel_embedder;
mod partitioner;
pub mod retrieval;
pub mod schema;
mod search_decision;
pub mod storage;
mod system_info;
mod warning;
mod working_dir_sentinel;

// Re-export commonly used types for convenience
pub use breadcrumb::encode_path_component;
pub use chunker::{Chunk, ChunkContext, chunk_content};
pub use crawl_config::{
    ChunkingStrategy, CompiledCrawlConfig, get_default_crawl_config, load_compiled_crawl_config,
};
pub use fts::{
    FtsHit, FtsIndex, FtsIndexingStats, FtsManifest, FtsSearchOutcome, fts_search,
    index_chunks_for_fts,
};
pub use fusion::{FusedHit, MethodHit, RankedContribution, fuse};
pub use git_ops::{
    BlobSource, CommitBlobSource, FileEntry, PackageIndex, WorkingDirBlobSource,
    extract_package_name_from_bytes, resolve_commit_oid,
};
pub use parallel_embedder::{ParallelConfig, ParallelEmbedder};
pub use partitioner::{
    ChunkQualityReport, PartitionConfig, PartitionDebug, SMALL_CHUNK_CHARS, TARGET_CHARS,
    partition_typescript,
};
pub use retrieval::RetrievalMethod;
pub use search_decision::{Decision, DecisionError, decide};
pub use system_info::{
    ResolvedEmbeddingConfig, compute_auto_embedding_config, estimate_ram_usage, format_bytes,
    get_physical_core_count,
};
pub use warning::{CrawlWarning, DecisionWarning, WarningSink};
pub use working_dir_sentinel::make_working_dir_source_sentinel;
