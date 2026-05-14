//! Purpose: Percent-encode reserved characters in breadcrumb path components and slugify markdown headings GitHub-style.
//! Edit here when: Changing the reserved-character set, percent-encoding rules, or heading slugification.
//! Do not edit here for: Identifier validation (see `identifier.rs`), breadcrumb composition by chunkers (see `chunker.rs`, `markdown_partitioner.rs`, `partitioner/`).

/// Percent-encodes reserved characters in a path component for use in locators (breadcrumbs).
///
/// Per the spec, these characters must be encoded: `:`, `@`, `=`, `+`, `#`, `%`,
/// whitespace, and control characters. `/` is NOT encoded (it's a path separator).
///
/// # Example
///
/// ```
/// use monodex::engine::breadcrumb::encode_path_component;
///
/// assert_eq!(encode_path_component("weird:file.ts"), "weird%3Afile.ts");
/// assert_eq!(encode_path_component("@scope/pkg"), "%40scope/pkg");
/// ```
pub fn encode_path_component(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            // Grammar-reserved characters
            ':' | '@' | '=' | '+' | '#' | '%' => {
                for byte in c.to_string().as_bytes() {
                    result.push_str(&format!("%{:02X}", byte));
                }
            }
            // Whitespace and control characters
            c if c.is_control() || c.is_whitespace() => {
                for byte in c.to_string().as_bytes() {
                    result.push_str(&format!("%{:02X}", byte));
                }
            }
            // Safe characters pass through
            _ => result.push(c),
        }
    }
    result
}

/// Slugifies a markdown heading using GitHub-style slugification.
///
/// Uses the `github-slugger` crate for consistent heading ID generation.
/// Duplicate headings get numbered suffixes (e.g., `examples`, `examples-1`).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_path_component_reserved_chars() {
        // Colon
        assert_eq!(encode_path_component("weird:file.ts"), "weird%3Afile.ts");

        // At sign
        assert_eq!(encode_path_component("@scope/pkg"), "%40scope/pkg");

        // Equals
        assert_eq!(encode_path_component("key=value"), "key%3Dvalue");

        // Plus
        assert_eq!(encode_path_component("a+b"), "a%2Bb");

        // Hash
        assert_eq!(encode_path_component("heading#id"), "heading%23id");

        // Percent
        assert_eq!(encode_path_component("100%"), "100%25");
    }

    #[test]
    fn test_encode_path_component_whitespace() {
        assert_eq!(encode_path_component("file name.ts"), "file%20name.ts");
        assert_eq!(encode_path_component("tab\there"), "tab%09here");
        assert_eq!(encode_path_component("new\nline"), "new%0Aline");
    }

    #[test]
    fn test_encode_path_component_control_chars() {
        // Null byte
        assert_eq!(encode_path_component("a\x00b"), "a%00b");

        // Delete character
        assert_eq!(encode_path_component("a\x7Fb"), "a%7Fb");
    }

    #[test]
    fn test_encode_path_component_preserves_safe_chars() {
        // Slashes are preserved (path separator)
        assert_eq!(encode_path_component("path/to/file.ts"), "path/to/file.ts");

        // Dots, dashes, underscores
        assert_eq!(encode_path_component("my-file_name.ts"), "my-file_name.ts");

        // Alphanumeric
        assert_eq!(encode_path_component("File123"), "File123");
    }

    #[test]
    fn test_breadcrumb_round_trip() {
        // Simulate building a breadcrumb with encoded components
        let package = encode_path_component("@scope/my-package");
        let file = encode_path_component("weird:file.ts");
        let symbol = encode_path_component("myFunction");

        let breadcrumb = format!("{}:{}:{}", package, file, symbol);

        // The breadcrumb should have all reserved chars encoded
        assert_eq!(breadcrumb, "%40scope/my-package:weird%3Afile.ts:myFunction");

        // But the structure should be parseable by splitting on unencoded colons
        let parts: Vec<&str> = breadcrumb.split(':').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "%40scope/my-package");
        assert_eq!(parts[1], "weird%3Afile.ts");
        assert_eq!(parts[2], "myFunction");
    }
}
