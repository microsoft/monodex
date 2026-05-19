//! Default context persistence (catalog/label selection).
//!
//! Purpose: Manage the default catalog/label context that persists between commands.
//! Edit here when: Changing how default context is stored, validated, or resolved.
//! Do not edit here for: CLI flags (see cli.rs), command handlers (see commands/).

// Field shape is mirrored in schemas/context.schema.json. When adding or renaming fields here, update the JSON Schema in the same change.

use anyhow::anyhow;

use crate::engine::identifier::{LabelId, validate_catalog, validate_label};
use crate::paths::Paths;

// =============================================================================
// Timestamp helpers (moved from util.rs)
// =============================================================================

/// Format current time as UTC RFC 3339 string (e.g., "2024-01-15T10:30:00Z").
/// Used for machine-readable timestamps like the context file's `set_at` field.
fn utc_rfc3339_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Convert to calendar time (UTC)
    let days = now / 86400;
    let secs_today = now % 86400;

    // Calculate date from days since 1970-01-01
    // Using the algorithm from: https://en.wikipedia.org/wiki/Julian_day
    let (year, month, day) = days_to_ymd(days as i64);

    let hour = (secs_today / 3600) as u8;
    let minute = ((secs_today % 3600) / 60) as u8;
    let second = (secs_today % 60) as u8;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

/// Convert days since 1970-01-01 to year, month, day.
/// Based on the Julian day algorithm.
fn days_to_ymd(days: i64) -> (i16, u8, u8) {
    // Julian day number for 1970-01-01 is 2440588
    let jd = days + 2440588;

    // Algorithm from Richards (2012)
    let f = jd + 1401 + (((4 * jd + 274277) / 146097) * 3) / 4 - 38;
    let e = 4 * f + 3;
    let g = (e % 1461) / 4;
    let h = 5 * g + 2;
    let day = ((h % 153) / 5) + 1;
    let month = ((h / 153 + 2) % 12) + 1;
    let year = e / 1461 - 4716 + (12 + 2 - month) / 12;

    (year as i16, month as u8, day as u8)
}

/// Default context for commands
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
pub struct DefaultContext {
    /// Default catalog name
    pub catalog: String,
    /// Default label name
    pub label: String,
    /// When the context was set
    pub set_at: String,
}

/// Load default context from file, validating identifiers at the boundary
pub fn load_default_context(paths: &Paths) -> Option<DefaultContext> {
    let path = paths.context_file();

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let ctx: DefaultContext = match serde_json::from_str(&content) {
                Ok(ctx) => ctx,
                Err(e) => {
                    eprintln!(
                        "Warning: Failed to parse default context file ({}): {}. \
                         Run 'monodex use --catalog <name> --label <name>' to reset.",
                        path.display(),
                        e
                    );
                    return None;
                }
            };

            // Validate identifiers at the trust boundary
            if let Err(e) = validate_catalog(&ctx.catalog) {
                eprintln!(
                    "Warning: Invalid catalog '{}' in default context: {}. \
                     Run 'monodex use --catalog <name> --label <name>' to reset.",
                    ctx.catalog, e
                );
                return None;
            }
            if let Err(e) = validate_label(&ctx.label) {
                eprintln!(
                    "Warning: Invalid label '{}' in default context: {}. \
                     Run 'monodex use --catalog <name> --label <name>' to reset.",
                    ctx.label, e
                );
                return None;
            }

            Some(ctx)
        }
        Err(_) => None,
    }
}

/// Save default context to file
pub fn save_default_context(paths: &Paths, catalog: &str, label: &str) -> anyhow::Result<()> {
    let path = paths.context_file();

    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let context = DefaultContext {
        catalog: catalog.to_string(),
        label: label.to_string(),
        set_at: utc_rfc3339_timestamp(),
    };

    let content = serde_json::to_string_pretty(&context)?;
    std::fs::write(path, content)?;

    Ok(())
}

