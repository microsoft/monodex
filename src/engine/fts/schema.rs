//! Tantivy schema for FTS indexing.
//!
//! Purpose: Define the Tantivy schema for full-text search on code chunks.
//! Edit here when: Adding/removing/modifying fields in the FTS index.
//! Do not edit here for: Tokenization rules (see tokenizer.rs), indexing logic (see indexing.rs).
//!
//! ## Schema design
//!
//! The FTS schema has two fields:
//!
//! - `row_id`: `STRING | STORED` - Indexed as untokenized keyword for exact match lookups.
//!   Stored so search hits can hydrate the row_id back from the document.
//!
//! - `text`: `TEXT` indexed with positions, configured to use `monodex-fts` tokenizer.
//!   NOT stored because chunk text lives in LanceDB and is retrieved by row_id.

use tantivy::schema::{IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions};

use super::tokenizer::FTS_TOKENIZER_NAME;

/// Build the Tantivy schema for FTS indexing.
///
/// Returns a schema with two fields:
/// - `row_id`: STRING field, stored, for exact match lookups
/// - `text`: TEXT field with positions and monodex-fts tokenizer, not stored
pub fn fts_schema() -> Schema {
    let mut schema_builder = Schema::builder();

    // row_id: STRING | STORED
    // Indexed as untokenized keyword for exact match lookups.
    // Stored so search hits hydrate the row_id back from the doc.
    schema_builder.add_text_field("row_id", STRING | STORED);

    // text: TEXT indexed with positions, configured to use monodex-fts tokenizer
    // NOT stored (chunk text is in LanceDB, retrieved by row_id)
    let text_indexing = TextFieldIndexing::default()
        .set_index_option(IndexRecordOption::WithFreqsAndPositions)
        .set_tokenizer(FTS_TOKENIZER_NAME);

    let text_options = TextOptions::default().set_indexing_options(text_indexing);

    schema_builder.add_text_field("text", text_options);

    schema_builder.build()
}

/// Field handles for the FTS schema.
///
/// These are returned by the schema builder and used to construct queries
/// and documents. The fields are identified by their `Field` handles,
/// which are stable for the lifetime of the schema.
pub struct FtsSchemaFields {
    pub row_id: tantivy::schema::Field,
    pub text: tantivy::schema::Field,
}

/// Get field handles from a schema.
///
/// This is used after constructing or opening an index to get the field handles
/// needed for document construction and querying.
pub fn get_fts_fields(schema: &Schema) -> FtsSchemaFields {
    FtsSchemaFields {
        row_id: schema.get_field("row_id").expect("row_id field must exist"),
        text: schema.get_field("text").expect("text field must exist"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fts_schema_constructible() {
        let schema = fts_schema();

        // Verify row_id field exists
        let row_id_field = schema.get_field("row_id");
        assert!(row_id_field.is_ok());

        // Verify text field exists
        let text_field = schema.get_field("text");
        assert!(text_field.is_ok());

        // Verify we can get field handles
        let fields = get_fts_fields(&schema);
        assert_eq!(fields.row_id, row_id_field.unwrap());
        assert_eq!(fields.text, text_field.unwrap());
    }

    #[test]
    fn test_fts_schema_field_count() {
        let schema = fts_schema();
        // Should have exactly 2 fields
        assert_eq!(schema.fields().count(), 2);
    }
}
