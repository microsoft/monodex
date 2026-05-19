# Crawl pipeline

This document expands on the crawl pipeline introduced in [architecture.md](./architecture.md). The same named steps are used as section headings here, with operational detail per step. After the steps, two longer sections cover the package index and working-directory mode in depth, followed by a section on partial-crawl semantics.

The relevant source files are `src/app/commands/crawl.rs` (top-level command handler), `src/app/crawl/phases.rs` (the per-phase functions corresponding to the steps below), `src/app/crawl/pipeline.rs` (parallel embedding and storage writes inside the file-processing step), `src/app/crawl/preamble.rs` (shared crawl-preamble preparation for both entry points, plus startup-time retrieval-selection messaging), and `src/engine/git_ops/` (Git tree enumeration, blob reading, working-directory walk).

## Step 1: Label upsert

Resolve `--commit` to a full 40-character SHA using `gix` (or, for `--working-dir`, generate a per-crawl-unique sentinel string and set `source_kind = "working-directory"`). Read the previous retrieval selection from the existing `label_metadata` row, if any, and compute the new selection from `--retrieval` (or "all methods" if absent). For each method in the new selection, set its source column to the resolved commit (or working-dir sentinel) and its completion flag to false. For each method previously in the selection but not in the new selection, set its source column to NULL.

Marking each in-selection method's completion flag false before any chunk work begins is what lets a later interrupted-crawl recovery distinguish "this method's phase was being written and the writer didn't finish" from "this method is intentionally in this state." The retrieval-selection update happens in the same upsert so that an interrupted crawl leaves the user-visible scope change visible: typing `--retrieval fts` drops vector from the selection at upsert time, even if the FTS phase then fails.

Concurrent writers against the same catalog (two `monodex crawl` invocations, or a `monodex crawl` running while a `monodex purge --catalog` of that catalog runs) are serialized by the writer-lock layer; concurrent writers against different catalogs run in parallel. Concurrent reads (`search`, `view`) during a crawl are lock-free and observe committed per-storage state. The full lock taxonomy and reader semantics are in [concurrency.md](./concurrency.md). The database location must be on a local filesystem; network filesystems and synced cloud folders are not supported.

For commit mode, the resolution step rejects ambiguous refs and unresolvable refs with a clear error rather than silently picking a default. For working-directory mode, no resolution is needed; the source_kind alone signals the contents are mutable.

## Step 2: Tree visitor

Two enumeration paths, depending on the source:

**Commit mode:** Use `gix` to walk the commit tree recursively. The walker emits a sequence of `(blob_id, relative_path)` pairs for every blob in the tree. Non-blob entries (submodules, symlinks under some repo configurations) are filtered out. Monodex doesn't follow submodule pointers and doesn't materialize symlink targets.

**Working-directory mode:** Build a map of Git's working-tree view using `git ls-files` (for tracked files) and `git status -u` (for untracked non-ignored files). The map contains `(relative_path, blob_id)` pairs for all files in Git's working-tree view: tracked files at their current working-tree contents (including local modifications), plus untracked non-ignored files. Deleted files are removed from the view. Files under hidden directories (`.github/`, `.vscode/`, etc.) are included because the enumeration is driven by the Git-derived blob map, not a filesystem walk. Blob IDs are computed by shelling out to `git hash-object`. `.gitignore`-excluded files are not included even when present on disk. The minimum required Git version is 2.35.0 (for `git ls-files --format`).

The blob-ID compatibility between the two modes is load-bearing: it's what makes a `--working-dir` re-crawl over an unchanged repo skip every file via the sentinel check, with no re-embedding. Earlier versions used a SHA-256 content hash for working-dir mode, which produced different `file_id` values from commit mode and broke incremental skipping. The current implementation uses `git ls-files`, `git status`, and `git hash-object --stdin-paths` so that `.gitattributes`, clean filters, and other repo-specific settings are respected and the resulting blob IDs match what `git` would compute on commit.

After enumeration, the file list is filtered through the loaded crawl config's `should_crawl()` predicate (see `src/engine/crawl_config.rs`), which combines file-type matching against `patternsToExclude` and `patternsToKeep`.

