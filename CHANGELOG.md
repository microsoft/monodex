# Change Log - Rush Monodex

<!--
CHANGELOG GUIDANCE:

- When starting new work after publishing, add an `## Unreleased` section
- `###` headings must be one of: Added, Changed, Fixed, Deprecated, Removed, Security
- CHANGELOG.md is for user-facing changes only (implementation details go in your Git commit description)
- Focus on user experience ("Fixed a problem where the crawler sometimes would report X") not implementation ("Added stricter validation in the f() function")
- Avoid jargon and complex sentences; assume your audience is a professional engineer with only superficial knowledge about Monodex

PUBLISHING PROCEDURE:

1. Choose an appropriate version number based on semantic versioning:
   - MAJOR: Breaking changes that require user action
   - MINOR: New features, backwards compatible
   - PATCH: Bug fixes, backwards compatible
2. Update `version` in Cargo.toml
3. Get the current UTC date with: `date -u +"%Y-%m-%d"`
4. Rename "## Unreleased" to "## X.Y.Z" and add the date from step 3
5. After publishing, the next PR author will add a new "## Unreleased" section
-->

## Unreleased

### Added

- **Full-text search.** Monodex now indexes chunk text into a Tantivy-backed full-text index alongside the existing vector index. The two are exposed as the `fts` and `vector` retrieval methods. `monodex search` queries both by default and fuses the results using reciprocal rank fusion (RRF). Each result line carries a `[f]`, `[v]`, or `[f+v]` marker indicating which method(s) ranked it.

- **`--retrieval` flag on `search` and `crawl`.** Repeatable. On `search`, restricts the query to the named methods (e.g. `--retrieval fts` for lexical-only). On `crawl`, restricts which methods are built for the label; subsequent crawls can narrow or widen the per-label retrieval selection. Without the flag, both commands act on every method available for the label.

- **`debug-fts` command.** Dumps the tokens the FTS tokenizer produces for a given chunk, and (with `--query`) explains how a query parses and scores against that chunk. Most "FTS can't find a thing I know is there" cases turn out to be tokenization rather than ranking.

- **`init-db --delete-everything`.** Deletes and recreates the database in one step. Intended for recovering from a schema-mismatch error after upgrading Monodex. Destructive by design; all catalogs must be re-crawled afterward.

- **Concurrent monodex invocations now coordinate via file locks.** Two writes against the same catalog wait for each other; writes against different catalogs run in parallel. See `docs/design/concurrency.md`.

### Changed

- **Breaking: existing databases are not readable.** The on-disk schema has changed and the chunk identity now incorporates the catalog name, so the same content at the same path in two different catalogs no longer produces colliding rows. Older databases are rejected with a schema-mismatch error on first use. The remedy is `monodex init-db --delete-everything` followed by re-crawling each catalog.

- **Breaking: `--incremental-warnings` removed.** The crawl-time `--incremental-warnings` flag and the `<database>/warnings-<catalog>.json` state file it controlled have been removed. Each crawl now emits whatever chunker-fallback warnings it produces this run, with no cross-run persistence. Pre-existing `warnings-<catalog>.json` files are inert on disk and can be removed by `monodex init-db --delete-everything`.

- **`monodex search` output format.** The preamble now names the methods being queried (`Searching: fts, vector`). Each result header gains a `[f]`/`[v]`/`[f+v]` marker, shown unconditionally including under single-method search. Empty result sets now print `No results.` rather than nothing.

- **`monodex search` warnings now print on stdout** alongside results, rather than stderr. This keeps warnings ordered relative to the results they describe and makes piped output deterministic.

### Fixed

- **Crashes on tree-sitter-unhandleable TypeScript files.** A pathological `.ts` or `.tsx` file that tree-sitter could not parse used to abort the whole crawl with a panic. Such files are now reported as a warning and skipped.

- **Git-tracked files under hidden directories are no longer dropped.** Working-directory crawls previously skipped files under `.github/`, `.vscode/`, `.config/`, and similar paths even when Git tracked them. Those files are now indexed.

- A handful of crawl-pipeline error-handling and cleanup-gate bugs that could leave a label in an inconsistent state if a phase failed partway through.
- Stale-hydration warnings in search results now appear in the right place relative to the result that triggered them.

## 0.5.1 (2026-04-30)

### Changed

- Improved the project documentation.

## 0.5.0 (2026-04-26)

### Changed

- **Breaking: switched vector storage from Qdrant to LanceDB**: Monodex now uses LanceDB as an embedded, in-process vector database instead of a separate Qdrant server.
  - No external service to install, run, or configure. The database is a directory on disk.
  - The on-disk format is incompatible with prior versions. Existing users must delete their old Qdrant collection and re-crawl.
  - Removes the `qdrant` section (`url`, `collection`, `maxUploadBytes`) from `config.json`. An old config containing a `qdrant` section will be rejected.

- **Breaking: new `init-db` command, required before first crawl**: Run `monodex init-db` once to create the database. This replaces the old step of provisioning a Qdrant collection.

- **Database location is configurable**: Defaults to `~/.monodex/default-db/`. Set `database.path` in `config.json` to override. The path must be absolute. Tilde expansion (`~`), environment variables (`$VAR`), and relative paths are not supported.

