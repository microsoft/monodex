//! Repository-specific configuration for the indexer
//!
//! This module provides backward-compatible functions that delegate to the
//! new `crawl_config` module. The actual configuration is now loaded from
//! `monodex-crawl.json` files.
//!
//! ## Config Discovery
//!
//! Configs are loaded in this precedence order:
//! 1. `<repo-root>/monodex-crawl.json` (repo-local)
//! 2. `~/.monodex/crawl.json` (user-global)
//! 3. Embedded default (compiled into binary)
//!
//! See `crawl_config.rs` for the full implementation.

use super::crawl_config::load_compiled_crawl_config;

/// Determines if a file should be skipped during indexing
///
/// This is a convenience function that uses the embedded default config.
/// For crawls that need repo-specific config, use `CompiledCrawlConfig::should_crawl()`.
///
/// # Arguments
///
/// * `path` - File path relative to repository root
///
/// # Returns
///
/// `true` if the file should be skipped, `false` if it should be indexed
#[allow(dead_code)]
pub fn should_skip_path(path: &str) -> bool {
    // Use embedded default for backward compatibility
    let config = load_compiled_crawl_config(None).expect("Embedded config should be valid");
    !config.should_crawl(path)
}

/// Determines chunking strategy for a file based on its extension
///
/// This is a convenience function that uses the embedded default config.
/// For crawls that need repo-specific config, use `CompiledCrawlConfig::get_strategy()`.
///
/// # Arguments
///
/// * `path` - File path
///
/// # Returns
///
/// `ChunkingStrategy` enum indicating how to process file
pub fn get_chunk_strategy(path: &str) -> ChunkingStrategy {
    // Use embedded default for backward compatibility
    let config = load_compiled_crawl_config(None).expect("Embedded config should be valid");
    match config.get_strategy(path) {
        Some("typescript") => ChunkingStrategy::TypeScript,
        Some("markdown") => ChunkingStrategy::Markdown,
        Some("lineBased") => ChunkingStrategy::LineBased,
        _ => ChunkingStrategy::Skip,
    }
}

/// Chunking strategy enumeration
///
/// This enum is kept for backward compatibility with existing code.
/// The new `crawl_config` module uses string-based strategy names.
#[derive(Debug, Clone, PartialEq)]
pub enum ChunkingStrategy {
    /// TypeScript files - AST-based semantic chunking
    TypeScript,

    /// Markdown files - Split by heading hierarchy (TODO: implement)
    Markdown,

    /// Simple line-based chunking (50-100 lines)
    LineBased,

    /// Skip this file (don't index)
    Skip,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_skip_build_outputs() {
        assert!(should_skip_path("libraries/foo/lib/index.js"));
        assert!(should_skip_path("libraries/foo/lib-commonjs/index.js"));
        assert!(!should_skip_path("libraries/foo/src/index.ts"));
    }

    #[test]
    fn test_should_skip_lock_files() {
        assert!(should_skip_path("package-lock.json"));
        assert!(should_skip_path("common/config/rush/pnpm-lock.yaml"));
        // package.json is now excluded by default (not useful for semantic search)
        assert!(should_skip_path("package.json"));
    }

    #[test]
    fn test_should_skip_node_modules() {
        assert!(should_skip_path("node_modules/foo/index.ts"));
        assert!(!should_skip_path("src/index.ts"));
    }

    #[test]
    fn test_chunk_strategy() {
        assert_eq!(get_chunk_strategy("foo.ts"), ChunkingStrategy::TypeScript);
        assert_eq!(get_chunk_strategy("README.md"), ChunkingStrategy::Markdown);
        assert_eq!(
            get_chunk_strategy("config.yaml"),
            ChunkingStrategy::LineBased
        );
        assert_eq!(get_chunk_strategy("image.png"), ChunkingStrategy::Skip);
        // .js and .json files are now excluded via patternsToExclude, not fake strategies
        assert_eq!(get_chunk_strategy("app.js"), ChunkingStrategy::Skip);
        assert_eq!(get_chunk_strategy("data.json"), ChunkingStrategy::Skip);
    }
}
