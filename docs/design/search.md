# Search

This document expands on the search side of Monodex: the two retrieval methods, the decision rules that pick which method runs for a given query, hybrid fusion, the tokenizer, and the output format. Read [architecture.md](./architecture.md) first; this doc assumes the data model and the retrieval-methods vocabulary from there.

The relevant source files are `src/app/commands/search.rs` (top-level command handler), `src/app/search.rs` (renderer and orchestration), `src/engine/search_decision.rs` (pure decision-rule evaluation), `src/engine/fusion.rs` (RRF), and `src/engine/fts/` (Tantivy-backed full-text search).

## Retrieval methods

Monodex ships two retrieval methods: `vector` (semantic similarity over chunk embeddings) and `fts` (lexical search over chunk text via Tantivy). Each is queried independently and produces ranked `row_id` results scoped to a label. They expose engine APIs as peers:

- `vector_search(label, embedding_query, limit)` over the `chunks` table's `vector` column.
- `fts_search(label, text_query, limit)` over the per-label Tantivy index at `<database-folder>/fts/<catalog>/<label>/`.

Each label carries a **retrieval selection**: the set of methods built for it. The selection is set at crawl time via `--retrieval` (see [crawl.md](./crawl.md)) and consulted at search time. A method not in the selection cannot be queried for the label.

Score values from the two methods (BM25 from FTS, distance from vector) are diagnostic-only and not comparable across methods. Cross-method ranking happens via reciprocal rank fusion (below), which uses ranks rather than scores.

## CLI surface

`monodex search` accepts a repeatable `--retrieval <method>` flag that filters which methods this query reads. Without the flag, all in-selection methods participate.

```bash
monodex search --text "..."                                 # query everything in selection
monodex search --text "..." --retrieval vector              # vector only
monodex search --text "..." --retrieval fts                 # fts only
monodex search --text "..." --retrieval vector --retrieval fts  # explicit hybrid
```

Asking for a method not in the label's selection is an error. Asking for a method that is in the selection but incomplete (its phase did not finish in the most recent crawl) behaves differently depending on how the request was framed:

- Without `--retrieval`: incomplete methods are filtered out of the active subset (with a warning per filtered-out method). If nothing remains, the search errors.
- With explicit `--retrieval`: requested incomplete methods stay in the active subset. The user asked for them by name, so the search proceeds with a warning rather than dropping them.

The full rule is in "Decision rules" below.

The search preamble names the methods being queried:

```text
Catalog: rushstack / Label: main / Searching: fts, vector
Catalog: rushstack / Label: main / Searching: fts only
```

The comma-joined form is used both for hybrid (`fts, vector`) and for the crawl-time `Label:` line; the helper is `format_selection` in `engine/retrieval.rs`. The hybrid preamble does not advertise "RRF"; per-result markers (below) carry the hybrid signal.

## Decision rules

The `decide` function in `engine/search_decision.rs` is a pure function from `(label_metadata, requested_methods)` to a structured `Decision` outcome. It runs without I/O, without backend dispatch, and is unit-testable in isolation.

The rules:

1. **Compute the active subset.** Start from the persistent retrieval selection (methods whose `<method>_source` column is non-NULL). If `--retrieval` was passed, intersect with the requested set. Without `--retrieval`, filter out methods whose `<method>_complete` is false (and emit a warning per filtered-out method).
2. **Dispatch on the active subset.**

| Active subset size | Source state | Behavior |
|--------------------|--------------|----------|
| 0 | n/a | Error. Either the persistent selection is empty, or every in-selection method is incomplete and got filtered out. |
| 1 | n/a | Use that method. |
| 2+ | sources equal | Hybrid (RRF). |
| 2+ | sources disagree | Hard error naming the per-method sources and the crawl command to reconcile. |

The "sources disagree" row is unreachable through normal CLI flows: every crawl upserts all in-selection method sources to the same value. The row is kept as a defensive invariant guard. Working-directory crawls record per-crawl-unique sentinels as their source. A single working-dir crawl writes the same sentinel to all in-selection methods in one upsert, so default hybrid still works against working-dir labels. Two separate selective working-dir crawls (e.g. `--retrieval fts`, then later `--retrieval vector`) produce different sentinels per method and will hit the disagree row on a subsequent default search; the conservative-unequal answer is correct because working-tree changes between crawls cannot be cheaply detected.

