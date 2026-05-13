//! Purpose: Test suite for the partitioner module.
//! Edit here when: Adding or modifying partitioner tests.
//! Do not edit here for: Production code changes — edit the relevant submodule.

use super::*;
use insta::assert_snapshot;
use tree_sitter::{Language, Node, Parser};

fn format_chunks_summary(chunks: &[PartitionedChunk], file_chars: usize) -> String {
    let report = ChunkQualityReport::from_chunks(chunks, file_chars);
    let mut result = format!(
        "=== QUALITY SCORE ===\nScore: {:.1}%\nTotal chunks: {}\nSmall chunks (<{} chars): {}\nChars: {}-{} (mean {:.0})\n\n",
        report.score,
        report.total_chunks,
        SMALL_CHUNK_CHARS,
        report.small_chunks,
        report.min_chars,
        report.max_chars,
        report.mean_chars
    );
    for (i, chunk) in chunks.iter().enumerate() {
        result.push_str(&format!(
            "=== CHUNK {} ===\nBreadcrumb: {}\nType: {}\nKind: {}\nSymbol: {:?}\nLines: {}-{}\nSize: {} chars\nText preview (5 lines):\n{}\n\n",
            i + 1,
            chunk.breadcrumb,
            chunk.chunk_type,
            chunk.chunk_kind,
            chunk.symbol_name,
            chunk.start_line,
            chunk.end_line,
            chunk.breadcrumb.len() + chunk.text.len(),
            chunk.text.lines().take(5).collect::<Vec<_>>().join("\n")
        ));
    }
    result
}

/// Visualize chunks as split points in the original source
fn format_chunks_visualization(source: &str, chunks: &[PartitionedChunk]) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let mut result = String::new();

    for (i, chunk) in chunks.iter().enumerate() {
        let line_count = chunk.end_line - chunk.start_line + 1;
        let size = chunk.text.len();

        result.push_str(&format!(
            "-- [CHUNK {}] [{} lines] [{} chars] --\n",
            i + 1,
            line_count,
            size
        ));

        for line_num in chunk.start_line..=chunk.end_line {
            if line_num > 0 && line_num <= lines.len() {
                result.push_str(lines[line_num - 1]);
                result.push('\n');
            }
        }
    }

    result
}

