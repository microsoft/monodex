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

Monodex is a CLI tool that indexes Rush monorepo source code and documentation into a local LanceDB database for fast semantic search. It supports **label-based indexing**, allowing you to maintain multiple queryable snapshots (Git branches, commits) within a single catalog.

See [CHANGELOG.md](./CHANGELOG.md) to see what's new.

### Features

- **Label-based indexing**: Maintain multiple queryable filesets (Git branches, commits) within a catalog
- **Commit-based crawling**: Reads directly from Git objects, not working tree (deterministic, reproducible)
- **AST-based chunking**: Tree-sitter powered intelligent splitting for TypeScript/TSX files
- **Breadcrumb context**: Full symbol paths like `@rushstack/node-core-library:JsonFile.ts:JsonFile.load`
- **Local code-aware embeddings**: Uses jina-embeddings-v2-base-code with ONNX Runtime: runs on commodity developer hardware, no external APIs or services required
- **Incremental sync**: Content-hash based change detection for fast re-indexing
- **Intelligent deduplication**: Identical content at same path across labels shares chunks
- **Rush-optimized**: Smart exclusion rules for Rush monorepo patterns

### Vocabulary

Monodex uses a few terms to describe the containment hierarchy:

- A **database** is the on-disk store: by default `~/.monodex/default-db`. Everything lives here.
- A **catalog** is a named monorepo registered in your config. You might have one catalog per codebase.
- A **label** is a named fileset within a catalog: typically a branch or commit. Searches are scoped to a label.
- A **chunk** is a unit of indexed content (function, class, section) with its embedding.

Hierarchy: **database** › **catalog** › **label** › **chunk**

## Agent Usage Guide

This tool is designed for AI assistants. The indexed database provides a complete, internally consistent snapshot of the codebase as it existed at crawl time. Independent of any local file changes, branches, or whether the repo is even cloned, this makes it more than a replacement for grep; it can be the primary way an agent learns about a codebase.

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

- **Model**: jina-embeddings-v2-base-code (auto-downloaded from Hugging Face on first use; cached locally by the `hf_hub` library, typically under `~/.cache/huggingface/`)

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

```js
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
  }

  // Embedding model configuration (optional, defaults shown)
  // "embeddingModel": {
  //   "modelInstances": "auto",
  //   "threadsPerInstance": "auto"
  // }
}
```

