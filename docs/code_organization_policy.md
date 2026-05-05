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

Lines = production code excluding `#[cfg(test)]` blocks and the module header. `mod.rs`, `lib.rs`, and `main.rs` files containing only declarations, re-exports, and small dispatch logic are not counted. Implementation-heavy `mod.rs` files are classified by what they contain and follow that row. A `mod.rs` full of algorithm code is an algorithm module, not exempt because of its filename.

Production modules that don't fit a more specific row use the algorithm/engine row.

The thresholds fire when the agent makes a non-trivial addition to a file. They do not require re-litigating files that the agent is only reading or making minor edits to.

**At the review threshold:** ask whether the new addition fits the file's existing edit intent. If it does, proceed. If it doesn't, the addition belongs in a different file. Find or create the right one.

**At the split-or-flag threshold:** apply the test below. The result is either a split (the test authorizes one) or a note in the "Out-of-scope notes" footer (the test is inconclusive or no clean split exists).

### Test before splitting

Name each proposed file and complete this sentence for it:

> "Edit this file when changing **\_\_**."

The split is valid only if all of the following hold:

- The two answers are different kinds of work, not the same work described two ways. "Predicate construction" and "predicate validation" are the same work. "Predicate primitives" and "label metadata storage" are different work.
- Neither answer is "helpers", "shared logic", "misc", or "utilities".
- Each answer names a change visible in existing code, tests, docs, or the current task: not a hypothetical future change.

If any of these fails, do not split. Note the situation in the footer instead.

### Calibrating judgement: signal → response

When working on a file, the agent will encounter situations that _look like_ split signals but aren't. Use this map:

- **The file crossed a threshold** → apply the test above. The threshold is not itself authorization to split.
- **A function is long but belongs to the same edit intent** → leave it. Length within one intent is not a split signal.
- **A type has many trait impls** → leave them with the type. Multiple impls are one edit intent (the type's behavior).
- **Inline tests make the file look large** → check the production-code line count against the threshold, not the total.
- **A new helper has only one caller** → leave it inline. Extract when a second caller appears, or when the helper itself represents a distinct edit intent.
- **A proposed split satisfies the test but produces files that don't fit the "where to put new code" map** → do not split. Note in the footer; this is a reorganization signal (see below).
- **A jobsheet directs a split** → apply the split as specified. The jobsheet is the authorization; do not re-derive the decision.

## When the local rules don't fit

The rules above describe local decisions: where one new piece of code goes, whether one file should split. They do not describe how the codebase as a whole is organized. The choice of axis along which the code is divided (by CLI command, by storage table, by phase, by backend) is a separate question, and reshaping it is the human's job.

If the agent finds:

- a file at the split-or-flag threshold whose contents pass the edit-intent test as one intent (no clean split exists),
- a proposed split whose pieces don't fit anywhere in the "where to put new code" map,
- a pattern of edits that keep needing to touch the same set of files together for unrelated reasons, or
- a "where to put new code" entry that no longer matches what the code is actually doing,

then the local rules are misaligned with the codebase's current shape. Do not attempt a reorganization. Complete the current task within the existing structure (accepting a less-than-ideal placement if needed) and add a note to the footer describing the misalignment. The human decides whether a reorganization is warranted.

## Out-of-scope notes (footer)

The agent ends its jobsheet completion report with a section titled `## Out-of-scope notes`. If the agent is operating without a jobsheet, this section goes at the end of its final report instead. The section captures things the agent observed but did not act on:

- Reorganization signals from the section above.
- Small policy-relevant deviations the agent noticed but didn't fix (e.g., an inline test block slightly over 300 lines, an existing file under 50 lines that doesn't fit a recognized small-file shape).
- Anything else the agent thinks the human should know about that wasn't on the jobsheet.

If there's nothing to report, the section is omitted.

This section is written when the agent is marking off final checkboxes, not in the middle of code edits. Reorganization questions are easier to think about with the work complete.

## Where to put new code

- **New CLI command** → new file in `app/commands/`, named after the subcommand. Add variant to `Commands` in `cli.rs`. Add dispatch arm in `main.rs`.
- **New storage operation** → pick the `engine/storage/` submodule by operation family: `database.rs` for connection/open, `chunks/` for chunk operations, `labels.rs` for label metadata. If a family outgrows a single file per the edit-intent test, split it into its own subdirectory (as `chunks/` already shows).
- **New partitioner heuristic** → `split_search.rs` for split-point logic, `node_analysis.rs` for AST node properties, `scoring.rs` for quality measurement.
- **New config field** → `app/config.rs` for app-level config, `engine/crawl_config.rs` for crawl filtering rules.
- **Shared utility** → `engine/util.rs` for engine-wide, `app/util.rs` for app-wide formatting/display. Only for helpers with multiple real call sites.

## Module header comments

Every non-trivial file must start with a header containing these three elements:

- **Purpose:** one line.
- **Edit here when:** the change intents this file serves.
- **Do not edit here for:** common wrong guesses: point to the right file.

In Rust source files, the header is a module doc comment (`//!`). Two shapes are in use and both are compliant: the bare three-line form, and a form with a leading summary line followed by the three elements separated by blank lines. When editing an existing file, match its existing shape. When creating a new file, default to the bare three-line form.

When editing an existing file whose header doesn't match this rule, update it only if the task touches the header, moves the file, or substantially changes the file's edit intent. Otherwise leave it and optionally note in the footer.

## Test placement

- **Inline `#[cfg(test)]` blocks must be under 300 lines.** Below that, prefer inline when tests are tightly coupled to private items in the file; otherwise use a sibling `tests.rs`.
- **Sibling test file** (`#[cfg(test)] mod tests;` in `mod.rs`, code in `tests.rs`): default for any directory-module.
- **Integration tests** (`tests/` at crate root): CLI-level and end-to-end behavior only.

## Naming

- Command handlers: named after the CLI subcommand (`purge.rs`, `search.rs`). Use `use_cmd.rs` for `use` (reserved keyword).
- Engine submodule directories: named after the concept (`partitioner/`, `storage/`).
- Type-only files: `types.rs` or `models.rs`.
- Test files: `tests.rs` (singular).

## Banned patterns

- No files named `helpers.rs`, `common.rs`, or `misc.rs`.
- No wildcard re-exports (`pub use submodule::*`). List re-exports explicitly.
- No putting unrelated items together just because they're small.
- No structural splits in the same change as feature or fix work, unless a jobsheet directs the split with a stated reason. Splits without that authorization are their own change.

## Small files

A file under 50 lines is acceptable when it is a `mod.rs` / `lib.rs` / `main.rs` of the kinds described above, or when it contains a single type, trait, or small concentrated vocabulary that is the public contract of its module. Other small files should be folded into their parent.
