<div>
  <br />
  <a href="https://github.com/microsoft/monodex">
    <img height="130" alt="Rush Monodex" src="./assets/monodex-logo.svg">
  </a>
  <p />
</div>

# Rush Monodex

[![crates.io](https://img.shields.io/crates/v/monodex.svg)](https://crates.io/crates/monodex)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

**Semantic search indexer for Rush monorepos**

## Overview

`monodex` is a CLI tool that indexes Rush monorepo source code and documentation into a local LanceDB database for fast semantic search. It supports **label-based indexing**, allowing you to maintain multiple queryable snapshots (branches, commits) within a single catalog.

See [CHANGELOG.md](./CHANGELOG.md) for release history.

### Features

- **Label-based indexing**: Maintain multiple queryable filesets (branches, commits) within a catalog
- **Commit-based crawling**: Reads directly from Git objects, not working tree (deterministic, reproducible)
- **AST-based chunking**: Tree-sitter powered intelligent splitting for TypeScript/TSX files
- **Breadcrumb context**: Full symbol paths like `@rushstack/node-core-library:JsonFile.ts:JsonFile.load`
- **Oversized chunk handling**: Functions split at natural AST boundaries (statement blocks, if/else, try/catch)
- **Local embeddings**: Uses jina-embeddings-v2-base-code with ONNX Runtime (no external APIs)
- **LanceDB integration**: Embedded vector database (no external services required)
- **Incremental sync**: Content-hash based change detection for fast re-indexing
- **Intelligent deduplication**: Identical content at same path across labels shares chunks
- **Rush-optimized**: Smart exclusion rules for Rush monorepo patterns

### Vocabulary

Monodex uses a few terms to describe the containment hierarchy:

- A **database** is the on-disk store — by default `~/.monodex/default-db`. Everything lives here.
- A **catalog** is a named monorepo registered in your config. You might have one catalog per codebase.
- A **label** is a named fileset within a catalog — typically a branch or commit. Searches are scoped to a label.
- A **chunk** is a unit of indexed content (function, class, section) with its embedding.

Hierarchy: **database** › **catalog** › **label** › **chunk**

## Agent Usage Guide

This tool is designed for AI assistants. The indexed database provides a complete, internally consistent snapshot of the codebase as it existed at crawl time — independent of any local file changes, branches, or whether the repo is even cloned. This makes it more than a replacement for grep; it can be the primary way an agent learns about a codebase.

**Typical workflow:**

1. **Set default context** (optional but recommended):

   ```bash
   monodex use --catalog rushstack --label main
   ```

2. **Start with semantic search** to find relevant code:

   ```bash
   monodex search --text "how does rush handle pnpm shrinkwrap files"
   ```

3. **View full chunks** using the `file_id:chunk_ordinal` from search results:

   ```bash
   monodex view --id 700a4ba232fe9ddc:3
   ```

4. **Get surrounding context** by viewing adjacent chunks:

   ```bash
   monodex view --id 700a4ba232fe9ddc:2-4
   ```

5. **Reconstruct entire files** by viewing all chunks:
   ```bash
   monodex view --id 700a4ba232fe9ddc
   ```

**Output format:** Search results prefix code lines with `>`, making them easy to distinguish from your own output and preventing injection attacks.

## Prerequisites

- **Rust**: 1.93+ (for edition 2024)

- **Protocol Buffers compiler (`protoc`)**: Required at build time by LanceDB's transitive dependencies. Install via your platform package manager:

  | Platform      | Command                                  |
  | ------------- | ---------------------------------------- |
  | Windows       | `winget install protobuf`                |
  | macOS         | `brew install protobuf`                  |
  | Debian/Ubuntu | `sudo apt-get install protobuf-compiler` |
  | Fedora/RHEL   | `sudo dnf install protobuf-compiler`     |
  | Arch          | `sudo pacman -S protobuf`                |

  Verify with `protoc --version` (any recent 3.x or 4.x/20+ release works). If `protoc` is installed in a non-standard location, set the `PROTOC` environment variable to its full path before building.

- **Model**: jina-embeddings-v2-base-code (auto-downloaded from HuggingFace to `models/` on first use)

## Installation

### From crates.io

```bash
cargo install monodex
```

### Build from Source

```bash
git clone https://github.com/microsoft/monodex.git
cd monodex
cargo build --release

# Binary will be at ./target/release/monodex
```

## Configuration

Create `~/.monodex/config.json`:

```json
{
  // Database configuration (optional, defaults to ~/.monodex/default-db)
  // "database": {
  //   "path": "/absolute/path/to/your/db"
  // },

  // Catalog definitions (required)
  "catalogs": {
    "sparo": {
      "type": "monorepo",
      "path": "/path/to/sparo"
    },
    "rushstack": {
      "type": "monorepo",
      "path": "/path/to/rushstack"
    }
  },

  // Embedding model configuration (optional, defaults shown)
  // "embeddingModel": {
  //   "modelInstances": "auto",
  //   "threadsPerInstance": "auto"
  // }
}
```

> **Note:** We use the [Sparo](https://github.com/tiktok/sparo) monorepo for development testing, since it's a small open-source Rush monorepo.

**Fields:**

| Field                               | Required | Description                                                                         |
| ----------------------------------- | -------- | ----------------------------------------------------------------------------------- |
| `catalogs.<name>.type`              | Yes      | Catalog type: `"monorepo"`                                                          |
| `catalogs.<name>.path`              | Yes      | Absolute path to the repository root                                                |
| `database.path`                     | No       | Custom database path (default: `~/.monodex/default-db`). If set, it must be an absolute path. Tilde (`~`), environment variables (`$VAR`), and relative paths are not supported. |
| `embeddingModel.modelInstances`     | No       | Number of ONNX model instances (default: `"auto"`). Primary driver of memory usage. |
| `embeddingModel.threadsPerInstance` | No       | Threads per model instance (default: `"auto"`). CPU tuning only.                    |

**Embedding model configuration:**

The `embeddingModel` section controls memory and CPU usage for embedding generation:

- **`modelInstances`**: Number of ONNX sessions. Each session uses approximately 700MB–1GB for the model weights and runtime, but the auto-detection heuristic plans for 2.5 GiB per instance to provide conservative headroom for memory fragmentation, peak usage during inference, and avoiding OOM on memory-constrained systems. Use `"auto"` to automatically size based on available system memory, or an integer ≥ 1 for explicit control.
- **`threadsPerInstance`**: Threads per ONNX session for intra-op parallelism. Use `"auto"` to automatically size based on CPU cores, or an integer ≥ 1 for explicit control.

**Catalog types:**

- **`monorepo`**: Walks upward to find the nearest `package.json` for package name resolution. Breadcrumbs show `@scope/package-name:File.ts:Symbol`.


## First-Time Setup

Before using monodex, initialize the database:

```bash
monodex init-db
```

This creates a local LanceDB database at `~/.monodex/default-db/`. No external services are required.

## Usage

### Global Options

```bash
# Use a custom config file location
monodex --config /path/to/config.json search --text "query"

# Enable verbose debug logging for storage operations
monodex --debug crawl --catalog myrepo --label main --commit HEAD

# Show help for any command
monodex --help
monodex crawl --help

# Show version
monodex --version
```

### Debug Mode

The `--debug` flag enables verbose logging for troubleshooting:

- Logs storage-layer operations
- Shows batch sizes during uploads
- Useful for diagnosing database issues

Example:

```bash
monodex --debug crawl --catalog sparo --label main --commit HEAD
```

### Label-Based Indexing

A **label** is a named, queryable fileset within a catalog. Labels typically represent branches or specific commits:

- a label named `main` under the `rushstack` catalog (main branch)
- a label named `feature-x` (a feature branch)
- a label named `v1.0.0` (a specific release tag)

**Key concept:** Chunks are immutable content. Labels track which chunks belong to which fileset. When you crawl a new commit under a label, membership is updated but identical content is reused.

### Set Default Context

The `use` command manages the default catalog and label for subsequent commands:

```bash
# Show current context
monodex use

# Set default catalog and label
monodex use --catalog sparo --label main

# Now you can omit --label in subsequent commands
monodex search --text "how to read JSON files"
```

Default context is stored in `~/.monodex/context.json`. Explicit flags always override defaults.

### Index a Repository

```bash
# Index working directory changes
monodex crawl --catalog rushstack --label working --working-dir

# Index HEAD commit under the "main" label
monodex crawl --catalog rushstack --label main --commit HEAD

# Index a specific branch
monodex crawl --catalog rushstack --label feature-x --commit feature-branch

# Index a specific commit SHA
monodex crawl --catalog rushstack --label v1.0.0 --commit a1b2c3d4e5f6
```

**Required arguments:** The `crawl` command requires `--label` and either `--working-dir` or `--commit`. This prevents accidental overwrites of important labels.

**Incremental sync:** The crawl is incremental — unchanged files are skipped. You can safely CTRL+C and resume later.

**Commit-based:** Crawling with `--commit` reads from Git objects, not the working tree. This ensures deterministic, reproducible indexing.

**Working directory mode:** Use `--working-dir` to index uncommitted changes. This reads directly from the filesystem instead of Git objects. The label metadata will show `source_kind = "working-directory"` and `commit_oid = ""`. Working directory labels are mutable — re-crawling updates the indexed content.

**Label reassignment:** When you re-crawl a label with a new commit, chunks from the old commit that no longer exist are removed from that label's membership.

**Incremental warnings:** By default, files with chunking warnings are always re-processed. Use `--incremental-warnings` to allow them to be skipped if unchanged (useful for large codebases with known chunking issues).

### Search the Database

```bash
# Semantic search (uses default context if set)
monodex search --text "how to read JSON files"

# With explicit catalog and label
monodex search --text "API Extractor" --catalog rushstack --label main --limit 10
```

### View Full Chunks

```bash
# View a specific chunk by ordinal
monodex view --id 30440fb2ecd5fa62:3

# View a range of chunks
monodex view --id 30440fb2ecd5fa62:2-4

# View from chunk 3 to the end
monodex view --id 30440fb2ecd5fa62:3-end

# View all chunks in a file (reconstruct entire file)
monodex view --id 30440fb2ecd5fa62

# View chunks from multiple files
monodex view --id 30440fb2ecd5fa62:3 --id a1b2c3d4e5f67890:1-2

# Show full filesystem paths
monodex view --id 30440fb2ecd5fa62 --full-paths

# Omit catalog preamble (chunks only)
monodex view --id 30440fb2ecd5fa62 --chunks-only

# Filter by catalog and label
monodex view --id 30440fb2ecd5fa62 --catalog rushstack --label main
```

### Debug Chunking Algorithm

```bash
# See how a file gets chunked (AST-only mode, reveals partitioner issues)
monodex dump-chunks --file ./src/JsonFile.ts

# Include fallback line-based splitting (production behavior)
monodex dump-chunks --file ./src/JsonFile.ts --with-fallback

# Visualize mode - show full chunk contents
monodex dump-chunks --file ./src/JsonFile.ts --visualize

# Debug mode - show partitioning decisions
monodex dump-chunks --file ./src/JsonFile.ts --debug

# Custom target chunk size (default: 6000 chars)
monodex dump-chunks --file ./src/JsonFile.ts --target-size 4000

# Audit chunking quality across multiple files (AST-only mode)
monodex audit-chunks --count 20 --dir /path/to/project
```

**Chunk Quality Score**: 0-100%, higher is better. Scores below 95% may indicate chunking issues. Note: `dump-chunks` and `audit-chunks` use AST-only mode (fallback disabled) to accurately measure partitioner quality.

### Purge Data

```bash
# Purge all chunks from a catalog (all labels)
monodex purge --catalog rushstack

# Purge entire database (all catalogs)
monodex purge --all
```

**Note:** Purge operates at catalog level. To remove a specific label's chunks, re-crawl that label with a different commit or manually update the `active_label_ids` field.

### Database Management

```bash
# Initialize the database (required before first crawl)
monodex init-db

# Re-run is safe - idempotent if database already exists
monodex init-db
```

The database is stored at `~/.monodex/default-db/` by default. You can customize this location via the `database.path` field in config.

## Development

When making a pull request, add a bullet under "## Unreleased" in [CHANGELOG.md](./CHANGELOG.md) describing the change from an end-user perspective. See CHANGELOG.md for the version history and publishing instructions.

Run CI checks using [Just](https://github.com/casey/just) (recommended):

```bash
# Install just
cargo install just

# Run all CI checks (format, clippy, check, test)
just ci

# Individual commands
just fmt          # Auto-format code
just fmt-check    # Check formatting
just clippy       # Run lints
just check        # Type check
just test         # Run tests
just build        # Build release binary
```

Or run directly with cargo:

```bash
# Run all CI checks
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked
cargo check --workspace --all-targets --locked
cargo test --workspace --all-targets --locked

# Build
cargo build --release

# Run with logging (use sparo for testing, not rushstack)
RUST_LOG=debug ./target/release/monodex crawl --catalog sparo --label main --commit HEAD
```

## Architecture

```
monodex/
├── src/
│   ├── lib.rs                    # Crate root module exports
│   ├── main.rs                   # CLI entry point and command dispatch
│   ├── app/                      # Application layer (CLI-specific)
│   │   ├── mod.rs                # Module exports
│   │   ├── cli.rs                # Clap CLI definitions
│   │   ├── config.rs             # App configuration loading
│   │   ├── context.rs            # Default catalog/label persistence
│   │   ├── util.rs               # Formatting and display utilities
│   │   ├── commands/             # CLI command handlers
│   │   │   ├── mod.rs            # Module exports
│   │   │   ├── use_cmd.rs        # `use` command
│   │   │   ├── crawl.rs          # `crawl` command
│   │   │   ├── search.rs         # `search` command
│   │   │   ├── view.rs           # `view` command
│   │   │   ├── purge.rs          # `purge` command
│   │   │   ├── dump_chunks.rs    # `dump-chunks` command
│   │   │   └── audit_chunks.rs   # `audit-chunks` command
│   │   └── crawl/                # Crawl pipeline implementation
│   │       ├── mod.rs            # Module exports
│   │       ├── types.rs          # Crawl-specific types
│   │       └── pipeline.rs       # Embed/upload orchestration
│   └── engine/                   # Reusable indexing engine
│       ├── mod.rs                # Module exports
│       ├── breadcrumb.rs         # Breadcrumb context generation
│       ├── chunker.rs            # File chunking dispatcher
│       ├── config.rs             # Engine configuration types
│       ├── crawl_config.rs       # Crawl filtering rules
│       ├── git_ops.rs            # Git tree enumeration and blob reading
│       ├── identifier.rs         # Catalog/label identifier validation
│       ├── markdown_partitioner.rs # Markdown heading-based chunking
│       ├── package_lookup.rs     # Package name resolution
│       ├── parallel_embedder.rs  # Parallel embedding with ONNX sessions
│       ├── schema.rs             # LanceDB schema definitions
│       ├── system_info.rs        # Memory and CPU detection
│       ├── util.rs               # Hash utilities for chunk IDs
│       ├── partitioner/          # TypeScript/TSX AST-based chunking
│       │   ├── mod.rs            # Module exports
│       │   ├── types.rs          # Partitioner types and config
│       │   ├── partition.rs      # Main entry point
│       │   ├── split_search.rs   # Split-point search algorithm
│       │   ├── node_analysis.rs  # AST node analysis helpers
│       │   ├── scoring.rs        # Chunk quality scoring
│       │   └── debug.rs          # Debug output types
│       └── storage/              # LanceDB storage layer
│           ├── mod.rs            # Module exports
│           ├── database.rs       # Database open and validation
│           ├── chunks.rs         # Chunk storage operations
│           ├── labels.rs         # Label metadata operations
│           └── rows.rs           # Row type definitions
├── Cargo.toml                    # Dependencies
├── DESIGN.md                     # Design documentation
└── README.md                     # This file
```

### Chunking Strategy

**TypeScript/TSX files** are chunked using AST-aware partitioning:

- Splits at semantic boundaries (functions, classes, methods, enums)
- Includes preceding JSDoc/TSDoc comments with each symbol
- Handles oversized functions by splitting at statement blocks
- Full breadcrumb context: `package:file:Class.method`

**Quality indicators in breadcrumbs:**

- No marker: Successful AST split with good chunk geometry
- `:[degraded-ast-split]`: AST split with poor geometry (tiny chunks)
- `:[fallback-split]`: No AST split found, used line-based recovery (failure mode)

**Markdown files** are split by heading hierarchy.

**JSON files** are skipped (low value for semantic search).

**Exclusions:** Folders like `node_modules` and files like `*.test.ts` are automatically skipped. Exclusion rules can be customized via `monodex-crawl.json` (see [Crawl Configuration](#crawl-configuration)).

### Crawl Configuration

The crawl behavior (which files to index and how to chunk them) can be customized via configuration files.

#### Config Discovery

Configs are loaded in this precedence order:

1. `<repo-root>/monodex-crawl.json` (repo-local)
2. `~/.monodex/crawl.json` (user-global)
3. Embedded default (compiled into binary)

No merging occurs — exactly one config is used.

#### Config Schema

JSON schemas are available in the `schemas/` directory for IDE autocomplete and validation. Copy the appropriate schema file to your project or reference it locally:

| Config File          | Schema File                   |
| -------------------- | ----------------------------- |
| `config.json`        | `schemas/config.schema.json`  |
| `monodex-crawl.json` | `schemas/crawl.schema.json`   |
| `context.json`       | `schemas/context.schema.json` |

Create a `monodex-crawl.json` file:

```json
{
  "version": 1,
  "fileTypes": {
    ".ts": "typescript",
    ".tsx": "typescript",
    ".md": "markdown",
    ".yaml": "lineBased"
  },
  "patternsToExclude": [
    "node_modules/",
    "dist/",
    "build/",
    "**/*.test.ts",
    "**/*.spec.ts"
  ],
  "patternsToKeep": ["src/", "test/"]
}
```

**Fields:**

| Field               | Type   | Description                              |
| ------------------- | ------ | ---------------------------------------- |
| `version`           | number | Must be `1`                              |
| `fileTypes`         | object | Maps file extension to chunking strategy |
| `patternsToExclude` | array  | Glob patterns for paths to skip          |
| `patternsToKeep`    | array  | Glob patterns that override exclusions   |

**Chunking strategies:**

| Strategy     | Description                          |
| ------------ | ------------------------------------ |
| `typescript` | AST-based semantic chunking (TS/TSX) |
| `markdown`   | Split by heading hierarchy           |
| `lineBased`  | Generic line-based chunking          |

**Evaluation rule:**

```text
shouldCrawl = matchesFileType && (matchesPatternsToKeep || !matchesPatternsToExclude)
```

- `fileTypes` is the primary filter — unsupported file types are never crawled
- `patternsToKeep` overrides `patternsToExclude` (useful for keeping test files in `src/`)
- Directory patterns (ending in `/`) match anywhere in the path

**Pattern syntax:**

- Glob patterns use the standard syntax: `**` for recursive, `*` for wildcard
- Directory patterns end with `/` (e.g., `node_modules/`)
- Example: `**/*.test.ts` matches test files at any depth

### Chunk Size Target

- **Target**: 6000 characters (text only)
- **Fits**: 8192-token embedding model limit (jina-embeddings-v2-base-code)
- **Breadcrumb**: Extra overhead for navigation context

## Status

This project is under active development. The crate is published to reserve the name. Expect breaking changes between versions.

## License

MIT

## Links

In this repo:

- [CODE_ORGANIZATION_POLICY.md](./CODE_ORGANIZATION_POLICY.md) - guidelines for where to add new code
- [DESIGN.md](./DESIGN.md) - more detailed architectural notes

Online references:

- [LanceDB](https://lancedb.github.io/lancedb/) - Serverless vector database
- [ONNX Runtime](https://onnxruntime.ai/) - Cross-platform ML inference
- [Rush Stack](https://rushstack.io/) - Monorepo toolkit for JavaScript/TypeScript

_This project was primarily developed using the Linux Foundation's [Goose](https://goose-docs.ai) AI agent with an open source LLM._
