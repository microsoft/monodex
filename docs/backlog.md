# Backlog

A sketch of directions Monodex is thinking about. Items are grouped by how settled the work is, not by scheduling. Nothing here is a commitment about what ships next.

For official feature requests, create a GitHub issue. If an issue needs higher priority, tell us via Zulip or join the monthly Rush Hour video call.

## Near term

<a id="BL38"></a>

**BL38 Test scenarios that need actual execution.** Several end-to-end behaviors of the crawl pipeline have never been exercised by an automated test, only by manual verification. The two that most need automation: interrupted-crawl resume (Ctrl+C mid-crawl, verify chunks already written persist with their label, sentinel chunks skip correctly on resume, the per-method completion flag stays false until the resumed crawl finishes, label reassignment does not run on the partial crawl but does run after the successful resume); file-moves-between-packages (move a file between packages between two crawls of the same label, verify new path produces new chunks via different `file_id`, old path's chunks lose the label during reassignment, orphan chunks are detectable for the planned GC). Sketch only; the implementing PR specifies exact reproduction steps and pass/fail criteria.

(severity=test-coverage, work=large)

<a id="BL39"></a>

**BL39 Database-on-network-filesystem refusal.** Users can configure `database.path` to point anywhere. NFS, SMB, and synced cloud folders (Dropbox, OneDrive, iCloud, Google Drive) have different concurrency and durability semantics from local filesystems and are not supported. Monodex should detect these at database-open time and refuse with a clear message rather than silently misbehaving. README's configuration section should also state this declaratively.

(severity=correctness, work=medium)

<a id="BL40"></a>

**BL40 Side-by-side dependency duplication guard.** The released binary is 152MB stripped; side-by-side copies of large dependencies (Tantivy, LanceDB, Arrow, ONNX runtime) compound this fast and are easy to introduce via transitive-dep version drift. Two-step at investigation time (`cargo tree -d`, then `cargo bloat --filter <crate>` per duplicate), one-step at steady state (a CI step that asserts no large-dependency duplicates appear, with the exact crate list tuned over time). Resolutions can be non-trivial: a downgrade or pin can break compat with a transitive dep that wanted the newer version. Current discipline (Tantivy aligned with LanceDB's transitive Tantivy version) is documented in `docs/design/architecture.md`; the broader guard keeps it enforced.

(severity=binary-size, work=medium)

<a id="BL42"></a>

**BL42 Cargo feature gating for `ort`/`tantivy`/`jieba-rs`.** Every `cargo build` of monodex unconditionally compiles `ort` (which triggers a ~50-100MB ONNX Runtime native library download via build script) and pulls in `tantivy` plus `jieba-rs` (the latter ships a 5MB Han dictionary). Runtime is correctly gated (FTS-only invocations do not construct `ParallelEmbedder` or download the Jina model; vector-only invocations do not open Tantivy), but build/distribution cost is not. Three implementation paths: keep current always-build model; add optional `vector`/`fts` Cargo features defaulting to both; make one method default with the other opt-in. The third is a strategy call as much as engineering. Adjacent to the existing dep-duplication-guard item, which deals with binary size for what gets built but not with whether you can build without the heavy deps.

(severity=build, work=medium)

<a id="BL101"></a>

**BL101 Separate CLI from invokable API.** Refactor the codebase so the CLI is a thin layer over a documented API rather than a place where algorithms live. Move useful logic out of `commands/*.rs` files and designate "public" entry points that expose complete functionality, so external scripts can drive Monodex directly without going through our CLI. The CLI becomes a client of the API rather than the source of truth. Mark everything as experimental and subject to change at any time, with no attempt to maintain compatibility for now; a later self-motivating work item will evolve this into a stable API when the lack of stability starts causing visible friction.

(severity=feature, work=medium)

<a id="BL48"></a>

**BL48 Retire the per-file sentinel mechanism.** Each chunk row carries a `file_complete` flag, and chunk 1 of every file is treated as a sentinel that flips true only after all of the file's chunks are durably written. This is a Qdrant-era workaround (Qdrant has no tables, so cross-row atomicity has to be encoded in row content). LanceDB has manifest-version atomicity, which makes the sentinel unnecessary if the writer batches all of a file's chunks into one `merge_insert` call. The "file is fully indexed" check then becomes "any chunk row exists for this file_id." Requires a small per-file buffer in the writer thread, removal of `file_complete` from the schema, and updates to all the fast-path predicates. The durability concern (chunk 1 being a marker that the previous crawl finished writing) is the load-bearing piece; the lookup half is just a Qdrant-era affordance LanceDB makes less necessary. Trigger: when sentinel mechanics start costing readability or correctness on a touching change, or when a future schema bump is rolling through anyway. Not aesthetics.

(severity=storage-refactor, work=medium)

<a id="BL51"></a>

**BL51 `monodex init` command, with `examples/` rename.** Generate `<config-folder>/monodex-config.json`, `<config-folder>/monodex-crawl-config.json`, and `<config-folder>/monodex-state.json` from the templates currently under `examples/`, with `$schema` URLs set to the published locations. Removes a setup step for new users. Implementation: `include_bytes!` to embed templates at compile time, plus a small command handler with the standard "file already exists" handling. Depends on the templates being embedded (trivial) and ideally on schema publication (otherwise `$schema` URLs are placeholders). The directory should be renamed from `examples/` to `config-templates/` as part of this work, since the current name is a misnomer.

(severity=feature, work=small)

<a id="BL58"></a>

**BL58 Rust language support.** Today TypeScript has a dedicated AST-based partitioner; markdown has its own splitter; everything else routes through generic line-based chunking. Rust is the natural next language to give a dedicated partitioner, both because Monodex itself is written in Rust (maintainer dogfooding capacity) and because the `tree-sitter-rust` grammar is mature. Other languages stay untracked; specific requests get their own items when they arrive.

(severity=feature, work=medium)

<a id="BL50"></a>

**BL50 Watch mode.** A long-running process that watches the filesystem for changes and incrementally updates the index, instead of being re-invoked for each crawl. Changes the pipeline from one-shot to long-lived: process-level locking semantics shift, the embedding-pool lifecycle changes, and error recovery has to happen in-process.

(severity=architecture-consequential, work=large)

## Good ideas

Items with at least one non-obvious insight worth recording, but no commitment to ship.

<a id="BL46"></a>

**BL46 Public benchmark suite for retrieval quality.** A repeatable evaluation that ships with the project, measuring retrieval quality on the public Rush Stack codebase against curated queries with known-good answers. Gives outside parties a shared baseline they can run themselves: anyone evaluating Monodex against another tool, anyone in the open-source community wanting to discuss tradeoffs between chunkers or ranking strategies, anyone running it on their own GitHub-visible monorepo. Most useful once both retrieval methods exist (they now do). Many of the items below explicitly trigger off this existing.

(severity=measurement-foundation, work=large)

<a id="BL47"></a>

**BL47 Multi-database support.** Named-database registry in `monodex-config.json`, `--db <name>` on commands. Today exactly one database path is supported; the registry shape is a straightforward extension.

(severity=feature, work=medium)

<a id="BL52"></a>

**BL52 Orphan reclamation garbage collection.** Three orphan kinds, swept by one `monodex gc` command: chunk-row orphans (rows in `chunks` with `active_label_ids = []`, typically from interrupted crawls; reclaimed by deleting the row), vector-payload orphans (non-NULL `vector` on a row no in-selection vector method points at; reclaimed by setting `vector = NULL`, row stays), Tantivy-directory orphans (a directory under `<db>/fts/<catalog>/<label>/` for a label whose selection no longer includes FTS, or no longer exists; reclaimed by deleting the directory). All three share the same conceptual structure (content unreferenced by any in-selection label state) and operational constraint (requires the database to be quiescent for a full scan). One feature, offline command, not continuous background work. Workaround until the verb exists: `purge` and rebuild from scratch. Revisit once databases live long enough that orphan accumulation matters in practice. Implementation note: an internal `null_vectors_for_row_ids` primitive already exists, which nulls vector columns while preserving the rows. It may be the right mechanism for vector-only invalidation or orphan cleanup.

(severity=feature, work=large)

<a id="BL53"></a>

**BL53 MCP server.** Expose Monodex as an MCP-compatible service so AI agent platforms can connect as a first-class tool rather than via CLI shell-out. MCP payloads map closely to existing CLI parameters; the value is service-style lifecycle (warm process with no per-call startup cost), not a separately-designed protocol. Priority is currently deferred because the CLI works well for the maintainers' own agent workflows. If users find the CLI insufficient for their integration, file an issue describing the gap; concrete community demand bumps the priority of this item.

(severity=feature, work=medium)

<a id="BL54"></a>

**BL54 Filter out alphanumeric-free chunks at chunking time.** The TypeScript AST partitioner can produce chunks whose text contains no alphanumeric characters at all, only punctuation and whitespace (typically trailing stretches of close-braces and semicolons). The existing whitespace-only filter does not catch these. They have non-zero line spans and sit at AST boundaries, so they look structurally legitimate, but they carry no identifiers and no useful content. FTS tokenizer correctly produces no tokens; vector embedding produces a meaningless vector that pollutes nearest-neighbor results without any visible warning. On rushstack about 0.05% of chunks fall into this category. Fix: chunker-side filter analogous to the whitespace-only filter. Not the same as suppressing the FTS zero-token warning at indexing time (that is a downstream symptom). Bumping `CHUNKER_ID` would force re-indexing; selective change to drop a chunk is benign without a bump (missing chunks won't be in newly-crawled labels; older labels keep them until re-crawled). Trigger: when the FTS-phase summary line reports non-trivial zero-token skips, or when the planned benchmark shows these chunks dragging down vector recall.

(severity=quality, work=small)

<a id="BL55"></a>

**BL55 Search result boosting.** Currently search results are ordered by similarity score alone. Plausible boosters: recency, breadcrumb specificity (deeper symbol-level matches over file-level), package importance signals, query-type-conditional boosts (error-message-looking queries weighted toward error-handling code). No urgency until the benchmark suite exists and shows where current ranking falls short; boosting without that signal is parameter-tuning without measurement.

(severity=quality, work=medium)

<a id="BL56"></a>

**BL56 Hardware-conditional GPU adapter.** A CUDA proof-of-concept by Nick Pape (rushdex-prototype PR #1) demonstrated 4-12x speedup over CPU baseline on an RTX 3090 with batched inference. The PR doesn't apply against current main due to intervening codebase changes; the same idea would need to be reimplemented. The maintainers will pick this up for the hardware we use when crawl speed actually pressures us, but most crawls after the first are cheap regardless of embedding speed (sentinel-based incremental skip), so that pressure has not arrived. If you have different hardware (CUDA, Metal, ROCm) and want faster crawls on it, a contributor PR adding that GPU path alongside the CPU runtime is welcome.

(severity=performance, work=medium)

<a id="BL57"></a>

**BL57 Vector indexing (ANN).** Vector search is currently a brute-force scan over the chunks table. Acceptable at current scale on a developer laptop; if it stops being acceptable, LanceDB supports IVF and HNSW via `Table::create_index`. Worth knowing the option exists before reaching for harder optimizations.

(severity=performance, work=small)

<a id="BL60"></a>

**BL60 Non-Git catalog types.** Today `monorepo` is the only catalog type. "Catalog" is a generic name for a data source Monodex indexes, not a synonym for "Git repository." Other plausible types (issue trackers, discussion forums, meeting notes) would be natural next steps.

(severity=feature, work=large)

<a id="BL63"></a>

**BL63 RRF tuning surface.** Per-method weights, configurable `k`, configurable candidate window. Triggered by retrieval-quality benchmarking showing measurable wins. The benchmark suite is the precursor. `k = 60` is currently hardcoded.

(severity=tuning, work=small)

<a id="BL67"></a>

**BL67 `monodex upgrade-db` verb.** The forward story for schema changes. `monodex_schema_version` exists in `monodex-meta.json` but no migration verb does. Today's policy (refuse to open old DBs with a clear error, tell users to delete and re-crawl) is appropriate while no user has a database old enough that recrawl is non-trivial. Trigger: the first user with a database large enough that recrawl is painful. The schema-mismatch error message already names this verb.

(severity=feature, work=large)

<a id="BL68"></a>

**BL68 Orphaned per-catalog lockfile cleanup command.** Per-catalog lockfiles get created lazily and never deleted; the lockfile directory grows monotonically as catalogs come and go. Bounded and tiny per the design's framing in `concurrency.md:134` and `:168`, but a real loose end with no current owner. A future maintenance command can sweep orphaned per-catalog lockfiles for catalogs no longer in `monodex-config.json`.

(severity=hygiene, work=small)

## Deferred

Items here are deferred with a stated rationale or trigger condition. The intent is to record both the idea and the reason it's not being acted on, so a future contributor (or future maintainer) can see whether the conditions have changed before re-proposing.

<a id="BL23a"></a>

**BL23a Tokenizer offsets remain zero (deliberate simplification with a documented trigger).** `MonodexFtsTokenStream` sets `offset_from` and `offset_to` to `0` for every token. This is a deliberate choice that keeps the tokenizer implementation smaller and has no observable downside under current Monodex paths. Tantivy's `QueryParser` builds phrase queries from `token.position` only (no offsets used in phrase matching; `test_phrase_query_matches_sequential_tokens` is consistent with that). The only Tantivy consumer of offsets is `SnippetGenerator` for result highlighting, and Monodex does not use it (preview lines come from LanceDB-stored chunk text via `render_preview_lines`). Trigger to revisit: before any Tantivy-backed snippets, highlighting, or exact-hit-span features are added. Any tokenizer behavior change at that point should also be considered against the `FTS_TOKENIZER_ID` policy.

(severity=simplification, work=small)
