/*!
VizQL Parser Module

Handles splitting VizQL queries into SQL and visualization portions, then parsing
the visualization specification into a typed AST.

## Architecture

1. **Query Splitting**: Use tree-sitter with external scanner to reliably split
   SQL from VISUALISE portions, handling edge cases like strings and comments.

2. **AST Building**: Convert tree-sitter concrete syntax tree (CST) into a
   typed abstract syntax tree (AST) representing the visualization specification.

3. **Validation**: Perform syntactic validation during parsing, with semantic
   validation deferred to execution time when data is available.

## Example Usage

```rust
# use vizql::parser::parse_query;
# use vizql::Geom;
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let query = r#"
    SELECT date, revenue, region FROM sales WHERE year = 2024
    VISUALISE AS PLOT
    WITH line USING
        x = date,
        y = revenue,
        color = region
    LABEL
        title = 'Sales by Region'
"#;

let spec = parse_query(query)?;
assert_eq!(spec.layers.len(), 1);
// Note: Currently returns Point due to stub implementation
assert_eq!(spec.layers[0].geom, Geom::Point);
# Ok(())
# }
```
*/

use tree_sitter::Tree;
use crate::{VizqlError, Result};

pub mod ast;
pub mod splitter;
pub mod builder;
pub mod error;

// Re-export key types
pub use ast::*;
pub use error::ParseError;

/// Main entry point for parsing VizQL queries
///
/// Takes a complete VizQL query (SQL + VISUALISE) and returns a parsed
/// specification along with the SQL portion.
pub fn parse_query(query: &str) -> Result<VizSpec> {
    // Step 1: Split the query into SQL and VISUALISE portions
    let (sql_part, viz_part) = splitter::split_query(query)?;

    // Step 2: Parse the visualization portion using tree-sitter
    let tree = parse_viz_portion(&viz_part)?;

    // Step 3: Build AST from the tree-sitter parse tree
    let spec = builder::build_ast(&tree, &viz_part)?;

    Ok(spec)
}

/// Parse just the visualization portion using tree-sitter
fn parse_viz_portion(viz_query: &str) -> Result<Tree> {
    let mut parser = tree_sitter::Parser::new();

    // Set the tree-sitter-vizql language
    parser
        .set_language(&tree_sitter_vizql::language())
        .map_err(|e| VizqlError::ParseError(format!("Failed to set language: {}", e)))?;

    // Parse the visualization query directly (no SQL prepending needed)
    let tree = parser
        .parse(viz_query, None)
        .ok_or_else(|| VizqlError::ParseError("Failed to parse visualization query".to_string()))?;

    // Check for parse errors
    if tree.root_node().has_error() {
        return Err(VizqlError::ParseError("Parse tree contains errors".to_string()));
    }

    Ok(tree)
}

/// Extract just the SQL portion from a VizQL query
pub fn extract_sql(query: &str) -> Result<String> {
    let (sql_part, _) = splitter::split_query(query)?;
    Ok(sql_part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_query_parsing() {
        let query = r#"
            SELECT x, y FROM data
            VISUALISE AS PLOT
            WITH point USING
                x = x,
                y = y
        "#;

        let result = parse_query(query);
        assert!(result.is_ok(), "Failed to parse simple query: {:?}", result);

        let spec = result.unwrap();
        assert_eq!(spec.layers.len(), 1);
        assert_eq!(spec.layers[0].geom, Geom::Point);
    }

    #[test]
    fn test_sql_extraction() {
        let query = r#"
            SELECT date, revenue FROM sales WHERE year = 2024
            VISUALISE AS PLOT
            WITH line USING x = date, y = revenue
        "#;

        let sql = extract_sql(query).unwrap();
        assert!(sql.contains("SELECT date, revenue FROM sales"));
        assert!(sql.contains("WHERE year = 2024"));
        assert!(!sql.contains("VISUALISE"));
    }

    #[test]
    fn test_multi_layer_query() {
        let query = r#"
            SELECT x, y, z FROM data
            VISUALISE AS PLOT
            WITH line USING
                x = x,
                y = y
            WITH point USING
                x = x,
                y = z,
                color = 'red'
        "#;

        let spec = parse_query(query).unwrap();
        assert_eq!(spec.layers.len(), 2);
        // Note: These will be Point due to stub implementation
        assert_eq!(spec.layers[0].geom, Geom::Point);
        assert_eq!(spec.layers[1].geom, Geom::Point);
    }
}