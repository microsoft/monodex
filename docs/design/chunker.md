# Chunking

This document covers the algorithms that split files into the units Monodex embeds and indexes. Three strategies exist: TypeScript AST partitioning (the bulk of the engineering investment), markdown heading-based partitioning, and generic line-based splitting. The dispatcher in `src/engine/chunker.rs` picks one per file by extension, using the loaded crawl config (see `src/engine/crawl_config.rs`).

The choice of chunking is fundamental, not just an implementation detail. The same chunks are the unit of both vector and full-text retrieval. Chunk-size targets, AST boundary detection, and quality scoring all flow from this central choice. Future tuning is expected.

## Embedding runtime and the chunk-size budget

Chunk size is determined by the embedding model. The current model is `jinaai/jina-embeddings-v2-base-code`, identified internally as `jina-embeddings-v2-base-code:v1` (see `EMBEDDER_ID` in `src/engine/util.rs`).

| Property   | Value                                                            |
| ---------- | ---------------------------------------------------------------- |
| Max tokens | 8192                                                             |
| Dimensions | 768                                                              |
| Model size | ~612 MB (FP32 ONNX)                                              |
| License    | Apache 2.0                                                       |
| Trained on | Code and documentation (github-code, ~150M code-docstring pairs) |

The model was selected for four reasons that still hold:

- **8192-token context** lets most functions and most markdown sections fit in a single chunk; smaller-context models (typical 512-token sentence-transformer models) would force aggressive splitting that fragments meaning.
- **Code-specific training** means the embeddings understand a wide range of programming languages and markup formats as code rather than as prose. Languages relevant to Rush Stack include TypeScript, CSS, HTML, Go, Rust, and Markdown; the model's full supported list is broader (see the model card on Hugging Face).
- **Docstring awareness** means natural-language queries like "how to read JSON files" can match `JsonFile.load()` even when the call site doesn't use the same words.
- **ONNX Runtime portability** runs well on commodity developer hardware including Apple Silicon, with no dependency on a specific accelerator.

**Target chunk size is 6000 characters.** The 8192-token model limit corresponds to roughly 6500-7500 characters of code in practice; the 6000 target leaves headroom for the breadcrumb prefix and tokenizer variance. Going closer to the model's maximum was tested and rejected: at very long lengths, semantic embedding quality degrades. The model's attention pools across more content and produces a more diffuse representation. 6000 characters is a balance between fitting whole semantic units (functions, classes, sections) and keeping the embedding focused. The constants `TARGET_CHARS` and `SMALL_CHUNK_CHARS` live in `src/engine/partitioner/types.rs`.

### Runtime

The runtime is ONNX Runtime CPU with a pool of parallel sessions, auto-tuned to system memory and core count via `src/engine/system_info.rs`. Each session processes one chunk at a time; the pool size is what provides parallelism. Session count and intra-op threads per session are computed deterministically from total RAM and physical core count (see the `"auto"` heuristic in `src/app/config.rs`). On a typical multi-core machine this runs around 12 ms per chunk. Implementation in `src/engine/parallel_embedder.rs`.

Embedding speed on CPU is sufficient for the design's amortization model: the first crawl of a repository pays the full embedding cost, and subsequent crawls skip unchanged files via sentinel checks (see [crawl.md](./crawl.md)), making per-commit incremental crawls cheap regardless of how fast or slow the first crawl was.

### Empirical findings on alternative runtimes

Several runtime alternatives have been investigated. The findings are platform-conditional in some cases, which matters when reasoning about future contributions.

**Apple CoreML / Metal GPU.** Significantly slower than CPU on tested Apple Silicon (M3 generation). The Jina v2 base code model is shallow at 12 transformer layers, tensors are small, and JIT-compilation plus data-transfer overhead dominate the per-inference cost. Not a useful path on Apple GPUs for this workload.

