//! Purpose: Git-aware enumeration and blob reading for crawl sources.
//! Edit here when: Adding or renaming git_ops submodules, or changing the public surface re-exported from this folder.
//! Do not edit here for: the `BlobSource` trait or `FileEntry` (see `blob_source.rs`), package-index lookup and extraction (see `package_index.rs`), gix-based commit traversal (see `commit.rs`), subprocess-based working-tree reading (see `working_dir.rs`).

pub mod commit;
pub mod working_dir;

mod blob_source;
mod package_index;
#[cfg(test)]
mod tests;

pub use blob_source::{BlobSource, CommitBlobSource, FileEntry, WorkingDirBlobSource};
pub use package_index::{PackageIndex, extract_package_name_from_bytes};

// Re-export public API from submodules
pub use self::commit::{
    build_package_index_for_commit, enumerate_commit_tree, read_blob_content, resolve_commit_oid,
};
pub use self::working_dir::{
    WorkingTreeBlobMap, build_package_index_for_working_dir, build_working_tree_blob_map,
    enumerate_working_directory, read_working_file_content,
};