## Hybrid retrieval (RRF)

When the active subset has 2+ methods with equal sources, search runs each method, fuses the ranked lists by reciprocal rank fusion, and renders the fused top-K.

The formula:

```text
rrf_score(row_id) = sum over methods of: 1 / (k + rank_in_method)
```

Ranks are 1-indexed. A `row_id` absent from a method's candidate list contributes nothing from that method. The constant `k = 60` is hardcoded; the literature converges on this value as the empirical default and exposing it as a knob would invite premature tuning.

Rank-only fusion is load-bearing. BM25 scores and vector distances live on different scales with opposite directions (BM25 higher-is-better, distance lower-is-better). Rank-only fusion sidesteps the score-normalization problem entirely.

### Candidate window

Each method fetches more than `--limit` candidates so fusion has room to reorder:

```text
candidate_limit = max(user_limit, 50)
```

The 50-element floor gives the typical default `--limit 10` a 5x over-fetch, enough to see real cross-method swaps. For `--limit > 50`, the marginal value of additional over-fetch declines fast and RRF's rank-only nature prevents over-fetch from starving either method out of the top-K. No multiplicative formula, no CLI knob.

### Tiebreak

RRF ties are routine. Tiebreak is lexicographic across four levels:

1. Fused RRF score, descending.
2. Best contributing rank, ascending. The minimum rank across all methods that ranked the row_id.
3. Contributor method in `RetrievalMethod` enum order. The enum is alphabetical (`Fts` before `Vector`); the row_id whose best rank came from the earlier method wins.
4. `row_id` lexicographic ascending. Final fallback for genuine indistinguishability.

A row_id whose best rank is held by multiple methods uses the earliest such method for level 3.

### Duplicate row_ids within a single method's list

Tantivy's commit window can in rare cases produce duplicate physical documents for the same logical `row_id`. Fusion is defensive: within a single method's input list, the first occurrence of each `row_id` is kept and later duplicates are ignored. This preserves the best (lowest) rank and prevents the row from double-contributing to its own score.

### Stale hydration and fill-from-lower-candidates

Fused row_ids are hydrated from LanceDB by `row_id` in one bulk fetch (not per-row lookups). A `row_id` may fail to hydrate (the chunk was deleted between FTS index time and search hydration). The hydration loop walks fused candidates in order, emits the first `limit` that successfully hydrate, and stops. Failed hydrations get an inline warning emitted just before the next successful result; missing slots backfill from the candidate-window over-fetch.

At very large `--limit` the rule degenerates: with `--limit >= 50`, `candidate_limit` equals `user_limit` and there is no spare capacity to backfill. Stale hydrations just shorten the output. The default `--limit 10` is the common case where backfill works.

## Backend failure semantics under hybrid

Either retriever can fail or degrade. The rules:

