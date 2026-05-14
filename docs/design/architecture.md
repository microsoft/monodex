# Architecture

This is the five-minute crash course for contributors. It assumes you've read the [README](../../README.md), so it doesn't repeat install instructions or basic CLI usage. Read this and the [code organization policy](../code_organization_policy.md) before proposing structural changes.

## Vocabulary

The README introduces the database/catalog/label/chunk hierarchy. Three refinements matter for code work:

- The `label_id` is an internal storage key of the form `<catalog>:<label>` (e.g., `rushstack:main`). Users never type or see this directly. The CLI takes `--catalog` and `--label` as separate flags. The qualified form appears only in database row fields, log output, and internal types.
- Chunks are immutable content; labels are mutable membership. A chunk row carries an `active_label_ids` list, and re-crawling a label updates that list rather than rewriting chunk content. This is what enables identical content shared between branches without re-embedding.
- A **retrieval method** is one way of querying a label's chunks. Monodex ships two: `vector` (semantic similarity over embeddings) and `fts` (lexical search via Tantivy). Each label carries a per-method retrieval selection set at crawl time. The CLI's `--retrieval` flag references methods by these names; the same names appear as on-disk column prefixes (`vector_source`, `fts_complete`) and engine API entry points (`vector_search`, `fts_search`).

## Data model

The database is a directory containing two LanceDB tables and a tree of per-label Tantivy index directories. The on-disk layout (the `.lance/` table convention, `monodex-meta.json`, the schema-versioning rule, the FTS directory tree) is in [monodex_files.md](./monodex_files.md).

The two LanceDB tables:

