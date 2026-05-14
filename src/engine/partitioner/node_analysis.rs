//! Purpose: AST node helpers — meaningful-children enumeration, symbol-name extraction, line-span computation, JSDoc/TSDoc collection.
//! Edit here when: Adding or modifying node-analysis heuristics, symbol extraction, or chunk metadata.
//! Do not edit here for: Debug logging (see `debug.rs`), split logic (see `split_search.rs`), scoring (see `scoring.rs`).

use super::debug::PartitionDebug;
use tree_sitter::Node;

/// Check if a node kind represents a complex type that should not be inlined.
fn is_complex_type(kind: &str) -> bool {
    matches!(
        kind,
        "arrow_function"
            | "function_expression"
            | "function_declaration"
            | "object"
            | "array"
            | "jsx_element"
            | "jsx_self_closing_element"
    )
}

pub(super) fn get_meaningful_children<'a>(
    node: Node<'a>,
    source: &[u8],
    debug: &PartitionDebug,
) -> Vec<Node<'a>> {
    let mut cursor = node.walk();
    let mut children = Vec::new();

    for child in node.children(&mut cursor) {
        if is_meaningful_split_point(child, source) {
            debug.log_meaningful_child(
                child.kind(),
                child.start_position().row + 1,
                child.end_position().row + 1,
            );
            children.push(child);
        }
    }

    children
}

/// Get the end line of import statements (0 if no imports)
pub(super) fn extract_imports_end_line(root: Node, _source: &[u8]) -> usize {
    let mut cursor = root.walk();
    let mut last_import_end = 0usize;

    for child in root.children(&mut cursor) {
        if child.kind() == "import_statement" {
            last_import_end = child.end_position().row + 1;
        } else if last_import_end > 0 {
            break;
        }
    }
    last_import_end
}

/// Get text for a line range (1-indexed, inclusive)
pub(super) fn get_lines_text(lines: &[&str], start_line: usize, end_line: usize) -> String {
    if start_line > end_line || start_line == 0 || start_line > lines.len() {
        return String::new();
    }
    let end = end_line.min(lines.len());
    lines[start_line - 1..end].join("\n")
}

/// Determine if an AST node represents a meaningful split boundary.
///
/// Meaningful nodes are those that can serve as split points between chunks.
/// This includes declarations (functions, classes, interfaces, etc.) and
/// large expression statements (e.g., event handler registrations).
///
/// Small nodes (like 1-line variable declarations) are technically meaningful
/// but may be filtered out later if they would create tiny chunks.
pub(super) fn is_meaningful_split_point(node: Node, source: &[u8]) -> bool {
    match node.kind() {
        "function_declaration"
        | "class_declaration"
        | "interface_declaration"
        | "type_alias_declaration"
        | "enum_declaration" => true,

        "export_statement" => true,

        "method_definition" => true,

        // Class fields and interface properties
        "public_field_definition" | "property_declaration" => {
            node.end_byte() - node.start_byte() > 50
        }

        // Object literal properties (key-value pairs)
        // Only meaningful if large enough or contains complex nested structure
        // Use effective size which includes attached leading comments
        "pair" => {
            let size = effective_size_with_comments(node, source);
            if size > 200 {
                return true;
            }
            // Check if the value contains complex structure (function, object, array)
            pair_has_complex_value(node)
        }

        // Interface property signatures - smaller threshold since interface members
        // are more naturally separable than runtime object-literal properties
        // Use effective size which includes attached leading comments
        "property_signature" => effective_size_with_comments(node, source) > 100,

        "lexical_declaration" | "variable_declaration" => {
            let text = String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()]);
            text.starts_with("const") || text.starts_with("let") || text.starts_with("var")
        }

        // Expression statements are meaningful if they're large enough
        // (e.g., event handlers like `ws.on('connection', ...)`)
        // OR if they contain nested functions (callback registrations)
        "expression_statement" => {
            let size = node.end_byte() - node.start_byte();
            if size > 500 {
                return true;
            }
            // Check if it contains a nested function (callback registration)
            // This makes callback registrations like `obj.on('event', () => {...})`
            // meaningful split points regardless of total size
            node_contains_complex_structure(node)
        }

        // Switch cases are meaningful split points for large switch statements
        // Each case is a logical unit that can be split independently
        "switch_case" => true,

        // If statements are meaningful split points when large enough
        // This helps split large methods with complex control flow
        "if_statement" => node.end_byte() - node.start_byte() > 400,

        // For loops are meaningful split points when large enough
        // This helps split large methods with multiple sequential loops
        "for_statement" | "for_in_statement" | "for_of_statement" => {
            node.end_byte() - node.start_byte() > 300
        }

        // JSX elements are meaningful split points for large JSX
        // Each JSX element is a logical unit that can be split independently
        "jsx_element" => true,

        _ => false,
    }
}

