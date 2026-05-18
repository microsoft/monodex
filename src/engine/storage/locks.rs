//! Purpose: OS-level file locking primitives for writer coordination.
//! Edit here when: Adding new lock primitives, changing lock acquisition behavior, or modifying the watchdog mechanism.
//! Do not edit here for: Storage operations (use storage/), CLI handlers (see app/commands/).

use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use fs4::fs_std::FileExt;

/// Time to wait before emitting the first progress message.
pub const FIRST_MESSAGE_THRESHOLD: Duration = Duration::from_secs(3);

/// Time between subsequent progress messages.
pub const MESSAGE_CADENCE: Duration = Duration::from_secs(60);

/// Interval for the watchdog thread to poll the waiting flag.
const WATCHDOG_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Action for the watchdog state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressAction {
    /// Emit a progress message with the elapsed time.
    Emit { elapsed: Duration },
    /// Continue waiting; no message yet.
    Wait,
}

/// Determines the next action for the watchdog state machine.
///
/// This is a pure function that takes elapsed time and the last emit time,
/// returning whether to emit a message or continue waiting.
pub fn next_progress_action(elapsed: Duration, last_emit: Option<Duration>) -> ProgressAction {
    match last_emit {
        None => {
            if elapsed >= FIRST_MESSAGE_THRESHOLD {
                ProgressAction::Emit { elapsed }
            } else {
                ProgressAction::Wait
            }
        }
        Some(last) => {
            if elapsed >= last + MESSAGE_CADENCE {
                ProgressAction::Emit { elapsed }
            } else {
                ProgressAction::Wait
            }
        }
    }
}

// ============================================================================
// RAII Guard Types
// ============================================================================

