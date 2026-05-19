//! Purpose: Parse chunk-selector strings (`:N`, `:N-M`, `:N-end`) from CLI arguments into `ChunkSelector` values.
//! Edit here when: Adding or modifying chunk selector parsing logic for view/debug-fts commands.
//! Do not edit here for: View command output (see `app/commands/view.rs`), chunk display (see `app/chunk_display.rs`).

/// Parsed selector for file-based chunk queries.
///
/// Used by `view` and `debug-fts` commands to parse chunk identifiers
/// like `700a4ba232fe9ddc:3` or `700a4ba232fe9ddc:2-4`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkSelector {
    /// All chunks in the file (no selector suffix)
    All,
    /// Single chunk at position N (1-indexed)
    Single(usize),
    /// Range from start to end (inclusive, 1-indexed)
    Range(usize, usize),
    /// Range from start to the end of file
    ToEnd(usize),
}

/// Parse file ID with optional selector suffix.
///
/// Formats:
/// - `700a4ba232fe9ddc` - all chunks in file (`ChunkSelector::All`)
/// - `700a4ba232fe9ddc:3` - chunk 3 (`ChunkSelector::Single(3)`)
/// - `700a4ba232fe9ddc:2-3` - chunks 2 through 3 (`ChunkSelector::Range(2, 3)`)
/// - `700a4ba232fe9ddc:3-end` - chunk 3 through the last chunk (`ChunkSelector::ToEnd(3)`)
///
/// Returns a tuple of `(file_id, selector)`. The file_id is validated to be
/// exactly 16 hexadecimal characters.
pub fn parse_chunk_selector(s: &str) -> anyhow::Result<(String, ChunkSelector)> {
    let s = s.trim();

    // Check for selector suffix
    if let Some(colon_pos) = s.find(':') {
        let file_id = s[..colon_pos].to_string();
        let selector = &s[colon_pos + 1..];

        // Validate file_id is 16 hex chars
        if file_id.len() != 16 || !file_id.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                file_id
            ));
        }

        // Parse selector
        if selector == "end" {
            // Invalid: ":end" without start
            Err(anyhow::anyhow!(
                "Invalid selector ':end'. Use ':N-end' format."
            ))
        } else if let Some(start_str) = selector.strip_suffix("-end") {
            // :N-end format
            let start: usize = start_str
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if start < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    start
                ));
            }
            Ok((file_id, ChunkSelector::ToEnd(start)))
        } else if selector.contains('-') {
            // :N-M format
            let parts: Vec<&str> = selector.split('-').collect();
            if parts.len() != 2 {
                return Err(anyhow::anyhow!(
                    "Invalid selector '{}'. Expected ':N-M' format.",
                    selector
                ));
            }
            let start: usize = parts[0]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid start chunk in selector '{}'", selector))?;
            let end: usize = parts[1]
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid end chunk in selector '{}'", selector))?;
            if start < 1 || end < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}:{}",
                    start,
                    end
                ));
            }
            if start > end {
                return Err(anyhow::anyhow!("Start chunk {} > end chunk {}", start, end));
            }
            Ok((file_id, ChunkSelector::Range(start, end)))
        } else {
            // :N format (single chunk)
            let chunk_num: usize = selector
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid chunk number in selector '{}'", selector))?;
            if chunk_num < 1 {
                return Err(anyhow::anyhow!(
                    "Chunk numbers are 1-indexed, got {}",
                    chunk_num
                ));
            }
            Ok((file_id, ChunkSelector::Single(chunk_num)))
        }
    } else {
        // No selector - validate file_id and return All selector
        if s.len() != 16 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(anyhow::anyhow!(
                "Invalid file ID '{}'. Expected 16 hex characters.",
                s
            ));
        }
        Ok((s.to_string(), ChunkSelector::All))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_file_id_all_chunks() {
        let (file_id, selector) = parse_chunk_selector("abcd1234efab5678").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::All));
    }

    #[test]
    fn test_parse_file_id_single_chunk() {
        let (file_id, selector) = parse_chunk_selector("abcd1234efab5678:3").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::Single(3)));
    }

    #[test]
    fn test_parse_file_id_range() {
        let (file_id, selector) = parse_chunk_selector("abcd1234efab5678:2-4").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::Range(2, 4)));
    }

    #[test]
    fn test_parse_file_id_to_end() {
        let (file_id, selector) = parse_chunk_selector("abcd1234efab5678:3-end").unwrap();
        assert_eq!(file_id, "abcd1234efab5678");
        assert!(matches!(selector, ChunkSelector::ToEnd(3)));
    }

    #[test]
    fn test_parse_file_id_invalid_file_id() {
        let result = parse_chunk_selector("invalid");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid file ID"));
    }

    #[test]
    fn test_parse_file_id_invalid_selector() {
        let result = parse_chunk_selector("abcd1234efab5678:abc");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid chunk number")
        );
    }

    #[test]
    fn test_parse_file_id_end_without_start() {
        let result = parse_chunk_selector("abcd1234efab5678:end");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid selector ':end'")
        );
    }

    #[test]
    fn test_parse_file_id_zero_chunk_number() {
        let result = parse_chunk_selector("abcd1234efab5678:0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("1-indexed"));
    }

    #[test]
    fn test_parse_file_id_reversed_range() {
        let result = parse_chunk_selector("abcd1234efab5678:5-2");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Start chunk 5 > end chunk 2")
        );
    }
}
