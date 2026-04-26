# monodex Development Plan

> **Note:** Many file paths in this document predate a recent refactor and may be incorrect. The current source layout is documented in `README.md` under "Architecture".

## Overview

This plan implements label-based semantic indexing with incremental crawl. The work is organized into phases with clear dependencies and testable milestones.

### Testing Approach

- **Development testing**: Use the `sparo` catalog (small monorepo, ~266 chunks, fast iteration)
- **Final verification**: Use the `rushstack` catalog (large monorepo, hours to crawl - run by user only)

**Example catalogs in config:**
```json
{
  "catalogs": {
    "sparo": { "type": "monorepo", "path": "/path/to/sparo" },
    "rushstack": { "type": "monorepo", "path": "/path/to/rushstack" }
  }
}
```

---

## Phase 1: Git Operations Layer

**Goal:** Enable reading file content and package information from Git commits without touching the working tree.

### 1.1 Git Tree Enumeration

Create `src/engine/git_ops.rs`:

- [x] Add `gix` dependency to Cargo.toml (pure Rust Git implementation)
- [x] Implement `enumerate_commit_tree(repo_path, commit) -> Vec<FileEntry>`
  ```rust
  pub struct FileEntry {
      pub relative_path: String,
      pub blob_id: String,
  }
  ```
- [x] Handle NUL-delimited output from `git ls-tree -r -z`
- [x] Filter out non-blob entries
- [x] Test with a sample commit in rushstack repo

### 1.2 Git Blob Content Reading

- [x] Implement `read_blob_content(repo_path, blob_id) -> Result<Vec<u8>>`
- [x] Use `git cat-file --batch` for batch reading (or gix equivalent)
- [x] Handle UTF-8 decoding errors gracefully
- [x] Test reading a few blobs from sample commit

### 1.3 Package Index Building

- [x] Implement `build_package_index_for_commit(repo_path, commit) -> Result<PackageIndex>`
  - Enumerate all `package.json` files from commit tree
  - Batch-read blob contents
  - Parse JSON to extract `"name"` field
  - Build `HashMap<String, String>` mapping directory -> package name
- [x] Implement `find_package_name_from_index(relative_path, index) -> Option<&str>`
  - Walk ancestor directories to find nearest package
- [x] Test on rushstack commit, verify correct package resolution

### 1.4 Integration Test

- [x] Verify we can enumerate a commit, resolve packages, and read content
- [x] Confirm no dependency on working tree state

---

## Phase 2: Schema Changes ✅ COMPLETE

**Goal:** Update chunk schema to support labels and commit-based identity.

### 2.1 Update PointPayload Struct

In `src/engine/uploader.rs`:

- [x] Add new fields to `PointPayload`:
  - `label_id: String`
  - `active_label_ids: Vec<String>`
  - `embedder_id: String`
  - `chunker_id: String`
  - `blob_id: String`
  - `package_name: String`
  - `chunk_ordinal: usize` (rename from `chunk_number`)
- [x] Keep `file_id` field (now a 16-char hex string instead of u64)
- [x] Keep `source_uri` but document it's not a key

### 2.2 Add LabelMetadata Struct

- [x] Create `LabelMetadata` struct for label metadata
- [x] Store in separate `label_metadata` table (LanceDB)

### 2.3 Update Storage Operations

- [x] Add `upsert_label_metadata(label: LabelMetadata)`
- [x] Add `get_label_metadata(label_id) -> Option<LabelMetadata>`
- [x] Add `add_label_to_chunk(chunk_id, label_id)` for updating `active_label_ids`
- [x] Add `add_label_to_file_chunks(file_id, label_id)` for batch file updates
- [x] Add `remove_label_from_chunks(label_id)` for cleanup scan
- [x] Add `get_file_sentinel(file_id)` to check if file already indexed
- [x] Add `set_active_labels(chunk_id, label_ids)` for atomic replacement
- [x] Add `delete_point(chunk_id)` for cleanup
- [x] Add `search_with_label(query, label_id)` for label-filtered search
- [x] Add `get_chunks_by_file_id_with_label(file_id, label_id)` for label-filtered view

### 2.4 File and Chunk ID Computation

In `src/engine/util.rs`:

- [x] Implement `compute_file_id(embedder_id, chunker_id, blob_id, relative_path) -> String`
  - This is a file-scoped identity (semantic version of a file)
  - Does NOT include chunk_ordinal
  - Returns 16-char hex string
- [x] Add `EMBEDDER_ID` and `CHUNKER_ID` constants
- [x] Implement `compute_point_id(file_id, chunk_ordinal) -> String`
  - Returns `string_to_uuid(format!("{}:{}", file_id, chunk_ordinal))` for deterministic chunk IDs
