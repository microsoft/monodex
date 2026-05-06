//! Custom tokenizer for FTS on code.
//!
//! Purpose: Tokenize code identifiers with case-splitting, underscore-splitting, and CJK support.
//! Edit here when: Changing tokenization rules (split boundaries, CJK handling, etc).
//! Do not edit here for: FTS schema (see `schema.rs`), indexing logic (see `indexing.rs`).
//!
//! ## Tokenization rules
//!
//! - Split on case transitions, underscores, dots, digit boundaries, and ASCII whitespace/punctuation.
//! - Keep both the original token and the splits (e.g. `getUserProfile` produces `getuserprofile`, `get`, `user`, `profile`).
//! - Lowercase all tokens.
//! - No stemming or stopwords.
//! - For runs of CJK characters, use Jieba word-segmentation.
//! - Token positions are globally monotonic across the full field stream.

use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

/// The name under which this tokenizer is registered on Tantivy's TokenizerManager.
pub const FTS_TOKENIZER_NAME: &str = "monodex-fts";

/// A custom tokenizer that combines identifier-aware splitting with Jieba for CJK.
///
/// This tokenizer:
/// 1. Splits on ASCII whitespace and punctuation to get initial tokens
/// 2. For each ASCII-ish token, applies identifier-aware splitting (case transitions, underscores, dots, digit boundaries)
/// 3. For runs of CJK characters, delegates to Jieba word-segmentation
/// 4. Emits the original token (lowercased) plus all splits
/// 5. Maintains globally monotonic token positions
#[derive(Clone, Default)]
pub struct MonodexFtsTokenizer;

impl Tokenizer for MonodexFtsTokenizer {
    type TokenStream<'a> = MonodexFtsTokenStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        MonodexFtsTokenStream::new(text)
    }
}

/// Token stream that produces tokens with identifier-aware splitting and CJK support.
pub struct MonodexFtsTokenStream {
    /// Tokens to emit, pre-computed from the text.
    tokens: Vec<String>,

    /// Current position in the tokens vector.
    token_index: usize,

    /// The current token being returned.
    current_token: Token,
}

impl MonodexFtsTokenStream {
    fn new(text: &str) -> Self {
        let tokens = tokenize_text(text);

        MonodexFtsTokenStream {
            tokens,
            token_index: 0,
            current_token: Token::default(),
        }
    }
}

impl TokenStream for MonodexFtsTokenStream {
    fn advance(&mut self) -> bool {
        if self.token_index < self.tokens.len() {
            let token_text = &self.tokens[self.token_index];
            self.current_token = Token {
                offset_from: 0, // We don't track exact byte offsets
                offset_to: 0,
                position: self.token_index,
                text: token_text.clone(),
                position_length: 1,
            };
            self.token_index += 1;
            true
        } else {
            false
        }
    }

    fn token(&self) -> &Token {
        &self.current_token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.current_token
    }
}

/// Tokenize text into a vector of tokens.
///
/// This is the main entry point for tokenization, handling both CJK and non-CJK text.
pub fn tokenize_text(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars = text.char_indices().peekable();
    let mut current_segment_start = 0;
    let mut current_segment_is_cjk: Option<bool> = None;

    for (idx, ch) in chars {
        let is_cjk = is_cjk_char(ch);

        match current_segment_is_cjk {
            None => {
                // First character
                current_segment_start = idx;
                current_segment_is_cjk = Some(is_cjk);
            }
            Some(prev_is_cjk) if is_cjk != prev_is_cjk => {
                // Transition between CJK and non-CJK; process the completed segment
                let segment = &text[current_segment_start..idx];
                if prev_is_cjk {
                    tokenize_cjk_segment(segment, &mut tokens);
                } else {
                    tokenize_non_cjk_segment(segment, &mut tokens);
                }
                current_segment_start = idx;
                current_segment_is_cjk = Some(is_cjk);
            }
            _ => {}
        }
    }

    // Process the final segment
    let segment = &text[current_segment_start..];
    if !segment.is_empty()
        && let Some(is_cjk) = current_segment_is_cjk
    {
        if is_cjk {
            tokenize_cjk_segment(segment, &mut tokens);
        } else {
            tokenize_non_cjk_segment(segment, &mut tokens);
        }
    }

    tokens
}