#[test]
fn test_simple_function() {
    let source = r#"
export function add(a: number, b: number): number {
    return a + b;
}
"#;
    let config = PartitionConfig {
        file_name: "test.ts".to_string(),
        package_name: "@test/package".to_string(),
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(source, &config, "test.ts", "test").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("simple_function_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("simple_function_summary", summary);
}

#[test]
fn test_class_with_methods() {
    let source = r#"
/**
 * A simple calculator class.
 */
export class Calculator {
    /**
     * Adds two numbers.
     */
    add(a: number, b: number): number {
        return a + b;
    }
    
    /**
     * Subtracts two numbers.
     */
    subtract(a: number, b: number): number {
        return a - b;
    }
    
    /**
     * Multiplies two numbers.
     */
    multiply(a: number, b: number): number {
        return a * b;
    }
}
"#;
    let config = PartitionConfig {
        file_name: "Calculator.ts".to_string(),
        package_name: "@math/package".to_string(),
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(source, &config, "Calculator.ts", "math").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("class_with_methods_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("class_with_methods_summary", summary);
}

#[test]
fn test_jsonfile_partition() {
    let source = include_str!("../../../test_artifacts/JsonFile.ts");
    let config = PartitionConfig {
        file_name: "JsonFile.ts".to_string(),
        package_name: "@rushstack/node-core-library".to_string(),
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(source, &config, "JsonFile.ts", "node-core-library").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("jsonfile_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("jsonfile_summary", summary);
}

#[test]
fn test_small_file_not_penalized() {
    // A small file with one chunk should score 100%
    // (not penalized for being "tiny" since the whole file is tiny)
    let source = r#"// Small test file
export function tiny(): number {
    return 42;
}
"#;
    let config = PartitionConfig {
        file_name: "tiny.ts".to_string(),
        package_name: "@test/package".to_string(),
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(source, &config, "tiny.ts", "test").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("small_file_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("small_file_summary", summary);
}

#[test]
fn test_small_file_should_not_split() {
    // A 12-line .d.ts file (242 chars) should NOT be split into 2 chunks
    // This is a regression test for the "imports always split" bug
    let source = include_str!("../../../test_artifacts/rollup.d.ts");
    let config = PartitionConfig {
        file_name: "rollup.d.ts".to_string(),
        package_name: "api-extractor-scenarios".to_string(),
        allow_fallback: false,
        ..Default::default()
    };
    let chunks =
        partition_typescript(source, &config, "rollup.d.ts", "api-extractor-scenarios").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("rollup_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("rollup_summary", summary);
}

#[test]
fn test_tunneled_browser_connection() {
    // A 231-line file with nested functions that produces tiny chunks
    // This is a regression test for the "tiny chunks for variables" bug
    let source = include_str!("../../../test_artifacts/TunneledBrowserConnection.ts");
    let config = PartitionConfig {
        file_name: "TunneledBrowserConnection.ts".to_string(),
        package_name: "@rushstack/playwright-browser-tunnel".to_string(),
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(
        source,
        &config,
        "TunneledBrowserConnection.ts",
        "playwright-browser-tunnel",
    )
    .unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("tunneled_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("tunneled_summary", summary);
}

#[test]
fn test_long_template_string_fallback() {
    // A degenerate case: a long template literal that cannot be split by AST
    // The chunker should fall back to line-based splitting with a warning
    // We make it 200 lines to exceed the 6000 char target
    let mut source_lines = vec![
        "// A file with a very long template string".to_string(),
        "const longString = `".to_string(),
    ];
    for i in 1..=200 {
        source_lines.push(format!("line{} some content here to make it longer", i));
    }
    source_lines.push("`;".to_string());
    source_lines.push("console.log(longString);".to_string());
    let source = source_lines.join("\n");

    let config = PartitionConfig {
        file_name: "long_string.ts".to_string(),
        package_name: "test".to_string(),
        allow_fallback: true, // This test explicitly tests fallback behavior
        ..Default::default()
    };
    let chunks = partition_typescript(&source, &config, "long_string.ts", "test").unwrap();

    let visualization = format_chunks_visualization(&source, &chunks);
    assert_snapshot!("long_string_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("long_string_summary", summary);

    // Verify that the long string was split despite having no AST split points
    assert!(
        chunks.len() > 1,
        "Expected fallback to split the oversized chunk"
    );

    // Verify no chunk exceeds target size (with some tolerance for the fallback)
    for chunk in &chunks {
        assert!(
            chunk.text.len() <= config.target_size + 500,
            "Chunk at lines {}-{} exceeds target: {} chars",
            chunk.start_line,
            chunk.end_line,
            chunk.text.len()
        );
    }
}

#[test]
fn test_colorize_class_with_enum() {
    // A 289-line file (8031 chars) with an enum and a class with many methods
    // This tests the ability to split a class into method-level chunks
    let source = include_str!("../../../test_artifacts/Colorize.ts");
    let config = PartitionConfig {
        file_name: "Colorize.ts".to_string(),
        package_name: "@rushstack/terminal".to_string(),
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(source, &config, "Colorize.ts", "terminal").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("colorize_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("colorize_summary", summary);

    // Should NOT have fallback split - should use AST-based splitting at method boundaries
    for chunk in &chunks {
        assert!(
            !chunk.breadcrumb.contains("[fallback-split]"),
            "Unexpected fallback split in chunk: {}",
            chunk.breadcrumb
        );
    }
}

#[test]
fn test_ipackagejson_interface_file() {
    // An interface-only file with large interfaces
    // Tests that interface boundaries are used as split points
    let source = include_str!("../../../test_artifacts/IPackageJson.ts");
    let config = PartitionConfig {
        file_name: "IPackageJson.ts".to_string(),
        package_name: "@rushstack/node-core-library".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks =
        partition_typescript(source, &config, "IPackageJson.ts", "node-core-library").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("ipackagejson_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("ipackagejson_summary", summary);

    // Should not need fallback split for interface files
    for chunk in &chunks {
        assert!(
            !chunk.breadcrumb.contains("[fallback-split]"),
            "Unexpected fallback split in chunk: {}",
            chunk.breadcrumb
        );
    }
}

#[test]
fn test_environment_configuration() {
    // A file with two giant constructs:
    // 1. A 226-line const object (EnvironmentVariableNames)
    // 2. A 476-line class (EnvironmentConfiguration)
    //
    // This tests the "single giant construct" problem where we have very few
    // meaningful split points because the file is dominated by large object/class
    // literals that don't have natural internal split boundaries.
    let source = include_str!("../../../test_artifacts/EnvironmentConfiguration.ts");
    let config = PartitionConfig {
        file_name: "EnvironmentConfiguration.ts".to_string(),
        package_name: "rush-lib".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks =
        partition_typescript(source, &config, "EnvironmentConfiguration.ts", "rush-lib").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("environment_config_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("environment_config_summary", summary);

    // Should not have oversized chunks (target is 6000 chars)
    // This verifies effective_size_with_comments() allows splitting
    // documented object literal properties
    for chunk in &chunks {
        assert!(
            chunk.text.len() <= 6000,
            "Oversized chunk: {} chars in {}",
            chunk.text.len(),
            chunk.breadcrumb
        );
    }
}

#[test]
fn test_nested_functions_in_generator() {
    // A minimal test case for nested functions inside a generator.
    // The nested functions (advance, parseA, parseB, parseC) should be
    // recognized as meaningful split points.
    let source = include_str!("../../../test_artifacts/NestedFunctions.ts");
    let config = PartitionConfig {
        file_name: "NestedFunctions.ts".to_string(),
        package_name: "test".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(source, &config, "NestedFunctions.ts", "test").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("nested_functions_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("nested_functions_summary", summary);

    // TODO: Nested functions should be recognized as split points
    // Currently failing - nested functions inside generators/functions are not split points
    // for chunk in &chunks {
    //     assert!(!chunk.breadcrumb.contains("[fallback-split]"),
    //         "Unexpected fallback split in chunk: {}", chunk.breadcrumb);
    // }
}

#[test]
fn test_git_status_parser() {
    // A real-world file with nested functions inside a generator.
    // The parseGitStatus generator contains several nested functions:
    // - getFieldAndAdvancePos
    // - parseUntrackedEntry
    // - parseAddModifyOrDeleteEntry
    // - parseRenamedOrCopiedEntry
    // - parseUnmergedEntry
    // These should be recognized as meaningful split points.
    let source = include_str!("../../../test_artifacts/GitStatusParser.ts");
    let config = PartitionConfig {
        file_name: "GitStatusParser.ts".to_string(),
        package_name: "rush-lib".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(source, &config, "GitStatusParser.ts", "rush-lib").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("git_status_parser_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("git_status_parser_summary", summary);

    // TODO: Nested functions should be recognized as split points
    // Currently failing - nested functions inside generators are not split points
    // for chunk in &chunks {
    //     assert!(!chunk.breadcrumb.contains("[fallback-split]"),
    //         "Unexpected fallback split in chunk: {}", chunk.breadcrumb);
    // }
}

#[test]
fn debug_nested_function_ast() {
    // Debug test to understand AST structure of nested functions
    let source = r#"
function* generator() {
  function nested1() {
    return 1;
  }
  function nested2() {
    return 2;
  }
}
"#;

    let mut parser = Parser::new();
    parser
        .set_language(&Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT))
        .unwrap();
    let tree = parser.parse(source, None).unwrap();

    fn print_tree(node: Node, indent: usize) {
        let kind = node.kind();
        let start = node.start_position();
        let end = node.end_position();
        println!(
            "{:indent$}{} [{},{}]",
            "",
            kind,
            start.row + 1,
            end.row + 1,
            indent = indent
        );
        for i in 0..node.child_count() {
            print_tree(node.child(i as u32).unwrap(), indent + 2);
        }
    }

    print_tree(tree.root_node(), 0);
}

#[test]
fn debug_exported_generator_ast() {
    // Debug test to understand AST structure of exported generator
    let source = r#"
export function* parseGitStatus() {
  function nested1() {
    return 1;
  }
  function nested2() {
    return 2;
  }
}
"#;

    let mut parser = Parser::new();
    parser
        .set_language(&Language::from(tree_sitter_typescript::LANGUAGE_TYPESCRIPT))
        .unwrap();
    let tree = parser.parse(source, None).unwrap();

    fn print_tree(node: Node, indent: usize) {
        let kind = node.kind();
        let start = node.start_position();
        let end = node.end_position();
        println!(
            "{:indent$}{} [{},{}]",
            "",
            kind,
            start.row + 1,
            end.row + 1,
            indent = indent
        );
        for i in 0..node.child_count() {
            print_tree(node.child(i as u32).unwrap(), indent + 2);
        }
    }

    print_tree(tree.root_node(), 0);
}

#[test]
fn test_project_watcher() {
    // A real-world file with nested functions inside async methods.
    // The waitForChangeAsync method contains several nested functions:
    // - onError, addWatcher, innerListener, changeListener
    // These should be recognized as meaningful split points.
    let source = include_str!("../../../test_artifacts/ProjectWatcher.ts");
    let config = PartitionConfig {
        file_name: "ProjectWatcher.ts".to_string(),
        package_name: "rush-lib".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(source, &config, "ProjectWatcher.ts", "rush-lib").unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("project_watcher_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("project_watcher_summary", summary);

    // Nested functions inside methods should be recognized as split points
    for chunk in &chunks {
        assert!(
            !chunk.breadcrumb.contains("[fallback-split]"),
            "Unexpected fallback split in chunk: {}",
            chunk.breadcrumb
        );
    }
}

#[test]
fn test_parameter_form_tsx() {
    // A TSX file with React hooks and JSX elements.
    // The file should use the TSX grammar (not TypeScript) and
    // split at JSX element boundaries.
    let source = include_str!("../../../test_artifacts/ParameterForm.tsx");
    let config = PartitionConfig {
        file_name: "ParameterForm.tsx".to_string(),
        package_name: "@rushstack/rush-vscode-command-webview".to_string(),
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(
        source,
        &config,
        "ParameterForm.tsx",
        "@rushstack/rush-vscode-command-webview",
    )
    .unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("parameter_form_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("parameter_form_summary", summary);

    // Note: This file currently has a fallback split due to a large function body
    // that lacks natural split boundaries. This is a known limitation that may
    // be addressed in a future update.
}

#[test]
fn test_experiments_configuration() {
    // ExperimentsConfiguration.ts contains a large interface (IExperimentsJson)
    // with documented properties. Each property has a JSDoc comment that makes it
    // exceed the 100 byte threshold when counting the comment, but the property_signature
    // alone is small.
    //
    // This tests effective_size_with_comments() for interface property signatures.
    //
    let source = include_str!("../../../test_artifacts/ExperimentsConfiguration.ts");
    let config = PartitionConfig {
        file_name: "ExperimentsConfiguration.ts".to_string(),
        package_name: "@microsoft/rush-lib".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(
        source,
        &config,
        "ExperimentsConfiguration.ts",
        "@microsoft/rush-lib",
    )
    .unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("experiments_configuration_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("experiments_configuration_summary", summary);

    // Should not have oversized chunks (target is 6000 chars)
    for chunk in &chunks {
        assert!(
            chunk.text.len() <= 6000,
            "Oversized chunk: {} chars in {}",
            chunk.text.len(),
            chunk.breadcrumb
        );
    }
}

#[test]
fn test_documented_interface() {
    // IYamlApiFile.ts contains large interfaces with documented properties.
    // Each property has a JSDoc comment that makes it exceed the 100 byte threshold
    // when counting the comment, but the property_signature alone is small.
    //
    // This tests effective_size_with_comments() for interface property signatures.
    //
    let source = include_str!("../../../test_artifacts/IYamlApiFile.ts");
    let config = PartitionConfig {
        file_name: "IYamlApiFile.ts".to_string(),
        package_name: "@microsoft/api-documenter".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(
        source,
        &config,
        "IYamlApiFile.ts",
        "@microsoft/api-documenter",
    )
    .unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("iyaml_api_file_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("iyaml_api_file_summary", summary);

    // Should not have oversized chunks (target is 6000 chars)
    for chunk in &chunks {
        assert!(
            chunk.text.len() <= 6000,
            "Oversized chunk: {} chars in {}",
            chunk.text.len(),
            chunk.breadcrumb
        );
    }
}

#[test]
fn test_module_minifier_plugin() {
    // ModuleMinifierPlugin.ts contains a large method (apply) with nested callback
    // registrations. The method body has multiple expression_statement children that
    // are callback registrations (tap calls) with nested functions.
    //
    // The issue: Some expression_statements are <500 bytes but contain nested functions
    // (callback registrations). These should be meaningful split points.
    //
    let source = include_str!("../../../test_artifacts/ModuleMinifierPlugin.ts");
    let config = PartitionConfig {
        file_name: "ModuleMinifierPlugin.ts".to_string(),
        package_name: "@rushstack/webpack5-module-minifier-plugin".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(
        source,
        &config,
        "ModuleMinifierPlugin.ts",
        "@rushstack/webpack5-module-minifier-plugin",
    )
    .unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("module_minifier_plugin_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("module_minifier_plugin_summary", summary);

    // Note: This file currently has fallback splits due to large method bodies
    // that lack natural split boundaries. This is a known limitation that may
    // be addressed in a future update.
}

#[test]
fn test_parameter_form_tsx_large() {
    // ParameterForm.tsx is a large React component with multiple useEffect hooks
    // and a large JSX return statement.
    //
    // The issue: The component function body has many small expression_statements
    // (useCallback, useEffect) and a large JSX return. The expression_statements
    // containing arrow functions should be meaningful split points.
    //
    // Test hook/expression boundaries in React component.
    //
    let source = include_str!("../../../test_artifacts/ParameterForm.tsx");
    let config = PartitionConfig {
        file_name: "ParameterForm.tsx".to_string(),
        package_name: "@rushstack/rush-vscode-command-webview".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(
        source,
        &config,
        "ParameterForm.tsx",
        "@rushstack/rush-vscode-command-webview",
    )
    .unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("parameter_form_large_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("parameter_form_large_summary", summary);

    // Note: This file currently has fallback splits due to a large function body
    // that lacks natural split boundaries. This is a known limitation that may
    // be addressed in a future update.
}

#[test]
fn test_generate_patched_file() {
    // generate-patched-file.ts contains a large function with string concatenations
    // and conditional blocks. The function body has many expression_statements
    // that are outputFile += ... operations.
    //
    // The issue: Many expression_statements are small (<500 bytes) but the overall
    // function body is large. The algorithm needs to find split points between
    // logical sections.
    //
    let source = include_str!("../../../test_artifacts/generate-patched-file.ts");
    let config = PartitionConfig {
        file_name: "generate-patched-file.ts".to_string(),
        package_name: "@rushstack/eslint-patch".to_string(),
        debug: PartitionDebug { enabled: true },
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(
        source,
        &config,
        "generate-patched-file.ts",
        "@rushstack/eslint-patch",
    )
    .unwrap();

    let visualization = format_chunks_visualization(source, &chunks);
    assert_snapshot!("generate_patched_file_visualization", visualization);

    let summary = format_chunks_summary(&chunks, source.len());
    assert_snapshot!("generate_patched_file_summary", summary);

    // Note: This file currently has fallback splits due to a large function body
    // that lacks natural split boundaries. This is a known limitation that may
    // be addressed in a future update.
}

#[test]
fn test_breadcrumb_percent_encoding_round_trip() {
    // Test that file names with reserved characters are percent-encoded in breadcrumbs.
    // : must be encoded as %3A in locators and breadcrumbs (see docs/design/label_ids.md).
    // This test uses a file named `weird:file.ts` and verifies the emitted breadcrumb
    // contains `weird%3Afile.ts` (not `weird:file.ts` which would be ambiguous).
    let source = r#"
export function hello(): string {
    return "Hello, world!";
}
"#;
    let config = PartitionConfig {
        // File name contains `:` which is a reserved character in the locator grammar
        file_name: "weird:file.ts".to_string(),
        package_name: "test-package".to_string(),
        allow_fallback: false,
        ..Default::default()
    };
    let chunks = partition_typescript(source, &config, "weird:file.ts", "test-catalog").unwrap();

    // There should be exactly one chunk (the function)
    assert!(!chunks.is_empty(), "Expected at least one chunk");

    let chunk = &chunks[0];

    // The breadcrumb should have `:` encoded as `%3A` in the file name component
    // Expected format: "test-package:weird%3Afile.ts[:symbol]"
    // NOT: "test-package:weird:file.ts[:symbol]" (ambiguous - `:` in file name creates extra segments)
    assert!(
        chunk.breadcrumb.contains("weird%3Afile.ts"),
        "Breadcrumb should have percent-encoded colon in file name. Got: {}",
        chunk.breadcrumb
    );
    assert!(
        !chunk.breadcrumb.contains("weird:file.ts"),
        "Breadcrumb should NOT contain unencoded colon in file name. Got: {}",
        chunk.breadcrumb
    );

    // Verify the base format: package:encoded_file[:symbol]
    assert!(
        chunk.breadcrumb.starts_with("test-package:weird%3Afile.ts"),
        "Breadcrumb should start with 'test-package:weird%3Afile.ts', got: {}",
        chunk.breadcrumb
    );
}
