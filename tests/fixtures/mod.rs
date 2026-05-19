//! Purpose: Shared integration-test fixtures: storage, Git-repo, and config setup.
//! Edit here when: Adding or renaming integration-test fixture modules, or changing the public surface of `tests/fixtures/`.
//! Do not edit here for: Storage fixtures (see `storage.rs`), Git-repo fixtures (see `git.rs`), test cases (see `tests/*.rs`).

mod git;
mod storage;

#[allow(unused_imports)]
pub use git::{create_test_config, create_test_git_repo, unique_temp_dir};
#[allow(unused_imports)]
pub use storage::{chunk_to_row, create_test_storage};