**CUDA on NVIDIA workstations.** A proof-of-concept by Nick Pape demonstrated approximately 4-12x speedup over CPU baseline on an RTX 3090 with batched inference. See [rushdex-prototype PR #1](https://github.com/octogonz/rushdex-prototype/pull/1) for the implementation and benchmark numbers; that PR predates the rename to Monodex and was never merged because of intervening codebase changes. Batching is essential on GPU — single-item inference offers no benefit due to kernel-launch overhead — and batch sizes are bounded above by variable-length-padding cost (attention is O(n²) in sequence length, so padding short sequences to match long ones wastes compute). A future contributor could revisit this as a hardware-conditional adapter alongside the CPU runtime; ongoing maintenance would require team access to the relevant hardware.

**CPU batching.** Slower than parallel single-item processing on CPU. Variable-length sequences require padding, attention is O(n²) in sequence length, and there's no GPU to amortize kernel-launch overhead. The parallel-sessions strategy gets the same parallelism benefit without the padding cost.

**INT8 quantization.** Approximately 2x faster than FP32 with 1-2% similarity-score accuracy loss. The accuracy cost was judged unacceptable for semantic search at the time of evaluation. Worth revisiting if quantization tooling improves or if a future use case has different accuracy tolerances.

## Dispatcher

`src/engine/chunker.rs` is the entry point. It looks up the chunking strategy for a file's extension in the loaded crawl config and routes to one of three partitioners. Each partitioner takes the file content, a `PartitionConfig` (target size, file name, package name for breadcrumbs), and the catalog name, and returns a sequence of `PartitionedChunk` records. The dispatcher then wraps each chunk with shared metadata: `file_id` (computed from `embedder_id + chunker_id + blob_id + relative_path`), `row_id`, content hash, breadcrumb prefix, and the active label list.

| Strategy     | File extensions                             | Source                               |
| ------------ | ------------------------------------------- | ------------------------------------ |
| `typescript` | `.ts`, `.tsx`                               | `src/engine/partitioner/`            |
| `markdown`   | `.md`                                       | `src/engine/markdown_partitioner.rs` |
| `lineBased`  | configurable (e.g., `.txt`, `.css`, `.yml`) | inline in the dispatcher             |

## TypeScript AST partitioning

The TypeScript partitioner is the part with substantial design content. It exists in `src/engine/partitioner/`, split across `partition.rs` (orchestrator), `split_search.rs` (the recursive descent), `node_analysis.rs` (AST node helpers), `scoring.rs` (quality measurement), `types.rs` (configuration and chunk types), and `debug.rs` (logging hooks).

### The two worlds model

The algorithm separates two concerns that look like they should be coupled but aren't:

- **Sizing world.** Treats the file as a sequence of line ranges. Knows the target chunk size and minimum-chunk-size constraints. Has no opinion about what an AST node is or what makes a "good" split point: only about whether a split produces well-sized chunks.
- **AST world.** Walks the tree-sitter syntax tree. Provides candidate split points at semantic boundaries (function boundaries, class members, statement-block children, JSX element children). Has no opinion about chunk sizes: it just describes structure.

The partitioner's job is to coordinate between them: ask the AST world for candidate splits within a given line range, ask the sizing world whether any of those splits produces an acceptable partition, and recurse on the resulting halves until everything fits.

The model is useful because it keeps the two kinds of code reasoning isolated. Changes to chunk-size targets or minimum-size constraints don't require touching AST traversal logic. Changes to the AST node-kind taxonomy don't require revisiting chunk arithmetic.

### Split scopes and transparent conduits

The AST world classifies tree-sitter node kinds into two categories:

A **split scope** is a node whose direct children define legal split boundaries. Examples: `program` (the file root, children are top-level statements); `class_body` (children are methods and fields); `statement_block` (children are statements). Splitting a file means picking a line that falls between two children of some split scope.

A **transparent conduit** is a node that wraps a deeper structure but doesn't itself define split boundaries. Examples: `class_declaration` wraps a `class_body`; `if_statement` wraps `statement_block` children; `arrow_function` wraps `statement_block`; `return_statement` and `throw_statement` may wrap call expressions that contain object literals with method values. The descent walks through conduits to reach the next split scope below.

The full lists of split scopes and transparent conduits are in `is_split_scope()` and `is_transparent_conduit()` at the top of `src/engine/partitioner/split_search.rs`. They're tuned empirically. When a real file in the Rush Stack codebase chunks badly, the fix is usually to reclassify a node kind (e.g., a wrapper that should have been transparent but was being treated as a leaf, blocking descent). Several test artifacts in the repo come from problem files identified during this tuning.

### The split-search algorithm

For a chunk that exceeds the target size:

1. Find the **shallowest split scope** that spans the chunk's line range. This is typically the file root or a top-level class/function body.
2. Get the candidate split points from that scope's direct children.
3. Look for a candidate that produces an acceptable partition (both halves above the minimum-chunk-size threshold). If found, split there and recurse on both halves.
4. If no candidate is acceptable, **descend** into the largest nested split scope (passing through any transparent conduits) and repeat.
5. While descending, keep track of the **least-bad split** seen at any level, scored by how close it comes to the ideal balanced partition.
6. If descent terminates without finding an acceptable split, fall back to the least-bad split. If the least-bad split creates a chunk below the minimum-size threshold, mark the result as a **degraded AST split** rather than a clean one.
7. If no AST split was found at any level (rare, typically pathological generated code), emit a **fallback line-based split** that picks an arbitrary line in the middle of the range.

The descent is what handles cases like a single class with one giant method. The shallowest scope (`program`) has only one child (the class), so splitting at the program level is impossible; the algorithm must descend through `class_declaration` (transparent) into `class_body` (split scope) and try again with the class's methods as candidates.

### Quality markers in breadcrumbs

The result of the split-search is reflected in the breadcrumb attached to each chunk:

- **No marker.** The chunk came from a successful AST split with both halves above the minimum-size threshold.
- **`:[degraded-ast-split]`.** The split was at an AST boundary but produced at least one tiny chunk. Operationally usable but indicates the file's structure didn't fit the algorithm's assumptions cleanly.
- **`:[fallback-split]`.** No AST split was found and the partitioner fell back to a line-based cut. This is the failure mode and indicates a genuine chunker problem worth investigating.

### Quality scoring

`src/engine/partitioner/scoring.rs` computes a 0-100% score for a complete partitioning, used by `audit-chunks` to summarize chunker behavior across a sample of files. The score combines two badnesses:

- **Count badness.** Penalizes producing too many chunks relative to the ideal partition (total content size divided by max chunk size, rounded up). A file that should partition into 3 chunks but produces 7 has high count badness.
- **Micro badness.** Penalizes individual chunks being either too small (size below the threshold) or too large (size at or above max). For each chunk, a per-chunk badness is computed and averaged across the partition.

The final score is `100 * (1 - count_badness)^α * (1 - micro_badness)^β` with both exponents currently set to 1. Scores below 95% are considered indicators of a chunking problem worth examining; the partitioner's quality is not a settled-once metric but something tuned over time, and these scoring weights are subject to revision.

### Development tools

Two CLI commands exist for chunker development:

- **`monodex dump-chunks --file <path>`.** Runs the partitioner on a single file and prints the resulting chunks with sizes, breadcrumbs, and quality markers. Supports `--debug` for verbose split-decision logging, `--visualize` for full chunk contents, `--with-fallback` to enable line-based fallback (off by default in this command), and `--target-size` to override the default 6000-character target.
- **`monodex audit-chunks --count <N> --dir <path>`.** Samples N TypeScript files from a directory, runs the partitioner on each (AST-only mode, no fallback), and reports aggregate quality scores. Useful for measuring the effect of partitioner changes across a real codebase.

Both commands run the partitioner without writing to the database, so they're safe to use during development.

## Markdown partitioning

`src/engine/markdown_partitioner.rs` splits markdown files at structural boundaries: headings (`#`, `##`, `###`, etc.), fenced code blocks (` ``` `), block quotes (`>`), and paragraphs. Heading hierarchy generates the breadcrumb context. A section under `## Installation` inside `# README` produces a breadcrumb fragment that includes both heading slugs.

Heading slugs use GitHub-style slugification via the `github_slugger` crate, which matches GitHub's anchor-link convention. This means a breadcrumb pointing at a heading section also points at the URL fragment that GitHub would generate for it.

The implementation is straightforward (~500 lines) and currently has limited handling of edge cases: oversized code blocks are kept as single units even if they exceed the size budget, and front-matter (YAML metadata) isn't specially recognized. These are candidates for future improvement.

## Line-based splitting

Line-based splitting handles file types where no language-aware partitioner is available. The strategy is simple: split into chunks of approximately the target size at line boundaries. The implementation lives inline in the dispatcher; there's no separate partitioner file. Line-based splits do not produce breadcrumbs beyond the file path itself; semantic context isn't extractable from generic line content.

The set of file extensions handled by `lineBased` is configurable via `monodex-crawl.json` and varies by repo; the current defaults in the embedded config include common configuration and stylesheet extensions. The role of this strategy is to be the base case when nothing better applies.