/// Check if a character is a CJK character (Han, Hiragana, Katakana, or Hangul).
fn is_cjk_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{4E00}'..='\u{9FFF}'       // CJK Unified Ideographs
        | '\u{3400}'..='\u{4DBF}'     // CJK Unified Ideographs Extension A
        | '\u{20000}'..='\u{2A6DF}'   // CJK Unified Ideographs Extension B
        | '\u{2A700}'..='\u{2B73F}'   // CJK Unified Ideographs Extension C
        | '\u{2B740}'..='\u{2B81F}'   // CJK Unified Ideographs Extension D
        | '\u{2B820}'..='\u{2CEAF}'   // CJK Unified Ideographs Extension E
        | '\u{F900}'..='\u{FAFF}'     // CJK Compatibility Ideographs
        | '\u{2F800}'..='\u{2FA1F}'   // CJK Compatibility Ideographs Supplement
        | '\u{3040}'..='\u{309F}'     // Hiragana
        | '\u{30A0}'..='\u{30FF}'     // Katakana
        | '\u{AC00}'..='\u{D7AF}'     // Hangul Syllables
        | '\u{1100}'..='\u{11FF}'     // Hangul Jamo
    )
}

/// Tokenize a non-CJK segment using identifier-aware splitting.
///
/// Splits on:
/// - ASCII whitespace and punctuation
/// - Case transitions (camelCase)
/// - Underscores
/// - Dots
/// - Digit boundaries
///
/// Emits both the original token (lowercased) and all splits.
fn tokenize_non_cjk_segment(segment: &str, tokens: &mut Vec<String>) {
    // First, split on whitespace and punctuation to get "words"
    for word in segment.split(|ch: char| ch.is_ascii_whitespace() || is_ascii_punctuation(ch)) {
        if word.is_empty() {
            continue;
        }

        // Apply identifier-aware splitting
        let splits = split_identifier(word);

        // Emit the original (lowercased)
        let lower_word = word.to_lowercase();

        // Check if we have meaningful splits
        if splits.len() == 1 && splits[0] == lower_word {
            // Single word, no additional splits needed
            tokens.push(lower_word);
        } else {
            // Emit the original lowercased, then the splits
            tokens.push(lower_word.clone());
            for split in splits {
                // Avoid duplicates (the original might match a split)
                if split != lower_word {
                    tokens.push(split);
                }
            }
        }
    }
}

/// Tokenize a CJK segment using Jieba word-segmentation.
fn tokenize_cjk_segment(segment: &str, tokens: &mut Vec<String>) {
    use jieba_rs::Jieba;

    let jieba = Jieba::new();
    let words = jieba.cut(segment, false); // false = not HMM mode

    for word in words {
        let word = word.trim();
        if !word.is_empty() {
            tokens.push(word.to_lowercase());
        }
    }
}

/// Check if a character is ASCII punctuation (excluding underscore and dot for identifier splitting).
fn is_ascii_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation() && ch != '_' && ch != '.'
}

