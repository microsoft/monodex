//! Identifier validation and composition for monodex.
//!
//! This module centralizes all identifier handling to ensure consistency
//! across the codebase. See the syntax spec at:
//! https://github.com/microsoft/monodex/issues/25
//!
//! ## Terminology
//!
//! - **catalog**: A bare identifier naming a catalog (e.g., "my-repo")
//! - **label**: A bare identifier naming a label (e.g., "main", "feature/x")
//! - **label_id**: The qualified storage form "catalog:label" (e.g., "my-repo:main")
//!
//! The user-facing API always takes separate `--catalog` and `--label` flags.
//! The qualified `label_id` form is internal only and never shown to users.

use std::fmt;
use thiserror::Error;

// ============================================================================
// Error Types
// ============================================================================

/// Error codes for identifier validation failures.
#[derive(Debug, Clone, Error)]
pub enum IdentifierError {
    /// Catalog name is invalid.
    #[error("[invalid_catalog] {message}")]
    Catalog { code: &'static str, message: String },

    /// Label name is invalid.
    #[error("[invalid_label] {message}")]
    Label { code: &'static str, message: String },

    /// Label ID has invalid syntax.
    #[error("[invalid_label_id] {message}")]
    LabelId { code: &'static str, message: String },
}

impl IdentifierError {
    /// Returns the error code for programmatic handling.
    #[allow(dead_code)]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Catalog { code, .. } => code,
            Self::Label { code, .. } => code,
            Self::LabelId { code, .. } => code,
        }
    }
}

// ============================================================================
// Validation Constants
// ============================================================================

/// Maximum length for catalog names (per spec §9.2).
const MAX_CATALOG_LENGTH: usize = 64;

/// Maximum length for label names (per spec §9.3).
const MAX_LABEL_LENGTH: usize = 128;

// ============================================================================
// Validation Functions
// ============================================================================

/// Validates a catalog name (kebab-case).
///
/// Rule: `^[a-z0-9]+(?:-[a-z0-9]+)*$`
///
/// Examples: `my-repo`, `frontend`, `backend-api`
pub fn validate_catalog(name: &str) -> Result<(), IdentifierError> {
    if name.is_empty() {
        return Err(IdentifierError::Catalog {
            code: "catalog_empty",
            message: "Catalog name cannot be empty".to_string(),
        });
    }

    if name.len() > MAX_CATALOG_LENGTH {
        return Err(IdentifierError::Catalog {
            code: "catalog_too_long",
            message: format!(
                "Catalog name exceeds maximum length of {} characters (got {} characters: '{}')",
                MAX_CATALOG_LENGTH,
                name.len(),
                name
            ),
        });
    }

    // Check for leading hyphen
    if name.starts_with('-') {
        return Err(IdentifierError::Catalog {
            code: "catalog_leading_hyphen",
            message: format!("Catalog name '{}' cannot start with a hyphen", name),
        });
    }

    // Check for trailing hyphen
    if name.ends_with('-') {
        return Err(IdentifierError::Catalog {
            code: "catalog_trailing_hyphen",
            message: format!("Catalog name '{}' cannot end with a hyphen", name),
        });
    }

    // Check for consecutive hyphens
    if name.contains("--") {
        return Err(IdentifierError::Catalog {
            code: "catalog_consecutive_hyphens",
            message: format!("Catalog name '{}' cannot contain consecutive hyphens", name),
        });
    }

    // Check for invalid characters (uppercase, underscore, special chars)
    for (i, c) in name.chars().enumerate() {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            if c.is_ascii_uppercase() {
                return Err(IdentifierError::Catalog {
                    code: "catalog_uppercase",
                    message: format!(
                        "Catalog name '{}' contains uppercase letter '{}' at position {}. Use lowercase only.",
                        name, c, i
                    ),
                });
            }
            if c == '_' {
                return Err(IdentifierError::Catalog {
                    code: "catalog_underscore",
                    message: format!(
                        "Catalog name '{}' contains underscore at position {}. Use hyphens instead (e.g., 'my-repo' not 'my_repo').",
                        name, i
                    ),
                });
            }
            // Check for reserved characters
            if c == ':' || c == '@' || c == '+' || c == '#' || c == '=' {
                return Err(IdentifierError::Catalog {
                    code: "catalog_reserved_char",
                    message: format!(
                        "Catalog name '{}' contains '{}', which is reserved for future grammar. Only lowercase letters, digits, and hyphens are allowed.",
                        name, c
                    ),
                });
            }
            return Err(IdentifierError::Catalog {
                code: "catalog_invalid_char",
                message: format!(
                    "Catalog name '{}' contains invalid character '{}' at position {}. Only lowercase letters, digits, and hyphens are allowed.",
                    name, c, i
                ),
            });
        }
    }

