//! CLI argument definitions using clap.
//!
//! Purpose: Define command-line interface types (Cli, Commands, CrawlSourceArgs).
//! Edit here when: Adding new CLI flags, commands, or changing help text.
//! Do not edit here for: Command handler logic (see commands/), config loading (see config.rs).

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// Fast, accurate code search for large Rush monorepos
/// https://www.rushstack.io
#[derive(Parser)]
#[command(name = "monodex", version, about)]
pub struct Cli {
    /// Config file path (default: ~/.monodex/config.json)
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Enable verbose debug logging for storage operations
    #[arg(long, global = true)]
    pub debug: bool,

    #[command(subcommand)]
    pub command: Commands,
}

/// Available commands
#[derive(Subcommand)]
pub enum Commands {
    /// Set default catalog and label for subsequent commands
    Use {
        /// Catalog name (optional - shows current context if omitted)
        #[arg(long)]
        catalog: Option<String>,

        /// Label name (optional - shows current context if omitted)
        #[arg(long)]
        label: Option<String>,
    },

    /// Initialize a new monodex database at the configured path.
    /// Creates database tables for chunks and label metadata.
    /// Idempotent: safe to run on an existing database.
    InitDb,

    /// Crawl source and index chunks (incremental sync).
    /// Reports warnings when AST chunking fails and fallback is used.
    /// These warnings indicate partitioner defects to investigate.
    Crawl {
        /// Catalog name (from config file, uses default context if not provided)
        #[arg(long)]
        catalog: Option<String>,

        /// Label name for this crawl (e.g., "main", "feature-x", "local")
        /// REQUIRED: Must be explicitly specified to avoid accidental overwrites.
        #[arg(long)]
        label: String,

        #[command(flatten)]
        source: CrawlSourceArgs,

        /// Allow files with chunking warnings to participate in incremental skipping
        #[arg(long, default_value_t = false)]
        incremental_warnings: bool,
    },

    /// Purge all chunks from a catalog, or the entire database
    Purge {
        /// Catalog name to purge (if not specified, purges the entire database)
        #[arg(long)]
        catalog: Option<String>,

        /// Purge the entire database (all catalogs)
        #[arg(long)]
        all: bool,
    },

    /// Dump chunks for a TypeScript file (for debugging chunking algorithm).
    /// Uses AST-only mode by default to reveal partitioner issues.
    /// Add --with-fallback to see production behavior with fallback mitigation.
    DumpChunks {
        /// TypeScript file path
        #[arg(long)]
        file: PathBuf,

        /// Target chunk size in chars
        #[arg(long, default_value = "6000")]
        target_size: usize,

        /// Show visualization mode (full chunk contents)
        #[arg(long)]
        visualize: bool,

        /// Enable fallback line-based splitting for oversized chunks.
        /// By default, dump-chunks uses strict AST-only mode to reveal
        /// where the partitioner failed to find good split points.
        #[arg(long)]
        with_fallback: bool,

        /// Enable debug logging for partitioning decisions
        #[arg(long)]
        debug: bool,
    },

    /// Search with compact blurb output (for AI assistants)
    Search {
        /// Search query text
        #[arg(long)]
        text: String,

        /// Number of results
        #[arg(long, default_value = "10")]
        limit: usize,

        /// Filter by label (uses default context if not provided)
        #[arg(long)]
        label: Option<String>,

        /// Filter by catalog (optional - uses label or default context)
        #[arg(long)]
        catalog: Option<String>,
    },

    /// View chunks by their file IDs with optional selectors
    View {
        /// File IDs with optional selectors (can be specified multiple times)
        /// Formats:
        ///   700a4ba232fe9ddc        - all chunks in file
        ///   700a4ba232fe9ddc:3      - chunk 3
        ///   700a4ba232fe9ddc:2-3    - chunks 2 through 3
        ///   700a4ba232fe9ddc:3-end  - chunk 3 through the last chunk
        #[arg(long)]
        id: Vec<String>,

        /// Filter by label (uses default context if not provided)
        #[arg(long)]
        label: Option<String>,

        /// Filter by catalog (optional - uses label or default context)
        #[arg(long)]
        catalog: Option<String>,

        /// Show full filesystem paths
        #[arg(long)]
        full_paths: bool,

        /// Omit catalog preamble (show only chunks)
        #[arg(long)]
        chunks_only: bool,
    },

    /// Audit chunking quality across multiple files (AST-only mode).
    /// Scores reflect AST partitioning quality without fallback mitigation.
    /// Use after eliminating crawl warnings to find suboptimal chunk boundaries.
    AuditChunks {
        /// Number of files to sample
        #[arg(long, default_value = "20")]
        count: usize,

        /// Directory to sample from
        #[arg(long)]
        dir: String,
    },
}

/// Source specification for crawl command.
/// One of --commit or --working-dir is required.
#[derive(Args, Clone, Debug)]
#[group(required = true, multiple = false)]
pub struct CrawlSourceArgs {
    /// Git commit to crawl (branch name, tag, or commit SHA)
    #[arg(long)]
    pub commit: Option<String>,

    /// Crawl the working directory instead of a Git commit.
    /// Indexes uncommitted changes.
    #[arg(long)]
    pub working_dir: bool,
}
