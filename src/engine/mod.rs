//! Purpose: Reusable LanceDB indexing engine — chunking, embedding, storage, identifiers, breadcrumbs, and Git-aware enumeration.
//! Edit here when: Adding a new top-level engine submodule or convenience re-export.
//! Do not edit here for: App-level concerns such as CLI, config, commands (see `app/`); details inside individual engine submodules.

pub mod breadcrumb;
pub mod chunker;
pub mod crawl_config;
pub mod fts;
pub mod git_ops;
pub mod identifier;
pub mod markdown_partitioner;
pub mod package_lookup;
pub mod parallel_embedder;
pub mod partitioner;
pub mod schema;
pub mod storage;
pub mod system_info;
pub mod util;
pub mod warning;

// Re-export commonly used types for convenience
pub use chunker::Chunk;
pub use crawl_config::ChunkingStrategy;
pub use fts::{FtsHit, FtsIndex, FtsManifest, FtsSearchOutcome, fts_search, index_chunks_for_fts};
pub use parallel_embedder::ParallelConfig;
pub use parallel_embedder::ParallelEmbedder;
pub use partitioner::{SMALL_CHUNK_CHARS, TARGET_CHARS};
