//! Retrieval method types.
//!
//! Purpose: Define the set of retrieval methods Monodex supports.
//! Edit here when: Adding new retrieval methods.
//! Do not edit here for: CLI flag parsing (see app/cli.rs), search dispatch (see commands/search.rs).

use std::fmt;
use std::str::FromStr;

/// Retrieval methods available for indexing and search.
///
/// Variants are ordered alphabetically so derived `Ord` produces alphabetical order.
/// This ensures consistent display order in CLI output and storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RetrievalMethod {
    /// Full-text search via Tantivy.
    Fts,
    /// Vector similarity search via LanceDB.
    Vector,
}

impl FromStr for RetrievalMethod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "fts" => Ok(RetrievalMethod::Fts),
            "vector" => Ok(RetrievalMethod::Vector),
            _ => Err(format!(
                "Unknown retrieval method '{}'. Valid values: fts, vector",
                s
            )),
        }
    }
}

impl fmt::Display for RetrievalMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RetrievalMethod::Fts => write!(f, "fts"),
            RetrievalMethod::Vector => write!(f, "vector"),
        }
    }
}

use std::collections::BTreeSet;

/// Format a retrieval selection for display.
///
/// Returns the inner text without parentheses, e.g. "fts, vector", "fts only",
/// "vector only", or "no retrieval methods".
///
/// The caller is responsible for adding parentheses if needed.
pub fn format_selection(selection: &BTreeSet<RetrievalMethod>) -> String {
    let has_fts = selection.contains(&RetrievalMethod::Fts);
    let has_vector = selection.contains(&RetrievalMethod::Vector);

    match (has_fts, has_vector) {
        (true, true) => "fts, vector".to_string(),
        (true, false) => "fts only".to_string(),
        (false, true) => "vector only".to_string(),
        (false, false) => "no retrieval methods".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alphabetical_ordering() {
        // Fts < Vector because F < V alphabetically
        assert!(RetrievalMethod::Fts < RetrievalMethod::Vector);
        assert!(RetrievalMethod::Vector > RetrievalMethod::Fts);
    }

    #[test]
    fn test_from_str() {
        assert_eq!(
            RetrievalMethod::from_str("fts").unwrap(),
            RetrievalMethod::Fts
        );
        assert_eq!(
            RetrievalMethod::from_str("FTS").unwrap(),
            RetrievalMethod::Fts
        );
        assert_eq!(
            RetrievalMethod::from_str("vector").unwrap(),
            RetrievalMethod::Vector
        );
        assert_eq!(
            RetrievalMethod::from_str("VECTOR").unwrap(),
            RetrievalMethod::Vector
        );
        assert!(RetrievalMethod::from_str("unknown").is_err());
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", RetrievalMethod::Fts), "fts");
        assert_eq!(format!("{}", RetrievalMethod::Vector), "vector");
    }

    #[test]
    fn test_all_variants_iter() {
        // Helper to collect all variants (useful for iteration in production code)
        let all: Vec<RetrievalMethod> = vec![RetrievalMethod::Fts, RetrievalMethod::Vector];
        assert_eq!(all.len(), 2);
        assert!(all.contains(&RetrievalMethod::Fts));
        assert!(all.contains(&RetrievalMethod::Vector));
    }

    #[test]
    fn test_format_selection() {
        let mut selection = BTreeSet::new();
        assert_eq!(format_selection(&selection), "no retrieval methods");

        selection.insert(RetrievalMethod::Fts);
        assert_eq!(format_selection(&selection), "fts only");

        selection.clear();
        selection.insert(RetrievalMethod::Vector);
        assert_eq!(format_selection(&selection), "vector only");

        selection.insert(RetrievalMethod::Fts);
        assert_eq!(format_selection(&selection), "fts, vector");
    }
}