/// Inner RAII guard that owns the file handle and the single Drop impl.
struct FileLockGuard {
    file: File,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// RAII guard for a shared database lock.
///
/// Holds the file handle and releases the lock on drop.
pub struct DatabaseLockShared(#[allow(dead_code)] FileLockGuard);

/// RAII guard for an exclusive database lock.
///
/// Holds the file handle and releases the lock on drop.
pub struct DatabaseLockExclusive(#[allow(dead_code)] FileLockGuard);

/// RAII guard for a catalog lock.
///
/// Holds the file handle and releases the lock on drop.
pub struct CatalogLock(#[allow(dead_code)] FileLockGuard);

/// RAII guard for the commit mutex.
///
/// Holds the file handle and releases the lock on drop.
pub struct CommitMutex(#[allow(dead_code)] FileLockGuard);

// ============================================================================
// Acquisition Functions
// ============================================================================

/// Lock mode for internal acquisition.
enum LockMode {
    Shared,
    Exclusive,
}

/// Internal helper that owns lockfile-open + shared/exclusive acquisition.
fn acquire_file_lock(lockfile_path: &Path, mode: LockMode) -> Result<FileLockGuard> {
    ensure_lock_dir(lockfile_path)?;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lockfile_path)?;

    match mode {
        LockMode::Shared => file.lock_shared()?,
        LockMode::Exclusive => file.lock_exclusive()?,
    }

    Ok(FileLockGuard { file })
}

/// Acquires a shared lock on the database.
///
/// Creates the lock directory if needed. Blocks until the lock is available.
/// Emits progress messages via the callback if waiting takes longer than
/// [`FIRST_MESSAGE_THRESHOLD`].
pub fn acquire_database_shared(
    db_path: &Path,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<DatabaseLockShared> {
    let lockfile_path = db_path.join("locks").join("database.lock");
    run_with_watchdog(progress, db_path, || {
        acquire_file_lock(&lockfile_path, LockMode::Shared).map(DatabaseLockShared)
    })
}

/// Acquires an exclusive lock on the database.
///
/// Creates the lock directory if needed. Blocks until the lock is available.
/// Emits progress messages via the callback if waiting takes longer than
/// [`FIRST_MESSAGE_THRESHOLD`].
pub fn acquire_database_exclusive(
    db_path: &Path,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<DatabaseLockExclusive> {
    let lockfile_path = db_path.join("locks").join("database.lock");
    run_with_watchdog(progress, db_path, || {
        acquire_file_lock(&lockfile_path, LockMode::Exclusive).map(DatabaseLockExclusive)
    })
}

/// Acquires an exclusive lock for a catalog.
///
/// Creates the lock directory if needed. Blocks until the lock is available.
/// Emits progress messages via the callback if waiting takes longer than
/// [`FIRST_MESSAGE_THRESHOLD`].
///
/// # Arguments
/// * `db_path` - Path to the database directory
/// * `catalog` - Catalog name (must be validated by caller as path-safe)
/// * `progress` - Callback for progress messages during wait
pub fn acquire_catalog_lock(
    db_path: &Path,
    catalog: &str,
    progress: &(dyn Fn(&str) + Sync),
) -> Result<CatalogLock> {
    let lockfile_path = db_path
        .join("locks")
        .join("per-catalog")
        .join(format!("{}.lock", catalog));
    run_with_watchdog(progress, db_path, || {
        acquire_file_lock(&lockfile_path, LockMode::Exclusive).map(CatalogLock)
    })
}

/// Acquires the commit mutex.
///
/// Creates the lock directory if needed. Blocks until the lock is available.
/// No progress callback; commit-mutex contention is expected to be millisecond-scale.
pub fn acquire_commit_mutex(db_path: &Path) -> Result<CommitMutex> {
    let lockfile_path = db_path.join("locks").join("commit.lock");
    acquire_file_lock(&lockfile_path, LockMode::Exclusive).map(CommitMutex)
}

// ============================================================================
// Internal Helpers
// ============================================================================

/// Ensures the parent directory for a lockfile exists.
fn ensure_lock_dir(lockfile_path: &Path) -> Result<()> {
    if let Some(parent) = lockfile_path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Runs a lock acquisition with a watchdog thread for progress reporting.
///
/// The watchdog spawns a thread that monitors elapsed time and calls the
/// progress callback when appropriate. Uses `thread::scope` to avoid
/// `'static` bounds on the callback.
fn run_with_watchdog<T>(
    progress: &(dyn Fn(&str) + Sync),
    db_path: &Path,
    lock_op: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let is_waiting = AtomicBool::new(true);
    let db_path_display = db_path.display().to_string();

    thread::scope(|s| {
        // Spawn watchdog thread
        let watchdog = s.spawn(|| {
            let start = Instant::now();
            let mut last_emit: Option<Duration> = None;

            while is_waiting.load(Ordering::Relaxed) {
                let elapsed = start.elapsed();
                match next_progress_action(elapsed, last_emit) {
                    ProgressAction::Emit { elapsed: e } => {
                        let secs = e.as_secs();
                        progress(&format!(
                            "Waiting for lock on {} ({}s elapsed)...",
                            db_path_display, secs
                        ));
                        last_emit = Some(e);
                    }
                    ProgressAction::Wait => {}
                }
                thread::sleep(WATCHDOG_POLL_INTERVAL);
            }
        });

        // Perform the lock operation
        let result = lock_op();

        // Signal watchdog to stop and wait for it
        is_waiting.store(false, Ordering::Relaxed);
        let _ = watchdog.join();

        result
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -------------------------------------------------------------------------
    // State machine tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_progress_action_no_emit_before_threshold() {
        assert_eq!(
            next_progress_action(Duration::from_secs(2), None),
            ProgressAction::Wait
        );
    }

    #[test]
    fn test_progress_action_first_emit_at_threshold() {
        assert_eq!(
            next_progress_action(Duration::from_secs(3), None),
            ProgressAction::Emit {
                elapsed: Duration::from_secs(3)
            }
        );
    }

    #[test]
    fn test_progress_action_no_second_emit_before_cadence() {
        assert_eq!(
            next_progress_action(Duration::from_secs(30), Some(Duration::from_secs(3))),
            ProgressAction::Wait
        );
    }

    #[test]
    fn test_progress_action_second_emit_at_cadence() {
        assert_eq!(
            next_progress_action(Duration::from_secs(63), Some(Duration::from_secs(3))),
            ProgressAction::Emit {
                elapsed: Duration::from_secs(63)
            }
        );
    }

    #[test]
    fn test_progress_action_third_emit_at_second_cadence() {
        assert_eq!(
            next_progress_action(Duration::from_secs(123), Some(Duration::from_secs(63))),
            ProgressAction::Emit {
                elapsed: Duration::from_secs(123)
            }
        );
    }

    // -------------------------------------------------------------------------
    // Lock acquisition tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_acquire_database_shared() {
        let tempdir = TempDir::new().unwrap();
        let progress = |_msg: &str| {};
        let guard = acquire_database_shared(tempdir.path(), &progress).unwrap();
        drop(guard);
        // Second acquisition should succeed
        let _guard2 = acquire_database_shared(tempdir.path(), &progress).unwrap();
    }

    #[test]
    fn test_acquire_database_shared_allows_concurrent_shared() {
        // Two shared holders can coexist - this is what distinguishes
        // DatabaseLockShared from an exclusive lock.
        let tempdir = TempDir::new().unwrap();
        let progress = |_msg: &str| {};
        let guard1 = acquire_database_shared(tempdir.path(), &progress).unwrap();
        // While guard1 is still held, acquire a second shared lock
        let guard2 = acquire_database_shared(tempdir.path(), &progress).unwrap();
        // Both should succeed
        drop(guard1);
        drop(guard2);
    }

    #[test]
    fn test_acquire_database_exclusive() {
        let tempdir = TempDir::new().unwrap();
        let progress = |_msg: &str| {};
        let guard = acquire_database_exclusive(tempdir.path(), &progress).unwrap();
        drop(guard);
        // Second acquisition should succeed
        let _guard2 = acquire_database_exclusive(tempdir.path(), &progress).unwrap();
    }

    #[test]
    fn test_acquire_catalog_lock() {
        let tempdir = TempDir::new().unwrap();
        let progress = |_msg: &str| {};
        let guard = acquire_catalog_lock(tempdir.path(), "test-catalog", &progress).unwrap();
        drop(guard);
        // Second acquisition should succeed
        let _guard2 = acquire_catalog_lock(tempdir.path(), "test-catalog", &progress).unwrap();
    }

    #[test]
    fn test_acquire_commit_mutex() {
        let tempdir = TempDir::new().unwrap();
        let guard = acquire_commit_mutex(tempdir.path()).unwrap();
        drop(guard);
        // Second acquisition should succeed
        let _guard2 = acquire_commit_mutex(tempdir.path()).unwrap();
    }
}