- **FTS `ParseError`**: hard error. The user typed something with FTS-meaningful syntax (a quote, a colon, a field-prefix); silently degrading to vector-only would surface results that miss the user's evident intent.
- **FTS `NoIndex`** (the folder genuinely doesn't exist; most likely a concurrent `purge --catalog` between metadata read and FTS open): warn and degrade to vector-only. Fires only when `fts_complete = true`; the `fts_complete = false` case is covered by the upstream incomplete-method warning and would be a duplicate.
- **FTS returns zero hits**: not a failure. Fusion proceeds with vector-only candidates; results show `[v]` markers.
- **Vector embedder/backend failure**: hard error. Vector failures are infrastructure problems (model load, ONNX runtime, LanceDB I/O); silently degrading would mask them.
- **Vector returns zero hits**: not a failure. This is reachable only when the label's chunk set is empty for vector (vector search is nearest-neighbor; non-empty corpus always returns the nearest chunks regardless of relevance).
- **Both methods return zero hits**: print `No results.` regardless of preceding warnings. The `No results.` line is a load-bearing tool signal: agents and machine consumers rely on its presence to mean "zero results" and its absence to mean "results follow."

Under single-method search (`--retrieval fts` against a label whose `fts_complete = true` but the on-disk folder is missing), the same load-bearing rule applies: a NoIndex warning fires, then `No results.` follows. There is no fallback path in single-method mode, so the warning makes the missing-state visible and `No results.` carries the zero-results signal that consumers rely on.

The `NoIndex` rule is the most asymmetric. It exists because `NoIndex` is the one failure mode that genuinely is "the data is gone, but the user's query is fine"; every other failure either reflects a user-input problem or an infrastructure problem.

## Sequential orchestration

The two retrievers run sequentially: FTS first, then vector, then fuse, then hydrate, then render. FTS-first is a fail-fast optimization; FTS `ParseError` is a hard error in hybrid, and collecting FTS first means a malformed query fails before paying the cost of `ParallelEmbedder::new()` plus query encoding on the vector side. Within a long-running process where the embedder is already constructed, the savings shrink to the per-query encoding cost.

Cosmetic `tokio::join!` would not buy real parallelism: both backends do meaningful synchronous work before their first `.await` (vector path: ONNX inference; FTS path: Tantivy index open and BM25 search). Real parallelism would require isolating blocking work onto separate runtime threads and dealing with thread-pool oversubscription between ONNX, LanceDB, and Tantivy. Sequential is the right MVP shape.

## Tokenizer

A single tokenizer for all FTS content: identifier-aware splitting plus Jieba word-segmentation for runs of CJK characters. Definition lives in `src/engine/fts/tokenizer.rs`.

Splitting rules:

- Split on case transitions, underscores, dots, digit boundaries, and ASCII whitespace and punctuation.
- Keep both the original token and the splits. `getUserProfile` produces `getuserprofile`, `get`, `user`, `profile`.
- At an upper-to-lower transition inside an identifier, the last uppercase character joins the following word. `HTTPServer` produces `httpserver`, `http`, `server` (not `https`, `erver`).
- Lowercase all tokens.
- No stemming. `parsing` and `parses` should not match `parse`; in code these are distinct symbols.
- No stop words. Every word might be a meaningful identifier.
- For runs of CJK characters, use Jieba word-segmentation. The Jieba dictionary is loaded once per process via `OnceLock`.

The same tokenizer is applied at index time and query time; the QueryParser is configured to use the `text` field's tokenizer for query parsing.

The tokenizer's behavior is versioned by the `FTS_TOKENIZER_ID` constant (alongside `FTS_SCHEMA_ID`). Changes to either invalidate existing FTS state and force a per-label rebuild on next crawl. The IDs are deliberately separate from `EMBEDDER_ID` and `CHUNKER_ID` so a tokenizer tweak does not force re-embedding.

## Output format

Each search result is rendered as a header line followed by up to three preview lines from the chunk's text, prefixed with `> `:

```text
36b72a1c3184612c:6 [f+v] website:security.md:assumption-shell-environment-variables-are-trusted
> ## Assumption: Shell environment variables are trusted
> 
> For the most part, the `git` CLI assumes that the shell environment variables are trusted...
```

The header line shape is `<file_id>:<chunk_ordinal> [<marker>] <breadcrumb>`. The marker is `v`, `f`, or `f+v` indicating which retrieval method(s) contributed to the result's ranking. Marker order is alphabetical (`f` before `v`); the two-method form is `f+v`. The marker is shown unconditionally on every result, including single-method searches, so an agent parsing mixed-mode transcripts does not need to special-case.

The `> ` preview-line prefix makes code lines visually distinct from surrounding tool output and prevents prompt-injection attacks from search results.

### Debug continuation

When the global `--debug` flag is passed, each result is followed by a `Debug:` continuation line surfacing method-local diagnostic scores:

```text
36b72a1c3184612c:6 [f+v] website:security.md:assumption-shell-environment-variables-are-trusted
Debug: rrf=0.0323, fts_bm25=1.754, vector_distance=0.234
> ## Assumption: Shell environment variables are trusted
> ...
```

Score keys are method-prefixed (`fts_bm25`, `vector_distance`) so the line is unambiguous when only one method contributed to a hybrid result. Precision: `rrf=` uses four decimal places; `fts_bm25=` and `vector_distance=` use three. RRF scores cluster in the 0.02-0.04 range for hybrid hits, where four decimals discriminates near-ties.

Under hybrid, `rrf=` appears for every result, even results with only one contributor (`[v]`-only or `[f]`-only): every result under hybrid went through fusion. Under single-method search, `rrf=` is absent.

### End-of-results sentinel

When the rendered result list is shorter than `--limit` and the underlying retrieval was genuinely exhausted, an `End of results` line follows the last result. The full rule: rendered count is non-zero, rendered count is less than `--limit`, and every active backend returned strictly fewer than `candidate_limit` results.

The third condition is the "genuinely exhausted" check. If either backend saturated its candidate window, there might be more matching content beyond the window and the sentinel cannot honestly fire. The sentinel rarely fires under hybrid against a real corpus because vector search is nearest-neighbor and saturates `candidate_limit` for any non-trivially-small label; when it does fire under hybrid, both methods after over-fetching aggressively still could not produce a full result set, and that is an actionable signal to callers: broadening the same query is unlikely to help.

### Renderer routing

All search-time output (preamble, warnings, results, sentinels) goes through a single renderer that takes `&mut dyn Write`. Production wires this to stdout. Tests pass `&mut Vec<u8>` and assert on the resulting bytes. Search-path warnings (FTS NoIndex degradation, stale-hydration skip notices, incomplete-method warnings) all flow through the same writer; they do not split between stdout and stderr.

This is a deliberate departure from the `eprintln!` pattern used elsewhere. A user piping `monodex search` to `grep` will pull warning lines along with results. The trade-off is testability and ordering: with one writer, the renderer decides where each piece of output goes rather than letting stdout/stderr buffering produce non-deterministic interleavings.

Output ordering is fixed by the renderer:

1. Preamble first.
2. Decision-time warnings in `RetrievalMethod` enum order.
3. Search-time pre-result warnings (FTS NoIndex degradation).
4. Result block, with `Debug:` continuations when applicable, with stale-hydration warnings emitted inline immediately before the next successful result.
5. End-of-results sentinel or `No results.`, if applicable.

Stderr is preserved for output that is outside the renderer's scope: crawl-time warnings, error messages from `anyhow` propagation, and panic-style infrastructure noise.

## debug-fts

`monodex debug-fts --catalog <C> --label <L> --id <chunk_id> [--query <Q>]` is a diagnostic command for FTS issues:

- Without `--query`: shows the tokens the configured tokenizer produced for the chunk's text. Addresses the "wrong tokens" failure mode, which is the most common cause of "FTS can't find a thing I know is there."
- With `--query`: also runs Tantivy's `Searcher::explain` for the chunk and the parsed query. Addresses the "wrong ranking" failure mode.

The arguments diverge meaningfully across retrieval methods (FTS-debug needs `--query` parsed into Tantivy's query AST; a hypothetical vector-debug would need different inputs entirely), so this is a standalone command rather than a subcommand of a generic `debug`.

The `tantivy-cli` crate is also usable against the on-disk FTS index for advanced introspection. The folder layout is in [monodex_files.md](./monodex_files.md).

## Concurrency

Search acquires no Monodex locks. A reader runs concurrently with a writing `monodex crawl` in another process. Tantivy supports multiple `IndexReader`s alongside one `IndexWriter`, with readers seeing the last-committed snapshot. LanceDB readers see committed table state.

A reader during a concurrent `purge --catalog X` may encounter folder-disappearance errors as the purge unlinks `<database-folder>/fts/<X>/`. The FTS read paths use typed-error discrimination: a `NotFound` from any Tantivy operation (open, search, segment access) on a per-label FTS path normalizes to `FtsSearchOutcome::NoIndex`, which surfaces as the "FTS state missing" warning rather than a raw IO error. Other Tantivy errors (corruption, mmap failures that are not `NotFound`) remain hard errors. See [concurrency.md](./concurrency.md) for the full reader contract.