## Step 3: Package indexing

Build a `HashMap<directory_path, package_name>` covering every `package.json` in the source. This step does its own enumeration of the source: it does not consume the file list produced by step 2, because the package index needs only the `package.json` files, not the whole crawl-eligible file set.

For commit mode, the strategy is two batched Git operations: `git ls-tree -r -z <commit>` to find every `package.json`, then `git cat-file --batch` over a single long-lived process to read all the blobs. This avoids per-file fork overhead and keeps the build to one focused tree enumeration plus one stream of blob reads.

For working-directory mode, the package index is built by iterating the Git-aware blob map (the same working-tree view used by working-directory file enumeration; see Step 2) and reading each `package.json` from the filesystem. The blob map includes both tracked and untracked non-ignored files; `package.json` files under hidden directories are included.

For each `package.json`, the `"name"` field is parsed out and stored under the directory's repo-relative path as the key. Repo-root `package.json` is keyed by the empty string `""`.

Lookup happens later, during file processing: given a file at `libraries/lib1/src/Example.ts`, the index is queried for ancestor directories in this order:

1. `libraries/lib1/src`
2. `libraries/lib1`
3. `libraries`
4. `""`

The first match wins, reproducing the "nearest ancestor `package.json` governs the file" rule. The lookup helper is `PackageIndex::find_package_name` in `src/engine/git_ops/package_index.rs`.

## Step 4: File processing

For each enumerated file, the work splits into a sentinel-check fast path and a chunk-embed-upsert slow path. This file-enumeration fast path governs whether chunk-row work happens; FTS-side incremental work happens later, in the FTS phase, as a separate batch reconciliation against the per-label Tantivy index.

**Sentinel-check fast path:** Compute `file_id` from `(embedder_id, chunker_id, catalog, blob_id, relative_path)`. Look up the `row_id` of the sentinel chunk (`{file_id}:1`). The qualification predicate is:

- **Vector in selection:** sentinel row exists, `file_complete = true`, and the sentinel row's `vector` column is non-NULL.
- **Vector not in selection:** sentinel row exists, `file_complete = true`. (No vector check needed because no vector work would happen anyway.)

If the file qualifies, add the current `label_id` to `active_label_ids` on every chunk row sharing this `file_id`. No content read, no chunking, no embedding.

The vector-in-selection predicate's vector-non-NULL check relies on the per-file invariant that for any file with `file_complete = true`, either all chunks have non-NULL `vector` or none do. A freshly-written file satisfies this by construction: the slow path writes vectors for all chunks before flipping the sentinel, or writes all chunks with `vector = NULL` (first-time FTS-only crawl on a file with no prior peer-label vectors) before flipping the sentinel. A file re-touched by an FTS-only crawl on top of a peer label's vectors can be in a partial-vector state; this is a known transient gap that will be closed by structural separation in a future release (see the FTS-only crawl path section below). While the invariant holds, a read of the sentinel row's `vector` is sufficient proof for the whole file.

**Slow path:** Read the blob bytes (commit mode: from Git, via the cat-file batch process; working-dir mode: from the filesystem). Resolve the package name via the package index. Compute the breadcrumb prefix. Dispatch to the chunker via `src/engine/chunker.rs` (see [chunker.md](./chunker.md) for the algorithm) to produce chunks. If vector is in the current selection, embed each chunk via the parallel ONNX embedder pool (see `src/engine/parallel_embedder.rs`); otherwise leave each chunk row's `vector` column NULL. Upsert each resulting `ChunkRow` to the `chunks` table, with `active_label_ids` containing the current `label_id`. The sentinel chunk (ordinal 1) gets `file_complete = true` once all chunks for the file have been written.



The pipeline is parallel: a worker thread pool drives chunking and embedding via `crossbeam` channels and `rayon`, with a separate writer thread doing the LanceDB upserts. Per-chunk failures (tokenizer edge cases, model issues on specific content) are tracked in `CrawlFailures` and reported at the end; structural errors (disk full, dataset corruption) abort immediately.

## Step 5: Label reassignment