- `chunks`: one row per indexed chunk. Carries the chunk text, embedding vector (nullable, so a chunk row can exist without an embedding when only FTS is in selection), identity fields, path/package context, and label membership. Schema in `src/engine/schema.rs`; the typed Rust row struct (`ChunkRow`) in `src/engine/storage/rows.rs`.
- `label_metadata`: one row per label. Carries the catalog, the bare label name, the qualified `label_id`, the source kind, and per-retrieval-method state: a nullable source column (commit OID, working-directory sentinel, or NULL when the method is out of the label's retrieval selection) and a completion boolean per method. The set of in-selection methods for a label is derived from which method-source columns are non-NULL.

FTS state lives outside the LanceDB tables, in a per-label Tantivy index directory at `<database-dir>/fts/<catalog>/<label>/`. Each label gets its own Tantivy index because BM25 statistics are computed per-corpus at index time; sharing one Tantivy index across labels would mix statistics from chunks that don't belong to the queried label. The per-label structure makes label-scoped FTS queries correct by construction.

The chunk row carries enough path/package/breadcrumb context to be displayed without a working tree or Git checkout. Search results stand alone.

A label-scoped vector search is the storage-layer filter `catalog == X AND active_label_ids CONTAINS "<catalog>:<label>"` combined with vector similarity on the embedding column. A label-scoped FTS search runs against the per-label Tantivy index directly (no `active_label_ids` filter needed because the index only contains that label's chunks). View queries use the `active_label_ids` filter without any retrieval-method work.

Designed scale: around 200K files and 600K chunks per catalog. Full crawls take 15-30 minutes; embedding throughput is around 12ms per chunk on a typical multi-core machine with the auto-tuned ONNX session pool.

### Chunk identity

```
file_id  = hash(embedder_id + chunker_id + catalog + blob_id + relative_path)
row_id = "{file_id}:{chunk_ordinal}"
```

The `file_id` is a 16-char hex string identifying a semantic version of a file. The `row_id` is the row's primary key. `chunk_ordinal` is 1-indexed.

The fact that `relative_path` is part of `file_id` matters: identical content at different paths produces different `file_id` values. Path renames create new chunks. This is intentional. Breadcrumb context is part of what gets indexed, so different paths mean different indexed artifacts.

The fact that `catalog` is part of `file_id` matters too: identical content at the same path in two different catalogs produces distinct `file_id` values, and therefore distinct rows. Catalogs are sovereign units; cross-catalog content sharing is not a feature, and the writer-lock layer relies on this isolation to permit parallel writers against different catalogs.

The `embedder_id` and `chunker_id` constants live in `src/engine/util.rs`. Bumping either invalidates reuse. Change them when chunking or embedding behavior changes in a way that should force re-indexing.

### Sentinel-based incremental crawl

Chunk 1 of each file is the sentinel: the row with `chunk_ordinal = 1` is also the only row with `file_complete = true` once the file finishes indexing. The crawl checks for the sentinel row by `row_id` lookup; if the file qualifies for the fast path, the file is skipped, and only `active_label_ids` is updated to add the current label. This is what makes re-crawling cheap.

This file-enumeration fast path serves the vector phase. Under nullable-vector chunks, the qualification predicate is "sentinel exists, `file_complete = true`, and the sentinel row's `vector` column is non-NULL"; the last clause is necessary because an FTS-only crawl can produce complete sentinels with NULL vectors, which a later vector crawl must not skip. The per-file invariant that all chunks of a file have the same vector-presence state when the sentinel flips complete is a known transient gap that will be resolved by structural separation in a future release; see [crawl.md](./crawl.md) for details.

The FTS phase does not enumerate files. It is a batch reconciliation, run once after the file-processing pass: read the label's chunks from LanceDB, derive the currently indexed set from Tantivy's term dictionary, apply additions and removals to the Tantivy index, commit.

## Crawl pipeline

A crawl run, end to end:

1. **Label upsert**: Resolve `--commit` to a full SHA (or note that this is a `--working-dir` run); update the label's retrieval selection from `--retrieval` (set per-method `source` columns to the resolved commit, NULL out methods being dropped) and mark each in-selection method's `complete` flag false.
2. **Tree visitor**: Enumerate files from the commit tree or from the working-directory blob map (`git ls-files` + `git status`).
3. **Package indexing**: Build the package index, a map from directory paths to package names, by reading every Git-tracked `package.json` in the source.
4. **File processing**: For each file: compute `file_id`, check the sentinel, and either skip-with-label-add or read-chunk-embed-upsert.
5. **Label reassignment**: After all files succeed, scan chunks tagged with this label, drop the label from any whose `file_id` wasn't touched, and delete chunks whose `active_label_ids` becomes empty.
6. **FTS phase**: If `fts` is in the new retrieval selection, batch-reconcile the per-label Tantivy index against the label's current chunks. Derive the currently indexed set from Tantivy's term dictionary, apply additions and removals, commit once. Schema/tokenizer ID mismatch on existing FTS state triggers a per-label rebuild.
7. **Crawl finalization**: For each method whose phase completed successfully, mark its `complete` flag true.

Step 5 only runs after a fully successful crawl. An interrupted crawl leaves stale chunks in the label, which the next successful crawl cleans up. Step 6 only runs if step 5 succeeded; step 7 finalizes whatever subset of methods reached completion. See [crawl.md](./crawl.md) for the working-directory identity model, the package-index implementation, and per-file vector-presence invariant enforcement during FTS-only reprocess. The named steps above are the same vocabulary `crawl.md` uses for its detail sections.

Concurrent operations against the database (multiple writers, readers running during a crawl) are coordinated by a writer-lock layer; the lock taxonomy and reader semantics are in [concurrency.md](./concurrency.md).

## Chunker dispatch

`src/engine/chunker.rs` is the dispatcher. It looks up the chunking strategy for a file extension in the loaded crawl config and routes to one of:

- `typescript` → AST-based partitioning via tree-sitter, using the "two worlds model" that separates size-driven splitting from AST structure. Lives in `src/engine/partitioner/`.
- `markdown` → heading-based splitting. Lives in `src/engine/markdown_partitioner.rs`.
- `lineBased` → simple line-window splitting for generic text formats.

Target chunk size is 6000 characters, well under the 8192-token limit of jina-embeddings-v2-base-code. See [chunker.md](./chunker.md) for the partitioning algorithm and quality markers.

## Search

`monodex search` reads the label's retrieval selection and dispatches to the in-selection methods. With one method active, it queries that method directly. With two or more active methods sharing a source, it queries each in sequence and fuses the ranked lists by reciprocal rank fusion (RRF). The decision-rule details (active-subset preprocessing, sources-disagree handling, empty-selection errors) live in [search.md](./search.md).

Each search result carries a single-letter provenance marker indicating which retrieval method(s) contributed: `[v]`, `[f]`, or `[f+v]`. The decision-rule logic is a pure function in `src/engine/search_decision.rs`, separable from backend dispatch and unit-testable without LanceDB or Tantivy. RRF lives in `src/engine/fusion.rs`. The renderer in `src/app/search.rs` takes a single `&mut dyn Write` so search-time output (preamble, warnings, results, sentinels) is testable from byte-buffer assertions.

See [search.md](./search.md) for the full decision rules, RRF mechanics, the candidate-window rule, the tokenizer, output format, the end-of-results sentinel, hybrid backend-failure semantics, and the `debug-fts` diagnostic command.

## Identifiers

Catalog and label names follow strict syntax rules: catalogs are kebab-case, labels are Git-like. Reserved characters (`:`, `@`, `+`, `#`) are forbidden in both. Validation lives in `src/engine/identifier.rs`. The full grammar, including planned typed-label and cross-catalog reference forms, is in [label_ids.md](./label_ids.md).

Four versioning constants live in `src/engine/util.rs` and govern when on-disk state must be rebuilt. `EMBEDDER_ID` and `CHUNKER_ID` participate in `file_id`; bumping either invalidates chunk reuse across crawls. `FTS_SCHEMA_ID` and `FTS_TOKENIZER_ID` do not participate in `file_id`; bumping either invalidates FTS state but leaves vector state untouched, so a tokenizer tweak does not force re-embedding. Treat all four as load-bearing: changes to them invalidate cached state and force expensive rebuild work.

## Filesystem footprint

Monodex reads and writes files in three places: the user's config folder (`~/.monodex/`), the repository being indexed (the repo-local crawl config and `package.json` files), and editor-consumed schema files that ship with the project. The full inventory, with notes on which files are user-editable and which are tool-managed, lives in [monodex_files.md](./monodex_files.md).

## Source tree

Module-organization rules — file size targets, where new code goes, banned patterns — are in the [code organization policy](../code_organization_policy.md). What follows is a one-or-two-line description of every non-test source file. Pure export-only `mod.rs` files are omitted; `mod.rs` files with substantive implementation are listed. Section headings are repo-relative directory paths, so `cli.rs` under `### src/app/` lives at `src/app/cli.rs`.

### src/

- `lib.rs`: Crate root; declares `app`, `engine`, and `paths` modules.
- `main.rs`: Binary entry point; parses CLI args and dispatches to command handlers.
- `paths.rs`: Resolves filesystem paths for tool state (config, context, crawl config) under the config folder.

### src/app/

Application-layer code, CLI-specific. Not reusable as a library.

- `cli.rs`: Clap argument definitions; `Cli`, `Commands`, `CrawlSourceArgs`. Edit here for new flags or subcommand wiring.
- `config.rs`: Load and validate `monodex-config.json` (catalogs, database path, embedding-model knobs). Contains the `Config` and `DatabaseConfig` structs and the resolver that picks the database path.
- `context.rs`: Persist and resolve the default catalog/label set by `monodex use`. Owns the `DefaultContext` struct and read/write to `<config-folder>/monodex-state.json`.
- `search.rs`: Search-output renderer. Takes a `&mut dyn Write` and emits preamble, warnings, results, debug continuations, and end-of-results sentinels in a fixed order. The single-writer routing is what makes search-time output testable from byte buffers; see [search.md](../design/search.md) for the output-ordering rule.
- `util.rs`: Formatting and display helpers: timestamps, durations, byte sizes, terminal sanitization for search output, chunk-selector parsing, and `format_source_pointer` for warning remediation strings.

### src/app/commands/

One file per CLI subcommand handler. Most are thin: parse args, call into the engine, format output.

- `audit_chunks.rs`: `audit-chunks`: sample TypeScript files from a directory and report aggregate chunk-quality scores. AST-only mode.
- `crawl.rs`: `crawl`: enumerate files (commit tree or working dir), drive the embed/upload pipeline, run label reassignment after success.
- `debug_fts.rs`: `debug-fts`: print tokens for a chunk and optionally explain query ranking. Diagnostic for FTS tokenization issues.
- `dump_chunks.rs`: `dump-chunks`: visualize partitioner output for a single file. Supports debug, visualize, and with-fallback modes.
- `init_db/`: `init-db`: create or validate a database directory, write the LanceDB tables, create the empty `fts/` directory, write `monodex-meta.json`, and handle `--delete-everything`.
- `purge.rs`: `purge`: delete chunks, label metadata, and FTS state for a catalog, or truncate/reinitialize database-scoped state for `--all`. Operates at catalog or database scope only; there is no per-label purge.
- `search.rs`: `search`: resolve label context, evaluate retrieval-method decision rules, collect vector/FTS hits, fuse via RRF when multiple methods are active, hydrate chunks, and pass a render model to `app/search.rs`. See [search.md](../design/search.md) for decision rules and output format.
- `use_cmd.rs`: `use`: set or display default catalog/label context. Named `use_cmd` because `use` is a Rust keyword.
- `view.rs`: `view`: retrieve chunks by `file_id` with selector syntax (`:N`, `:N-M`, `:N-end`, or absent for the whole file). Reconstructs files from chunks.

### src/app/crawl/

Crawl-pipeline orchestration shared between command handlers.

- `phases.rs`: Per-phase functions corresponding to the named steps of the crawl pipeline (label upsert, file classification, chunk-new-files, label cleanup, FTS phase, finalization, summary). The handlers in `commands/crawl.rs` call into these in order.
- `pipeline.rs`: Coordinate parallel embedding and LanceDB writes via crossbeam channels and rayon. Also owns the FTS-only NULL-vector upsert path used when vector is not in the current crawl's selection. Tracks per-chunk embedding failures, ETA/progress output, and memory-warning checks.
- `types.rs`: Crawl-phase state shared across command handlers: `PhaseResults`, `CrawlSourceMetadata`, and the `CrawlFailures` tracker. Embedding failures are tracked per-chunk; structural errors abort immediately.
- `warning.rs`: Render in-flight crawl warnings to stdout/stderr. Distinct from `engine/warning.rs`, which defines the warning types.

### src/engine/

Reusable indexing engine. Does not depend on `src/app/`.

- `breadcrumb.rs`: Percent-encode reserved characters (`:`, `@`, `=`, `+`, `#`, `%`, whitespace) in breadcrumb path components. Slugify markdown headings GitHub-style.
- `chunker.rs`: Strategy dispatcher: pick a chunking strategy by file extension and produce `Chunk` records. Computes `file_id` and `row_id` for each chunk.
- `crawl_config.rs`: Load and compile `monodex-crawl.json` (file types, exclude/keep patterns) with `globset`. Implements the `should_crawl()` and `get_strategy()` evaluation rules. Holds the embedded default config and the `ChunkingStrategy` enum.
- `fusion.rs`: Reciprocal rank fusion (RRF) for hybrid retrieval. Pure algorithm: takes per-method ranked lists of `MethodHit`s and produces fused `FusedHit`s with per-method `RankedContribution` provenance. No I/O, no storage coupling. See [search.md](./search.md) for the algorithm and tiebreak rule.
- `identifier.rs`: Validate catalog and label syntax. Owns the `LabelId` type, qualified-form parsing/composition, and identifier error reporting.
- `markdown_partitioner.rs`: Custom markdown parser that splits at headings, fenced code blocks, block quotes, and paragraphs. Generates breadcrumbs from heading hierarchy.
- `package_lookup.rs`: Filesystem-only fallback that walks up to find the nearest `package.json` and extracts its `name`. Used only by `dump-chunks`; the main crawl path resolves packages from the package index.
- `parallel_embedder.rs`: Pool of ONNX sessions for parallel embedding generation. Each session uses limited intra-op threads; pool size and threads are auto-tuned from RAM and core count via `system_info`.
- `retrieval.rs`: `RetrievalMethod` enum (`Fts`, `Vector` — alphabetical so derived `Ord` is alphabetical) and the `format_selection` helper used by the search and crawl preambles. The CLI's `--retrieval` flag and the `label_metadata` table's per-method columns both reference this type.
- `schema.rs`: Arrow schema definitions for the `chunks` and `label_metadata` LanceDB tables. Holds `MONODEX_SCHEMA_VERSION`, which must be bumped on any change to column shape; see [monodex_files.md](./monodex_files.md) for the rationale.
- `search_decision.rs`: Pure function `decide(metadata, requested) -> Decision`. Computes the active subset, applies the decision table, returns a structured `Decision` outcome (`SingleMethod`, `Hybrid`, `Error`) with structured `DecisionWarning`s. No I/O, no backend dispatch; unit-testable in isolation. The orchestrator translates `DecisionWarning`s into pre-formatted `SearchWarning`s before passing them to the renderer.
- `system_info.rs`: Detect total RAM, cgroup limits, CPU cores. Implements the `"auto"` heuristic for embedding-model `modelInstances` and `threadsPerInstance`. Cgroup-aware so containerized installs warn correctly.
- `util.rs`: Hash utilities: `compute_file_id` (xxhash of embedder/chunker/catalog/blob/path), `compute_row_id`, `compute_hash`. Holds the four versioning constants: `EMBEDDER_ID` and `CHUNKER_ID` (participate in `file_id`; bumping forces re-vectorization), `FTS_SCHEMA_ID` and `FTS_TOKENIZER_ID` (do not participate in `file_id`; bumping invalidates only FTS state).
- `warning.rs`: `CrawlWarning` enum for in-flight crawl events and `DecisionWarning` enum for search-decision events translated to `SearchWarning` by the app layer.
- `working_dir_sentinel.rs`: Generate per-crawl-unique sentinel strings for working-directory crawls.

### src/engine/fts/

Tantivy-based full-text search. Per-label index directories under `<database-dir>/fts/<catalog>/<label>/`. See [concurrency.md](./concurrency.md) for the writer contract and [monodex_files.md](./monodex_files.md) for the on-disk layout.

Keep the direct `tantivy` dependency aligned with the version resolved through LanceDB. After dependency changes, `cargo tree -i tantivy` should show a single Tantivy version; two side-by-side versions in the dep graph mean `tantivy::Index` from our crate and from LanceDB's are different types, with real binary-size cost.

- `error.rs`: Helpers for typed discrimination of Tantivy NotFound-style errors. Used by FTS read paths to normalize directory disappearance to absent-index outcomes (`open_existing` returns `FtsOpenExistingOutcome::NoIndex`, `fts_search` returns `FtsSearchOutcome::NoIndex`) instead of propagating raw IO errors.
- `index.rs`: Open and create per-label Tantivy indexes. Owns the `FtsIndex` handle and the heap-budget constant. Write paths use `open_or_create`; read paths use `open_existing` so a missing FTS directory has no mkdir side effect.
- `indexing.rs`: `index_chunks_for_fts` and `FtsIndexingStats`. Reads the label's chunks from LanceDB, derives the currently indexed set from Tantivy's term dictionary, applies additions and removals, commits once, writes the manifest. See [crawl.md](./crawl.md) for the indexing flow.
- `manifest.rs`: Per-label FTS compatibility metadata at `<database-dir>/fts/<catalog>/<label>/manifest.json`. Stores `FTS_SCHEMA_ID` and `FTS_TOKENIZER_ID` for stale-index detection after upgrade. Read result is the typed `ManifestRead` enum (`Missing`, `Present`, `IdMismatch`, `Unreadable`); the four cases dispatch differently.
- `schema.rs`: Tantivy schema for the FTS index (`row_id` as `STRING | STORED` for hit hydration; `text` as `TEXT` not stored). Distinct from `engine/schema.rs` (the LanceDB Arrow schema for the chunks/labels tables).
- `search.rs`: `fts_search` and the `FtsHit` / `FtsSearchOutcome` result types (`Found`, `NoIndex`, `Stale`, `ParseError`). `Stale` carries an `FtsStaleReason` indicating why the index cannot be queried safely; the app layer emits a warning and skips Tantivy. Builds a Tantivy query parser bound to the monodex tokenizer.
- `tokenizer.rs`: Custom tokenizer for source code. Splits on case transitions, underscores, dots, digit boundaries, ASCII whitespace and punctuation; keeps both the original token and the splits; the upper-to-lower transition keeps the last uppercase letter with the following word (`HTTPServer` → `httpserver`, `http`, `server`). Jieba word-segmentation for CJK runs, loaded once per process via `OnceLock`.

### src/engine/git_ops/

Git-aware enumeration and blob reading. The `BlobSource` trait abstracts over commit and working-directory crawl sources so the crawl pipeline can stay free of mode-branching.

- `mod.rs`: Public `BlobSource` trait, `FileEntry`, `PackageIndex`, the two `BlobSource` implementations, and `package.json` name extraction.
- `commit.rs`: gix-based commit-tree enumeration, blob reading, and per-commit package-index construction.
- `working_dir.rs`: Subprocess-based working-tree reading. Shells out to `git ls-files` / `git status` / `git hash-object` (Git ≥ 2.35.0) so blob IDs match commit-mode IDs after `.gitattributes` and clean filters apply.

### src/engine/partitioner/

TypeScript/TSX AST-based chunking. See [chunker.md](./chunker.md) for the algorithm.

- `debug.rs`: Debug logging hooks for split decisions; `PartitionDebug` flag struct controlling verbose output.
- `node_analysis.rs`: AST node helpers: meaningful-children enumeration, symbol-name extraction, line-span computation, JSDoc/TSDoc collection.
- `partition.rs`: Top-level entry point: parse with tree-sitter, drive the recursive split search, finalize chunks with metadata.
- `scoring.rs`: Quality scoring (0-100%) and the `ChunkQualityReport` consumed by `audit-chunks`. Penalizes tiny chunks and oversized chunks.
- `split_search.rs`: Recursive search for split points at AST boundaries; descent into nested scopes when no shallow split fits the budget.
- `types.rs`: `PartitionedChunk`, `PartitionConfig`, `SplitResult`, the `TARGET_CHARS` and `SMALL_CHUNK_CHARS` constants.

### src/engine/storage/

LanceDB storage layer. Typed operations on the two tables.

- `database.rs`: Open a database directory, validate `monodex-meta.json` schema version, expose table handles. Single source of database-open errors.
- `labels.rs`: Read, upsert, and delete `label_metadata` rows. Handles the per-method retrieval-selection and completion lifecycle.
- `locks.rs`: OS-level file-locking primitives (database, catalog, commit mutex) backing the writer-lock taxonomy. Watchdog thread for long-acquisition progress reporting. See [concurrency.md](./concurrency.md).
- `predicate.rs`: LanceDB SQL predicate builders (`eq_str`, `in_quoted_strs`, etc.) used across the storage layer. Callers must pre-validate inputs: catalog names by `validate_catalog`, label IDs by `LabelId::parse`.
- `rows.rs`: Plain-Rust `ChunkRow` and `LabelMetadataRow` types with conversion to/from Arrow `RecordBatch`. The rest of the engine deals only in these row types, never in raw Arrow.

### src/engine/storage/chunks/

Sub-module for chunk-table operations, separated from the rest of `storage/` because it's larger than the others and has its own tests file.

- `mod.rs`: Upsert-with-vectors, upsert-without-vectors, vector search, label-membership add/remove, per-file chunk lookup, sentinel checks, row-id hydration, deletion, truncation; see [crawl.md](./crawl.md) for the vector-presence invariant.

## All markdown files in this repo

Every `.md` file in the repo, with a one-line description. Add an entry when adding a doc, remove when deleting. Top-level files (README, CHANGELOG, SECURITY, LICENSE) are listed alongside `docs/` files; new top-level docs are rare.

- [`README.md`](../../README.md): User-facing landing page: what Monodex is, install, configuration, CLI usage. Also serves as the crates.io page, so kept short.
- [`CHANGELOG.md`](../../CHANGELOG.md): Release history. Contains an HTML comment at the top with the version-bump procedure (semver rules, when to add the `## Unreleased` heading, which `###` subheadings are allowed); read it before publishing.
- [`SECURITY.md`](../../SECURITY.md): Microsoft boilerplate pointing at `aka.ms/SECURITY.md`. No project-specific content.
- [`docs/code_organization_policy.md`](../code_organization_policy.md): Required reading for contributors. File size targets, where new code goes, banned patterns, test placement, naming.
- [`docs/smoke_test.md`](../smoke_test.md): Minimal end-to-end verification procedure: configure Sparo as a catalog, purge, crawl, search, view. Run after any change to confirm the build actually works.
- [`docs/backlog.md`](../backlog.md): Maintainer scratch pad for what might come next; items grouped by priority bucket. For official feature requests, the README points at GitHub issues, Zulip, and the Rush Hour video call.
- [`docs/design/architecture.md`](./architecture.md): This file.
- [`docs/design/label_ids.md`](./label_ids.md): Identifier and reference syntax: catalogs, labels, breadcrumbs, cross-catalog references, planned typed-label grammar, path encoding rules at locator boundaries.
- [`docs/design/crawl.md`](./crawl.md): Crawl pipeline in detail: package index implementation, working-directory identity model, label reassignment, FTS phase reconciliation, per-file vector-presence invariant.
- [`docs/design/search.md`](./search.md): Search-side behavior: retrieval methods, decision rules, RRF fusion, tokenizer, output format, debug-fts.
- [`docs/design/chunker.md`](./chunker.md): Chunking algorithms: TypeScript AST partitioning (the "two worlds model"), markdown splitting, quality markers and scoring.
- [`docs/design/concurrency.md`](./concurrency.md): Writer lock taxonomy (database, catalog, commit mutex), reader-lock-free contract, interaction with LanceDB MVCC and Tantivy's per-directory locks.
- [`docs/design/monodex_files.md`](./monodex_files.md): Inventory of files monodex reads or writes: config-folder state, repo-local config files monodex reads from the indexed repo, editor-consumed schemas, init templates.
- [`schemas/editing.md`](../../schemas/editing.md): Cross-reference back to the Rust structs that mirror these schemas, plus a policy reminder that these files are publicly published artifacts.
