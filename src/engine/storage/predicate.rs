//! LanceDB SQL predicate construction helpers.
//!
//! Purpose: Centralize the vocabulary of filter expressions used across the storage layer.
//!
//! Edit here when: Adding new predicate patterns, changing sanitization rules.
//! Do not edit here for: Storage operations themselves (see chunks/storage.rs, labels.rs),
//!   or validation logic (see engine/identifier.rs).
//!
//! Validation contract: Callers must pass already-validated values. Catalog names
//! are validated by `validate_catalog`, label IDs by `LabelId::parse`, and
//! `row_id`/`file_id` are derived from internal computation. The `debug_assert!`
//! checks here are developer-time invariant checks that catch contract violations
//! during testing. They do not run in release builds.

/// Construct an equality predicate for a string column: `<col> = '<val>'`.
///
/// # Panics
/// In debug builds, panics if `val` contains a single quote.
pub fn eq_str(col: &str, val: &str) -> String {
    debug_assert!(!val.contains('\''), "Value contains single quote: {}", val);
    format!("{} = '{}'", col, val)
}

/// Construct an `array_contains` predicate for a string array column:
/// `array_contains(<col>, '<val>')`.
///
/// # Panics
/// In debug builds, panics if `val` contains a single quote.
pub fn array_contains_str(col: &str, val: &str) -> String {
    debug_assert!(!val.contains('\''), "Value contains single quote: {}", val);
    format!("array_contains({}, '{}')", col, val)
}

/// Construct an `IN` predicate for a string column with quoted values:
/// `<col> IN ('<v1>', '<v2>', ...)`.
///
/// For an empty `vals` slice, returns the literal predicate `1 = 0`
/// (a no-match expression LanceDB accepts as SQL).
///
/// # Panics
/// In debug builds, panics if any element in `vals` contains a single quote.
pub fn in_quoted_strs(col: &str, vals: &[&str]) -> String {
    if vals.is_empty() {
        return "1 = 0".to_string();
    }
    for val in vals {
        debug_assert!(!val.contains('\''), "Value contains single quote: {}", val);
    }
    let quoted: Vec<String> = vals.iter().map(|v| format!("'{}'", v)).collect();
    format!("{} IN ({})", col, quoted.join(", "))
}

/// Construct a SQL array literal from a slice of strings: `['<v1>', '<v2>', ...]`.
///
/// For an empty slice, returns `[]`.
///
/// # Panics
/// In debug builds, panics if any element contains a single quote.
pub fn quoted_str_array(vals: &[&str]) -> String {
    for val in vals {
        debug_assert!(!val.contains('\''), "Value contains single quote: {}", val);
    }
    let quoted: Vec<String> = vals.iter().map(|v| format!("'{}'", v)).collect();
    format!("[{}]", quoted.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eq_str() {
        assert_eq!(eq_str("row_id", "abc123"), "row_id = 'abc123'");
        assert_eq!(eq_str("catalog", "my-catalog"), "catalog = 'my-catalog'");
    }

    #[test]
    fn test_array_contains_str() {
        assert_eq!(
            array_contains_str("active_label_ids", "my-catalog:main"),
            "array_contains(active_label_ids, 'my-catalog:main')"
        );
    }

    #[test]
    fn test_in_quoted_strs() {
        assert_eq!(
            in_quoted_strs("row_id", &["abc", "def"]),
            "row_id IN ('abc', 'def')"
        );
        assert_eq!(
            in_quoted_strs("row_id", &["single"]),
            "row_id IN ('single')"
        );
    }

    #[test]
    fn test_in_quoted_strs_empty() {
        // Empty slice returns a no-match predicate
        assert_eq!(in_quoted_strs("row_id", &[]), "1 = 0");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Value contains single quote")]
    fn test_eq_str_rejects_single_quote() {
        let _ = eq_str("col", "val'ue");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Value contains single quote")]
    fn test_array_contains_str_rejects_single_quote() {
        let _ = array_contains_str("col", "val'ue");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Value contains single quote")]
    fn test_in_quoted_strs_rejects_single_quote() {
        let _ = in_quoted_strs("col", &["good", "bad'value"]);
    }

    #[test]
    fn test_quoted_str_array() {
        assert_eq!(
            quoted_str_array(&["label1", "label2"]),
            "['label1', 'label2']"
        );
        assert_eq!(quoted_str_array(&["single"]), "['single']");
    }

    #[test]
    fn test_quoted_str_array_empty() {
        // Empty slice returns empty array
        assert_eq!(quoted_str_array(&[]), "[]");
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "Value contains single quote")]
    fn test_quoted_str_array_rejects_single_quote() {
        let _ = quoted_str_array(&["good", "bad'value"]);
    }
}