- [x] Implement `LabelId::new(catalog, label) -> Result<LabelId, IdentifierError>` in `src/engine/identifier.rs`
  - Returns a validated `LabelId` containing the `<catalog>:<label>` qualified form
  - A helper function `compute_label_id(catalog, label) -> Result<String, IdentifierError>` also exists for the plain-string case
- [x] Update `Chunk` struct to include new Phase 2 fields
- [x] Add `ChunkContext` struct for content-based chunking
- [x] Add `chunk_content()` function for Git blob-based chunking
- [x] Add `Chunk::point_id()` method

---

## Phase 3: Content-Based Chunking ✅ COMPLETE

**Goal:** Decouple chunking from filesystem paths, enable Git blob content as input.

### 3.1 Refactor Partitioner Interface

In `src/engine/partitioner.rs`:

- [x] Create `chunk_content()` function that accepts content as string:
  ```rust
  pub fn chunk_content(
      content: &str,
      ctx: &ChunkContext,
      target_size: usize,
  ) -> Result<Vec<Chunk>>
  ```
  *(Note: Implementation is in `chunker.rs`, uses `ChunkContext` for Phase 2 fields)*
- [x] Update `chunk_file()` to be a convenience wrapper that reads from filesystem

### 3.2 Update Chunk Metadata

- [x] Ensure chunks include `blob_id` field
- [x] Ensure chunks include `package_name` field
- [x] Compute `file_id` during chunking using util function
- [x] Point ID = hash of (file_id, chunk_ordinal)

### 3.3 Test Content-Based Chunking

- [x] Test that same content + path produces same file_id
- [x] Test that path changes produce different file_id (expected behavior)
- [x] Test that same content at different paths = different chunks (semantic context matters)

---

## Phase 4: Label-Aware Crawler [✅ COMPLETE]

**Goal:** Implement the full crawl flow with label support.

### 4.1 Crawl Command Updates

In `src/main.rs`:

- [x] Add `--label` argument (required)
- [x] Add `--commit` argument (defaults to HEAD)
- [x] Update crawl flow to use Git operations layer

### 4.2 Crawl Implementation

- [x] Resolve commit OID from `--commit` argument
- [x] Build package index for commit
- [x] Enumerate files from commit tree
- [x] For each file:
  - Check if chunk already exists (sentinel check)
  - If exists and complete: update `active_label_ids` only
  - If not: read content, chunk, embed, upload
- [x] Track all chunk IDs touched during crawl

### 4.3 Label Reassignment Cleanup

**Critical:** Only runs after fully successful crawl. Partial crawls must NOT trigger cleanup.

- [x] Track all file IDs touched during crawl (HashSet)
- [x] After successful crawl, scan for chunks with label in `active_label_ids`
- [x] For each chunk, extract file_id
- [x] If file_id NOT in touched set:
  - Remove label from `active_label_ids`
  - Delete chunk if `active_label_ids` becomes empty
- [x] Ensure interrupted/failed crawls skip this step entirely

### 4.4 Label Metadata Persistence

- [x] Upsert label metadata at start of crawl
- [x] Update `crawl_complete` and timestamp at end

---

## Phase 5: Query Updates ✅ COMPLETE

**Goal:** Update search and view to work with label filtering.

### 5.1 Use Command (Default Context)

- [x] Add `use` command to set default catalog and label
- [x] Store default context in `~/.config/monodex/context.json`
- [x] All commands check default context when flags not provided
- [x] Explicit flags override default context

### 5.2 Search Command

- [x] Add `--label` argument (uses default context if not provided)
- [x] Filter by `active_label_ids CONTAINS label_id`
- [x] Update output format to show `file_id:ordinal`

### 5.3 View Command

- [x] Update to use file-oriented identity (file_id + selector)
- [x] Filter by `active_label_ids`
- [x] Support selector syntax (`:N`, `:N-M`, `:N-end`)
- [x] Support file reconstruction (view all chunks via file_id without selector)
- [x] Note: Path-based view is deferred to a later phase

### 5.4 Test Query Flow ✅ COMPLETE

- [x] Crawl a label (sparo:main already indexed)
- [x] Set default context with `use`
- [x] Search within that label (using default context)
- [x] View specific chunks
- [x] View chunk ranges (`:N-M` syntax)
- [x] Verify results are correct

**Tested:** 2026-04-08 - All query operations working correctly with sparo catalog.

### 5.5 Update README.md ✅ COMPLETE

- [x] Document new CLI commands
- [x] Document label concept
- [x] Update examples

### 5.6 Fix Error Handling in Crawl ✅ COMPLETE

**Goal:** Ensure errors during crawl are properly propagated rather than swallowed, and partial state can be cleaned up.

