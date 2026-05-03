# Architecture

This is the five-minute crash course for contributors. It assumes you've read the [README](../../README.md), so it doesn't repeat install instructions or basic CLI usage. Read this and the [code organization policy](../code_organization_policy.md) before proposing structural changes.

## Vocabulary

The README introduces the database/catalog/label/chunk hierarchy. Two refinements matter for code work:

- The `label_id` is an internal storage key of the form `<catalog>:<label>` (e.g., `rushstack:main`). Users never type or see this directly — the CLI takes `--catalog` and `--label` as separate flags. The qualified form appears only in database row fields, log output, and internal types.
- Chunks are immutable content; labels are mutable membership. A chunk row carries an `active_label_ids` list, and re-crawling a label updates that list rather than rewriting chunk content. This is what enables identical content shared between branches without re-embedding.

## Data model

The database is a directory containing two LanceDB tables. The on-disk layout (the `.lance/` table convention, `monodex-meta.json`, the schema-versioning rule, and the future home for the Tantivy index) is in [monodex_files.md](./monodex_files.md).

The two tables:

- `chunks` — one row per indexed chunk. Carries the chunk text, embedding vector, identity fields, path/package context, and label membership. Schema in `src/engine/schema.rs`; the typed Rust row struct (`ChunkRow`) in `src/engine/storage/rows.rs`.
- `label_metadata` — one row per label. Carries the catalog, the bare label name, the qualified `label_id`, the resolved commit OID (or empty for working-directory labels), the source kind, and a `crawl_complete` flag.

The chunk row carries enough path/package/breadcrumb context to be displayed without a working tree or Git checkout — search results stand alone.

A label-scoped search is the storage-layer filter `catalog == X AND active_label_ids CONTAINS "<catalog>:<label>"` combined with vector similarity on the embedding column. View queries use the same `active_label_ids` filter without the vector search.

Designed scale: around 200K files and 600K chunks per catalog. Full crawls take 15–30 minutes; embedding throughput is around 12ms per chunk on a typical multi-core machine with the auto-tuned ONNX session pool.

### Chunk identity

```
file_id  = hash(embedder_id + chunker_id + blob_id + relative_path)
point_id = "{file_id}:{chunk_ordinal}"
```

The `file_id` is a 16-char hex string identifying a semantic version of a file. The `point_id` is the row's primary key. `chunk_ordinal` is 1-indexed.

The fact that `relative_path` is part of `file_id` matters: identical content at different paths produces different `file_id` values. Path renames create new chunks. This is intentional — breadcrumb context is part of what gets indexed, so different paths mean different indexed artifacts.

The `embedder_id` and `chunker_id` constants live in `src/engine/util.rs`. Bumping either invalidates reuse — change them when chunking or embedding behavior changes in a way that should force re-indexing.

### Sentinel-based incremental crawl

Chunk 1 of each file is the sentinel: the row with `chunk_ordinal = 1` is also the only row with `file_complete = true` once the file finishes indexing. The crawl checks for the sentinel row by `point_id` lookup; if it exists and is complete, the file is skipped, and only `active_label_ids` is updated to add the current label. This is what makes re-crawling cheap.

## Crawl pipeline

A crawl run, end to end:

1. **Label upsert** — Resolve `--commit` to a full SHA (or note that this is a `--working-dir` run); upsert label metadata with `crawl_complete = false`.
2. **Tree visitor** — Enumerate files from the commit tree or walk the working directory.
3. **Package indexing** — Build the package index, a map from directory paths to package names, by reading every `package.json` in the tree.
4. **File processing** — For each file: compute `file_id`, check the sentinel, and either skip-with-label-add or read-chunk-embed-upsert.
5. **Label reassignment** — After all files succeed, scan chunks tagged with this label, drop the label from any whose `file_id` wasn't touched, and delete chunks whose `active_label_ids` becomes empty.
6. **Crawl finalization** — Mark `crawl_complete = true`.

Step 5 only runs after a fully successful crawl. An interrupted crawl leaves stale chunks in the label, which the next successful crawl cleans up. See [crawl.md](./crawl.md) for the working-directory identity model and the package-index implementation. The named steps above are the same vocabulary `crawl.md` uses for its detail sections.

## Chunker dispatch

`src/engine/chunker.rs` is the dispatcher. It looks up the chunking strategy for a file extension in the loaded crawl config and routes to one of:

- `typescript` → AST-based partitioning via tree-sitter, using the "two worlds model" that separates size-driven splitting from AST structure. Lives in `src/engine/partitioner/`.
- `markdown` → heading-based splitting. Lives in `src/engine/markdown_partitioner.rs`.
- `lineBased` → simple line-window splitting for generic text formats.

Target chunk size is 6000 characters, well under the 8192-token limit of jina-embeddings-v2-base-code. See [chunker.md](./chunker.md) for the partitioning algorithm and quality markers.

## Identifiers

