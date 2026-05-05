# Concurrency

This doc describes how Monodex coordinates concurrent operations against a database. It covers the writer lock taxonomy, reader semantics, and how the model interacts with LanceDB's and Tantivy's own concurrency mechanisms.

The core property: **writers are serialized; readers are lock-free.** Two writers against the same logical partition of a database wait for each other. Writers against different partitions run in parallel where physically safe. Readers proceed against committed per-storage state without coordinating with writers; they may observe a label mid-crawl, but never torn writes.

## Why writers need coordination

A Monodex database holds two LanceDB tables (`chunks`, `label_metadata`) and a per-label tree of Tantivy index directories under `fts/`. The chunks table is a single physical dataset shared across all catalogs; isolation between catalogs is logical, enforced by `catalog == X` predicates on every read and write. Tantivy's directories are per-catalog and per-label, physically separate.

Three failure modes motivate the lock design:

- *Logical interference within a catalog.* Two `monodex crawl` invocations against the same catalog touch the same chunk rows, the same `active_label_ids` arrays, and the same label-reassignment scan. Concurrent writes can produce phantom data loss: writer A's reassignment scan removes the label from rows that writer B is still in the middle of upserting, and B's later writes finish without the label tag. No error fires.
- *LanceDB commit contention.* The chunks table is one optimistically-concurrent dataset. Concurrent `merge_insert` or `delete` calls against it produce `CommitConflict` errors that callers would otherwise have to retry. Serializing LanceDB writes at our layer eliminates the contention rather than handling it.
- *Cross-subsystem inconsistency (post-FTS).* The FTS design's advisory staleness manifest, written after Tantivy commits, assumes no other writer is interleaving Tantivy commits and manifest writes. The catalog-level lock is what makes that assumption true.

Different catalogs share none of these failure modes at the logical level. They share only the LanceDB dataset's commit point. The lock design distinguishes the two.

Catalog isolation at the row level is a load-bearing invariant. `compute_file_id` includes the catalog name as input, so two catalogs containing identical content at identical paths produce distinct `row_id` values. The lock taxonomy assumes this: per-catalog parallel writers are safe only because their writes target disjoint rows. Removing catalog from `compute_file_id` would silently re-introduce cross-catalog row contention that no lock at this layer could fix.

## Lock taxonomy

Three primitives:

**Database lock** at `<db>/locks/database.lock`. A reader-writer file lock. Held in shared mode by per-catalog operations; held in exclusive mode by database-spanning operations. Two per-catalog operations both hold shared and proceed in parallel. A database-spanning operation waits for all shared holds to release before acquiring exclusive, and blocks new shared acquisitions while it holds.

**Catalog lock** at `<db>/locks/per-catalog/<catalog>.lock`, one file per catalog, created lazily on first use. Always exclusive. Held for the duration of any per-catalog write operation. Two operations against the same catalog serialize on this lock; two operations against different catalogs do not contend. Catalog names are kebab-case per the validator in `engine/identifier.rs`, so the catalog name is path-safe as a filename without escaping or hashing.

**Commit mutex** at `<db>/locks/commit.lock`. One per database. Always exclusive. Held briefly around each LanceDB write call (one `merge_insert` or `delete` per acquisition). The commit mutex is what serializes physical writes to the shared LanceDB dataset across catalogs whose logical writes don't otherwise contend.

The three primitives compose. A typical writer holds shared(database) + exclusive(catalog) for its entire run, and acquires the commit mutex briefly around each LanceDB call.

## Acquisition protocol per operation

| Operation | Database | Catalog | Commit mutex |
|---|---|---|---|
| `crawl --catalog X` | shared | exclusive(X) | acquired around each LanceDB write |
| `purge --catalog X` | shared | exclusive(X) | acquired around each LanceDB write |
| `purge --all` | exclusive | none | acquired around each LanceDB write |
| `init-db` | exclusive | none | acquired around each LanceDB write |
| `search`, `view`, `audit-chunks`, `dump-chunks` | none | none | none |
| `use` | none | none | none |

Acquisition order is always database -> catalog -> commit mutex. Release is the reverse order, governed by Rust's drop order. No operation reaches past a level it doesn't need to acquire.

`use` writes `~/.monodex/context.json` in the user's tool home, not the database directory, so it does not interact with the lock taxonomy at all.

## Contention behavior

All contended lock acquisitions block until the lock is available. Contention on any of the three locks indicates legitimate sequencing rather than user error:

