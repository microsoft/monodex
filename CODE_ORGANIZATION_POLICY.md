# Code Organization Policy

Rules for where code lives and how files are structured. Scan this before adding or moving code.

## File size limits

| Category | Target | Hard max |
|---|---|---|
| Command handler | 150–350 lines | 500 |
| Algorithm / engine module | 250–500 lines | 700 |
| Types-only file | any | 300 |
| Test-only file | any | 800 |

Lines = production code excluding `#[cfg(test)]` blocks. Hard maxes are triggers for review, not automatic fragmentation. A coherent file at 710 lines is better than two incoherent files at 350.

## Core principle: split by edit intent

Each file should have **one dominant reason to be edited**. Not "one subsystem" and not "one visibility level" — one change intent. If two functions only change together when different kinds of work happen, they belong in different files.

## Where to put new code

- **New CLI command** → new file in `app/commands/`, named after the subcommand. Add variant to `Commands` in `cli.rs`. Add dispatch arm in `main.rs`.
- **New storage operation** → pick the `engine/storage/` submodule by operation family: `database.rs` for connection/open, `chunks.rs` for chunk operations, `labels.rs` for label metadata operations.
- **New partitioner heuristic** → `split_search.rs` for split-point logic, `node_analysis.rs` for AST node properties, `scoring.rs` for quality measurement.
- **New config field** → `app/config.rs` for app-level config, `engine/config.rs` for engine-level chunking config, `engine/crawl_config.rs` for crawl filtering rules.
- **Shared utility** → `engine/util.rs` for engine-wide, `app/util.rs` for app-wide formatting/display.

## Module header comments

Every non-trivial file must start with:

```rust
//! Purpose: <one line>.
//! Edit here when: <the change intents this file serves>.
//! Do not edit here for: <common wrong guesses — point to the right file>.
```

## Test placement

- **Inline** (`#[cfg(test)] mod tests` in same file): only when the file is under 500 total lines and tests are short, white-box, and tightly coupled to one function or type.
- **Sibling test file** (`#[cfg(test)] mod tests;` in `mod.rs`, code in `tests.rs`): default for any directory-module. Tests stay inside the module tree and can access private items.
- **Integration tests** (`tests/` at crate root): CLI-level and end-to-end behavior only.
- **Rule of thumb**: if the `#[cfg(test)]` block would exceed 300 lines, it must be a separate file.

## Naming

- Command handlers: named after the CLI subcommand (`purge.rs`, `search.rs`). Use `use_cmd.rs` for `use` (reserved keyword).
- Engine submodule directories: named after the concept (`partitioner/`, `storage/`).
- Type-only files: `types.rs` or `models.rs`.
- Test files: `tests.rs` (singular).

## Banned patterns

- No files named `helpers.rs`, `common.rs`, or `misc.rs`.
- No wildcard re-exports (`pub use submodule::*`). List re-exports explicitly.
- Avoid files under 50 lines unless they are `mod.rs` files or contain a type/trait that is the sole public API of a module.
- No putting unrelated items together just because they're small.

## Formatting discipline

For move-heavy refactors, keep formatting in a separate commit. Commit 1: mechanical relocation only, approved by the human. Commit 2: formatter-only changes on touched files. Never combine repo-wide formatting with a structural refactor commit.
