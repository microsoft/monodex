//! Purpose: Handler for the `use` command — set or display the default catalog/label context.
//! Edit here when: Modifying how default context is set or displayed by the command.
//! Do not edit here for: Context persistence (see `../context.rs`), identifier validation (see `engine/identifier.rs`).

use crate::app::{Config, load_default_context, save_default_context};
use crate::engine::identifier::{validate_catalog, validate_label};

pub fn run_use(
    catalog: Option<&str>,
    label: Option<String>,
    config: &Config,
) -> anyhow::Result<()> {
    match (catalog, label) {
        (None, None) => {
            // Show current context
            match load_default_context(&config.paths) {
                Some(ctx) => {
                    println!("Current context:");
                    println!("  Catalog: {}", ctx.catalog);
                    println!("  Label: {}", ctx.label);
                }
                None => {
                    println!("No default context set.");
                    println!();
                    println!("Usage:");
                    println!("  monodex use --catalog <name> --label <name>");
                }
            }
        }
        (Some(catalog_name), Some(label)) => {
            // Validate catalog name syntax
            validate_catalog(catalog_name)
                .map_err(|e| anyhow::anyhow!("Invalid catalog name '{}': {}", catalog_name, e))?;

            // Validate label name syntax
            validate_label(&label)
                .map_err(|e| anyhow::anyhow!("Invalid label name '{}': {}", label, e))?;

            // Validate that catalog exists in config
            if !config.catalogs.contains_key(catalog_name) {
                return Err(anyhow::anyhow!(
                    "Catalog '{}' not found in config. Available catalogs: {}",
                    catalog_name,
                    config
                        .catalogs
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }

            // Set new context
            save_default_context(&config.paths, catalog_name, &label)?;

            println!("✓ Default context set to:");
            println!("  Catalog: {}", catalog_name);
            println!("  Label: {}", label);
            println!();
            println!(
                "Commands will now use this context when --catalog/--label are not specified."
            );
        }
        (Some(_), None) | (None, Some(_)) => {
            // Partial specification - error
            return Err(anyhow::anyhow!(
                "Both --catalog and --label are required to set context.\n\n                Usage:\n  monodex use --catalog <name> --label <name>\n\n                Or run 'monodex use' without arguments to see current context."
            ));
        }
    }

    Ok(())
}