    Ok(())
}

/// Validates a label name (Git-like identifier).
///
/// Rule: `^[a-z0-9]+(?:[./=-][a-z0-9]+)*$`
///
/// Examples: `main`, `feature/x`, `release/v1.2.3`, `branch=main`, `repo/sub/feature`
///
/// Note: `=` is a permitted separator character but is not interpreted as a typed-form
/// delimiter today. A label containing `=` is an opaque identifier (per spec §5).
pub fn validate_label(name: &str) -> Result<(), IdentifierError> {
    if name.is_empty() {
        return Err(IdentifierError::Label {
            code: "label_empty",
            message: "Label name cannot be empty".to_string(),
        });
    }

    if name.len() > MAX_LABEL_LENGTH {
        return Err(IdentifierError::Label {
            code: "label_too_long",
            message: format!(
                "Label name exceeds maximum length of {} characters (got {} characters: '{}')",
                MAX_LABEL_LENGTH,
                name.len(),
                name
            ),
        });
    }

    // Validate as label payload (single-pass validation per spec §9.3)
    validate_label_payload(name)
}

/// Validates a label payload (the label itself, or the part after `=` in future typed forms).
///
/// Rule: `^[a-z0-9]+(?:[./=-][a-z0-9]+)*$`
fn validate_label_payload(payload: &str) -> Result<(), IdentifierError> {
    if payload.is_empty() {
        return Err(IdentifierError::Label {
            code: "label_empty",
            message: "Label name cannot be empty".to_string(),
        });
    }

    // Track position for better error messages
    let mut segment_len = 0usize;

    for (pos, c) in payload.chars().enumerate() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            segment_len += 1;
        } else if c == '.' || c == '/' || c == '-' || c == '=' {
            // Separator: must have at least one char before it
            if segment_len == 0 {
                return Err(IdentifierError::Label {
                    code: "label_leading_separator",
                    message: format!(
                        "Label '{}' has a leading separator '{}' at position {}",
                        payload, c, pos
                    ),
                });
            }
            segment_len = 0;
        } else if c.is_ascii_uppercase() {
            return Err(IdentifierError::Label {
                code: "label_uppercase",
                message: format!(
                    "Label '{}' contains uppercase letter '{}' at position {}. Use lowercase only.",
                    payload, c, pos
                ),
            });
        } else if c == '_' {
            return Err(IdentifierError::Label {
                code: "label_underscore",
                message: format!(
                    "Label '{}' contains underscore at position {}. Use hyphens or slashes instead.",
                    payload, pos
                ),
            });
        } else if c == ':' {
            return Err(IdentifierError::Label {
                code: "label_reserved_char",
                message: format!(
                    "Label '{}' contains ':', which is reserved. If you meant to specify a catalog, use the --catalog flag.",
                    payload
                ),
            });
        } else if c == '@' || c == '+' || c == '#' {
            return Err(IdentifierError::Label {
                code: "label_reserved_char",
                message: format!(
                    "Label '{}' contains '{}', which is reserved for future grammar.",
                    payload, c
                ),
            });
        } else {
            return Err(IdentifierError::Label {
                code: "label_invalid_char",
                message: format!(
                    "Label '{}' contains invalid character '{}' at position {}. Allowed: lowercase letters, digits, '.', '/', '-', '='",
                    payload, c, pos
                ),
            });
        }
    }

    // Must end with an alphanumeric segment
    if segment_len == 0 {
        return Err(IdentifierError::Label {
            code: "label_trailing_separator",
            message: format!("Label '{}' cannot end with a separator", payload),
        });
    }

    Ok(())
}

// ============================================================================
// LabelId Newtype
// ============================================================================

/// A validated label identifier in storage form (catalog:label).
///
/// This type guarantees that the label_id has been validated and contains
/// only allowed characters. It is used internally for storage and
/// is never shown directly to users.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LabelId {
    catalog: String,
    label: String,
    combined: String,
}

impl LabelId {
    /// Creates a new LabelId from validated catalog and label names.
    pub fn new(catalog: &str, label: &str) -> Result<Self, IdentifierError> {
        validate_catalog(catalog)?;
        validate_label(label)?;

        Ok(Self {
            catalog: catalog.to_string(),
            label: label.to_string(),
            combined: format!("{}:{}", catalog, label),
        })
    }

    /// Creates a LabelId without validation (for reading from storage).
    #[cfg(test)]
    pub fn new_unchecked(catalog: &str, label: &str) -> Self {
        Self {
            catalog: catalog.to_string(),
            label: label.to_string(),
            combined: format!("{}:{}", catalog, label),
        }
    }

