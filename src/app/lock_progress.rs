//! Purpose: Stderr progress messaging while waiting for a writer lock: shared formatting used by commands that contend on the database lock.
//! Edit here when: Adding or modifying lock-acquisition progress messaging.
//! Do not edit here for: Crawl progress formatting (see `app/crawl/progress_format.rs`), command handlers (see `app/commands/`).

/// Progress callback for lock acquisitions that writes to stderr.
///
/// This is the shared progress callback for database, catalog, and other lock
/// acquisitions across init-db, crawl, and purge commands.
pub fn stderr_lock_progress(msg: &str) {
    eprintln!("{}", msg);
}
