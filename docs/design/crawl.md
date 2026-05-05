# Crawl pipeline

This document expands on the crawl pipeline introduced in [architecture.md](./architecture.md). The same six named steps are used as section headings here, with operational detail per step. After the steps, two longer sections cover the package index and working-directory mode in depth, followed by a section on partial-crawl semantics.

The relevant source files are `src/app/commands/crawl.rs` (top-level command handler), `src/app/crawl/pipeline.rs` (parallel embedding and storage writes), and `src/engine/git_ops/` (Git tree enumeration, blob reading, working-directory walk).

## Step 1: Label upsert

Resolve `--commit` to a full 40-character SHA using `gix` (or, for `--working-dir`, set `commit_oid = ""` and `source_kind = "working-directory"`). Upsert the label metadata row with `crawl_complete = false`.

Marking the label in-progress before any chunk work begins is what lets a later interrupted-crawl recovery distinguish "this label was being written and the writer didn't finish" from "this label is intentionally in this state."

Concurrent writers against the same catalog (two `monodex crawl` invocations, or a `monodex crawl` running while a `monodex purge --catalog` of that catalog runs) are serialized by the writer-lock layer; concurrent writers against different catalogs run in parallel. Concurrent reads (`search`, `view`) during a crawl are lock-free and observe committed per-storage state. The full lock taxonomy and reader semantics are in [concurrency.md](./concurrency.md). The database location must be on a local filesystem; network filesystems and synced cloud folders are not supported.

For commit mode, the resolution step rejects ambiguous refs and unresolvable refs with a clear error rather than silently picking a default. For working-directory mode, no resolution is needed; the source_kind alone signals the contents are mutable.

## Step 2: Tree visitor

Two enumeration paths, depending on the source:

**Commit mode:** Use `gix` to walk the commit tree recursively. The walker emits a sequence of `(blob_id, relative_path)` pairs for every blob in the tree. Non-blob entries (submodules, symlinks under some repo configurations) are filtered out. Monodex doesn't follow submodule pointers and doesn't materialize symlink targets.

**Working-directory mode:** Walk the filesystem from the repo root using `walkdir`, skipping hidden directories (except `.git`), `node_modules`, `target`, `dist`, `build`, `.cache`, and `temp`. For each surviving file, compute a Git-compatible blob ID by shelling out to the `git` CLI. The minimum required Git version is 2.35.0 (for `git ls-files --format`).

The blob-ID compatibility between the two modes is load-bearing: it's what makes a `--working-dir` re-crawl over an unchanged repo skip every file via the sentinel check, with no re-embedding. Earlier versions used a SHA-256 content hash for working-dir mode, which produced different `file_id` values from commit mode and broke incremental skipping. The current implementation uses `git ls-files`, `git status`, and `git hash-object --stdin-paths` so that `.gitattributes`, clean filters, and other repo-specific settings are respected and the resulting blob IDs match what `git` would compute on commit.

After enumeration, the file list is filtered through the loaded crawl config's `should_crawl()` predicate (see `src/engine/crawl_config.rs`), which combines file-type matching against `patternsToExclude` and `patternsToKeep`.

## Step 3: Package indexing

Build a `HashMap<directory_path, package_name>` covering every `package.json` in the source. This step does its own enumeration of the source: it does not consume the file list produced by step 2, because the package index needs only the `package.json` files, not the whole crawl-eligible file set.

For commit mode, the strategy is two batched Git operations: `git ls-tree -r -z <commit>` to find every `package.json`, then `git cat-file --batch` over a single long-lived process to read all the blobs. This avoids per-file fork overhead and keeps the build to one focused tree enumeration plus one stream of blob reads.

For working-directory mode, the package index is built by walking the filesystem for `package.json` files and reading them directly. (The package-resolution fallback in `src/engine/package_lookup.rs` is a separate code path used only by `dump-chunks`, not by the main crawl.)

For each `package.json`, the `"name"` field is parsed out and stored under the directory's repo-relative path as the key. Repo-root `package.json` is keyed by the empty string `""`.

Lookup happens later, during file processing: given a file at `libraries/lib1/src/Example.ts`, the index is queried for ancestor directories in this order:

1. `libraries/lib1/src`
2. `libraries/lib1`
3. `libraries`
4. `""`

The first match wins, reproducing the "nearest ancestor `package.json` governs the file" rule. The lookup helper is `PackageIndex::find_package_name` in `src/engine/git_ops/mod.rs`.

## Step 4: File processing

For each enumerated file, the work splits into a sentinel-check fast path and a chunk-embed-upsert slow path.

**Sentinel-check fast path:** Compute `file_id` from `(embedder_id, chunker_id, catalog, blob_id, relative_path)`. Look up the `row_id` of the sentinel chunk (`{file_id}:1`). If the row exists and has `file_complete = true`, the file has already been indexed under some previous label; add the current `label_id` to its `active_label_ids` (and to every other chunk row sharing this `file_id`). No content read, no chunking, no embedding.

