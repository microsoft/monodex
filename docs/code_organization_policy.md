# Code Organization Policy

Rules for where code lives and how files are structured. Scan this before adding or moving code.

## Core principle: split by edit intent

Each file should have **one dominant reason to be edited**. Not "one subsystem" and not "one visibility level": one change intent.

The test for whether two pieces of code share an edit intent: name the changes that would cause each to be edited. If the changes are the same kind of work (both edited when the label-id format changes; both edited when the search ranking algorithm changes), they share an edit intent. If the changes are different kinds of work that just happen to land in the same PR sometimes, they do not.

A coherent file at 750 lines is better than two incoherent files at 400. The thresholds below are diagnostic; this principle is the rule.

## File size

| Category                  | Target        | Review threshold | Split-or-flag threshold |
| ------------------------- | ------------- | ---------------- | ----------------------- |
| Command handler           | 150-350 lines | 500              | 700                     |
| Algorithm / engine module | 250-500 lines | 700              | 1000                    |
| Types-only file           | any           | 500              | 800                     |
| Test-only file            | any           | 800              | 1200                    |

Lines = production code excluding `#[cfg(test)]` blocks and the module header. `mod.rs`, `lib.rs`, and `main.rs` files containing only declarations, re-exports, and small dispatch logic are not counted.

Production modules that don't fit a more specific row use the algorithm/engine row.

The thresholds apply when making a non-trivial addition to a file, not when only reading or making minor edits.

**At the review threshold:** ask whether the new addition fits the file's existing edit intent. If it does, proceed. If it doesn't, the addition belongs in a different file. Find or create the right one.

**At the split-or-flag threshold:** apply the test below. The result is either a split (the test authorizes one) or a note in the "Out-of-scope notes" footer (the test is inconclusive or no clean split exists).

**Hard ceiling:** no file exceeds 2000 lines total (everything in the file, not the production-only count used above). At this size the file is too large to navigate as one unit regardless of edit intent, so the edit-intent test does not apply and footer-noting is not an option. Raise it as a reorganization signal per "When the local rules don't fit" below.

**Optional header element: size note.** A file at or over a review or split-or-flag threshold may stay in place if its module header carries a `Size note:` line stating the current line count and a one-sentence justification. Revisit when the file grows another 100 lines past the count recorded in the note. The hard ceiling does not admit a size note (see above).

### Test before splitting

Name each proposed file and complete this sentence for it:

> "Edit this file when changing **\_\_**."

The split is valid only if all of the following hold:

- The two answers are different kinds of work, not the same work described two ways. "Predicate construction" and "predicate validation" are the same work. "Predicate primitives" and "label metadata storage" are different work.
- Each answer names a change visible in existing code, tests, docs, or the current task: not a hypothetical future change.

If any of these fails, do not split. Note the situation in the footer instead.

### Calibrating judgement: signal → response

Some situations look like split signals but aren't. Use this map:

