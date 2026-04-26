//! monodex: Semantic search indexer for Rush monorepos
//!
//! Semantic search indexer for Rush monorepos using LanceDB vector database
//! Intelligently chunks code and documentation for high-quality semantic search

use clap::Parser;
use monodex::app::commands::run_use;
use monodex::app::{Cli, Commands};
use monodex::app::{load_config, resolve_label_context};

fn main() -> anyhow::Result<()> {
    // Warn if old tool home files exist
    monodex::paths::warn_old_tool_home_if_present();

    let cli = Cli::parse();

    // Load config
    let config_path = match cli.config {
        Some(path) => path,
        None => monodex::paths::config_path()?,
    };
    let config = load_config(&config_path)?;

    match cli.command {
        Commands::Use { catalog, label } => {
            run_use(catalog.as_deref(), label, &config)?;
        }
        Commands::InitDb => {
            monodex::app::commands::run_init_db(&config)?;
        }
        Commands::Crawl {
            catalog,
            label,
            source,
            incremental_warnings,
        } => {
            // Resolve label context from explicit flags or default context
            let (_, catalog_name, label) = resolve_label_context(Some(&label), catalog.as_deref())?;

            if source.working_dir {
                monodex::app::commands::run_crawl_working_dir(
                    &config,
                    &catalog_name,
                    &label,
                    incremental_warnings,
                    cli.debug,
                )?;
            } else {
                // Safe to unwrap: clap ArgGroup ensures one of commit/working_dir is set
                monodex::app::commands::run_crawl_label(
                    &config,
                    &catalog_name,
                    &label,
                    source.commit.as_ref().unwrap(),
                    incremental_warnings,
                    cli.debug,
                )?;
            }
        }
        Commands::Purge { catalog, all } => {
            monodex::app::commands::run_purge(&config, catalog.as_deref(), all, cli.debug)?;
        }
        Commands::DumpChunks {
            file,
            target_size,
            visualize,
            with_fallback,
            debug,
        } => {
            monodex::app::commands::run_dump_chunks(
                &file,
                target_size,
                visualize,
                with_fallback,
                debug,
            )?;
        }
        Commands::Search {
            text,
            limit,
            label,
            catalog,
        } => {
            monodex::app::commands::run_search(
                &config,
                &text,
                limit,
                label.as_deref(),
                catalog.as_deref(),
                cli.debug,
            )?;
        }
        Commands::View {
            id,
            label,
            catalog,
            full_paths,
            chunks_only,
        } => {
            monodex::app::commands::run_view(
                &config,
                &id,
                label.as_deref(),
                catalog.as_deref(),
                full_paths,
                chunks_only,
                cli.debug,
            )?;
        }
        Commands::AuditChunks { count, dir } => {
            monodex::app::commands::run_audit_chunks(count, dir)?;
        }
    }

    Ok(())
}