    /// Parses a label_id string (catalog:label format).
    pub fn parse(s: &str) -> Result<Self, IdentifierError> {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(IdentifierError::LabelId {
                code: "label_id_missing_colon",
                message: format!(
                    "invalid stored label identifier '{}': expected 'catalog:label' format",
                    s
                ),
            });
        }
        Self::new(parts[0], parts[1])
    }

    /// Returns the catalog component.
    #[cfg(test)]
    pub fn catalog(&self) -> &str {
        &self.catalog
    }

    /// Returns the label component.
    #[cfg(test)]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the full label_id string.
    pub fn as_str(&self) -> &str {
        &self.combined
    }
}

impl fmt::Display for LabelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.combined)
    }
}

impl AsRef<str> for LabelId {
    fn as_ref(&self) -> &str {
        &self.combined
    }
}

impl std::ops::Deref for LabelId {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.combined
    }
}

impl serde::Serialize for LabelId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.combined)
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Computes the label_id string from catalog and label names.
#[cfg(test)]
pub fn compute_label_id(catalog: &str, label: &str) -> Result<String, IdentifierError> {
    validate_catalog(catalog)?;
    validate_label(label)?;
    Ok(format!("{}:{}", catalog, label))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_catalog_valid() {
        assert!(validate_catalog("sparo").is_ok());
        assert!(validate_catalog("rushstack").is_ok());
        assert!(validate_catalog("my-repo").is_ok());
        assert!(validate_catalog("backend-api").is_ok());
    }

    #[test]
    fn test_validate_catalog_invalid() {
        // Empty
        assert_eq!(validate_catalog("").unwrap_err().code(), "catalog_empty");

        // Uppercase not allowed
        assert_eq!(
            validate_catalog("MyRepo").unwrap_err().code(),
            "catalog_uppercase"
        );

        // Underscore not allowed
        assert_eq!(
            validate_catalog("my_repo").unwrap_err().code(),
            "catalog_underscore"
        );

        // Double hyphen
        assert_eq!(
            validate_catalog("my--repo").unwrap_err().code(),
            "catalog_consecutive_hyphens"
        );

        // Leading hyphen
        assert_eq!(
            validate_catalog("-repo").unwrap_err().code(),
            "catalog_leading_hyphen"
        );

        // Trailing hyphen
        assert_eq!(
            validate_catalog("repo-").unwrap_err().code(),
            "catalog_trailing_hyphen"
        );
    }

    #[test]
    fn test_validate_label_valid() {
        assert!(validate_label("main").is_ok());
        assert!(validate_label("feature/x").is_ok());
        assert!(validate_label("release/v1.2.3").is_ok());
        assert!(validate_label("working-dir").is_ok());
        assert!(validate_label("repo/sub/feature").is_ok());
    }

    #[test]
    fn test_validate_label_typed_form() {
        // `=` is a permitted separator character but not interpreted as typed-form
        // These are all valid opaque identifiers:
        assert!(validate_label("branch=main").is_ok());
        assert!(validate_label("branch=feature/x").is_ok());
        assert!(validate_label("commit=abc123").is_ok());
        assert!(validate_label("tag=v1.2.3").is_ok());
        assert!(validate_label("local=working-dir").is_ok());

        // Multiple `=` are now valid since `=` is just a separator
        assert!(validate_label("foo=bar=baz").is_ok());

        // Mixed separators
        assert!(validate_label("repo/branch=name").is_ok());
    }

    #[test]
    fn test_validate_label_invalid() {
        // Empty
        assert_eq!(validate_label("").unwrap_err().code(), "label_empty");

        // Uppercase
        assert_eq!(
            validate_label("Main").unwrap_err().code(),
            "label_uppercase"
        );

        // Underscore (invalid separator)
        assert_eq!(
            validate_label("feature_x").unwrap_err().code(),
            "label_underscore"
        );

        // Trailing separator
        assert_eq!(
            validate_label("feature/").unwrap_err().code(),
            "label_trailing_separator"
        );

        // Leading separator (= is now a valid separator, but cannot start label)
        assert_eq!(
            validate_label("=main").unwrap_err().code(),
            "label_leading_separator"
        );
    }

    #[test]
    fn test_label_id() {
        let id = LabelId::new("sparo", "main").unwrap();
        assert_eq!(id.catalog(), "sparo");
        assert_eq!(id.label(), "main");
        assert_eq!(id.as_str(), "sparo:main");
    }

    #[test]
    fn test_label_id_parse() {
        let id = LabelId::parse("sparo:main").unwrap();
        assert_eq!(id.catalog(), "sparo");
        assert_eq!(id.label(), "main");

        let err = LabelId::parse("sparo").unwrap_err();
        assert_eq!(err.code(), "label_id_missing_colon");
    }

    #[test]
    fn test_compute_label_id_function() {
        let result = compute_label_id("sparo", "main").unwrap();
        assert_eq!(result, "sparo:main");
    }
}
