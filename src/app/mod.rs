//! Purpose: Application layer — CLI, config, commands, and crawl orchestration.
//! Edit here when: Adding or modifying CLI commands, user-facing config, or the high-level crawl orchestration.
//! Do not edit here for: Engine internals (see `engine/`).

mod chunk_display;
mod chunk_selector;
mod cli;
pub mod commands;
pub mod config;
mod context;
mod crawl;
mod lock_progress;
mod number_format;
mod search;
mod terminal_output;

pub use cli::{Cli, Commands, CrawlSourceArgs};
pub use config::{
    CatalogConfig, Config, EmbeddingModelConfig, EmbeddingSizeValue, load_config,
    print_memory_warning, resolve_database_path, resolve_embedding_config, validate_config_path,
};
pub use context::{
    DefaultContext, load_default_context, resolve_label_context, save_default_context,
};
pub use crawl::{CrawlFailures, run_embed_upload_pipeline, run_upsert_without_vectors};