- Two operations against the same catalog (e.g. `crawl --label main` and `crawl --label feature-x` against the same catalog) are a real workflow. Watch mode and CI scripts will both produce this pattern routinely; failing fast would force script authors to retry-loop in shell.
- `purge --all` waiting for in-flight per-catalog crawls is intended: killing them mid-write would leave LanceDB and FTS in inconsistent states.
- Brief commit-mutex contention between parallel crawls is the design working as intended.

For waits that exceed three seconds, a progress message identifies the database path and elapsed time, repeating roughly once a minute thereafter. The destination of these messages is owned by the caller, not the lock helper (see "Progress reporting" below). This serves two audiences: an interactive user who started a duplicate command in another terminal sees what's happening and can Ctrl-C; a CI machine that would otherwise hang silently on a contended lock leaves a clear trace in its log.

The progress mechanism uses a separate thread that polls an atomic flag and emits messages based on elapsed time. The main thread sits in the kernel-level blocking lock call. This delegates fairness to the kernel (Linux/macOS `flock`, Windows `LockFileEx`) rather than implementing application-level fairness with a spin-with-backoff loop, and avoids the starvation risk inherent to spin patterns.

Lock-holder identification is not displayed. The OS-level file-lock APIs do not expose the holding process directly. The non-racy alternative (write the PID into the lockfile contents while holding the exclusive lock; readers, that is other contenders, read it without the lock at progress-message time) is implementable but rejected: the motivating scenario is "I forgot I started monodex in another terminal," for which `ps -ef | grep monodex` is the natural debugging path and would point at the same process. The marginal value of in-message PID display does not justify the extra write+fsync per acquisition or the lockfile gaining content (currently empty by design). The database path alone is the disambiguating context that matters for both interactive and CI debugging.

## Reader semantics

Readers never acquire any lock. The consistency guarantees come from the storage layers themselves:

- LanceDB uses MVCC. Each write commits a new manifest version atomically (via filesystem rename); readers see the last fully-committed version. A reader running concurrently with a writer sees pre-write state until the writer commits, then post-write state on next query. There is no visible mid-write state.
- Tantivy's `IndexReader::reload()` similarly snapshots against the last committed `meta.json`. A reader sees the segment set as of the last `IndexWriter::commit()`.

Both subsystems' commits are atomic against their own state. The catalog-level writer lock prevents same-catalog writers from interleaving, but it does not make LanceDB and Tantivy a single transaction; readers may still observe different committed phases of the same crawl across the two subsystems.

What readers do *not* see is logical-label coherence across the whole crawl. A `monodex crawl` runs many batched commits over its 15-30 minute lifetime; a `search` running mid-crawl can see new chunks that have been committed alongside stale chunks that haven't yet been cleaned up by the label-reassignment phase, with the label's `crawl_complete` flag still false. The reader sees committed per-storage state, not a transitional-or-final logical state.

This is the correct UX answer in the absence of a global write barrier. The FTS design's "warn on incomplete state" UX (search emits a yellow warning when the queried label has `crawl_complete=false` or `fts_complete=false`) extends naturally to vector search and is the right mechanism for surfacing transitional reads to users. The lock layer does not attempt to provide it; surfacing transitional state is a search-path responsibility, not a lock-layer responsibility.

A reader during `purge --all` sees pre-purge data until the purge commits, then post-purge data. Because purge is not a multi-table transaction, a reader could in principle observe a state where chunks have been truncated but `label_metadata` has not (or vice versa). Current `search` and `view` paths do not consult `label_metadata` for retrieval, so this gap is not user-visible today; it would become visible if those paths gain a `crawl_complete` consultation step in the future.

A reader querying FTS state during a concurrent `purge --catalog X` may encounter directory-disappearance errors as the purge unlinks the `fts/<X>/` tree. The search-path implementation routes these through the FTS design's "absent state" warning (yellow message naming the catalog and the crawl command that would rebuild) rather than surfacing a raw IO error. The exact failure point varies by platform: on POSIX the unlink succeeds and the reader's open mappings stay valid against the unreferenced inodes (it just sees pre-purge data until close), while on Windows the purger's `DeleteFile` against an mmapped segment fails with a sharing violation and the deletion stalls until the reader releases. Either is a defensible "user is destroying data they're querying" outcome; neither needs additional locking on the reader side.

## Storage-layer integration

