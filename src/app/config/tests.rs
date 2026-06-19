//! Purpose: Test suite for config loading and validation.
//! Edit here when: Adding or modifying tests for config loading and validation.
//! Do not edit here for: Config implementation (see `../config.rs`).

use super::*;
use std::io::Write;
use tempfile::tempdir;

#[test]
fn test_catalog_config_validates_monorepo_type() {
    let config = CatalogConfig {
        r#type: "monorepo".to_string(),
        path: "/some/path".to_string(),
    };
    assert!(config.validate().is_ok());
}

#[test]
fn test_catalog_config_rejects_unsupported_type() {
    let config = CatalogConfig {
        r#type: "folder".to_string(),
        path: "/some/path".to_string(),
    };
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("Unsupported catalog type 'folder'")
    );
    assert!(err.to_string().contains("Supported types: monorepo"));
}

#[test]
fn test_catalog_config_rejects_unknown_type() {
    let config = CatalogConfig {
        r#type: "unknown".to_string(),
        path: "/some/path".to_string(),
    };
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string()
            .contains("Unsupported catalog type 'unknown'")
    );
}

#[test]
fn test_load_config_validates_catalog_types() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    // Config with invalid catalog type
    writeln!(
        file,
        r#"{{
                "catalogs": {{
                    "test": {{
                        "type": "invalid",
                        "path": "/tmp"
                    }}
                }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let result = load_config(paths);
    let err = result.unwrap_err();
    assert!(err.to_string().contains("Invalid catalog 'test'"));
    assert!(
        err.to_string()
            .contains("Unsupported catalog type 'invalid'")
    );
}

#[test]
fn test_load_config_accepts_monorepo_type() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{
                    "sparo": {{
                        "type": "monorepo",
                        "path": "/tmp/sparo"
                    }}
                }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    assert_eq!(config.catalogs.get("sparo").unwrap().r#type, "monorepo");
}

#[test]
fn test_load_config_accepts_database_path() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{
                    "sparo": {{
                        "type": "monorepo",
                        "path": "/tmp/sparo"
                    }}
                }},
                "database": {{ "path": "/custom/db" }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    assert!(config.database.is_some());
    let db_config = config.database.unwrap();
    assert_eq!(db_config.path, Some("/custom/db".to_string()));
}

#[test]
fn test_load_config_database_section_optional() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{
                    "sparo": {{
                        "type": "monorepo",
                        "path": "/tmp/sparo"
                    }}
                }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    assert!(config.database.is_none());
}

#[test]
fn test_resolve_database_path_rejects_tilde() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{}},
                "database": {{ "path": "~/my-db" }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    let result = resolve_database_path(&config);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("tilde (~) is not supported"),
        "Expected error about tilde, got: {}",
        err
    );
    assert!(
        err.contains("database.path"),
        "Expected error to mention field name, got: {}",
        err
    );
}

#[test]
fn test_resolve_database_path_rejects_env_var() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{}},
                "database": {{ "path": "$HOME/my-db" }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    let result = resolve_database_path(&config);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("environment variable substitution is not supported"),
        "Expected error about env var, got: {}",
        err
    );
    assert!(
        err.contains("database.path"),
        "Expected error to mention field name, got: {}",
        err
    );
}

#[test]
fn test_resolve_database_path_rejects_bare_name() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{}},
                "database": {{ "path": "my-db" }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    let result = resolve_database_path(&config);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("'./'"),
        "Expected error mentioning './', got: {}",
        err
    );
    assert!(
        err.contains("database.path"),
        "Expected error to mention field name, got: {}",
        err
    );
}

#[test]
fn test_resolve_database_path_accepts_absolute_path() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{}},
                "database": {{ "path": "/custom/db" }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    let path = resolve_database_path(&config).unwrap();
    assert_eq!(path, PathBuf::from("/custom/db"));
}

#[test]
fn test_resolve_database_path_accepts_dot_relative() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{}},
                "database": {{ "path": "./my-db" }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    let path = resolve_database_path(&config).unwrap();
    assert_eq!(path, dir.path().join("my-db"));
}

#[test]
fn test_resolve_database_path_accepts_dotdot_relative() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{}},
                "database": {{ "path": "../sibling-db" }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    let path = resolve_database_path(&config).unwrap();
    assert_eq!(path, dir.path().parent().unwrap().join("sibling-db"));
}

#[test]
fn test_validate_config_path_accepts_dot_relative_for_catalog() {
    let config_folder = PathBuf::from("/etc/monodex");
    let result = validate_config_path("catalog path", "./repos/foo", &config_folder).unwrap();
    assert_eq!(result, PathBuf::from("/etc/monodex/repos/foo"));
}

#[test]
fn test_validate_config_path_rejects_bare_name_for_catalog() {
    let config_folder = PathBuf::from("/etc/monodex");
    let result = validate_config_path("catalog path", "repos/foo", &config_folder);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("'./'"),
        "Expected error mentioning './', got: {}",
        err
    );
    assert!(
        err.contains("catalog path"),
        "Expected error to mention field name, got: {}",
        err
    );
}