This step runs only after every file in step 4 succeeded. Partial crawls skip it entirely.

The crawl maintains a `HashSet<file_id>` of every file_id touched during step 4 (both fast-path label-adds and slow-path upserts). After step 4 completes, the cleanup pass runs:

1. Query all chunks where `active_label_ids` contains the current `label_id`.
2. For each chunk, extract its `file_id` field.
3. If `file_id` is not in the touched set, remove `label_id` from `active_label_ids`. If `active_label_ids` becomes empty, delete the chunk row.

The result: the label's membership now exactly reflects the current commit's tree. Chunks shared with other labels survive (only the current label was removed); chunks unique to the previous version of this label are deleted.

The "only after success" rule matters because the touched set is only complete on success. An interrupted crawl with a half-built touched set would incorrectly reassign chunks for files it hadn't gotten to yet, leaving the label's content corrupted. Skipping reassignment on failure is the safe behavior: stale chunks remain associated with the label until the next successful crawl, which produces a complete touched set and runs the cleanup correctly.

## Step 6: FTS phase

This step runs only if `fts` is in the new retrieval selection. It is a batch reconciliation against the per-label Tantivy index at `<database-dir>/fts/<catalog>/<label>/`, not a per-file fast path.

The phase reads the label's chunks from LanceDB (via `get_chunks_for_label`) and derives the currently indexed `row_id` set from Tantivy's term dictionary (using the alive-bitset filter, so tombstoned-but-not-yet-merged docs do not appear). The diff is computed as a set difference of `row_id`s: chunks present in LanceDB but not in Tantivy are added; chunks present in Tantivy but no longer in LanceDB are removed via `delete_term`. After all additions and deletions are queued, the phase calls `commit()` once. For commit-mode crawls, the phase then calls `wait_merging_threads()` to consolidate; for working-dir crawls, it skips this and accepts fragmentation as the cost of speed (full re-crawl will clean up).

After `commit()` succeeds, the manifest at `<database-dir>/fts/<catalog>/<label>/manifest.json` is written with the `FTS_SCHEMA_ID` and `FTS_TOKENIZER_ID` constants the index was built with. The manifest stores only compatibility metadata; it does not track row_ids.

**Schema and tokenizer ID mismatch.** The schema and tokenizer behavior are versioned by `FTS_SCHEMA_ID` and `FTS_TOKENIZER_ID` constants in `src/engine/identity.rs`. Mismatch is detected via the manifest's stored IDs, not by introspecting Tantivy's on-disk schema:

- If the manifest's IDs do not match the current constants: delete the per-label FTS directory and rebuild from scratch. The intent is to recover automatically from version bumps.
- If Tantivy fails to open with the manifest's IDs matching, or if the manifest is unreadable while Tantivy state exists: error out with a clear message. This is corruption and should reach a human, not be papered over by a silent rebuild.

These IDs do not participate in `row_id`. Chunk identity is a chunk-storage concept; FTS has its own invalidation surface, and a tokenizer tweak does not force re-embedding.

**FTS-only crawl path preserves vectors.** The FTS-only crawl path upserts chunks without modifying existing vector columns. This avoids clobbering vectors from peer labels that share the same blob (same `row_id` derived from `file_id`).

The per-file invariant (for any file with `file_complete = true`, either all chunks have non-NULL `vector` or none do) is what makes the vector-phase fast-path predicate sufficient (see Step 4). This invariant will be maintained by structural separation in a future release: a vector crawl will process all chunks of a file atomically, and an FTS-only crawl does not touch vectors. Until then, an interrupted vector-crawl-then-FTS-only-crawl sequence can leave a file in a partial-vector state (some chunks with vectors, some without). This is a known transient gap; the structural separation will close it by enforcing that a label's retrieval selection cannot mix vector and FTS-only modes on the same file.

**Tokenizer.** The tokenizer used during the FTS phase is the same one used at query time. Behavior spec is in [search.md](./search.md).

## Step 7: Crawl finalization

For each in-selection method whose phase completed successfully, mark its completion flag true. Update the label metadata row's timestamp.

This is the closing handshake. Once finalized, search and view operations against the label see a consistent view of its content.