LanceDB writes are wrapped in commit-mutex acquisition inside the storage methods themselves. A caller invoking `chunk_storage.upsert_chunks(...)` does not need to know about the commit mutex; the method acquires it, performs the LanceDB call, releases. The discipline is uniform: every LanceDB write goes through the commit mutex, including writes from `purge --all`. The latter holds the database-exclusive lock and could not in principle conflict with anyone, but the storage methods don't know that, and keeping the rule "every LanceDB write takes the commit mutex" without exceptions is simpler than tracking whether each call site is already protected by an outer lock.

The methods covered by this rule include every LanceDB-mutating storage operation: `merge_insert` for upserts, `delete` for tombstoning, `update` for in-place column changes, plus the higher-level methods built on these (label-add, label-removal, file-complete sentinel updates, per-catalog truncate). The commit mutex is not recursive. POSIX `flock` and Windows `LockFileEx` both produce undefined behavior or deadlock on recursive acquisition. Storage methods that compose other storage methods must acquire the mutex only at the outermost level; the inner methods detect that the mutex is already held by being structured as private helpers that don't acquire on their own. Concretely, the mutex is acquired inside the public-facing storage methods (the ones called from command handlers and the crawl pipeline), not inside the internal helpers they call.

Operations that report a row count (such as `delete_by_catalog`'s "deleted N chunks" message) compute the count from rows matching the operation's predicate, under the same commit-mutex acquisition that performs the delete. Counting total table rows before and after the delete and subtracting would race against concurrent writes to other catalogs and report the wrong number; predicate-scoped counts under the mutex are accurate.

Tantivy writes follow a different shape. A Tantivy `IndexWriter` is held for the duration of an FTS phase, accumulates document additions and deletions in memory, and commits once at the end. There is no per-write contention point analogous to LanceDB's `merge_insert`. The protection Tantivy's writes need is provided by the per-catalog lock that the surrounding crawl already holds: no other Monodex writer can have an `IndexWriter` open against the same catalog's directories.

Tantivy's own per-directory lockfile (the `INDEX_WRITER_LOCK` it acquires when an `IndexWriter` opens) is redundant under our discipline. It guards Tantivy state from same-directory concurrent writers within Tantivy's own model, but that scenario can't occur if our per-catalog lock is held: only one Monodex process is in the FTS phase for catalog X at a time. Redundant but harmless: we don't disable it, and we accept that a panic mid-FTS can leave a Tantivy lockfile that needs manual cleanup before the next crawl proceeds. Stale-lockfile recovery is a Tantivy concern, not ours.

The asymmetry between LanceDB's per-write commit mutex and Tantivy's per-phase locking reflects the asymmetry in their physical layouts. LanceDB has one shared dataset across all catalogs; Tantivy has per-catalog directories. The commit mutex is for cross-catalog physical contention on the shared dataset, and Tantivy doesn't need an analog because it doesn't have that contention.

**Acquisition timing.** The database lock and catalog lock are acquired at the start of the writer operation, before any database I/O. For CLI use, "the start of the writer operation" is the synchronous entry point of the command handler, before `block_on(...)`; for a future long-lived host (such as an MCP server), it is the start of the request handler that performs the write. Either way, the lock is acquired before any catalog-scoped sidecar I/O (including `warnings-<catalog>.json` reads) and before commit resolution or other expensive setup that should not be repeated after waiting for the lock. Lock acquisition is per-operation, not per-process: a long-lived host acquires and releases for each request rather than holding any lock across requests.

**Async runtimes.** The database lock and catalog lock acquisitions are blocking syscalls; in CLI use they happen on the synchronous entry point before any tokio runtime starts. A long-lived host that processes requests on a tokio runtime would acquire these via `tokio::task::spawn_blocking` (or equivalent) rather than calling them directly from a runtime worker. The commit mutex is acquired from inside async storage methods; under contention the acquisition briefly blocks a runtime worker, which is acceptable because commits are millisecond-scale. If profiling ever shows commit-mutex contention starving the runtime, `tokio::task::block_in_place` is the natural escape hatch.

**Per-acquisition file handles.** Each lock acquisition opens its own `File` handle for the lockfile and holds it in the returned guard until release. Handles are not shared across acquisitions. This matters for in-process concurrency: POSIX `flock` semantics across multiple file descriptors against the same path differ subtly from Windows `LockFileEx` semantics across multiple handles, but with one fresh handle per acquisition, both platforms behave the way callers expect. In CLI use this is automatic (one process, one acquisition per command); in a long-lived host with concurrent in-process acquisitions, the per-handle discipline is what makes them work correctly.

**Progress reporting.** The lock helper does not write progress messages to a destination it chooses. Instead, the caller passes a callback (or, equivalently, the helper emits to a sink). The CLI's writer commands today install a callback that writes to stderr, matching the rest of the codebase's current direct-print pattern. A future long-lived host installs a callback that routes the same information differently (a structured progress notification over a protocol surface, a log entry, a response field). When the FTS design's progress-sink abstraction lands, the lock helper participates in it on the same terms as the embedding pipeline. The FTS design names the discipline explicitly: "subsystems do not print directly; the orchestrator owns presentation." Until that abstraction lands, the callback shape is an implementation detail of the helper's signature, not a project-wide trait.

**`init-db` short-circuit and under-lock recheck.** An idempotent re-run of `init-db` against an already-initialized database should not block waiting for in-flight crawls. The implementation does an existence check on `monodex-meta.json` before acquiring the database-exclusive lock; if the database is already initialized, `init-db` returns immediately with the "already initialized" message. If the pre-lock check sees an uninitialized state, the lock is acquired, and the existence check is repeated under the lock. Two concurrent `init-db` invocations could both pass the pre-lock check, but only one will pass the under-lock check; the other observes the just-completed initialization and returns. This is the standard double-checked pattern.

**`init-db` empty-directory tolerance.** The existing `init-db` treats a database directory containing only `.monodex.lock` as "empty enough to initialize." Under this design the same logic must accept a `locks/` subdirectory and its contents as ignorable initialization detritus, since a crash after creating `<db>/locks/database.lock` but before writing `monodex-meta.json` would otherwise be misreported as "not a monodex database."

**Old lockfile.** The existing `init_db.rs` creates `<db>/.monodex.lock` at the database root. Under this design, that file is detritus: the new code creates and uses `<db>/locks/database.lock` instead. Per the project's pre-1.0 schema-migration policy, no cleanup of the old file is performed; it sits inert in the database directory and harms nothing.

## Lockfile lifecycle and on-disk layout

```
<db>/
  monodex-meta.json
  chunks.lance/
  label_metadata.lance/
  fts/                          (post-FTS PR1)
  locks/
    database.lock
    commit.lock
    per-catalog/
      <catalog-name>.lock       (one per catalog, lazily created)
```

Lockfiles are persistent. Once created, they are not deleted on lock release. The OS-level file lock is what carries the locked-or-not state, tracked by the kernel against the file descriptor; the file on disk is purely a rendezvous point. This is the same discipline the existing `init_db.rs` uses for its lockfile (which becomes `locks/database.lock` under this design).

Each helper function ensures its lockfile's parent directory exists before opening the lockfile, using `fs::create_dir_all`. The cost is one extra syscall per acquisition (microseconds, dominated by the open and lock syscalls themselves), and the result is that no caller has to remember an ordering invariant about which operation creates the `locks/` tree.

The lockfile contents are empty. Nothing is written into them. They exist only as named handles for `flock` / `LockFileEx`.

`rm -rf <db>/locks/` is safe when no Monodex process is running. After a reboot, every lock from the prior boot has already been released by the kernel, and the files on disk are pure detritus. Running monodex again creates whatever lockfiles it needs. Running `rm -rf locks/` while a Monodex writer is active is unsupported behavior in the same category as `rm -rf chunks.lance/` mid-crawl.

A future maintenance command can clean up orphaned per-catalog lockfiles for catalogs that have been removed from `config.json`. None is implemented today; the accumulated detritus is bounded and tiny.

## Crash recovery

There is no application-level crash recovery for the locks. OS file locks are released by the kernel when the holding process exits, including on crash, kill -9, or power loss. This is true on POSIX (`flock` against an inode, released on `close()` of the last fd) and on Windows (`LockFileEx`, released by the OS on process termination, with a small caveat that release can take a few hundred milliseconds in pathological cases). No PID file, no heartbeat, no lease, no recovery code.

Files left in a partially-written state by a crashed writer are a separate concern handled by LanceDB's manifest-version atomicity (a partial write that didn't commit doesn't move the manifest pointer) and Tantivy's commit-boundary durability (uncommitted documents are in process memory and lost on crash, not on disk). The lock layer adds nothing to either recovery story.

The on-disk lockfiles persist across crashes. They carry no information from the prior process; they are simply already-created rendezvous points that the next writer can immediately lock.

## Cross-platform notes

The `fs4` crate provides cross-platform file locking via `fs4::fs_std::FileExt::lock_exclusive`, `lock_shared`, `try_lock_exclusive`, and `try_lock_shared`. POSIX uses `flock(LOCK_EX)` / `flock(LOCK_SH)`; Windows uses `LockFileEx` with `LOCKFILE_EXCLUSIVE_LOCK` (or without, for shared). The semantics are functionally identical for our purposes: both block when contended, both release on process exit.

One platform-visible difference: Windows lock release on process termination can take a few hundred milliseconds in pathological cases, while POSIX release is typically instantaneous. A second writer started immediately after the first dies may briefly see contention before the kernel finishes releasing. The blocking-with-progress-message mechanism handles this transparently.

Network filesystems (NFS, SMB, etc.) are out of scope: the README explicitly disallows them as database storage, and `flock` semantics on those filesystems are unreliable. The lock design assumes a local filesystem.

## What the lock design does not do

- It does not prevent corruption from filesystem-level mishandling: simultaneous database access from multiple machines via a network filesystem, `rm -rf` against in-use directories, or copying a database directory while it's being written are all undefined.
- It does not coordinate across Monodex versions. A database written by `monodex 0.5` and accessed by `monodex 0.6` proceeds under the schema-version check, not under any lock-version compatibility scheme.
- It does not provide fairness guarantees beyond what the kernel provides. POSIX `flock` and Windows `LockFileEx` are typically fair on uncontended-then-contended sequences, but neither documents FIFO ordering. POSIX `flock` is specifically not writer-preferring, so in theory a steady stream of shared-lock acquisitions can starve a pending exclusive request; in practice Monodex's workload (15-30 minute crawls, infrequent operations) does not produce the high-frequency contention pattern that would manifest as starvation.
- It does not aim to maximize parallelism. Two crawls against the same catalog serialize on the catalog lock; two crawls against different catalogs run mostly in parallel but serialize briefly at LanceDB commit points. Workloads that need finer-grained parallelism than this provides are out of scope; the right answer there would be a different storage layout, not finer locks.
- It does not coordinate `monodex use` invocations against `~/.monodex/context.json`. That file is tool-home state, not database state, and is out of scope for this design. Concurrent `use` invocations could in principle race on the file; if that ever matters, the fix is a separate context-file lock, not a database lock.
- It does not prevent `purge --all` from waiting indefinitely behind in-flight catalog writers. This is intentional: killing a crawl mid-write would leave LanceDB and FTS in an inconsistent state. A `purge --all` invocation effectively means "drain in-flight writes, then destroy everything."

## Future directions

The lock taxonomy is intended to support several future extensions without redesign:

- **Watch mode** (post-FTS PR1) holds a long-lived `IndexWriter` per actively-watched label. Under this design, watch mode would hold the per-catalog lock for the duration of the watch session. Per-command acquisition is what's implemented today; long-held acquisition is a lifetime change, not a model change.
- **Long-lived host process** (such as an MCP server). Acquires locks per-request rather than per-process: each request that writes acquires the relevant locks, runs, releases. The lock taxonomy is unchanged. Two implementation surfaces matter: blocking lock acquisitions move to `spawn_blocking` to avoid stalling tokio runtime workers, and the progress callback installs a route through the host's protocol surface rather than printing. Both points are noted in the storage-layer integration section. Reader-side, a long-lived host holding open `Database` handles must be robust to a concurrent purge invalidating its read state, the same way `monodex search` is.
- **Schema upgrade** (`monodex upgrade-db`, planned for the first reference customer with a multi-month-old database). The shape of the upgrade operation is undecided: it could be in-place rewrite of the existing database directory, or a friendlier "delete and recrawl" that automates what users do manually today under the current schema-bump policy. The lock implications are different. In-place rewrite acquires the database-exclusive lock against the existing database, blocks readers from opening it during the rewrite (the lock-free reader contract here assumes no destructive in-place rewrites are happening behind readers' backs, which has to be revisited if the upgrade goes this way), and faces the same Windows-mmap-during-write platform difference noted for purge above. Recrawl-into-fresh-directory doesn't really need the lock; it operates on a path that no reader has open. Picking between these is upgrade-design work, not lock-design work; the lock taxonomy supports either, but the shape of upgrade affects what the reader contract has to promise.
- **Orphaned-lockfile cleanup** (a future maintenance command) scans `locks/per-catalog/`, cross-references against `config.json`, and deletes lockfiles for catalogs no longer in config. Acquires each lockfile briefly with `try_lock` before deleting, to avoid removing actively-held files.
- **Future FTS storage changes.** The catalog-level writer contract is the stable interface; future internal restructuring of FTS state should preserve it.
