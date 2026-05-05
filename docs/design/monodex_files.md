# Files Monodex reads and writes

This document inventories every file involved in Monodex's runtime contract: tool-home state, the database directory, repo-local files Monodex reads from the indexed repository, and the schema and template files that ship with the project. It is the central reference for "what is this file, who owns it, what writes it, and is it safe to modify by hand."

Two placeholders are used throughout:

- `<tool-home>`: the Monodex tool home directory. Defaults to `~/.monodex/`, overridable via the `MONODEX_HOME` environment variable. Resolution logic in `src/paths.rs`. A relative `MONODEX_HOME` is resolved against the current working directory at process start; empty or whitespace-only values are treated as unset.
- `<database-dir>`: the database directory. Defaults to `<tool-home>/default-db/`, relocatable via the `database.path` field in `config.json`. Must be an absolute path on a local filesystem.

## A note on validation

The user-editable JSON files have two layers of validation that look like one but aren't:

- **Editor-time validation** comes from JSON Schema files under `schemas/`. The user's editor reads the `$schema` URL from the file being edited, fetches the schema, and uses it for autocomplete and inline error reporting. This is the Rush Stack / VS Code path; it is not specific to Monodex.
- **Runtime validation** comes from typed Rust structs that derive `serde::Deserialize`, with `#[serde(deny_unknown_fields)]` on each struct so that misspelled keys or stale field names produce a clear error rather than being silently ignored. The structs live alongside the loader code in `src/app/config.rs`, `src/app/context.rs`, and `src/engine/crawl_config.rs`.

The two layers describe the same shapes but are independently maintained. Adding or renaming a field requires updating both: the JSON Schema (for editor experience) and the Rust struct (for runtime correctness). This is duplication, and it is deliberate. The alternative would be code-generating one side from the other, which has its own maintenance burden and tooling cost. Treat this as a known coupling: a config-shape change is not done until both sides agree.

One distinction worth knowing: `config.json` and `crawl.json` reject unknown fields at load time, but `context.json` does not. This is intentional, not an oversight. The user-edited files want strict failure (an unknown field is almost always a typo that would otherwise silently do nothing). The tool-managed `context.json` wants lenient parsing so that an older binary can still read a state file written by a newer binary. The alternative is that downgrading Monodex breaks until the user manually edits or deletes their context.

## Tool home

The tool home contains three user-facing JSON files plus the default database directory.

### `<tool-home>/config.json`

User-editable. Defines catalogs (data sources Monodex indexes), an optional `database.path` override, and embedding-model knobs. Editor schema in `schemas/config.schema.json`; runtime validation in `src/app/config.rs`. Edit by hand or via your editor's JSON-Schema integration; Monodex itself does not write to this file.

### `<tool-home>/context.json`

Tool-managed. Records the default catalog and label set by `monodex use`, so subsequent commands don't need `--catalog` and `--label` on every invocation. Written by the `use` subcommand; read by every other subcommand. Editor schema in `schemas/context.schema.json`; runtime loader in `src/app/context.rs`. Users can edit it by hand if desired, but the supported workflow is `monodex use <catalog>:<label>`.

### `<tool-home>/crawl.json`

User-editable, optional. The user-global crawl config: file-type-to-strategy mappings and exclude/keep glob patterns. If absent, an embedded default (compiled into the binary, source in `src/engine/crawl_config.rs`) is used. Discovery precedence is repo-local `monodex-crawl.json` → user-global `<tool-home>/crawl.json` → embedded default; first found wins, no merging. Editor schema in `schemas/crawl.schema.json`; runtime loader in `src/engine/crawl_config.rs`.

This file is not auto-created by current Monodex. Auto-creation on first run, with a starter template seeded from `examples/monodex-crawl.json`, is part of the planned `monodex init` flow.

### Migration warning

Earlier prerelease versions placed these files at platform-dependent locations (`~/.config/monodex/` for `config.json` and `context.json`; OS-default config dir for `crawl.json`). On startup, `src/paths.rs` checks for files at those old paths and prints a non-suppressible one-line warning to stderr instructing the user to move them. The check exists to handle leftover files from prerelease use, not because there is a documented user base to support; the function is intended to be deleted once enough time has passed.

## Database directory

`<database-dir>` contains a metadata file, the LanceDB tables, and per-catalog warning state. It is not designed to be edited by hand. Every file in it is tool-managed except where noted.

The database location must be on a local filesystem. Network filesystems and synced cloud folders (NFS, SMB, Dropbox, OneDrive, iCloud, Google Drive, etc.) are not supported. The single-writer process lock that will guard concurrent crawls (see [backlog.md](../backlog.md)) is intended to span all storage formats in this directory under one invariant.

### `<database-dir>/monodex-meta.json`

Records the schema version, creation timestamp, the binary version that created the database, and the Lance format version at creation time. Written by `monodex init-db`; read on every database open. Defined in `src/engine/storage/database.rs`.