Catalog and label names follow strict syntax rules: catalogs are kebab-case, labels are Git-like. Reserved characters (`:`, `@`, `+`, `#`) are forbidden in both. Validation lives in `src/engine/identifier.rs`. The full grammar, including planned typed-label and cross-catalog reference forms, is in [label_ids.md](./label_ids.md).

## Filesystem footprint

Monodex reads and writes files in three places: the user's tool home (`~/.monodex/`), the repository being indexed (the repo-local crawl config and `package.json` files), and editor-consumed schema files that ship with the project. The full inventory, with notes on which files are user-editable and which are tool-managed, lives in [monodex_files.md](./monodex_files.md).

## Source tree

Module-organization rules — file size targets, where new code goes, banned patterns — are in the [code organization policy](../code_organization_policy.md). What follows is a one-or-two-line description of every non-test source file. Each directory has a `mod.rs` for module exports; those are not listed individually. Section headings are repo-relative directory paths, so `cli.rs` under `### src/app/` lives at `src/app/cli.rs`.

### src/

- `lib.rs` — Crate root; declares `app`, `engine`, and `paths` modules.
- `main.rs` — Binary entry point; parses CLI args and dispatches to command handlers.
- `paths.rs` — Resolves filesystem paths for tool state (config, context, crawl config, warnings) under `~/.monodex/` or the `MONODEX_HOME` override.

### src/app/

Application-layer code, CLI-specific. Not reusable as a library.

- `cli.rs` — Clap argument definitions; `Cli`, `Commands`, `CrawlSourceArgs`. Edit here for new flags or subcommand wiring.
- `config.rs` — Load and validate `config.json` (catalogs, database path, embedding-model knobs). Contains the `Config` and `DatabaseConfig` structs and the resolver that picks the database path.
- `context.rs` — Persist and resolve the default catalog/label set by `monodex use`. Owns the `DefaultContext` struct and read/write to `~/.monodex/context.json`.
- `util.rs` — Formatting and display helpers: timestamps, durations, byte sizes, terminal sanitization for the `>`-prefixed search output.

### src/app/commands/

One file per CLI subcommand handler. Most are thin: parse args, call into the engine, format output.

- `audit_chunks.rs` — `audit-chunks`: sample TypeScript files from a directory and report aggregate chunk-quality scores. AST-only mode.
- `crawl.rs` — `crawl`: enumerate files (commit tree or working dir), drive the embed/upload pipeline, run label reassignment after success, persist warnings.
- `dump_chunks.rs` — `dump-chunks`: visualize partitioner output for a single file. Supports debug, visualize, and with-fallback modes.
- `init_db.rs` — `init-db`: create a new database directory, write the LanceDB tables and `monodex-meta.json`. Idempotent.
- `purge.rs` — `purge`: delete all chunks for a catalog or for the entire database. Operates at catalog level only — there is no per-label purge.
- `search.rs` — `search`: embed a query string and run a label-scoped vector search. Output uses `>`-prefixed lines and reports distance, not score.
- `use_cmd.rs` — `use`: set or display default catalog/label context. Named `use_cmd` because `use` is a Rust keyword.
- `view.rs` — `view`: retrieve chunks by `file_id` with selector syntax (`:N`, `:N-M`, `:N-end`, or absent for the whole file). Reconstructs files from chunks.

### src/app/crawl/

Crawl-pipeline orchestration shared between command handlers.

- `pipeline.rs` — Coordinate parallel embedding and LanceDB writes via crossbeam channels and rayon. Track per-chunk failures, format ETA and progress output, drive memory-warning checks.
- `types.rs` — Crawl source kinds and the `CrawlFailures` tracker. Embedding failures are tracked per-chunk; structural errors (disk full, dataset corruption) abort immediately.

### src/engine/

Reusable indexing engine. Does not depend on `src/app/`.

- `breadcrumb.rs` — Percent-encode reserved characters (`:`, `@`, `=`, `+`, `#`, `%`, whitespace) in breadcrumb path components. Slugify markdown headings GitHub-style.
- `chunker.rs` — Strategy dispatcher: pick a chunking strategy by file extension and produce `Chunk` records. Computes `file_id` and `point_id` for each chunk.
- `crawl_config.rs` — Load and compile `monodex-crawl.json` (file types, exclude/keep patterns) with `globset`. Implements the `should_crawl()` and `get_strategy()` evaluation rules. Holds the embedded default config and the `ChunkingStrategy` enum.
- `git_ops.rs` — Enumerate Git commit trees, read blob content, build the package index, walk the working directory. Working-dir mode shells out to `git ls-files`/`git status`/`git hash-object` so blob IDs match commit-mode IDs.
- `identifier.rs` — Validate catalog and label syntax. Owns the `LabelId` type and the qualified-form composer; will host the parser for typed labels and cross-catalog references when those land.
- `markdown_partitioner.rs` — Custom markdown parser that splits at headings, fenced code blocks, block quotes, and paragraphs. Generates breadcrumbs from heading hierarchy.
- `package_lookup.rs` — Filesystem-only fallback that walks up to find the nearest `package.json` and extracts its `name`. Used only by `dump-chunks`; the main crawl path resolves packages from the package index.
- `parallel_embedder.rs` — Pool of ONNX sessions for parallel embedding generation. Each session uses limited intra-op threads; pool size and threads are auto-tuned from RAM and core count via `system_info`.
- `schema.rs` — Arrow schema definitions for the `chunks` and `label_metadata` LanceDB tables. Holds `MONODEX_SCHEMA_VERSION`, which must be bumped on any change to column shape; see [monodex_files.md](./monodex_files.md) for the rationale.
- `system_info.rs` — Detect total RAM, cgroup limits, CPU cores. Implements the `"auto"` heuristic for embedding-model `modelInstances` and `threadsPerInstance`. Cgroup-aware so containerized installs warn correctly.
- `util.rs` — Hash utilities: `compute_file_id` (xxhash of embedder/chunker/blob/path), `compute_point_id`, `compute_hash`. Holds the `EMBEDDER_ID` and `CHUNKER_ID` constants.

