# Smoke test

This document describes a minimal end-to-end procedure for verifying that a Monodex build actually works. It exists because `cargo test` passing is not sufficient evidence. The unit tests do not exercise the embedding model, the LanceDB writer, or the full CLI surface, so a build can pass tests and still be entirely broken at runtime.

A coding agent finishing a change should run the test procedure below before claiming the change is complete. A human verifying their own build should do the same. The setup procedure runs once per machine; the test procedure runs every time.

## One-time setup

Do this once on each machine where the smoke test will run. The state created by these steps persists between runs.

### Clone Sparo

The test uses the [Sparo](https://github.com/tiktok/sparo) monorepo as the corpus. Sparo is a small open-source Rush monorepo (~266 chunks at typical commits), small enough to crawl in a few minutes and small enough that obviously-broken behavior is easy to spot.

```
git clone https://github.com/tiktok/sparo.git /path/to/sparo
```

### Configure Sparo as a catalog

Edit `~/.monodex/config.json` to register Sparo as a catalog:

```
{
  "catalogs": {
    "sparo": {
      "type": "monorepo",
      "path": "/path/to/sparo"
    }
  }
}
```

If `~/.monodex/config.json` does not exist, create it. Use the absolute path where Sparo was cloned.

### Initialize the database

```
./target/release/monodex init-db
```

The command should complete without error and create `~/.monodex/default-db/` containing `monodex-meta.json`, `chunks.lance/`, `label_metadata.lance/`, an empty `fts/` directory (per-label Tantivy indexes are created lazily on first FTS write), and a `locks/` directory used by the writer-lock layer.

This command is idempotent; running it again on an existing database is safe.

## Test procedure

Run this every time you want to verify a Monodex build. The procedure starts with a build and then a purge, so that a passing-looking search cannot be returning stale chunks from a previous build's crawl.

### 0. Build

```
cargo build --release
```

This step matters: Cargo puts binaries in different folders for debug versus release, and an agent that just modified code can easily forget which one is current. Always run this so the test runs against the build that includes your changes. The binary is at `./target/release/monodex` after a successful build.

### 1. Purge

```
./target/release/monodex purge --catalog sparo
```

The command should complete without error. After this, the database has no chunks for the `sparo` catalog.

### 2. Crawl

```
./target/release/monodex crawl --catalog sparo --label main --commit HEAD
```

The expected output is progress reporting as files are processed, then a summary indicating how many chunks were written. Typical scale for Sparo at recent commits is around 250-300 chunks.

If the crawl produces warnings about fallback splits, that is normal: those are chunker quality reports, not crawl failures. The crawl is successful as long as it completes without an error exit.

### 3. Search

```
./target/release/monodex search --catalog sparo --label main --text "invoke the shell command that clones the git repo"
```

The expected output is a preamble line, then a list of search results in `>`-prefixed format. The preamble looks like:

```
Catalog: sparo / Label: main / Searching: fts, vector
```

Each result is a header line of the form:

```
<file_id>:<chunk_ordinal> [<marker>] <breadcrumb>
```

where `<marker>` is `v`, `f`, or `f+v` indicating which retrieval method(s) ranked the result. The header is followed by three lines of the chunk's text quoted with `>`, and a blank separator. Results should look thematically related to Git repository cloning. Sparo wraps Git operations, so any code dealing with `git clone`, repository setup, or shell invocation should rank highly.

If results come back empty or as completely unrelated chunks, the search index is broken even if the crawl appeared to succeed.

### 4. Search FTS-only

```
./target/release/monodex search --catalog sparo --label main --text "GitCloneSubcommand" --retrieval fts
```

The expected preamble is `Searching: fts only`. Result markers should all be `[f]`. A specific identifier from the Sparo codebase (such as `GitCloneSubcommand` or any other class/function name visible in the source) should rank highly because lexical search matches identifiers directly.

If the FTS search returns no results for a known-identifier query, the FTS index is not populated correctly even if the crawl appeared to succeed. If it warns about the FTS state being missing, the crawl did not complete the FTS phase.

### 5. View a chunk

Pick the `file_id` from the header line of any search result (the part before the colon, e.g., `700a4ba232fe9ddc`). Run:

```
./target/release/monodex view --catalog sparo --label main --id <file_id>:1
```

The expected output is the text content of chunk 1 of that file, prefixed with breadcrumb metadata. The output should match what the search results showed for that chunk.

### 6. Set default context (optional)

```
./target/release/monodex use --catalog sparo --label main
```

After this, subsequent `search` and `view` commands can omit `--catalog` and `--label`. If this works, the context-persistence path is functional.

## Clean-slate variant

The procedure above runs against `~/.monodex/`, which is shared with the user's normal Monodex installation. For most verification work this is fine: the purge in step 1 ensures the test starts fresh, and re-using the same catalog and database between runs saves time.

A clean-slate variant runs the same test against a completely fresh tool home, with no shared state. Set `MONODEX_HOME` to a temporary directory before any of the steps:

```
export MONODEX_HOME=/tmp/monodex-smoke-test
```

Then run the **One-time setup** section against this fresh location (configure `$MONODEX_HOME/config.json`, run `init-db`), followed by the **Test procedure**. Everything Monodex reads or writes will be under `/tmp/monodex-smoke-test/` instead of `~/.monodex/`.

The clean-slate variant is slower than the normal procedure because it has to download the embedding model on first crawl, and it leaves a populated database in `/tmp/` after the test. Use the clean-slate variant only when verifying first-run behavior or reproducing a setup-time issue; otherwise, prefer the normal procedure.

## What this catches

The procedure catches the most common ways a build can be broken while unit tests still pass:

- The embedding model failed to download or load.
- The LanceDB writer is broken or produces no rows.
- The chunker silently produces zero chunks.
- The vector search query path is broken (vector lookup, label filtering, result formatting, RRF when both methods are in selection).
- The FTS index path is broken (Tantivy build, tokenization, lexical search, hit hydration).
- The view path is broken (chunk lookup by `file_id`, text reconstruction).
- The context-persistence path is broken.
- Path resolution for `~/.monodex/` is broken.

## What this does not catch

The procedure does not exercise:

- Working-directory crawls (use `--working-dir` instead of `--commit HEAD`).
- Label reassignment on re-crawls (run step 2 twice with different commits to test).
- The `audit-chunks`, `dump-chunks`, and `debug-fts` development commands.
- Schema-mismatch behavior on databases from older binary versions (covered by integration tests; the smoke test always starts from a freshly-purged catalog).
- Manifest reconciliation paths in the FTS phase. The smoke test runs a single fresh crawl, so the divergence-recovery branches that fire on stale or unreadable manifests are not exercised here.
- Any error-handling path.
- Cross-process locking behavior. Two `monodex crawl` invocations against the same catalog should serialize on the catalog lock; two against different catalogs should run in parallel. The smoke test runs commands sequentially and does not exercise this. A maintainer can verify locking manually by starting two `crawl` invocations against a larger catalog (long enough for the wait to be visible) from separate terminals: the second should print a progress message after a few seconds and both should complete successfully. Sparo crawls finish too quickly for this check to be reliable.

These are reasonable extensions when the procedure starts proving inadequate. The doc should grow as new commands and surfaces are added.