The `monodex_schema_version` field is the load-bearing one. Every database open reads it and compares it to the `MONODEX_SCHEMA_VERSION` constant in `src/engine/schema.rs`. A mismatch fails the open with a clear error rather than attempting silent migration. Bumping the schema version is a breaking change to existing databases (users have to rebuild), and any change to the schema's column shape requires a bump. This includes adding columns, even though LanceDB itself can store rows with unset columns: an older binary running against a newer database has no contract that says "blank cells in this column are OK," so the safe rule is to treat any shape change as breaking. The compatibility cost of avoiding a bump (writing code to read schemas with unfamiliar columns, deciding what to do with new columns when writing rows) is not worth absorbing without a concrete need.

### `<database-dir>/chunks.lance/` and `<database-dir>/label_metadata.lance/`

LanceDB tables. The `.lance/` suffix is LanceDB's directory-based table format: every LanceDB table is a directory with that suffix containing data files, transaction logs, and index files. The suffix is a LanceDB convention, not a Monodex one; that's why the LanceDB tables are sibling directories under `<database-dir>` rather than nested inside a `vectordb/` subdirectory. Schema definitions live in `src/engine/schema.rs`; row types in `src/engine/storage/rows.rs`.

When Tantivy full-text search is added, its index is planned to live at `<database-dir>/fts/`. Tantivy doesn't impose a directory-suffix convention, so the layout can be simpler than the LanceDB equivalent. The naming convention for `<database-dir>` siblings is: any directory ending in `.lance/` is a LanceDB table; everything else is something else.

### `<database-dir>/warnings-<catalog>.json`

One file per catalog. Records the list of repo-relative paths that produced chunker warnings (`[fallback-split]` markers; see [chunker.md](./chunker.md)) on the most recent crawl, used by the next crawl to decide which previously-warned files to revisit. Format: a JSON array of repo-relative path strings. Written at the end of each crawl by `src/app/util.rs`; read at the start of each crawl. Safe to delete; the next crawl will rebuild it.

## Repo-local files

Monodex reads two kinds of files from the repository being indexed.

### `<repo-root>/monodex-crawl.json`

User-editable, optional. If present at the root of the indexed repo, this overrides the user-global `<tool-home>/crawl.json` for crawls of that repo. Same shape as the user-global file (editor schema in `schemas/crawl.schema.json`, runtime loader in `src/engine/crawl_config.rs`). Repo-local config is the right place for repo-specific exclusions and any file-type strategies that should ship with the repo; user-global config is the right place for personal preferences that span all the user's repos. Monodex does not write to this file.

### `package.json` files anywhere in the repo

Read during the package-indexing step of every crawl (see [crawl.md](./crawl.md)). Monodex reads only the `"name"` field; other fields are ignored. The package index is built by enumerating every `package.json` in the commit tree (commit-mode crawls) or the working directory (working-dir crawls) and resolving each indexed file to its nearest-ancestor package by directory. Monodex does not write to `package.json` files.

## Shipped artifacts

Two sets of files travel with the Monodex source code but are neither documentation nor source: JSON-Schema files and config-file templates.

The current state is best described as not-yet-plumbed. Cargo doesn't natively support shipping non-source files alongside a binary, so a `cargo install monodex` produces a binary without these files on the user's system. The schemas and templates exist in the repo today for development purposes — a contributor can edit them, hand-copy them to a test installation, validate them against the loader — but they are not part of a user's installation.

The intended end state is: schemas published to a Microsoft-hosted schema server (URLs of the form `https://developer.microsoft.com/json-schemas/monodex/v0/...`, hosted from the [microsoft/json-schemas](https://github.com/microsoft/json-schemas) repo, manually published per Rush Stack convention); templates embedded in the binary via `include_bytes!` and written into `<tool-home>/` by a `monodex init` command. Both are future work, not yet implemented.

### `schemas/*.schema.json`

JSON-Schema files following the Rush Stack convention for user-editable JSON. Three files: `config.schema.json`, `context.schema.json`, `crawl.schema.json`. Their purpose is editor integration: a user editing a config file with VS Code (or any JSON-Schema-aware editor) gets autocomplete, validation, and inline documentation by way of the `$schema` field at the top of the file pointing at the appropriate schema URL.

These are not used at runtime; runtime validation comes from the typed Rust structs described in the validation note at the top of this document. The schemas and the structs describe the same shape and must be kept in sync by hand. Treat the schemas as a release artifact: once published, a schema change requires a coordinated update on the json-schemas server.

### `examples/*.json`

Templates for the user-editable JSON files, in JSON-with-comments format. Three files corresponding to the three schemas: `config.json`, `context.json`, `monodex-crawl.json`. Each is a fully-commented example of the corresponding format with sensible defaults.

JSON-with-comments is the format Rush Stack uses for user-editable JSON. Comments serve two purposes: as ambient documentation that survives editing (the user keeps the comments when they tweak a value, so the next time they open the file they remember what each field does), and as an upgrade vector. When the comment guidance changes, Monodex can offer to upgrade the comments in a user's existing file while preserving their values, analogous to how Debian package upgrades present new versions of `/etc` config files for diff-and-merge. This is not a settled industry convention; calling the format JSONC is misleading because several different specifications use that name. The format is JSON-with-comments. It is not JSON5 (a JavaScript subset much broader than JSON-with-comments).

The current directory name `examples/` is a misnomer: these files are templates first and examples second. A future rename to something like `config-templates/` is a candidate for the backlog.