/// Split an identifier on case transitions, underscores, dots, and digit boundaries.
///
/// Returns a list of lowercase parts.
fn split_identifier(ident: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current_part = String::new();
    let mut prev_was_upper = false;
    let mut prev_was_digit = false;

    for ch in ident.chars() {
        let is_upper = ch.is_uppercase();
        let is_digit = ch.is_ascii_digit();
        let is_separator = ch == '_' || ch == '.';

        // Check for split conditions
        let should_split = if !current_part.is_empty() {
            // Split on separator
            is_separator
                // Split on transition from lowercase to uppercase (camelCase)
                || (!prev_was_upper && is_upper)
                // Split on transition from uppercase to lowercase when previous was uppercase
                // (handles "HTTPServer" -> "HTTP", "Server")
                || (prev_was_upper && !is_upper && current_part.len() > 1)
                // Split on digit boundary
                || (prev_was_digit != is_digit)
        } else {
            false
        };

        if should_split {
            // Push current part if non-empty
            if !current_part.is_empty() {
                let part = current_part.to_lowercase();
                if !parts.contains(&part) {
                    parts.push(part);
                }
            }
            current_part.clear();

            // Skip separators entirely
            if is_separator {
                prev_was_upper = is_upper;
                prev_was_digit = is_digit;
                continue;
            }
        }

        // Add character to current part
        current_part.push(ch);
        prev_was_upper = is_upper;
        prev_was_digit = is_digit;
    }

    // Push final part
    if !current_part.is_empty() {
        let part = current_part.to_lowercase();
        if !parts.contains(&part) {
            parts.push(part);
        }
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_identifier_camel_case() {
        let parts = split_identifier("getUserProfile");
        assert_eq!(parts, vec!["get", "user", "profile"]);
    }

    #[test]
    fn test_split_identifier_snake_case() {
        let parts = split_identifier("parse_user_input");
        assert_eq!(parts, vec!["parse", "user", "input"]);
    }

    #[test]
    fn test_split_identifier_with_digits() {
        let parts = split_identifier("index2D");
        assert_eq!(parts, vec!["index", "2", "d"]);
    }

    #[test]
    fn test_split_identifier_single_word() {
        let parts = split_identifier("parsing");
        assert_eq!(parts, vec!["parsing"]);
    }

    #[test]
    fn test_split_identifier_no_stemming() {
        // "parses" should not be collapsed to "parse"
        let parts = split_identifier("parses");
        assert_eq!(parts, vec!["parses"]);
    }

    #[test]
    fn test_is_cjk_char() {
        assert!(is_cjk_char('中'));
        assert!(is_cjk_char('文'));
        assert!(is_cjk_char('搜'));
        assert!(is_cjk_char('索'));
        assert!(!is_cjk_char('a'));
        assert!(!is_cjk_char('Z'));
        assert!(!is_cjk_char('0'));
    }

    #[test]
    fn test_tokenize_get_user_profile() {
        let tokens = tokenize_text("getUserProfile");
        assert!(tokens.contains(&"getuserprofile".to_string()));
        assert!(tokens.contains(&"get".to_string()));
        assert!(tokens.contains(&"user".to_string()));
        assert!(tokens.contains(&"profile".to_string()));

        // Verify positions are sequential
        let mut stream = MonodexFtsTokenStream::new("getUserProfile");
        let mut positions = Vec::new();
        while stream.advance() {
            positions.push(stream.token().position);
        }
        // Positions should be 0, 1, 2, 3 (globally monotonic)
        assert_eq!(positions, vec![0, 1, 2, 3]);
    }

    #[test]
    fn test_tokenize_parse_user_input() {
        let tokens = tokenize_text("parse_user_input");
        assert!(tokens.contains(&"parse_user_input".to_string()));
        assert!(tokens.contains(&"parse".to_string()));
        assert!(tokens.contains(&"user".to_string()));
        assert!(tokens.contains(&"input".to_string()));
    }

    #[test]
    fn test_tokenize_index_2d() {
        let tokens = tokenize_text("index2D");
        assert!(tokens.contains(&"index2d".to_string()));
        assert!(tokens.contains(&"index".to_string()));
        assert!(tokens.contains(&"2".to_string()));
        assert!(tokens.contains(&"d".to_string()));
    }

    #[test]
    fn test_tokenize_single_word_no_duplicate() {
        let tokens = tokenize_text("parsing");
        // Single lowercase identifier should produce only one token
        assert_eq!(tokens, vec!["parsing"]);
    }

    #[test]
    fn test_tokenize_no_stemming() {
        let tokens = tokenize_text("parses");
        // "parses" should not be collapsed to "parse"
        assert_eq!(tokens, vec!["parses"]);
    }

    #[test]
    fn test_tokenize_cjk_produces_multiple_segments() {
        // Use a phrase that Jieba actually segments into multiple parts
        // "中华人民共和国" is in the dictionary as a single word, but
        // "我来到北京清华大学" produces multiple segments
        let tokens = tokenize_text("我来到北京清华大学");
        // Jieba should segment this into at least 2 tokens: ["我", "来到", "北京", "清华大学"]
        assert!(
            tokens.len() >= 2,
            "Expected at least 2 tokens for CJK text, got {:?}",
            tokens
        );
    }

    #[test]
    fn test_tokenize_multiple_words_sequential_positions() {
        // "getUserProfile parseInput" should have sequential positions across all tokens
        let mut stream = MonodexFtsTokenStream::new("getUserProfile parseInput");
        let mut tokens_with_positions = Vec::new();
        while stream.advance() {
            tokens_with_positions.push((stream.token().text.clone(), stream.token().position));
        }

        // Should have: getuserprofile(0), get(1), user(2), profile(3), parseinput(4), parse(5), input(6)
        assert!(
            tokens_with_positions
                .iter()
                .any(|(t, _)| t == "getuserprofile")
        );
        assert!(tokens_with_positions.iter().any(|(t, _)| t == "parseinput"));

        // Verify positions are globally monotonic
        let positions: Vec<usize> = tokens_with_positions.iter().map(|(_, p)| *p).collect();
        for i in 1..positions.len() {
            assert_eq!(
                positions[i],
                positions[i - 1] + 1,
                "Positions should be sequential"
            );
        }
    }

    #[test]
    fn test_phrase_query_matches_sequential_tokens() {
        use tantivy::Index;
        use tantivy::collector::TopDocs;
        use tantivy::query::QueryParser;

        // Build schema with our tokenizer
        let schema = super::super::schema::fts_schema();
        let row_id_field = schema.get_field("row_id").unwrap();
        let text_field = schema.get_field("text").unwrap();

        // Create in-memory index
        let index = Index::create_in_ram(schema);

        // Register our tokenizer
        index
            .tokenizers()
            .register(FTS_TOKENIZER_NAME, MonodexFtsTokenizer);

        // Index a document with "getUserProfile" which produces tokens:
        // getuserprofile(0), get(1), user(2), profile(3)
        let mut writer = index.writer(50_000_000).expect("writer should succeed");
        writer
            .add_document(tantivy::doc!(
                row_id_field => "test:1",
                text_field => "getUserProfile"
            ))
            .expect("add_document should succeed");
        writer.commit().expect("commit should succeed");

        // Create a reader and searcher
        let reader = index.reader().expect("reader should succeed");
        reader.reload().expect("reload should succeed");
        let searcher = reader.searcher();

        // Build a phrase query for "user profile" - these tokens are at positions 2 and 3
        let query_parser = QueryParser::for_index(&index, vec![text_field]);
        let query = query_parser
            .parse_query("\"user profile\"")
            .expect("parse_query should succeed");

        // Search - the phrase query should match because "user" and "profile"
        // are at adjacent positions (2 and 3)
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(10))
            .expect("search should succeed");

        // The document should match the phrase query
        assert_eq!(
            top_docs.len(),
            1,
            "Phrase query 'user profile' should match document with getUserProfile"
        );
    }

    #[test]
    fn test_cjk_query_parser_respects_tokenizer() {
        use tantivy::Index;
        use tantivy::collector::TopDocs;
        use tantivy::query::QueryParser;

        // Build schema with our tokenizer
        let schema = super::super::schema::fts_schema();
        let row_id_field = schema.get_field("row_id").unwrap();
        let text_field = schema.get_field("text").unwrap();

        // Create in-memory index
        let index = Index::create_in_ram(schema);

        // Register our tokenizer
        index
            .tokenizers()
            .register(FTS_TOKENIZER_NAME, MonodexFtsTokenizer);

        // Index a document with CJK text
        let mut writer = index.writer(50_000_000).expect("writer should succeed");
        writer
            .add_document(tantivy::doc!(
                row_id_field => "test:cjk:1",
                text_field => "我来到北京清华大学"
            ))
            .expect("add_document should succeed");
        writer.commit().expect("commit should succeed");

        // Create a reader and searcher
        let reader = index.reader().expect("reader should succeed");
        reader.reload().expect("reload should succeed");
        let searcher = reader.searcher();

        // Build a query parser that uses our tokenizer for the text field
        let query_parser = QueryParser::for_index(&index, vec![text_field]);

        // Parse a CJK query - Jieba should segment this
        // "北京" is one of the segments from "我来到北京清华大学"
        let query = query_parser
            .parse_query("北京")
            .expect("parse_query should succeed");

        // Search
        let top_docs = searcher
            .search(&query, &TopDocs::with_limit(10))
            .expect("search should succeed");

        // The document should match the CJK query
        assert!(
            !top_docs.is_empty(),
            "CJK query '北京' should match document containing '我来到北京清华大学'"
        );
    }
}