**Slow path:** Read the blob bytes (commit mode: from Git, via the cat-file batch process; working-dir mode: from the filesystem). Resolve the package name via the package index. Compute the breadcrumb prefix. Dispatch to the chunker via `src/engine/chunker.rs` (see [chunker.md](./chunker.md) for the algorithm) to produce chunks. Embed each chunk via the parallel ONNX embedder pool (see `src/engine/parallel_embedder.rs`). Upsert each resulting `ChunkRow` to the `chunks` table, with `active_label_ids` containing the current `label_id`. The sentinel chunk (ordinal 1) gets `file_complete = true` once all chunks for the file have been written.

Files that the chunker reports warnings for (`[fallback-split]` quality marker; see [chunker.md](./chunker.md)) are tracked in a sidecar warnings file. By default, files with warnings are always re-processed on subsequent crawls so chunker improvements can take effect; the `--incremental-warnings` flag opts into skipping them when unchanged, useful for large repos with known chunker issues that aren't blocking work.

The pipeline is parallel: a worker thread pool drives chunking and embedding via `crossbeam` channels and `rayon`, with a separate writer thread doing the LanceDB upserts. Per-chunk failures (tokenizer edge cases, model issues on specific content) are tracked in `CrawlFailures` and reported at the end; structural errors (disk full, dataset corruption) abort immediately.

## Step 5: Label reassignment

This step runs only after every file in step 4 succeeded. Partial crawls skip it entirely.

The crawl maintains a `HashSet<file_id>` of every file_id touched during step 4 (both fast-path label-adds and slow-path upserts). After step 4 completes, the cleanup pass runs:

1. Query all chunks where `active_label_ids` contains the current `label_id`.
2. For each chunk, extract its `file_id` field.
3. If `file_id` is not in the touched set, remove `label_id` from `active_label_ids`. If `active_label_ids` becomes empty, delete the chunk row.

The result: the label's membership now exactly reflects the current commit's tree. Chunks shared with other labels survive (only the current label was removed); chunks unique to the previous version of this label are deleted.

The "only after success" rule matters because the touched set is only complete on success. An interrupted crawl with a half-built touched set would incorrectly reassign chunks for files it hadn't gotten to yet, leaving the label's content corrupted. Skipping reassignment on failure is the safe behavior: stale chunks remain associated with the label until the next successful crawl, which produces a complete touched set and runs the cleanup correctly.

## Step 6: Crawl finalization

Update the label metadata row: set `crawl_complete = true`, store the resolved commit OID (or `""` for working-dir), update the timestamp.

This is the closing handshake. Once finalized, search and view operations against the label see a consistent view of its content.

## Working-directory mode

A working-directory crawl indexes uncommitted changes from the filesystem rather than from Git objects. The label produced is mutable: re-crawling updates the indexed content based on the current filesystem state, which contrasts with commit-based labels (immutable for a given commit; re-crawling the same commit is idempotent).

Use cases:

- Indexing work-in-progress before committing.
- Comparing uncommitted changes with committed code by maintaining parallel labels (e.g., `--label main --commit HEAD` and `--label working --working-dir`).
- Agents that need to understand the current state of the codebase as the developer is editing it.

The identity model differs from commit mode in two fields, but `file_id` matches:

| Property      | Commit-based         | Working directory                    |
| ------------- | -------------------- | ------------------------------------ |
| `blob_id`     | Git blob SHA-1       | Git blob SHA-1 (computed on the fly) |
| `commit_oid`  | Resolved 40-char SHA | `""` (empty)                         |
| `source_kind` | `"git-commit"`       | `"working-directory"`                |

Because `blob_id` matches between modes, a file that's been committed without modification produces the same `file_id` whether crawled from `HEAD` or from the working tree. This is what enables an agent to maintain both a commit-based label and a working-dir label cheaply: only the actually-different files re-embed.

The blob-ID compatibility comes from delegating to the `git` CLI, which respects every detail of the repo's blob-construction rules (`.gitattributes`, `core.autocrlf`, clean filters, etc.). Reimplementing those rules in Rust would be a maintenance burden; the shell-out cost is small relative to embedding time.

## Partial-crawl semantics

Crawls can be interrupted (Ctrl+C, OOM, network blip during ONNX runtime download, disk full). The pipeline is designed so interruption leaves the database in a recoverable state.

What survives an interrupted crawl:

- Chunks already written to the database stay written. Their `active_label_ids` correctly include the current label. The work isn't lost.
- The label metadata row exists with `crawl_complete = false`. This is the signal that the label is in an unfinished state.
- The sentinel rows for fully-processed files have `file_complete = true`, so a resumed crawl skips them via the fast path.

What does _not_ happen:

- Label reassignment (step 5) does not run, because the touched set is incomplete. Stale chunks from previous crawls of this label remain associated with the label.
- The label metadata is not finalized.

A subsequent successful crawl recovers the label cleanly: files already indexed are picked up by the sentinel fast path, files that hadn't been touched yet get processed, and step 5 then runs with a complete touched set, removing the stale chunks. The user never has to do anything special. Re-running the same crawl command is the recovery procedure.

A consequence of this design: orphaned chunks are possible. If a file was uploaded but its `active_label_ids` didn't get the label added before the crawl was interrupted, the chunk is in the database but tagged with no label. It's invisible to label-scoped search and view, but it occupies space. This is the case the planned offline GC command (see [backlog.md](../backlog.md)) is intended to address. Inline cleanup during crawl is not sufficient because there's no way to safely identify orphans without scanning the whole `chunks` table.