> **Note:** We use the [Sparo](https://github.com/tiktok/sparo) monorepo for development testing, since it's a small open-source Rush monorepo.

**Fields:**

<!-- prettier-ignore-start -->

| Field                               | Required | Description                                                                         |
| ----------------------------------- | -------- | ----------------------------------------------------------------------------------- |
| `catalogs.<name>.type`              | Yes      | Catalog type: `"monorepo"`                                                          |
| `catalogs.<name>.path`              | Yes      | Absolute path to the repository root                                                |
| `database.path`                     | No       | Custom database path (default: `~/.monodex/default-db`). If set, it must be an absolute path. Tilde (`~`), environment variables (`$VAR`), and relative paths are not supported. The path must point to a local filesystem; network filesystems (NFS, SMB) and synced cloud folders (Dropbox, OneDrive, iCloud, Google Drive) are not supported. |
| `embeddingModel.modelInstances`     | No       | Number of ONNX model instances (default: `"auto"`). Primary driver of memory usage. |
| `embeddingModel.threadsPerInstance` | No       | Threads per model instance (default: `"auto"`). CPU tuning only.                    |

<!-- prettier-ignore-end -->

**Embedding model configuration:**

The `embeddingModel` section controls memory and CPU usage for embedding generation:

- **`modelInstances`**: Number of ONNX sessions. Each session uses approximately 700MB-1GB for the model weights and runtime, but the auto-detection heuristic plans for 2.5 GiB per instance to provide conservative headroom for memory fragmentation, peak usage during inference, and avoiding OOM on memory-constrained systems. Use `"auto"` to automatically size based on available system memory, or an integer ≥ 1 for explicit control.
- **`threadsPerInstance`**: Threads per ONNX session for intra-op parallelism. Use `"auto"` to automatically size based on CPU cores, or an integer ≥ 1 for explicit control.

**Catalog types:**

- **`monorepo`**: Walks upward to find the nearest `package.json` for package name resolution. Breadcrumbs show `@scope/package-name:File.ts:Symbol`.

## First-Time Setup

Before using Monodex, initialize the database:

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

**Incremental sync:** The crawl is incremental. Unchanged files are skipped. You can safely CTRL+C and resume later.

**Commit-based:** Crawling with `--commit` reads from Git objects, not the working tree. This ensures deterministic, reproducible indexing.

**Working directory mode:** Use `--working-dir` to index uncommitted changes. This reads directly from the filesystem instead of Git objects. The label metadata will show `source_kind = "working-directory"` and `commit_oid = ""`. Working directory labels are mutable. Re-crawling updates the indexed content.

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

### Concurrency

Multiple `monodex` invocations against the same database coordinate via OS-level file locks. Concurrent crawls against the same catalog wait for each other; concurrent crawls against different catalogs run in parallel. Read-only commands (`search`, `view`) acquire no locks and run alongside writers. See `docs/design/concurrency.md` for details.

## Development

When making a pull request, add a bullet under "## Unreleased" in [CHANGELOG.md](./CHANGELOG.md) describing the change from an end-user perspective. See CHANGELOG.md for the version history and publishing instructions.

Run CI checks using [Just](https://github.com/casey/just) (recommended):

```bash
# Install just
cargo install just

# Run all CI checks: format, clippy, all tests
just ci

# Run quick CI checks: format, clippy, fast tests only (slow tests skipped)
just ci-quick

# Individual commands
just fmt          # Auto-format code
just fmt-check    # Check formatting
just clippy       # Run lints
just test         # Run all tests
just test-quick   # Run tests excluding slow ones
just build        # Build release binary
```

Use `just ci-quick` during iteration. Run `just ci` before declaring work complete, and at any intermediate point worth a deeper check. See the "Quick CI tier" section of [docs/code_organization_policy.md](docs/code_organization_policy.md) for the policy.

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

## Crawl Configuration

The crawl behavior (which files to index and how to chunk them) can be customized via configuration files.

### Config Discovery

Configs are loaded in this precedence order:

1. `<repo-root>/monodex-crawl.json` (repo-local)
2. `~/.monodex/crawl.json` (user-global)
3. Embedded default (compiled into binary)

No merging occurs. Exactly one config is used.

### Config Schema

JSON schemas are available in the `schemas/` directory for IDE autocomplete and validation. Reference the appropriate schema in your config file via the `$schema` field:

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

- `fileTypes` is the primary filter. Unsupported file types are never crawled.
- `patternsToKeep` overrides `patternsToExclude` (useful for keeping test files in `src/`)
- Directory patterns (ending in `/`) match anywhere in the path

**Pattern syntax:**

- Glob patterns use the standard syntax: `**` for recursive, `*` for wildcard
- Directory patterns end with `/` (e.g., `node_modules/`)
- Example: `**/*.test.ts` matches test files at any depth

## Status

This project is under active development. Expect breaking changes between versions.

## Documentation

For contributors and curious users:

- [`docs/design/architecture.md`](./docs/design/architecture.md): Five-minute crash course for working on the codebase: vocabulary, data model, crawl pipeline overview, chunker dispatch, source tree.
- [`docs/design/label_ids.md`](./docs/design/label_ids.md): Identifier and reference syntax: catalogs, labels, breadcrumbs, path encoding, planned typed-label and cross-catalog reference grammar.
- [`docs/design/crawl.md`](./docs/design/crawl.md): Crawl pipeline in detail: package index, working-directory identity model, label reassignment, partial-crawl semantics.
- [`docs/design/chunker.md`](./docs/design/chunker.md): Chunking algorithms: embedding model, TypeScript AST partitioning (the "two worlds model"), markdown splitting, quality scoring, empirical findings on alternative runtimes.
- [`docs/design/concurrency.md`](./docs/design/concurrency.md): Writer lock taxonomy, reader semantics, and how the model interacts with LanceDB's and Tantivy's own concurrency mechanisms.
- [`docs/design/monodex_files.md`](./docs/design/monodex_files.md): Inventory of files Monodex reads or writes: tool-home state, database directory, repo-local config, shipped artifacts.
- [`docs/code_organization_policy.md`](./docs/code_organization_policy.md): File size targets, where new code goes, banned patterns. Required reading for contributors.
- [`docs/backlog.md`](./docs/backlog.md): Maintainer scratch pad for what might come next.
- [`docs/smoke_test.md`](./docs/smoke_test.md): End-to-end verification procedure to run after any change.

## License

MIT

---

This project was primarily developed using the Linux Foundation's [Goose](https://goose-docs.ai) AI agent with an open source LLM.

Monodex is part of the [Rush Stack](https://rushstack.io/) family of projects.