A failed FTS phase still finalizes vector's completion flag if vector phase and label reassignment both succeeded. The error is propagated after finalize, so resume re-runs only the FTS phase rather than redoing vector work unnecessarily.

## Working-directory mode

A working-directory crawl indexes uncommitted changes from the filesystem rather than from Git objects. The label produced is mutable: re-crawling updates the indexed content based on the current filesystem state, which contrasts with commit-based labels (immutable for a given commit; re-crawling the same commit is idempotent).

Use cases:

- Indexing work-in-progress before committing.
- Comparing uncommitted changes with committed code by maintaining parallel labels (e.g., `--label main --commit HEAD` and `--label working --working-dir`).
- Agents that need to understand the current state of the codebase as the developer is editing it.

The identity model differs from commit mode in two fields, but `file_id` matches:

| Property                            | Commit-based         | Working directory                                |
| ----------------------------------- | -------------------- | ------------------------------------------------ |
| `blob_id`                           | Git blob SHA-1       | Git blob SHA-1 (computed on the fly)             |
| Per-method source on `label_metadata` | Resolved 40-char SHA | Per-crawl-unique sentinel string                 |
| `source_kind`                       | `"git-commit"`       | `"working-directory"`                            |

Two working-directory crawls of the same label always have unequal per-method source sentinels, even if the working tree is unchanged between them. The conservative-unequal answer is right: detecting working-tree equality cheaply is not feasible, and treating two working-dir crawls as comparing equal would let inconsistent state look consistent.

Because `blob_id` matches between modes, a file that's been committed without modification produces the same `file_id` whether crawled from `HEAD` or from the working tree. This is what enables an agent to maintain both a commit-based label and a working-dir label cheaply: only the actually-different files re-embed.

The blob-ID compatibility comes from delegating to the `git` CLI, which respects every detail of the repo's blob-construction rules (`.gitattributes`, `core.autocrlf`, clean filters, etc.). Reimplementing those rules in Rust would be a maintenance burden; the shell-out cost is small relative to embedding time.

## Partial-crawl semantics

Crawls can be interrupted (Ctrl+C, OOM, network blip during ONNX runtime download, disk full). The pipeline is designed so interruption leaves the database in a recoverable state.

What survives an interrupted crawl:

- Chunks already written to the database stay written. Their `active_label_ids` correctly include the current label. The work isn't lost.
- The label metadata row exists with at least one in-selection method's completion flag false. This is the signal that the label is in an unfinished state for that method.
- The sentinel rows for fully-processed files have `file_complete = true`, so a resumed crawl skips them via the fast path.

What does _not_ happen:

- Label reassignment (step 5) does not run, because the touched set is incomplete. Stale chunks from previous crawls of this label remain associated with the label.
- The label metadata is not finalized.

A subsequent successful crawl recovers the label cleanly: files already indexed are picked up by the sentinel fast path, files that hadn't been touched yet get processed, and step 5 then runs with a complete touched set, removing the stale chunks. The user never has to do anything special. Re-running the same crawl command is the recovery procedure.

A consequence of this design: orphaned chunks are possible. If a file was uploaded but its `active_label_ids` didn't get the label added before the crawl was interrupted, the chunk is in the database but tagged with no label. It's invisible to label-scoped search and view, but it occupies space. This is the case the planned offline GC command (see [backlog.md](../backlog.md)) is intended to address. Inline cleanup during crawl is not sufficient because there's no way to safely identify orphans without scanning the whole `chunks` table.

**Interrupted FTS phase.** Tantivy's durability boundary is `commit()`. Anything between commits (documents added but not yet committed) lives in process memory and is lost on crash; there is no on-disk partial-segment state. The FTS phase commits exactly once at its end, so an interrupted FTS phase reverts the on-disk Tantivy state to whatever it was before the phase started. The label metadata row, however, was already updated at upsert time: `fts_source` is set to the new commit OID and `fts_complete = false`. The next crawl sees this incomplete state, computes the diff against current chunks, redoes whatever FTS work didn't get committed, commits, and sets `fts_complete = true`. Resume re-does only FTS work, not vector work.
