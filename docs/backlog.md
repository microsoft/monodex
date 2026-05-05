# Backlog

This is a maintainer scratch pad for sketching out what might come next for Monodex. Items are organized by priority bucket; ordering within a bucket is rough.

For official feature requests, create a GitHub issue. If an issue needs higher priority, tell us via Zulip or join the monthly Rush Hour video call.

## 1. Immediate

This bucket is for stabilization and quick wins. These items should land before or alongside the next major investment so that bigger work doesn't have to navigate around them.

**Test scenarios that need actual execution.** Several end-to-end behaviors of the crawl pipeline have never been exercised by an automated test, only by manual verification during development. The two that most need automation are interrupted-crawl resume (Ctrl+C mid-crawl, verify chunks already written persist with their label, sentinel chunks skip correctly on resume, label_metadata stays at `crawl_complete=false` until the resumed crawl finishes, label reassignment does not run on the partial crawl but does run after the successful resume) and file-moves-between-packages (move a file from `packages/A/src/example.ts` to `packages/B/src/example.ts` between two crawls of the same label, verify the new path produces new chunks via a different `file_id`, verify the old path's chunks lose the label during reassignment, verify the orphan chunks are detectable for the planned GC). This entry needs further refinement before it becomes an actionable jobsheet. Past attempts to write these tests have been claimed-done without actual execution, so the next pass should specify the exact reproduction steps and pass/fail criteria the test asserts on.

**Single-writer process lock on the database directory.** Take a lock at the start of every command that writes to the database, and release it on completion or process exit. The tool should reject overlapping crawls with a clear error identifying the holder. The current correctness story relies on LanceDB's optimistic concurrency control, which works for a single storage subsystem but won't generalize as more storage subsystems are added.

**Database-on-network-filesystem refusal.** Users can configure `database.path` to point at any absolute path. NFS, SMB, and synced cloud folders (Dropbox, OneDrive, iCloud, Google Drive) have different concurrency and durability semantics from local filesystems and are not supported. Monodex should detect these at database-open time and refuse with a clear message rather than silently misbehaving. The README's configuration section should also state this declaratively.

## 2. In active development

**Full-text search and hybrid retrieval.** Active work. Adds a second retrieval method alongside vector search, with a hybrid-default-search fusing both.

## 3. After the storage layout settles

These are known-needed features whose architectural ripples are easier to reason about once the in-flight full-text-search work has landed and the storage layout has settled.

**Benchmark suite for retrieval quality.** A repeatable evaluation measuring retrieval quality on the Rush Stack codebase against curated queries with known-good answers. The point is to make chunker changes, ranking changes, and model changes measurable rather than vibes-based. Most useful once both retrieval methods exist.

**Multi-database support.** Named-database registry in `config.json`, `--db <name>` on commands. Today exactly one database path is supported; the registry shape is a straightforward extension.

**Structured JSON output for `search` and `view`.** This is a community contribution from [PR #24](https://github.com/microsoft/monodex/pull/24). The motivating use case is composing Monodex into small scripts and editor helpers: workflows where a stable named-field interface is more useful than terminal-formatted output, even though the default text mode remains better for direct agent usage. The work introduces two formats: a full `--format=json` and a reduced `--format=json-lite` that strips the high-token-cost fields. The PR has an attached jobsheet covering the shape of the work, and the maintainer's intended implementation differs from the original PR's approach, so a future contributor should use the jobsheet as the spec rather than the PR diff.

**Watch mode.** A long-running process that periodically re-crawls or watches the filesystem for changes and incrementally updates the index. Watch mode is architecturally consequential because it changes the pipeline from one-shot to long-lived: process-level locking semantics shift, the embedding-pool lifecycle changes, and error recovery has to happen in-process rather than at the next invocation. It should be designed after the storage layout has settled.

**`monodex init` command, with `examples/` rename.** Generate `<tool-home>/config.json`, `<tool-home>/crawl.json`, and `<tool-home>/context.json` from the templates currently under `examples/`, with `$schema` URLs set to the published locations. This removes a setup step for new users. The implementation is straightforward: use `include_bytes!` to embed templates at compile time, plus a small command handler that writes them to disk with the standard "file already exists" handling. The work depends on the templates being embedded (a trivial Rust idiom; not a real blocker) and ideally on **Schema publication and Microsoft hosting** below (otherwise the `$schema` URLs are placeholders).

The directory should be renamed from `examples/` to `config-templates/` as part of this work, since the current name is a misnomer that confuses the relationship between schemas, templates, and the init flow.

## 4. Good ideas

These items have at least one non-obvious insight worth recording, but no commitment to ship. Some may never happen; they're here so the thinking isn't lost. A workaround exists for each.

**Orphan-chunks garbage collection.** A chunk with empty `active_label_ids` is invisible to label-scoped search and view but still occupies space. Orphans accumulate from interrupted crawls (chunks uploaded before label assignment finishes). The non-obvious part is that inline cleanup during crawl can't safely identify orphans. The only safe way is a full scan of the `chunks` table comparing `active_label_ids` against known labels, which requires the database to be quiescent. This works as an offline `monodex gc` command, not as continuous background work. The workaround is to run `monodex purge` and rebuild from scratch. The workaround is acceptable today because no user is running long enough that orphan accumulation matters; this should be revisited when there are users with multi-month-old databases.

**MCP server.** Expose Monodex as an MCP-compatible service so AI agent platforms can connect to it as a first-class tool rather than via CLI shell-out. The intent is for MCP payloads to map closely to existing CLI parameters; the value is the service-style lifecycle (a warm process with no per-call startup cost), not a separately-designed protocol. This is easy to add at any time, since the Rust ecosystem has MCP server libraries and the existing CLI already enumerates the operations that would map to MCP tools. It is lower priority than it might appear, because agents using the CLI today are not blocked: they shell out, parse output, and proceed.

**Search result boosting.** Currently search results are ordered by similarity score alone. Plausible boosters include recency (more-recently-edited files weighted higher), breadcrumb specificity (deeper symbol-level matches over file-level matches), package importance signals, or query-type-conditional boosts (a query that looks like an error message weighted toward error-handling code). There is no urgency until the **Benchmark suite for retrieval quality** exists and shows where current ranking falls short. Without that benchmark, boosting is parameter-tuning by vibes.

**Hardware-conditional GPU adapter.** A CUDA proof-of-concept by Nick Pape ([rushdex-prototype PR #1](https://github.com/octogonz/rushdex-prototype/pull/1)) demonstrated 4-12x speedup over CPU baseline on an RTX 3090 with batched inference. It was not merged because of intervening codebase changes, and reapplying it is non-trivial. This is lower priority than a strict speed comparison would suggest, because most crawls after the first are cheap regardless of embedding speed (the sentinel-based incremental skip path), and ongoing GPU support would require team access to the relevant hardware. A future contributor could revisit this as a hardware-conditional adapter alongside the CPU runtime; details and rejected alternatives are in [chunker.md](./design/chunker.md).

**Vector indexing (ANN).** Vector search is currently a brute-force scan over the chunks table. Acceptable at current scale on a developer laptop; if it stops being acceptable, LanceDB supports IVF and HNSW indexes via `Table::create_index`. Worth knowing the option exists before reaching for harder optimizations.

**Broader language support.** TypeScript is the only language with a custom AST partitioner today. Markdown and `lineBased` cover other text formats with simpler strategies. Adding language-specific partitioners would extend accuracy to more codebases; Go and Rust are the most likely next candidates. This is lower priority until TypeScript chunking has demonstrated itself.

**Schema publication and Microsoft hosting.** Per Rush Stack convention, JSON schemas live at `https://developer.microsoft.com/json-schemas/...` URLs hosted from the [microsoft/json-schemas](https://github.com/microsoft/json-schemas) repo. Publication is a manual procedure handled outside Monodex. Today Monodex schemas aren't published anywhere, and the templates in `examples/` have placeholder or absent `$schema` URLs. The agent maintaining Monodex should know this workflow exists, that it's manual, and that every Monodex schema change involves a coordinated step outside this repo.

**Non-Git catalog types.** Today `monorepo` is the only catalog type. "Catalog" is a generic name for a data source Monodex indexes, not a synonym for "Git repository." Other plausible types (issue trackers, discussion forums, meeting notes) are conceivable but no concrete work is planned.
