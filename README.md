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

**Semantic search indexer for Rush monorepos using Qdrant vector database**

## Overview

`monodex` is a CLI tool that indexes Rush monorepo source code and documentation into a Qdrant vector database for scalable semantic search. It supports **label-based indexing**, allowing you to maintain multiple queryable snapshots (branches, commits) within a single catalog.

See [CHANGELOG.md](./CHANGELOG.md) for release history.

### Features

- **Label-based indexing**: Maintain multiple queryable filesets (branches, commits) within a catalog
- **Commit-based crawling**: Reads directly from Git objects, not working tree (deterministic, reproducible)
- **AST-based chunking**: Tree-sitter powered intelligent splitting for TypeScript/TSX files
- **Breadcrumb context**: Full symbol paths like `@rushstack/node-core-library:JsonFile.ts:JsonFile.load`
- **Oversized chunk handling**: Functions split at natural AST boundaries (statement blocks, if/else, try/catch)
- **Local embeddings**: Uses jina-embeddings-v2-base-code with ONNX Runtime (no external APIs)
- **Qdrant integration**: Direct batch uploads to Qdrant vector database
- **Incremental sync**: Content-hash based change detection for fast re-indexing
- **Intelligent deduplication**: Identical content at same path across labels shares chunks
- **Rush-optimized**: Smart exclusion rules for Rush monorepo patterns

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

- **Rust**: 1.91+ (for edition 2024)
- **Qdrant**: Vector database running on localhost:6333 (only needed for crawling/searching)

  Installation instructions can be found in the [Qdrant Quickstart](https://qdrant.tech/documentation/quickstart/) documentation.

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

## Qdrant Setup

Create the collection before first use:

```bash
curl -X PUT "http://localhost:6333/collections/monodex" \
  -H "Content-Type: application/json" \
  -d '{
    "vectors": {
      "size": 768,
      "distance": "Cosine"
    }
  }'
```

Verify the collection exists:

```bash
curl http://localhost:6333/collections/monodex | jq '.result.status'
```

The collection uses:

- **768 dimensions** (jina-embeddings-v2-base-code output size)
- **Cosine distance** (best for semantic similarity)

## Usage

### Global Options

```bash
# Use a custom config file location
monodex --config /path/to/config.json search --text "query"

# Enable verbose debug logging for network requests
monodex --debug crawl --catalog myrepo --label main --commit HEAD

# Show help for any command
monodex --help
monodex crawl --help

# Show version
monodex --version
```

### Debug Mode

The `--debug` flag enables verbose logging for troubleshooting:

- Logs HTTP request/response details for Qdrant API calls
- Shows batch sizes and payload sizes during uploads
- Useful for diagnosing connectivity or payload issues

Example:

```bash
monodex --debug crawl --catalog sparo --label main --commit HEAD
```

### Configuration

Create `~/.config/monodex/config.json`:

```json
{
  "qdrant": {
    "url": "http://localhost:6333",
    "collection": "monodex"
  },
  "catalogs": {
    "sparo": {
      "type": "monorepo",
      "path": "/path/to/sparo"
    },
    "rushstack": {
      "type": "monorepo",
      "path": "/path/to/rushstack"
    }
  }
}
```

> **Note:** We use the [Sparo](https://github.com/tiktok/sparo) monorepo for development testing, since it's a small open-source Rush monorepo.

**Fields:**

| Field                   | Required | Description                                          |
| ----------------------- | -------- | ---------------------------------------------------- |
| `qdrant.url`            | No       | Qdrant server URL (default: `http://localhost:6333`) |
| `qdrant.collection`     | Yes      | Qdrant collection name                               |
| `qdrant.maxUploadBytes` | No       | Max upload payload size in bytes (default: 30MB)     |
| `catalogs.<name>.type`  | Yes      | Catalog type: `"monorepo"`                           |
| `catalogs.<name>.path`  | Yes      | Absolute path to the repository root                 |

**Catalog types:**

- **`monorepo`**: Walks upward to find the nearest `package.json` for package name resolution. Breadcrumbs show `@scope/package-name:File.ts:Symbol`.

### Label-Based Indexing

A **label** is a named, queryable fileset within a catalog. Labels typically represent branches or specific commits:

- `rushstack:main` - main branch
- `rushstack:feature-x` - feature branch
- `rushstack:v1.0.0` - specific release tag

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

Default context is stored in `~/.config/monodex/context.json`. Explicit flags always override defaults.

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

# With explicit label (full format: catalog:label)
monodex search --text "API Extractor" --label rushstack:main --limit 10

# JSON output for scripting/tool use
monodex search --text "API Extractor" --label rushstack:main --json

# Compact JSON output (minimal fields)
monodex search --text "API Extractor" --label rushstack:main --json --compact
```

`--compact` is only valid with `--json` and omits larger metadata fields like `text` and `source_uri`.

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

# Filter by label (full format: catalog:label)
monodex view --id 30440fb2ecd5fa62 --label rushstack:main

# JSON output for structured chunk retrieval
monodex view --id 30440fb2ecd5fa62:2-4 --label rushstack:main --json

# Compact JSON output (minimal fields)
monodex view --id 30440fb2ecd5fa62:2-4 --label rushstack:main --json --compact
```

For `view --json`, the output is a top-level object with a `requests` array so missing selectors can be reported alongside successful chunk results.

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

# Purge entire collection (all catalogs)
monodex purge --all
```

**Note:** Purge operates at catalog level. To remove a specific label's chunks, re-crawl that label with a different commit or manually update the `active_label_ids` field.

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
│   ├── main.rs                    # CLI entry point
│   └── engine/                    # Reusable indexing engine
│       ├── mod.rs                 # Module exports
│       ├── crawl_config.rs        # Crawl config loading, validation, and pattern matching
│       ├── config.rs              # Legacy compatibility wrapper (delegates to crawl_config)
│       ├── chunker.rs             # File chunking dispatcher
│       ├── partitioner.rs         # AST-based TypeScript chunking
│       ├── markdown_partitioner.rs # Markdown heading-based chunking
│       ├── git_ops.rs             # Git tree enumeration and blob reading
│       ├── parallel_embedder.rs   # Parallel embedding with multiple ONNX sessions
│       ├── package_lookup.rs      # Package name resolution (walk up to package.json)
│       ├── uploader.rs            # Qdrant HTTP client
│       └── util.rs                # Hash utilities for chunk IDs
├── Cargo.toml                     # Dependencies
├── DESIGN.md                      # Design documentation
└── README.md
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
2. `~/.config/monodex/crawl.json` (user-global)
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

## Related

- [Qdrant](https://qdrant.ai/) - Vector similarity search engine
- [ONNX Runtime](https://onnxruntime.ai/) - Cross-platform ML inference
- [Rush Stack](https://rushstack.io/) - Monorepo toolkit for JavaScript/TypeScript