/// Resolve label context from explicit flags or default context.
/// Returns (label_id, catalog, label) or error if neither provided.
///
/// Per #25: --label takes a bare label name, --catalog takes a bare catalog name.
/// The qualified "catalog:label" form is no longer accepted.
pub fn resolve_label_context(
    paths: &Paths,
    explicit_label: Option<&str>,
    explicit_catalog: Option<&str>,
) -> anyhow::Result<(LabelId, String, String)> {
    // If explicit label provided, validate it
    if let Some(label_str) = explicit_label {
        // Reject legacy qualified form "catalog:label"
        if label_str.contains(':') {
            return Err(anyhow!(
                "Invalid --label value '{}'. Use separate flags: --catalog <catalog> --label <label>",
                label_str
            ));
        }

        // Validate the bare label name
        validate_label(label_str)
            .map_err(|e| anyhow!("Invalid label name '{}': {}", label_str, e))?;
    }

    // If explicit catalog provided, validate it
    if let Some(catalog_str) = explicit_catalog {
        validate_catalog(catalog_str)
            .map_err(|e| anyhow!("Invalid catalog name '{}': {}", catalog_str, e))?;
    }

    // Resolve from explicit flags or default context
    match (
        explicit_catalog,
        explicit_label,
        load_default_context(paths),
    ) {
        (Some(catalog), Some(label), _) => {
            // Both explicitly provided
            let label_id = LabelId::new(catalog, label).map_err(|e| anyhow!("{}", e))?;
            Ok((label_id, catalog.to_string(), label.to_string()))
        }
        (Some(catalog), None, Some(ctx)) => {
            // Catalog explicit, label from context
            let label = ctx.label;
            validate_label(&label)
                .map_err(|e| anyhow!("Invalid label in default context '{}': {}", label, e))?;
            let label_id = LabelId::new(catalog, &label).map_err(|e| anyhow!("{}", e))?;
            Ok((label_id, catalog.to_string(), label))
        }
        (None, Some(label), Some(ctx)) => {
            // Label explicit, catalog from context
            let catalog = ctx.catalog;
            validate_catalog(&catalog)
                .map_err(|e| anyhow!("Invalid catalog in default context '{}': {}", catalog, e))?;
            let label_id = LabelId::new(&catalog, label).map_err(|e| anyhow!("{}", e))?;
            Ok((label_id, catalog, label.to_string()))
        }
        (None, None, Some(ctx)) => {
            // Both from context
            let catalog = ctx.catalog;
            let label = ctx.label;
            validate_catalog(&catalog)
                .map_err(|e| anyhow!("Invalid catalog in default context '{}': {}", catalog, e))?;
            validate_label(&label)
                .map_err(|e| anyhow!("Invalid label in default context '{}': {}", label, e))?;
            let label_id = LabelId::new(&catalog, &label).map_err(|e| anyhow!("{}", e))?;
            Ok((label_id, catalog, label))
        }
        (None, Some(_), None) | (Some(_), None, None) => Err(anyhow!(
            "Missing context. Provide both --catalog and --label, or set defaults with:\n  monodex use --catalog <name> --label <name>"
        )),
        (None, None, None) => Err(anyhow!(
            "No context set. Use --catalog and --label, or set defaults with:\n  monodex use --catalog <name> --label <name>"
        )),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_example_context_validates_against_schema() {
        use jsonschema::Validator;

        // Load the schema
        let schema_path = "schemas/context.schema.json";
        let schema_str = std::fs::read_to_string(schema_path)
            .expect("Failed to read context.schema.json - run from project root");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_str).expect("Failed to parse context.schema.json as JSON");

        // Compile the schema
        let validator = Validator::new(&schema).expect("Failed to compile JSON schema");

        // Load and validate the example context
        let example_path = "examples/monodex-state.json";
        let example_str = std::fs::read_to_string(example_path)
            .expect("Failed to read examples/monodex-state.json - run from project root");
        let example: serde_json::Value = serde_json::from_str(&example_str)
            .expect("Failed to parse examples/monodex-state.json as JSON");

        assert!(
            validator.is_valid(&example),
            "examples/monodex-state.json does not validate against schema"
        );
    }

    #[test]
    fn test_context_schema_accepts_schema_field() {
        use jsonschema::Validator;

        // Load the schema
        let schema_path = "schemas/context.schema.json";
        let schema_str = std::fs::read_to_string(schema_path)
            .expect("Failed to read context.schema.json - run from project root");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_str).expect("Failed to parse context.schema.json as JSON");

        // Compile the schema
        let validator = Validator::new(&schema).expect("Failed to compile JSON schema");

        // Test that a $schema field is accepted
        let context_with_schema = serde_json::json!({
            "$schema": "https://example.com/schemas/monodex-state.json",
            "catalog": "my-repo",
            "label": "main",
            "set_at": "2024-01-15T10:30:00Z"
        });

        assert!(
            validator.is_valid(&context_with_schema),
            "Schema should accept $schema field in monodex-state.json"
        );
    }

    #[test]
    fn test_context_schema_rejects_invalid_timestamp() {
        use jsonschema::Validator;

        // Load the schema
        let schema_path = "schemas/context.schema.json";
        let schema_str = std::fs::read_to_string(schema_path)
            .expect("Failed to read context.schema.json - run from project root");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_str).expect("Failed to parse context.schema.json as JSON");

        // Compile the schema
        let validator = Validator::new(&schema).expect("Failed to compile JSON schema");

        // Test that an HH:MM:SS timestamp is rejected (the format lacks date/timezone)
        let bad_context = serde_json::json!({
            "catalog": "my-repo",
            "label": "main",
            "set_at": "14:30:00"  // Wrong format - should be RFC 3339
        });

        assert!(
            !validator.is_valid(&bad_context),
            "Schema should reject HH:MM:SS timestamp format"
        );
    }
}
