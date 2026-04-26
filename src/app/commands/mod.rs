//! CLI command handlers.
//!
//! Edit here when: Adding a new command or modifying command dispatch.
//! Do not edit here for: CLI argument definitions (see `../cli.rs`).

pub mod audit_chunks;
pub mod crawl;
pub mod dump_chunks;
pub mod init_db;
pub mod purge;
pub mod search;
pub mod use_cmd;
pub mod view;

#[cfg(test)]
mod test_helpers;

// Re-export command entry points
pub use audit_chunks::run_audit_chunks;
pub use crawl::{run_crawl_label, run_crawl_working_dir};
pub use dump_chunks::run_dump_chunks;
pub use init_db::run_init_db;
pub use purge::run_purge;
pub use search::run_search;
pub use use_cmd::run_use;
pub use view::run_view;
