//! Reusable indexing engine for LanceDB semantic search
//!
//! This module contains general-purpose indexing logic that works
//! for any Rush monorepo. It is designed to be reusable across projects.
//!
//! Repository-specific configuration lives in `src/app/config.rs`

pub mod breadcrumb;
pub mod chunker;
pub mod config;
pub mod crawl_config;
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

// Re-export commonly used types for convenience
pub use chunker::Chunk;
pub use parallel_embedder::ParallelConfig;
pub use parallel_embedder::ParallelEmbedder;
pub use partitioner::SMALL_CHUNK_CHARS;