#[cfg(not(target_os = "windows"))]
#[test]
fn test_validate_config_path_rejects_backslash_on_nonwindows() {
    // On non-Windows, backslash-relative paths like .\ and ..\ are not valid
    // relative paths and should be rejected like any other bare name.
    let config_folder = PathBuf::from("/etc/monodex");
    let result = validate_config_path("database.path", ".\\my-db", &config_folder);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("'./'"),
        "Expected error mentioning './', got: {}",
        err
    );
}

#[cfg(target_os = "windows")]
#[test]
fn test_validate_config_path_accepts_backslash_on_windows() {
    // On Windows, .\ and ..\ are valid relative paths and should resolve.
    let config_folder = PathBuf::from(r"C:\etc\monodex");
    let result = validate_config_path("database.path", r".\my-db", &config_folder).unwrap();
    assert_eq!(result, PathBuf::from(r"C:\etc\monodex\my-db"));
}

#[test]
fn test_resolve_database_path_defaults_to_config_folder() {
    // When no database.path, should use <config_folder>/default-db
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{}}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    let path = resolve_database_path(&config).unwrap();

    // Should be <config_folder>/default-db
    assert_eq!(path, dir.path().join("default-db"));
}

#[test]
fn test_resolve_database_path_config_without_database_section() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    writeln!(
        file,
        r#"{{
                "catalogs": {{}}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    let path = resolve_database_path(&config).unwrap();

    // Should be <config_folder>/default-db
    assert_eq!(path, dir.path().join("default-db"));
}

#[test]
fn test_config_rejects_unknown_fields() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    // Config with an unknown field "qdrant" (old Qdrant-era config)
    writeln!(
        file,
        r#"{{
                "catalogs": {{}},
                "qdrant": {{
                    "url": "http://localhost:6333",
                    "collection": "monodex"
                }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let result = load_config(paths);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("unknown field"),
        "Expected error about unknown field, got: {}",
        err
    );
}

#[test]
fn test_load_config_missing_file_centralized_error() {
    // Test that the "config file not found" error uses the centralized wording
    let temp_dir = tempdir().unwrap();
    let paths = Paths::for_test(temp_dir.path().into());
    let result = load_config(paths);

    assert!(result.is_err());
    let err = result.unwrap_err().to_string();

    // Exact match on the centralized error message format
    assert!(
        err.starts_with("No config found at"),
        "Expected error to start with 'No config found at', got: {}",
        err
    );
    assert!(
        err.contains("See the README for instructions on creating a config file."),
        "Expected error to contain README hint, got: {}",
        err
    );
}

#[test]
fn test_load_config_parses_jsonc_with_line_comments() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    // Config with // line comments (JSONC)
    writeln!(
        file,
        r#"{{
                // This is a line comment
                "catalogs": {{
                    // Another comment
                    "sparo": {{
                        "type": "monorepo", // inline comment
                        "path": "/tmp/sparo"
                    }}
                }}
                // Final comment
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let config = load_config(paths).unwrap();
    assert_eq!(config.catalogs.get("sparo").unwrap().r#type, "monorepo");
}

#[test]
fn test_example_config_validates_against_schema() {
    use jsonschema::Validator;

    // Load the schema
    let schema_path = "schemas/config.schema.json";
    let schema_str = std::fs::read_to_string(schema_path)
        .expect("Failed to read config.schema.json - run from project root");
    let schema: serde_json::Value =
        serde_json::from_str(&schema_str).expect("Failed to parse config.schema.json as JSON");

    // Compile the schema
    let validator = Validator::new(&schema).expect("Failed to compile JSON schema");

    // Load the example config (JSONC - has comments)
    let example_path = "examples/monodex-config.json";
    let example_str = std::fs::read_to_string(example_path)
        .expect("Failed to read examples/monodex-config.json - run from project root");

    // Strip comments and parse (same approach as load_config)
    let stripped = json_comments::StripComments::new(example_str.as_bytes());
    let example: serde_json::Value = serde_json::from_reader(stripped)
        .expect("Failed to parse examples/monodex-config.json as JSON");

    assert!(
        validator.is_valid(&example),
        "examples/monodex-config.json does not validate against schema"
    );
}

#[test]
fn test_config_accepts_schema_field() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("monodex-config.json");
    let mut file = std::fs::File::create(&config_path).unwrap();

    // Config with a $schema field should be accepted
    writeln!(
        file,
        r#"{{
                "$schema": "https://example.com/schemas/monodex-config.json",
                "catalogs": {{
                    "my-repo": {{
                        "type": "monorepo",
                        "path": "/path/to/repo"
                    }}
                }}
            }}"#
    )
    .unwrap();

    let paths = Paths::for_test(dir.path().into());
    let result = load_config(paths);
    assert!(
        result.is_ok(),
        "Config with $schema field should be accepted"
    );
}
