//! Application-level concerns: CLI, config, commands, crawl pipeline.
//!
//! Edit here when: Adding or modifying CLI commands, user-facing config,
//! or the high-level crawl orchestration.
//! Do not edit here for: Engine internals (see `engine/`).

pub mod cli;
pub mod commands;
pub mod config;
pub mod context;
pub mod crawl;
pub mod util;

pub use cli::{Cli, Commands, CrawlSourceArgs};
pub use config::{
    CatalogConfig, Config, EmbeddingModelConfig, EmbeddingSizeValue, load_config,
    print_memory_warning, resolve_database_path, resolve_embedding_config, validate_config_path,
};
pub use context::{
    DefaultContext, load_default_context, resolve_label_context, save_default_context,
};
pub use crawl::{CrawlFailures, run_embed_upload_pipeline};
pub use util::{
    chrono_timestamp, format_duration, format_eta, load_warning_state, sanitize_for_terminal,
    save_warning_state,
};
