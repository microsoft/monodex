# Change Log - monodex

This log was last updated on Wed, 16 Apr 2026 17:50:00 GMT.

<!--
PUBLISHING INSTRUCTIONS (DO NOT MODIFY):

1. Choose an appropriate version number based on semantic versioning:
   - MAJOR: Breaking changes that require user action
   - MINOR: New features, backwards compatible
   - PATCH: Bug fixes, backwards compatible

2. Update `version` in Cargo.toml

3. Rename "## Unreleased" to "## X.Y.Z" and add release date:
   ## 0.3.0 (2026-04-14)

4. Update the "last generated" date in the header

5. After publishing, the next PR author will add a new "## Unreleased" section

Subsubheadings must be one of: Added, Changed, Fixed, Deprecated, Removed, Security
-->

## Unreleased

### Added

- **Structured JSON output for search and view**: `monodex search` and `monodex view` now support `--json` output for scripting and tool integration, plus `--compact` for minimal JSON records.

## 0.3.0 (2026-04-16)

### Changed

- **crawl command now requires explicit source**: Must specify `--label` AND either `--working-dir` OR `--commit`
  - Previously: `monodex crawl --catalog myrepo --label main` (defaulted to HEAD)
  - Now: `monodex crawl --catalog myrepo --label main --commit HEAD`
  - This prevents accidental overwrites of labels and makes crawl intent explicit
  - CLI now shows proper usage: `monodex crawl --label <LABEL> <--commit <COMMIT>|--working-dir>`

### Fixed

- **Working directory blob IDs now match Git blob IDs**: `--working-dir` mode now uses Git CLI batch commands (`git ls-files`, `git status`, `git hash-object`) to compute blob IDs that respect `.gitattributes`, clean filters, and other repo-specific settings. This ensures identical content produces the same `file_id` in both `--commit` and `--working-dir` modes, enabling proper incremental skipping.

## 0.2.0

### Updates

- Add `--debug` CLI flag for verbose network request logging
- Add `maxUploadBytes` config setting for Qdrant payload limit (default 30MB)
- Implement rewind upload algorithm for large batch splitting to avoid Qdrant payload limits
- Improve upload error handling: preserve chunks on failure, report clear error messages

## 0.1.0

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

## 0.0.1

- Initial release