- **The file crossed a threshold** → apply the test above. The threshold is not itself authorization to split.
- **A function is long but belongs to the same edit intent** → leave it. Length within one intent is not a split signal.
- **A type has many trait impls** → leave them with the type. Multiple impls are one edit intent (the type's behavior).
- **Inline tests make the file look large** → check the production-code line count against the threshold, not the total.
- **A new helper has only one caller** → leave it inline. Extract when a second caller appears, or when the helper itself represents a distinct edit intent.
- **A proposed split satisfies the test but produces files that don't fit the "where to put new code" map** → do not split. Note in the footer; this is a reorganization signal (see below).
- **A planned reorganization calls for a split** → apply the split as planned; do not re-derive the decision.

## When the local rules don't fit

The rules above describe local decisions: where one new piece of code goes, whether one file should split. They do not describe how the codebase as a whole is organized. The choice of axis along which the code is divided (by CLI command, by storage table, by phase, by backend) is a separate question, and reshaping it is out of scope for this policy and should be raised with the maintainer.

If you find:

- a file at the split-or-flag threshold whose contents pass the edit-intent test as one intent (no clean split exists),
- a proposed split whose pieces don't fit anywhere in the "where to put new code" map,
- a pattern of edits that keep needing to touch the same set of files together for unrelated reasons, or
- a "where to put new code" entry that no longer matches what the code is actually doing,

then the local rules are misaligned with the codebase's current shape. Do not attempt a reorganization. Complete the current task within the existing structure (accepting a less-than-ideal placement if needed) and add a note to the footer describing the misalignment. The maintainer decides whether a reorganization is warranted.

## Calling out deviations

Any policy-relevant thing observed but not acted on should be mentioned in the PR description. Examples include:

- Reorganization signals from the section above.
- Small policy-relevant deviations not fixed at the time (e.g., an inline test block slightly over 300 lines, an existing file under 50 lines that doesn't fit a recognized small-file shape).
- Anything else worth flagging.

This section can be omitted when there is nothing to flag.

## Where to put new code

- **New CLI command** → new file in `app/commands/`, named after the subcommand. Add variant to `Commands` in `cli.rs`. Add dispatch arm in `main.rs`.
- **New storage operation** → pick the `engine/storage/` submodule by operation family: `database.rs` for connection/open, `chunks/` for chunk operations, `labels.rs` for label metadata. If a family outgrows a single file per the edit-intent test, split it into its own subdirectory (as `chunks/` already shows).
- **New partitioner heuristic** → `split_search.rs` for split-point logic, `node_analysis.rs` for AST node properties, `scoring.rs` for quality measurement.
- **New config field** → `app/config.rs` for app-level config, `engine/crawl_config.rs` for crawl filtering rules.
- **Shared utility** → name it for what it actually holds, narrowest accurate name. Rename when contents change.

## Module header comments

Every non-trivial file must start with a header containing these three elements:

- **Purpose:** one line.
- **Edit here when:** the change intents this file serves.
- **Do not edit here for:** common wrong guesses: point to the right file.

In Rust source files, the header is a module doc comment (`//!`). Two shapes are in use and both are compliant: the bare three-line form, and a form with a leading summary line followed by the three elements separated by blank lines. When editing an existing file, match its existing shape. When creating a new file, default to the bare three-line form.

When editing an existing file whose header doesn't match this rule, update it only if the task touches the header, moves the file, or substantially changes the file's edit intent. Otherwise leave it and optionally note in the footer.

## Test strategy

This codebase does not pursue 100% line coverage. Several specific decisions reflect the testing posture used here:

- **Pure decision logic is extracted and unit-tested; effectful orchestration is covered by integration tests at real seams.** `engine/search_decision.rs` is a pure function with focused unit tests. `app/commands/crawl.rs`'s `run_crawl_async` orchestrator is covered by `tests/active_labels_preserve.rs`, `tests/label_add.rs`, and `tests/vector_preserve.rs` running against real LanceDB, not against mocked storage.
- **Some short predicates carry one test per named input category.** `should_skip_label_cleanup` in `app/commands/crawl.rs` is a three-term boolean with four tests, one per failure category.
- **Stable user-facing output is snapshot-tested rather than asserted with substring batteries.** A snapshot diff is reviewable as a single user-experience change.
- **State invariants are checked at construction with `assert!` / `debug_assert!`.** `file_id` and `row_id` derivation in `engine/identity.rs` and identifier validation in `engine/identifier.rs` are the examples. The test suite does not duplicate these checks across input combinations.
- **Bug-fix PRs add a regression test when the bug class is plausibly re-introducible on future edits.** A miswritten conditional caught by review does not need its own test; a subtle ordering bug in a phase-gating predicate does.

## Test placement

- **Inline `#[cfg(test)]` blocks must be under 300 lines.** Below that, prefer inline when tests are tightly coupled to private items in the file; otherwise use a sibling `tests.rs`.
- **Sibling test file** (`#[cfg(test)] mod tests;` in `mod.rs`, code in `tests.rs`): default for any directory-module.
- **Integration tests** (`tests/` at crate root): CLI-level and end-to-end behavior only.

**Prefer extracting helpers and unit-testing them before reaching for integration tests.** Integration tests are slow, heavy, and tend to over-assert on incidental behavior. When the work being tested is conceptually a pure function over already-resolved inputs (a decision rule over metadata, a hydration walk over a fused-hit list, a rendering pass over a populated model), extract that function and test it inline or in a sibling `tests.rs` with hand-built fixtures. Integration tests should focus on orchestration and user-visible contract points; byte-level output shape should usually be pinned at the renderer/helper boundary. The decision-rule extraction in `engine/search_decision.rs` and the renderer pass in `app/search.rs` are existing examples of this pattern.

Within the integration-test layer itself, prefer fewer tests that exercise realistic end-to-end user paths over many tests covering edge cases one at a time. When asserting on stable user-facing output (CLI `--help`, fixed banners, structured error templates), snapshot tests are usually a better fit than a battery of substring checks: a snapshot diff shows the change in context and can be reviewed as a single user-experience change.

## Quick CI tier

### Purpose

`just ci-quick` is the fast variant of `just ci`. Same fmt, clippy, and facade checks, but with the slowest tests filtered out at runtime. It exists for the developer inner loop, the moments between edits when fast feedback matters. Repository CI workflow selection is managed separately; this section only defines the local quick tier and the invariant that `just ci` remains the full gate.

### Mechanism

A test function whose name contains `__quick_excluded` is filtered out by `cargo test -- --skip __quick_excluded`. The match is a substring against the full test path, so the suffix works wherever the test lives, in `src/` or in `tests/`. The slow code still compiles and links on every run; only its execution is skipped.

Do not conflate `__quick_excluded` with `#[ignore]`. They are orthogonal. `#[ignore]` means "do not run by default anywhere," used for tests too expensive even for full CI, flaky pending fix, or requiring external setup. `__quick_excluded` means "runs in `just ci`, skipped in `just ci-quick`." A test can carry either, both, or neither.

### Where the time goes

On CI, a clean workspace recompile is part of every run. On a developer machine with cargo's incremental cache warm, compile is usually cheap and most of the wall-clock is the tests themselves. The tagging mechanism only affects test execution, not compile, so the speedup it delivers is most visible to developers. CI timings are a nice deterministic report but not the right decision-making criterion for what to tag.

### When to retag

When `just ci-quick` stops feeling quick, retag. The diagnostic question is which tests are eating the most test-execution time. A clean `cargo test --workspace --locked` shows per-binary timings: each file under `tests/` is its own binary, and `src/` unit tests aggregate into one. The slowest binaries or the slowest tests inside them are the candidates.

There is no fixed threshold. The judgment is relative: tag the largest contributors until `ci-quick` is meaningfully faster than `ci`. If most of the time is one file, tagging that file is enough. If the time is spread evenly across many tests, the suffix mechanism is the wrong tool and the feature itself should be reconsidered.

## Naming

### File and directory names

- Command handlers: named after the CLI subcommand (`purge.rs`, `search.rs`). Use `use_cmd.rs` for `use` (reserved keyword).
- Engine submodule directories: named after the concept (`partitioner/`, `storage/`).
- Type-only files: `types.rs` or `models.rs`.
- Test files: `tests.rs` (singular).
- No semantically vapid filenames. `utilities.rs`, `helpers.rs`, `common.rs`, `misc.rs` are free to write and tell the next reader nothing; half the codebase is "utilities" of some sort. The work of naming is finding what the functions actually have in common, and that shared trait is usually a better name: `formatting.rs` if the trait is formatting, `test_mocks.rs` or `test_fixtures.rs` if the trait is test setup. `test_helpers.rs` is acceptable only when no narrower trait is visible. Pick the narrowest accurate name today; rename when contents change.

### Folder vs directory

Prefer "folder" in identifiers, prose, doc comments, error messages, and clap help text.

The cases that stay "directory":

- **Established compounds**, in their established meaning: "working directory" when it means the Git enlistment folder or `std::env::current_dir()`; "current directory" / "current working directory" for `std::env::current_dir()`; "root directory" for a filesystem root; "home directory" for `$HOME`.
- **Vendor and standard-library API surface**: type names, trait names, function names, error variants, and terms-of-art from third-party documentation. `std::fs::read_dir`, `std::fs::create_dir_all`, Tantivy's `Directory` trait, `MmapDirectory`, `OpenDirectoryError`, LanceDB's "directory-based table format" all stay as-is; renaming them would prevent readers from finding the underlying documentation.

The two cases above identify objects that keep the word "directory". Prose specifically describing one of those objects inherits the word, so the sentence agrees with the symbol it names. A doc comment on `MmapDirectory::open` says "opens the directory at the given path"; a sentence about a function called `parse_working_directory_arg` says "parses the working directory argument." This is a derived rule, not a third independent criterion.

Counter-examples: a loop variable iterating folders is `current_folder`, not `current_directory` (the first rule requires the literal `current_dir()` meaning). A test fixture holding a `TempDir` may keep `_tmp_dir`; it mirrors the crate's type, not a Monodex concept.

## Banned patterns

- No putting unrelated items together just because they're small.
- No structural splits in the same change as feature or fix work. Splits are their own change unless explicitly authorized by the maintainer or the planned reorganization being applied.

## Module organization at the directory level

A directory is an organizational unit: a name that predicts what's inside. The `mod.rs` is the directory's table of contents, holding module declarations, explicit re-exports, and the header; non-trivial code lives in named sibling files. A small number of simple constants that are part of the directory's public surface is fine; move them out when they grow into an implementation vocabulary with its own edit intent.

### Facade integrity

A directory's `mod.rs` is its public boundary: what an outside caller can reach is decided by `mod.rs` re-exports, not by the caller writing a deep path. This is a structural rule about boundaries, not a rule about minimizing visibility keywords; the goal is that each directory has exactly one declared surface, so a reader knows where to look and a caller cannot quietly bypass it. The boundary rule has three parts.

**Child modules are declared with plain `mod`.** In a directory `mod.rs`, child modules are declared `mod child;`, never `pub mod child;` or `pub(crate) mod child;`. Items that outside callers need are surfaced by explicit `pub use child::Item;` re-exports in `mod.rs`. The sole exception is a child that must currently be named from outside the library crate, from `tests/` or `main.rs` via a `monodex::...` path naming the child directly. Such a child stays `pub mod`, but only when listed in the facade-check allowlist (see below). An allowlisted `pub mod` is a deliberate, marked exception; plain `mod` is the default.

**Cross-directory reach goes through the facade.** A use site outside `a/b/c/` that reaches `crate::a::b::c::internal` by deep path is a facade violation, whatever visibility keywords are involved. The fix is a re-export in the relevant `mod.rs`, never a wider `mod` declaration. Re-export at the item's own visibility (`pub use` for a `pub` item, `pub(crate) use` for a `pub(crate)` item); do not widen the item to make re-exports uniform. List re-exports explicitly, one item per name; wildcard re-exports (`pub use child::*`) are banned, because they make the facade's surface unreadable. A deep-path reach that looks intentional means the target belongs in the facade's surface, not that the rule should bend.

**Sibling reach stays out of the facade.** An item used only by siblings in the same directory needs no `mod.rs` entry: mark it `pub(super)` and siblings reach it directly.

Two scope clarifications. *Item visibility inside a file* is not governed here. A `pub` item that is externally unreachable carries no information but is not a violation, and need not be demoted. (The `unreachable_pub` lint flags such items. It is deliberately not adopted, because it enforces a stricter, different rule of item-level minimum visibility, and an agent silencing it may add an unwanted re-export rather than improve the boundary.) *Machine check:* `just check-facades` (run by both `just ci` and `just ci-quick`) scans every `src/` `mod.rs` and fails on any `pub mod` / `pub(crate) mod` child not in the allowlist. The check is source-tree-only; `mod.rs` files under `tests/` are not policed, and integration-test fixture facades may use their own local style. The allowlist is the set of children currently named from `tests/` or `main.rs` by a direct `monodex::...` path. That set is presently an accident of what the integration tests reach, and is to be redrawn deliberately once an intended crate API surface is designed.

When the check fails on a new declaration, the allowlist is not the default remedy: re-export the needed item from the facade, or change the caller to a path the facade already exposes. Add to the allowlist only when a caller intentionally needs to name the child module itself and no facade path can serve, and call the addition out in the PR description.

**File layout.** This project uses `<dir>/mod.rs`, not the `<dir>.rs` + `<dir>/` form Rust 2018 also permits.

A directory with both production files and child subdirectories is fine when its files and the child directory share an edit intent. A directory holding only subdirectories is suspect: either the parent has an unwritten job, or one level of nesting is gratuitous.

Many files in a directory (rough sense: more than ~8) is a signal worth pausing on, not a rule. Apply the edit-intent test to the directory: name the change that would cause this directory to be edited, and check whether all the children answer the same question.

**Creating a new directory.** Two cases. Decomposing a single file into multiple production files: create `mod.rs` from the start, declaring the children and curating the surface. Extracting an inline test block via `#[cfg(test)] mod tests;` in a parent `.rs` file: Rust resolves the tests to `<parent>/tests.rs`, and the directory holds `tests.rs` only, with no `mod.rs`.

If a directory in the second form later acquires a second production child, promote it: convert the parent `.rs` to `<parent>/mod.rs`, move the original contents to a named sibling, keep `tests.rs`.

## Configuration at the edges

The program reads its environment exactly once at startup, in `main`. Past that point, the rest of the code never touches `std::env`. Inputs from the environment are parsed and validated into typed values, and the typed values are passed down as parameters.

The realization of this rule for the config folder is the `Paths` struct in `src/paths.rs`. It carries `config_folder` as a resolved `PathBuf`, with method accessors for derived files (`config_file()`, `context_file()`, `crawl_config()`). Code below `main` takes `&Paths` rather than reading `MONODEX_CONFIG_FOLDER` ambient state. The pattern is the same as Rush Stack's `IRushConfiguration`.

The class of bug this rules out: env-var cache poisoning, stale ambient state across test runs, and silent test isolation breakage when one test mutates an env var another test reads. Because the input is a parameter, parameters can't be forgotten and the bug class becomes architecturally impossible to express. New code that needs configuration takes the configuration as a parameter; if a function or module needs `Paths`, it accepts `&Paths` rather than reaching for `std::env`.

## Small files

A file under 50 lines is acceptable when it is a `mod.rs` / `lib.rs` / `main.rs` of the kinds described above, or when it contains a single type, trait, or small concentrated vocabulary that is the public contract of its module. Other small files should be folded into their parent.