### src/engine/partitioner/

TypeScript/TSX AST-based chunking. See [chunker.md](./chunker.md) for the algorithm.

- `debug.rs` — Debug logging hooks for split decisions; `PartitionDebug` flag struct controlling verbose output.
- `node_analysis.rs` — AST node helpers: meaningful-children enumeration, symbol-name extraction, line-span computation, JSDoc/TSDoc collection.
- `partition.rs` — Top-level entry point: parse with tree-sitter, drive the recursive split search, finalize chunks with metadata.
- `scoring.rs` — Quality scoring (0–100%) and the `ChunkQualityReport` consumed by `audit-chunks`. Penalizes tiny chunks and oversized chunks.
- `split_search.rs` — Recursive search for split points at AST boundaries; descent into nested scopes when no shallow split fits the budget.
- `types.rs` — `PartitionedChunk`, `PartitionConfig`, `SplitResult`, the `TARGET_CHARS` and `SMALL_CHUNK_CHARS` constants.

### src/engine/storage/

LanceDB storage layer. Typed operations on the two tables.

- `database.rs` — Open a database directory, validate `monodex-meta.json` schema version, expose table handles. Single source of database-open errors.
- `labels.rs` — Read, upsert, and delete `label_metadata` rows. Handles the `crawl_complete` lifecycle.
- `rows.rs` — Plain-Rust `ChunkRow` and `LabelMetadataRow` types with conversion to/from Arrow `RecordBatch`. The rest of the engine deals only in these row types, never in raw Arrow.

### src/engine/storage/chunks/

Sub-module for chunk-table operations, separated from the rest of `storage/` because it's larger than the others and has its own tests file.

- `mod.rs` — Insert, upsert, vector search, label-membership add/remove, per-file chunk lookup, and sentinel checks against the `chunks` table.

## All markdown files in this repo

Every `.md` file in the repo, with a one-line description. Add an entry when adding a doc, remove when deleting. Top-level files (README, CHANGELOG, SECURITY, LICENSE) are listed alongside `docs/` files; new top-level docs are rare.

- `README.md` — User-facing landing page: what Monodex is, install, configuration, CLI usage. Also serves as the crates.io page, so kept short.
- `CHANGELOG.md` — Release history. Contains an HTML comment at the top with the version-bump procedure (semver rules, when to add the `## Unreleased` heading, which `###` subheadings are allowed); read it before publishing.
- `SECURITY.md` — Microsoft boilerplate pointing at `aka.ms/SECURITY.md`. No project-specific content.
- `docs/code_organization_policy.md` — Required reading for contributors. File size targets, where new code goes, banned patterns, test placement, naming.
- `docs/smoke_test.md` — Minimal end-to-end verification procedure: configure Sparo as a catalog, purge, crawl, search, view. Run after any change to confirm the build actually works.
- `docs/backlog.md` — Maintainer scratch pad for what might come next; items grouped by priority bucket. For official feature requests, the README points at GitHub issues, Zulip, and the Rush Hour video call.
- `docs/design/architecture.md` — This file.
- `docs/design/label_ids.md` — Identifier and reference syntax: catalogs, labels, breadcrumbs, cross-catalog references, planned typed-label grammar, path encoding rules at locator boundaries.
- `docs/design/crawl.md` — Crawl pipeline in detail: package index implementation, working-directory identity model, label reassignment.
- `docs/design/chunker.md` — Chunking algorithms: TypeScript AST partitioning (the "two worlds model"), markdown splitting, quality markers and scoring.
- `docs/design/monodex_files.md` — Inventory of files monodex reads or writes: tool-home state, repo-local config files monodex reads from the indexed repo, editor-consumed schemas, init templates.
- `schemas/editing.md` — Cross-reference back to the Rust structs that mirror these schemas, plus a policy reminder that these files are publicly published artifacts.
