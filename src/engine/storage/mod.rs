//! LanceDB storage layer for monodex.
//!
//! Purpose: Provide a clean typed API for reading/writing chunks and label metadata.
//! This is the narrow seam between the application and LanceDB.
//!
//! Edit here when: Adding new storage operations, changing LanceDB table schemas,
//!   or modifying the database open/validate logic.
//! Do not edit here for: Chunking logic (see engine/partitioner/), CLI handlers (see app/commands/).

mod chunks;
mod database;
mod labels;
mod locks;
mod predicate;
mod rows;

pub use chunks::ChunkStorage;
pub use chunks::SentinelStatus;
pub use chunks::StorageProgressEvent;
pub use database::{Database, META_FILE, MetaFile, err_schema_mismatch};
pub use labels::LabelStorage;
pub use labels::read_selection;
pub use locks::{
    CatalogLock, CommitMutex, DatabaseLockExclusive, DatabaseLockShared, acquire_catalog_lock,
    acquire_commit_mutex, acquire_database_exclusive, acquire_database_shared,
};
pub use rows::{
    ChunkRow, LabelMetadataRow, SOURCE_KIND_GIT_COMMIT, SOURCE_KIND_WORKING_DIRECTORY,
    ScoredChunkRow,
};

/// LanceDB crate version. Keep in sync with Cargo.toml `lancedb` dependency.
pub const LANCEDB_CRATE_VERSION: &str = "0.27";
