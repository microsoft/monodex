//! Purpose: Binary entry point — parse CLI args, run process-level setup, dispatch to command handlers.
//! Edit here when: Wiring a new command into top-level dispatch or changing process-level setup (warnings, exit codes).
//! Do not edit here for: CLI argument definitions (see `app/cli.rs`), command handler logic (see `app/commands/`).

use clap::Parser;
use monodex::app::commands::run_use;
use monodex::app::{Cli, Commands};
use monodex::app::{load_config, resolve_label_context};
use monodex::paths::Paths;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Resolve paths from environment and CLI overrides
    let paths = Paths::resolve_from_env(cli.config.clone())?;

    // Warn if old tool home files exist (before load_config, so it fires even
    // when there's no config file at the new location)
    monodex::paths::warn_old_tool_home_if_present(&paths);

    // Load config
    let config = load_config(paths)?;

    match cli.command {
        Commands::Use { catalog, label } => {
            run_use(catalog.as_deref(), label, &config)?;
        }
        Commands::InitDb { delete_everything } => {
            monodex::app::commands::run_init_db(&config, delete_everything)?;
        }
        Commands::Crawl {
            catalog,
            label,
            source,
            retrieval,
        } => {
            // Resolve label context from explicit flags or default context
            let (_, catalog_name, label) =
                resolve_label_context(&config.paths, Some(&label), catalog.as_deref())?;

            if source.working_dir {
                monodex::app::commands::run_crawl_working_dir(
                    &config,
                    &catalog_name,
                    &label,
                    retrieval,
                    cli.debug,
                )?;
            } else {
                // Safe to unwrap: clap ArgGroup ensures one of commit/working_dir is set
                monodex::app::commands::run_crawl_label(
                    &config,
                    &catalog_name,
                    &label,
                    source.commit.as_ref().unwrap(),
                    retrieval,
                    cli.debug,
                )?;
            }
        }
        Commands::Purge { args } => {
            monodex::app::commands::run_purge(
                &config,
                args.catalog.as_deref(),
                args.all,
                cli.debug,
            )?;
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
            retrieval,
        } => {
            // Normalize retrieval Vec to Option<BTreeSet>
            // Empty Vec = None (all methods in selection)
            // Non-empty Vec = Some(BTreeSet) with deduplication
            let retrieval_set = if retrieval.is_empty() {
                None
            } else {
                Some(retrieval.into_iter().collect())
            };

            // Acquire stdout lock and pass to run_search
            let stdout = std::io::stdout();
            let mut stdout_lock = stdout.lock();
            monodex::app::commands::run_search(
                &mut stdout_lock,
                &config,
                &text,
                limit,
                label.as_deref(),
                catalog.as_deref(),
                retrieval_set,
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
        Commands::DebugFts {
            id,
            label,
            catalog,
            query,
        } => {
            monodex::app::commands::run_debug_fts(
                &config,
                &id,
                label.as_deref(),
                catalog.as_deref(),
                query.as_deref(),
                cli.debug,
            )?;
        }
    }

    Ok(())
}