/// Compute the effective size of a node, including any attached leading comments.
///
/// This treats a property + its documentation as a single semantic unit when
/// evaluating whether it forms a meaningful split boundary.
/// Works for both `pair` (object literal properties) and `property_signature` (interface properties).
pub(super) fn effective_size_with_comments(node: Node, source: &[u8]) -> usize {
    let mut start = node.start_byte();
    let end = node.end_byte();

    // Walk backward through previous siblings to find attached comments
    let mut current = node.prev_sibling();

    while let Some(sibling) = current {
        // Only include comment nodes
        if sibling.kind() == "comment" {
            let comment_end = sibling.end_byte();
            let comment_start = sibling.start_byte();

            // Check for blank line gap between comment and current start
            // A blank line means there's more than just whitespace between them
            let between = &source[comment_end..start];
            let has_blank_line = between.windows(2).any(|w| w == b"\n\n")
                || between.windows(4).any(|w| w == b"\r\n\r\n");

            if has_blank_line {
                // Comment is separated by blank line, stop
                break;
            }

            // Include this comment
            start = comment_start;

            // Continue walking backward
            current = sibling.prev_sibling();
        } else {
            // Non-comment sibling, stop
            break;
        }
    }

    end - start
}

/// Check if a pair node contains a complex value (function, object, array)
/// that would make it a meaningful split boundary.
pub(super) fn pair_has_complex_value(pair_node: Node) -> bool {
    // A pair has the structure: key : value
    // We want to check if the value part contains complex structure
    for i in 0..pair_node.child_count() {
        if let Some(child) = pair_node.child(i as u32) {
            match child.kind() {
                // Direct complex value types
                kind if is_complex_type(kind) => {
                    return true;
                }
                // Nested expression might wrap a complex value
                "parenthesized_expression" | "as_expression"
                    if node_contains_complex_structure(child) =>
                {
                    return true;
                }
                _ => {}
            }
        }
    }
    false
}

/// Check if a node recursively contains complex structure
pub(super) fn node_contains_complex_structure(node: Node) -> bool {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            match child.kind() {
                kind if is_complex_type(kind) => {
                    return true;
                }
                _ => {
                    // Recursively check children
                    if node_contains_complex_structure(child) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Get chunk metadata (type, symbol name, breadcrumb suffix) for a line range
pub(super) fn get_chunk_metadata(
    root: Node,
    source: &[u8],
    start_line: usize,
    end_line: usize,
) -> (String, Option<String>, String) {
    let mut best_node: Option<Node> = None;
    let mut best_size = usize::MAX;

    find_best_node(
        root,
        source,
        start_line,
        end_line,
        &mut best_node,
        &mut best_size,
    );

    if let Some(node) = best_node {
        let chunk_type = get_chunk_type(node);
        let symbol_name = get_symbol_name(node, source);
        let breadcrumb_suffix = symbol_name.clone().unwrap_or_default();
        (chunk_type, symbol_name, breadcrumb_suffix)
    } else {
        ("code".to_string(), None, String::new())
    }
}

pub(super) fn find_best_node<'a>(
    node: Node<'a>,
    source: &[u8],
    chunk_start: usize,
    chunk_end: usize,
    best_node: &mut Option<Node<'a>>,
    best_size: &mut usize,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    if node_start > chunk_start || node_end < chunk_end {
        return;
    }

    if is_meaningful_split_point(node, source) {
        let node_size = node_end - node_start;
        if node_size < *best_size {
            *best_node = Some(node);
            *best_size = node_size;
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_best_node(child, source, chunk_start, chunk_end, best_node, best_size);
    }
}

pub(super) fn get_chunk_type(node: Node) -> String {
    if node.kind() == "export_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let child_type = match child.kind() {
                "function_declaration" => "function",
                "class_declaration" => "class",
                "interface_declaration" => "interface",
                "type_alias_declaration" => "type",
                "enum_declaration" => "enum",
                "lexical_declaration" | "variable_declaration" => "variable",
                _ => continue,
            };
            return child_type.to_string();
        }
        return "code".to_string();
    }

    match node.kind() {
        "function_declaration" | "method_definition" => "function",
        "class_declaration" => "class",
        "interface_declaration" => "interface",
        "type_alias_declaration" => "type",
        "enum_declaration" => "enum",
        "lexical_declaration" | "variable_declaration" => "variable",
        "public_field_definition" | "property_declaration" => "field",
        _ => "code",
    }
    .to_string()
}

pub(super) fn get_symbol_name(node: Node, source: &[u8]) -> Option<String> {
    if node.kind() == "export_statement" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(name) = get_symbol_name(child, source) {
                return Some(name);
            }
        }
        return None;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier" | "property_identifier" | "type_identifier" => {
                let name = String::from_utf8_lossy(&source[child.start_byte()..child.end_byte()]);
                return Some(name.to_string());
            }
            _ => {}
        }
    }
    None
}