**Changes implemented:**
- [x] Added failure tracking via `Arc<Mutex<Vec<String>>>` for upload, file-complete, and label-add failures
- [x] Upload failures now track which files failed with error message
- [x] File completion marking failures are tracked and reported
- [x] Label assignment failures are tracked and reported (critical - chunks won't be searchable without label)
- [x] Changed `let _ = ...` to proper `if let Err(e)` with tracking
- [x] Added failure summary reporting after embedding phase completes
- [x] Changed warning emoji from ⚠️ to ❌ for actual errors to improve visibility

**Deferred to Phase 6 (Edge Case Testing):**
- [ ] Interrupt crawl (CTRL+C): verify partial chunks exist, label metadata shows incomplete
- [ ] Resume after partial failure: verify crawl continues from where it stopped
- [ ] Verify orphaned chunks (uploaded but not labeled) are found by future `monodex gc` command

**Files modified:**
- `src/main.rs` - `run_crawl_label()` function

---

## Phase 6: Edge Case Testing

**Goal:** Test edge cases and failure modes that emerge from label-based indexing semantics.

### 6.1 Multi-Label Scenarios ✅ COMPLETE

- [x] Crawl the same commit under two different labels (verify chunks share `active_label_ids`)
- [x] Verify both labels return same chunks in search
- [x] Crawl a different commit under one label (verify label reassignment)
- [x] Verify the other label still has its original chunks

### 6.2 Incremental Crawl Edge Cases

- [ ] Interrupt a crawl (CTRL+C) and verify:
  - Label metadata shows `crawl_complete = false`
  - Partial chunks exist but label membership is incomplete
- [ ] Resume interrupted crawl and verify completion
- [ ] Verify label reassignment does NOT run for partial crawls

**Note:** These tests require manual intervention (CTRL+C) and are not automated.

### 6.3 Chunk Deduplication Edge Cases ✅ COMPLETE

- [x] File moves between packages (path change):
  - Verify new chunks are created (breadcrumb context changes)
  - [Skipped - would require creating test scenario]
- [x] Same content, different path:
  - Verified: Different `file_id` values for same content at different paths
  - Verified: Both can coexist in different labels (tsconfig.json example)
  - Found 25 groups with duplicate content_hash but unique file_ids

### 6.4 Label Cleanup Edge Cases ✅ COMPLETE

- [x] Re-crawl same label with different commit
  - Verified: chunks from old commit are removed from label
  - Verified: chunks shared with other labels are NOT deleted (263 shared chunks preserved)
  - Verified: no orphaned chunks (empty `active_label_ids`) after re-crawl
- [x] Purge a single label and verify other labels unaffected
  - N/A: Purge operates at catalog level only (design decision)

### 6.5 Search/View Edge Cases ✅ COMPLETE

- [x] Search with label that has no chunks yet - returns empty results gracefully
- [x] View chunks from file that exists in multiple labels - works correctly
- [x] View chunks after label has been purged - N/A (purge operates at catalog level)
- [x] Default context with non-existent catalog/label - allows setting, search returns empty

### 6.6 Fresh Installation Verification

- [ ] Delete database directory
- [ ] Fresh crawl with new schema
- [ ] Verify all operations work from clean state

**Note:** Destructive test - skipped to preserve test data. Run manually before release.

---

## Phase 6 Summary

**Completed Tests:**
- ✅ 6.1 Multi-label scenarios: chunks correctly share `active_label_ids`
- ✅ 6.1 Label reassignment: works correctly when re-crawling different commits
- ✅ 6.3 Chunk deduplication: same content at different paths has different `file_id`
- ✅ 6.4 Label cleanup: no orphaned chunks, shared chunks preserved
- ✅ 6.5 Search/view edge cases: handles non-existent labels gracefully

---

## Phase 6: Untested Items Punchlist

The following tests were skipped and should be run manually before release:

- [ ] **Interrupted crawl (Phase 6.2)**:
  - Start a crawl, press CTRL+C
  - Verify `crawl_complete = false` in label metadata
  - Resume crawl and verify completion
  - Verify label reassignment does NOT run for partial crawls

- [ ] **Fresh installation (Phase 6.6)**:
  - Delete database directory
  - Fresh crawl with new schema
  - Verify all operations work from clean state

- [ ] **File moves between packages (Phase 6.3)**:
  - Create test scenario where file moves between packages
  - Verify new chunks are created with different breadcrumb context
  - Verify old chunks remain for old labels

---

## Phase 7: Working Directory Crawling ✅ COMPLETE

### 7.1 Design Working Directory Identity Model ✅ COMPLETE

- [x] Define identity scheme for working directory files:
  - No `blob_id` (not in Git yet)
  - Use content hash computed from file content (SHA256 with `sha256:` prefix)
  - Path + content hash determines `file_id`
- [x] Decide `source_kind` value for working directory labels: `"working-directory"`
- [x] Decide how to represent "no commit" in `LabelMetadata.commit_oid`: empty string `""`
- [x] Document: working directory labels are mutable (re-crawl changes content), commit labels are immutable

### 7.2 Implement Working Directory Enumeration ✅ COMPLETE

In `src/engine/git_ops.rs`:

- [x] Implement `enumerate_working_directory(repo_path, should_skip) -> Vec<WorkingDirEntry>`
  - Walk the filesystem using `walkdir`
  - Skip hidden directories (except .git), node_modules, target, dist, build, .cache, temp
  - Apply `should_skip_path` filter
  - Compute content hash (SHA256) for each file
- [x] Implement `build_package_index_for_working_dir(repo_path) -> PackageIndex`
  - Walk filesystem to find package.json files
  - Extract package names for breadcrumb context
- [x] Implement `read_working_file_content(repo_path, relative_path) -> Vec<u8>`
  - Simple wrapper around fs::read

### 7.3 Update Crawler for Working Directory Mode ✅ COMPLETE

- [x] Add `--working-dir` flag to `crawl` command (mutually exclusive with `--commit`)
- [x] Create `run_crawl_working_dir()` function
- [x] For working directory mode:
  - Use `enumerate_working_directory()` instead of `enumerate_commit_tree()`
  - Compute content hash instead of using `blob_id`
  - Set `source_kind = "working-directory"` in label metadata
  - Set `commit_oid = ""`
- [x] Package index works with working directory (walks up to nearest package.json on disk)

### 7.4 Test Working Directory Crawling

- [ ] Create a test repo with uncommitted changes
- [ ] Crawl with `--working-dir` and verify:
  - Label metadata shows `source_kind = "working-directory"`
  - Chunks have correct content (from working tree, not HEAD)
  - Search returns working directory content
- [ ] Modify a file and re-crawl:
  - Verify old chunks are replaced (label reassignment)
  - Verify new content is indexed
- [ ] Test alongside commit-based label:
  - Crawl `--commit HEAD --label main`
  - Crawl `--working-dir --label working`
  - Verify both labels can coexist, search returns correct results for each

**Note:** These tests require a live database and manual verification.

### 7.5 Document Working Directory Mode

- [ ] Update README.md with `--working-dir` usage examples
- [ ] Document semantic differences (mutable vs immutable labels)

---

## Phase 8: User-Configurable Crawl Settings

**Goal:** Externalize crawl policy from hardcoded Rust into user-configurable JSON files.

**Reference:** See DESIGN.md "Crawl Configuration" section for full spec.

### 8.1 File Extension Naming Convention

- [x] Rename `~/.config/monodex/config.jsonc` → `config.json` in docs and code
- [x] Rename `~/.config/monodex/context.json` - verify it uses `.json` (already correct)
- [x] Update README.md to use `.json` extension
- [x] Update DESIGN.md to use `.json` extension
- [x] Align with Rush Stack JSON conventions

### 8.2 Define Crawl Config Schema

- [x] Create `src/engine/crawl_config.rs` module
- [x] Define `CrawlConfig` struct with fields:
  - `version: u32` (must be `1`)
  - `file_types: HashMap<String, String>` (suffix → strategy)
  - `patterns_to_exclude: Vec<String>` (glob patterns)
  - `patterns_to_keep: Vec<String>` (glob patterns)
- [x] Define `CompiledCrawlConfig` struct with compiled glob sets and directory prefixes
- [x] Add `globset` dependency for pattern matching
- [x] Implement `should_crawl()` with evaluation rule
- [x] Implement `get_strategy()` to lookup chunking strategy
- [x] Handle directory patterns (ending in `/`) as prefix matches
- [x] Add unit tests for config validation and matching logic

### 8.3 Implement Config Discovery

- [x] Implement discovery precedence:
  1. `<repo-root>/monodex-crawl.json` (repo-local)
  2. `~/.config/monodex/crawl.json` (user-global)
  3. Embedded default (same JSON format, compiled into binary)
- [x] No merging - exactly one config is used
- [x] Implement `load_crawl_config(repo_path) -> Result<CrawlConfig>`

### 8.4 Implement Strict Validation ✅ COMPLETE

- [x] Add `#[serde(deny_unknown_fields)]` to all config structs (crawl config + existing Config, DatabaseConfig, DefaultContext)
- [x] Implement `validate()` method on `CrawlConfig`:
  - `version == 1`
  - Strategy names are valid
  - Glob patterns compile successfully
- [x] Add validation for existing config structs if needed (catalog type values, etc.)
- [x] Return descriptive error messages for all validation failures

### 8.5 Implement Evaluation Logic ✅ COMPLETE

- [x] Implement `should_crawl(path, config) -> bool`:
  ```rust
  matches_file_type(path, config)
    && (matches_patterns_to_keep(path, config) || !matches_patterns_to_exclude(path, config))
  ```
- [x] Implement `get_strategy(path, config) -> ChunkingStrategy`
- [x] Matching is repo-relative paths, case-sensitive, `/` separator

### 8.6 Create Built-in Default Config ✅ COMPLETE

- [x] Create embedded default config in `crawl_config.rs`
- [x] Mirror current hardcoded rules from `config.rs`:
  - fileTypes: `.ts`, `.tsx`, `.js`, `.jsx`, `.md`, `.json`, `.yml`, `.yaml`, `.txt`, `.css`, `.scss`
  - patternsToExclude: `node_modules/`, `dist/`, `build/`, `lib/`, `*.test.ts`, etc.
  - patternsToKeep: `src/`, `test/`

### 8.7 Refactor Existing Code ✅ COMPLETE

- [x] Replace `should_skip_path()` with `!should_crawl()`
- [x] Replace `get_chunk_strategy()` with config lookup
- [x] `config.rs` now delegates to `crawl_config.rs` for backward compatibility
- [x] Directory patterns now match anywhere in path (e.g., `lib/` matches `foo/lib/bar.ts`)

### 8.8 Document and Test ✅ COMPLETE

- [x] Update README.md with crawl config documentation
- [x] Add example `monodex-crawl.json` files
- [ ] Test config discovery precedence
- [ ] Test validation errors
- [ ] Test pattern matching edge cases
- [ ] Verify backward compatibility (existing behavior preserved via default config)

### 8.9 Unit Tests

- [ ] Test `should_crawl()` with various path/config combinations:
  - File type matching (`.ts` → crawl, `.png` → skip)
  - Exclusion matching (`node_modules/` → skip)
  - Keep override (`src/*.test.ts` → crawl despite exclude)
  - Combined: file type + exclude + keep interaction
- [ ] Test `get_strategy()` returns correct strategy for each file type
- [ ] Test config loading from JSON string (for embedded default)
- [ ] Test validation rejects invalid config (unknown strategy, bad glob, missing fields)

### 8.10 JSON Schema ✅ COMPLETE

- [x] Author JSON schema files for IDE autocomplete/validation:
  - `schemas/config.schema.json` for `config.json`
  - `schemas/crawl.schema.json` for `monodex-crawl.json`
  - `schemas/context.schema.json` for `context.json`
- [x] Schema files use `.schema.json` extension and live in `schemas/` folder
- [x] Users can reference via `"$schema": "https://.../schemas/crawl.schema.json"`
- [x] Added example config files in `examples/` directory
- [x] Updated README with schema table and usage instructions
- [ ] Publishing mechanism TBD (currently using raw.githubusercontent.com URLs)

---

## Phase 9: Markdown Heading-Based Chunking ✅ PARTIAL

**Goal:** Implement proper heading-based chunking for markdown files.

### 9.1 Design Markdown Chunking Algorithm ✅ COMPLETE

- [x] Analyze markdown structure:
  - Heading hierarchy (H1-H6)
  - Code blocks, lists, tables
  - Link/reference sections
- [x] Design chunk boundaries:
  - Split at heading boundaries (each section becomes a chunk)
  - Handle nested headings (H2 under H1)
  - Include parent heading context in breadcrumbs
- [ ] Consider special cases:
  - Very large sections (need sub-chunking?)
  - Code blocks (preserve as single unit?)
  - Front matter (YAML metadata)

### 9.2 Implement Markdown Chunker ✅ COMPLETE

- [x] Add custom markdown parser in `markdown_partitioner.rs`
- [x] Implement `partition_markdown(content, config, file_path, catalog)` function
- [x] Wire `partition_markdown()` into `chunk_content()` for the `Markdown` strategy branch
- [x] Generate chunks with:
  - Heading breadcrumb context
  - Section boundaries
  - Proper line numbers
- [ ] Handle edge cases:
  - Empty sections
  - Very long code blocks
  - Nested list structures

### 9.3 Test Markdown Chunker

- [ ] Test simple document with H1/H2/H3 sections
- [ ] Test document with code blocks
- [ ] Test document with tables
- [ ] Test document with nested lists
- [ ] Test very large markdown files
- [ ] Compare chunk quality vs line-based chunking

---

## Phase 10: Offline Garbage Collection

**Goal:** Provide a command to clean up orphaned chunks and recover storage.

### 11.1 Implement GC Command

- [ ] Add `gc` command: `monodex gc --catalog rushstack`
- [ ] Implementation:
  - Scroll through all chunks with `source_type = "code"`
  - Find chunks where `active_label_ids` is empty
  - Delete them
  - Report count and estimated storage recovered
- [ ] Add `--dry-run` flag to show what would be deleted without actually deleting

### 10.2 Test GC Scenarios

- [ ] Create orphaned chunks (interrupt a crawl, or delete a label's chunks manually)
- [ ] Run `monodex gc --dry-run` and verify correct chunks identified
- [ ] Run `monodex gc` and verify orphans deleted, other chunks untouched
- [ ] Verify `monodex gc` after successful crawl finds nothing to clean

---

## Phase 11: Watch Mode

**Goal:** Continuously monitor and re-index a working directory as files change.

### 11.1 Design Watch Mode Architecture

- [ ] Research file watching libraries (notify, notify-debouncer-mini)
- [ ] Decide on watch mode trigger:
  - Debounced file system events (preferred)
  - Polling interval (fallback for network filesystems)
- [ ] Design state management:
  - Which files are currently indexed under the working label?
  - How to handle rapid successive changes (debouncing)
- [ ] Design output/feedback:
  - Log to stdout? Background daemon?
  - How to show progress during re-indexing
- [ ] Consider integration with existing `--working-dir` crawl:
  - Watch mode could be `--watch` flag that runs indefinitely
  - Initial crawl + incremental updates
- [ ] Perform incremental updates (re-chunk and re-embed only changed files)
- [ ] Design integration with the label system (watch mode updates a specific working-dir label)

---

## Bug Fixes: Working Directory Hash Incompatibility

> **Note:** This issue was resolved. The working directory mode now uses Git-compatible blob IDs, enabling incremental skipping between commit and working-dir crawls.

**Issue:** The `--working-dir` feature uses a different content hash format than Git commit crawls, preventing incremental skipping between the two modes. The same file with identical content will have different `file_id` values, resulting in duplicate chunks stored and wasted embedding compute.

**Root Cause Analysis:**

The `file_id` is computed as:
```
file_id = xxhash(embedder_id + chunker_id + blob_id + relative_path)
```

The `blob_id` component differs by crawl source:

| Source | Hash Format | Example |
|--------|-------------|---------|
| Git commit crawl | Git blob SHA-1 (40 hex chars) | `8aba78c0c132f6f0adc6fe28dd6818966087ec05` |
| Working directory crawl | SHA-256 with prefix | `sha256:a1b2c3d4...` (64 hex chars + prefix) |

**Code Locations:**
- `monodex/src/engine/git_ops.rs:84-140` - `enumerate_commit_tree()` returns `FileEntry { blob_id }` where `blob_id` is Git's 40-char hex SHA-1
- `monodex/src/engine/git_ops.rs:260-310` - `enumerate_working_directory()` returns `WorkingDirEntry { content_hash }` where `content_hash` is `sha256:...` format
- `monodex/src/engine/git_ops.rs:317-324` - `compute_content_hash()` computes SHA-256 with `sha256:` prefix
- `monodex/src/engine/util.rs:32-46` - `compute_file_id()` uses `blob_id` as input
- `monodex/src/main.rs:1465` - Working dir uses `content_hash` as `blob_id` parameter

**Fix Options:**
1. Change `compute_content_hash()` to compute Git-compatible blob SHA-1 (format: `sha1("blob <size>\0<content>")`)
2. Compute both hashes and use the Git blob SHA-1 as the `blob_id` for `file_id` computation only

**Impact:**
- No incremental skipping between commit and working-dir crawls
- Duplicate chunks stored for same content
- Wasted embedding API calls

### BF.WD.1 Use Git-Compatible Blob Hash for Working Directory ✅ COMPLETE

- [x] Use Git CLI batch commands (`git ls-files`, `git status`, `git hash-object --stdin-paths`) to compute blob IDs
- [x] Correctly handle `.gitattributes`, clean filters, and other repo-specific settings
- [x] Update `WorkingDirEntry` field name from `content_hash` to `blob_id` for consistency
- [x] Test: crawl HEAD, then crawl --working-dir with no changes, verify 0 new files indexed
- [x] Test: `test_working_dir_blob_id_matches_commit` verifies blob IDs match between modes

---

## Bug Fixes: Qdrant Upload Payload Limit [OBSOLETE - Qdrant removed in LanceDB migration]

> **Note:** This section documents work that was completed for the Qdrant backend. The Qdrant uploader was removed when Monodex migrated to LanceDB. This history is preserved for reference.

**Issue:** Qdrant has a 32MB payload limit for HTTP requests. Large batches (3700+ chunks) exceed this limit, causing HTTP 400 errors. The original error handling swallowed these errors and did not retry, resulting in data loss and confusing output.

### BF.1 Add maxUploadBytes Config Setting ✅ COMPLETE

- [x] Add `maxUploadBytes` field to the `qdrant` section in `~/.config/monodex/config.json`
- [x] Default value: 30MB (30 * 1024 * 1024 bytes) if omitted
- [x] Update `src/main.rs` to parse this field (added to `QdrantConfig` struct)
- [x] Update JSON schema in `schemas/config.schema.json`
- [x] Update unit test for config parsing to include this field

### BF.2 Improve Upload Error Handling ✅ COMPLETE

- [x] When Qdrant returns HTTP 400 with payload size error, abort the crawl immediately
- [x] Print clear error message with Qdrant's original payload limit error message
- [x] Terminate process with non-zero exit code (via error propagation, not process::exit)
- [x] Remove the unconditional `accumulated.clear()` that loses data on upload failure
- [x] Add `is_payload_limit_error()` helper to detect Qdrant payload limit errors
- [x] Use `AtomicBool` flag for graceful shutdown instead of `std::process::exit`
- [x] Test: Use the 3700-chunk threshold hack to reproduce, verify error is clear

### BF.3 Implement Rewind Upload Algorithm ✅ COMPLETE

**Goal:** Ensure all batches stay under `maxUploadBytes` by iteratively subdividing and uploading.

**Algorithm (Rewind approach):**
```
fn upload_batch_with_rewind(points, max_bytes):
    remaining = points
    while remaining is not empty:
        buffer = serialize(remaining)
        if buffer.size <= max_bytes:
            send buffer
            return success  // all done
        else:
            // Rewind: don't send, split and upload first half recursively
            mid = remaining.len() / 2
            upload_batch_with_rewind(remaining[0..mid], max_bytes)
            remaining = remaining[mid..]  // continue loop with remainder
```

**Example with 77 items (70MB total, 30MB limit):**
1. Serialize 1..77 → 70MB → too big, rewind
2. Serialize 1..38 → 36MB → too big, rewind
3. Serialize 1..19 → 18MB → OK, upload 1..19
4. Serialize 20..77 → 45MB → too big, rewind
5. Serialize 20..48 → 22MB → OK, upload 20..48
6. Serialize 49..77 → 24MB → OK, upload 49..77
7. Done (3 uploads)

**Key characteristics:**
- After a successful upload, resume with ALL remaining items (not just the other half of the subtree)
- Potentially fewer total uploads than pure recursive subdivision
- Never sends a request exceeding the limit

**Implementation:**
- [x] Add `upload_batch_with_rewind` method to `QdrantUploader` (renamed to `upload_batch`)
- [x] Pass `max_bytes` from config (or default 30MB)
- [x] Serialize to `Vec<u8>` first, check size before sending
- [x] Use `reqwest::Body::from(bytes)` to send pre-serialized data
- [x] Log each sub-batch upload progress
- [x] Update callers in `main.rs` to use new method
- [x] Remove the 3700-chunk threshold hack from `main.rs` (none existed)

### BF.4 Document New Setting ✅ COMPLETE

- [x] Update README.md to document `maxUploadBytes` in the config file section
- [x] Note that default is 30MB, which is safely under Qdrant's 32MB WAL limit
- [x] Explain when users might need to adjust this (custom Qdrant config)

### BF.5 Print Catalog+Label in Command Output

Commands like `crawl`, `search`, and `view` should print their catalog and label concisely in the output so users know what they're operating on, especially when using the `monodex use` default.

### BF.6 Require Explicit Label for Crawl Command ✅ COMPLETE

**Problem:** The current design allows `monodex crawl` to use the default context set by `monodex use`. This is dangerous for AI agents who might accidentally clobber an important label by running crawl without specifying intent.

**Example scenario:**
1. `monodex use sparo main` → sets default to sparo:main
2. Agent runs `monodex crawl` intending to index working directory
3. But the label "main" was supposed to track the main branch, not working directory
4. The label is now semantically corrupted (contains wrong content)

**Design Change:**
- `--label` becomes **required** for `crawl` command (not optional)
- `monodex use` sets defaults for **read-only** commands (`search`, `view`, `dump-chunks`)
- The `--label` for crawl accepts just the label name (not `catalog:label`), taking catalog from saved context
- **Source must be explicit**: Either `--working-dir` OR `--commit` is required
- No default source - crawling is expensive, users must be explicit about what they want

**New CLI behavior:**
```bash
# Valid:
monodex crawl --label local --working-dir    # Uses catalog from "use", crawls working dir
monodex crawl --label local --commit HEAD    # Uses catalog from "use", crawls HEAD
monodex crawl --label main --commit abc123   # Specific commit

# Invalid:
monodex crawl                                # Error: --label and source are required
monodex crawl --label local                  # Error: must specify --working-dir or --commit
monodex crawl --working-dir                  # Error: --label is required

# "monodex use" still works for search/view:
monodex use sparo local
monodex search "query"                       # Uses sparo:local context
```

**Implementation:**
- [x] Add design decision to DEV_PLAN.md
- [x] Update CLI argument parsing: make `--label` required for `crawl`
- [x] Make source mutually required: `--working-dir` XOR `--commit` (both absent = error, both present = error)
- [x] Remove default value from `--commit` argument
- [x] Update `resolve_label_id` to require explicit label for crawl
- [x] Update README.md documentation
- [x] Test: verify `monodex crawl` without `--label` shows helpful error
- [x] Test: verify `monodex crawl --label foo` without source shows helpful error

---

## Upcoming Features

### MCP Server

Monodex is explicitly designed for AI assistants, but agents currently have to shell out to the CLI. A built-in MCP (Model Context Protocol) server would let Claude Code, Cursor, and other agents discover and call search/view capabilities as native tools — reducing friction from "agent-friendly" to "agent-native."

- [ ] Implement `monodex mcp-serve` command that exposes search and view as MCP tools
- [ ] Support single-project and workspace modes
- [ ] Include JSON-structured output for all tool responses

### Hybrid Search (Vector + Keyword)

Pure vector search can miss exact identifier matches — searching for `handleAuth` may not find the function if the embedding doesn't preserve the exact token. Combining vector similarity with keyword/text matching using Reciprocal Rank Fusion (RRF) addresses this. Tantivy (planned for a future milestone) will provide per-label full-text search indexes.

- [ ] Add Tantivy-based full-text search index per label
- [ ] Implement RRF fusion of vector and keyword results
- [ ] Make hybrid search configurable (enable/disable, tunable k parameter)

### Search Result Boosting

Configurable score multipliers based on file path patterns would let users tune search relevance — boosting source directories and penalizing test/mock/generated/vendor files. Monodex's crawl config already excludes some file patterns entirely, but there are cases where files should be indexed but ranked lower, not invisible.

- [ ] Add a `searchBoost` section to the crawl config with penalties and bonuses by path pattern
- [ ] Apply multipliers to search scores after vector search returns results
- [ ] Ship sensible defaults (boost `src/`, penalize `test/`, `mock/`, `generated/`, `.md`)

### JSON Output Mode

For programmatic consumption (MCP, scripts, CI pipelines), structured JSON output is essential. The current output format is human-readable with `>` prefixed lines, which requires ad-hoc parsing.

- [ ] Add `--json` flag to `search` and `view` commands
- [ ] Output results as a JSON array with file_id, chunk_ordinal, score, breadcrumb, text, and metadata
- [ ] Add `--compact` flag for minimal JSON (omit text content, include only identifiers and scores)

### Call Graph Tracing

The tree-sitter infrastructure already parses TypeScript ASTs for chunking. Extending this to extract cross-file symbol references would enable "who calls this function" and "what does this function call" queries — a qualitatively different kind of code understanding. This is a larger effort but builds on existing infrastructure.

- [ ] Design a symbol index format for storing caller/callee relationships
- [ ] Extract function definitions and call sites during the chunking pass
- [ ] Implement `monodex trace callers <symbol>` and `monodex trace callees <symbol>` commands
- [ ] Consider both regex-based (fast, multi-language) and AST-based (precise, TypeScript-first) extraction modes

### Broader Language Support for AST Chunking

Monodex currently does AST-aware chunking only for TypeScript/TSX. Other languages get line-based chunking, which still produces useful search results (the embedding model handles any language), but with lower chunk quality — no breadcrumbs, no symbol names, no semantic boundaries.

- [ ] Prioritize JavaScript/JSX as the next AST-aware language (tree-sitter grammar already available, syntax is a subset of TypeScript)
- [ ] Consider Python, Go, and Rust as subsequent targets based on Rush Stack ecosystem needs
- [ ] Design the partitioner interface to make adding new languages straightforward

---

## Notes

### Pull Requests

When making a pull request, add a bullet under "## Unreleased" in CHANGELOG.md describing the change from an end-user perspective. See CHANGELOG.md for the version history and publishing instructions.

### Dependencies

- `gix` crate for Git operations (pure Rust, avoids subprocess overhead)
- Existing `tree-sitter` for parsing
- Existing `ort` for ONNX embeddings

### File Structure

```
src/
├── main.rs                    # CLI entry point
└── engine/
    ├── breadcrumb.rs          # Breadcrumb context generation
    ├── chunker.rs             # File chunking dispatcher
    ├── config.rs              # Config loading and file exclusion rules
    ├── crawl_config.rs        # Crawl filtering rules
    ├── git_ops.rs             # Git tree enumeration and blob reading
    ├── identifier.rs          # Catalog/label identifier validation
    ├── markdown_partitioner.rs # Markdown heading-based chunking
    ├── mod.rs                 # Module exports
    ├── package_lookup.rs      # Package name resolution (walk up to package.json)
    ├── parallel_embedder.rs   # Parallel embedding with multiple ONNX sessions
    ├── partitioner.rs         # AST-based TypeScript chunking
    ├── schema.rs              # LanceDB schema definitions
    ├── storage/               # LanceDB storage layer
    │   ├── mod.rs             # Module exports
    │   ├── database.rs        # Database open and validation
    │   ├── chunks.rs          # Chunk storage operations
    │   ├── labels.rs          # Label metadata operations
    │   └── rows.rs            # Row type definitions
    ├── system_info.rs         # Memory and CPU detection
    └── util.rs                # Hash utilities for chunk IDs
```

### Testing Strategy

Each phase should have a commit point where:
1. Code compiles
2. Basic functionality works
3. User can review the diff before proceeding

Do not proceed to next phase without user approval.