- **Search output now reports distance instead of score**: Lower numbers are better. Output format is `dist=N.NNN`.

- **Tool home moved to `~/.monodex/`**: All monodex state files now live under `~/.monodex/` instead of `~/.config/monodex/`. This provides a consistent location across all platforms (Linux, macOS, Windows). Set the `MONODEX_HOME` environment variable to override the default location. On first run, if old config files are found at the previous location, monodex prints a warning suggesting migration.

### Added

- **Chunking warning persistence**: Files that require fallback line-based splitting (when AST chunking fails) are now tracked and persisted to `<database>/warnings-<catalog>.json`. The crawl command reports these files with their relative paths and shows a warning count during progress.

- **Documentation updates**: README now shows Configuration before First-Time Setup, correct Rust version (1.93+), and updated debug flag description. DESIGN.md has a new vocabulary orientation, corrected schema documentation, and a current-state error-handling section.

### Fixed

- **Config files now support JSONC (JSON with comments)**: Config files can include `//` line comments, per Rush Stack convention.

- **Example config catalog names**: The example `config.json` now uses valid kebab-case catalog names (`my-monorepo`, `another-monorepo`) instead of invalid underscored names that would fail validation.

## 0.4.0 (2026-04-18)

### Changed

- **Breaking: Stricter identifier validation**: Catalog and label names must now follow strict syntax rules. Catalogs use kebab-case (e.g., `my-repo`). Labels use Git-like identifiers (e.g., `main`, `feature/x`, `release/v1.2.3`). Labels may contain `=` as a permitted separator character (e.g., `branch=main`, `commit=abc123`). These are opaque identifiers today; the `kind=payload` convention is reserved for a future typed-label grammar and is not currently interpreted by Monodex. The Qdrant payload field `label_name` has been renamed to `label`. Existing collections with non-conforming identifiers must be recreated. See [#25](https://github.com/microsoft/monodex/issues/25) for the full syntax specification.

### Added

- **Deterministic embedding memory control**: The `embeddingModel` config section now supports `"auto"` values for `modelInstances` and `threadsPerInstance`, which are computed deterministically from system properties (total RAM, CPU cores, and Linux cgroup limits). This prevents OOM failures on memory-constrained machines while maximizing parallelism on capable hardware.
- **Startup memory warning**: Before embedding begins, monodex prints available RAM and estimated usage (based on resolved config). If the estimate exceeds available RAM, a warning suggests adjusting config values.

### Fixed

- **Cgroup-aware memory warning**: Fixed a bug where memory warnings in containerized environments would compare against host-level available RAM instead of cgroup-limited available RAM. This caused the warning to never fire even when the container was at risk of OOM.
- **Config field mapping**: The `embeddingModel` field in `config.json` is now correctly mapped to the Rust struct via `#[serde(rename = "embeddingModel")]`. Previously, this field was silently ignored due to snake_case/camelCase mismatch.

## 0.3.0 (2026-04-16)

### Changed

- **crawl command now requires explicit source**: Must specify `--label` AND either `--working-dir` OR `--commit`
  - Previously: `monodex crawl --catalog myrepo --label main` (defaulted to HEAD)
  - Now: `monodex crawl --catalog myrepo --label main --commit HEAD`
  - This prevents accidental overwrites of labels and makes crawl intent explicit
  - CLI now shows proper usage: `monodex crawl --label <LABEL> <--commit <COMMIT>|--working-dir>`

### Fixed

- **Working directory blob IDs now match Git blob IDs**: `--working-dir` mode now uses Git CLI batch commands (`git ls-files`, `git status`, `git hash-object`) to compute blob IDs that respect `.gitattributes`, clean filters, and other repo-specific settings. This ensures identical content produces the same `file_id` in both `--commit` and `--working-dir` modes, enabling proper incremental skipping.

## 0.2.0 (2026-04-14)

### Updates

- Add `--debug` CLI flag for verbose network request logging
- Add `maxUploadBytes` config setting for Qdrant payload limit (default 30MB)
- Implement rewind upload algorithm for large batch splitting to avoid Qdrant payload limits
- Improve upload error handling: preserve chunks on failure, report clear error messages

## 0.1.0 (2026-04-10)

### Minor changes

- Add JSON schemas for `config.json`, `monodex-crawl.json`, and `context.json` for IDE autocomplete
- Add user-configurable crawl settings via `monodex-crawl.json` (file types, exclusions, keep patterns)
- Add `--working-dir` flag to index uncommitted changes from the filesystem
- Add label-based indexing: maintain multiple queryable snapshots (branches, commits) within a catalog
- Add `use` command to set default catalog/label context for subsequent commands
- Add Git-based crawling: reads from Git objects, not working tree (deterministic, reproducible)
- Switch to jina-embeddings-v2-base-code model (768 dimensions, 8192 token limit)
- Increase chunk target size from 1800 to 6000 characters

### Patches

- Fix crawl error handling: track and report upload failures, label assignment failures
- Fix `source_uri` path separator on Windows
- Fix catalog validation in `use` command
- Fix race condition in crawl checkpointing
- Increase HTTP timeout for wait=true operations

## 0.0.1 (2026-04-08)

- Initial release
