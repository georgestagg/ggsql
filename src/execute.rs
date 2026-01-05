//! Query execution module for ggSQL
//!
//! Provides shared execution logic for building data maps from queries,
//! handling both global SQL and layer-specific data sources.

use std::collections::HashMap;
use crate::{parser, DataFrame, GgsqlError, Result, VizSpec};

#[cfg(feature = "duckdb")]
use crate::reader::{DuckDBReader, Reader};

/// Result of preparing data for visualization
pub struct PreparedData {
    /// Data map with global and layer-specific DataFrames
    pub data: HashMap<String, DataFrame>,
    /// Parsed and resolved visualization specifications
    pub specs: Vec<VizSpec>,
}

/// Build data map from a query using a custom query executor function
///
/// This is the most flexible variant that works with any query execution strategy,
/// including shared state readers in REST API contexts.
///
/// # Arguments
/// * `query` - The full ggSQL query string
/// * `execute_query` - A function that executes SQL and returns a DataFrame
pub fn prepare_data_with_executor<F>(query: &str, execute_query: F) -> Result<PreparedData>
where
    F: Fn(&str) -> Result<DataFrame>,
{
    // Split query into SQL and viz portions
    let (sql_part, viz_part) = parser::split_query(query)?;

    // Parse visualization portion
    let mut specs = parser::parse_query(query)?;

    if specs.is_empty() {
        return Err(GgsqlError::ValidationError(
            "No visualization specifications found".to_string(),
        ));
    }

    // Check if we have any visualization content
    if viz_part.trim().is_empty() {
        return Err(GgsqlError::ValidationError(
            "The visualization portion is empty".to_string(),
        ));
    }

    // Build data map for multi-source support
    let mut data_map: HashMap<String, DataFrame> = HashMap::new();

    // Execute global SQL if present
    if !sql_part.trim().is_empty() {
        let df = execute_query(&sql_part)?;
        data_map.insert("__global__".to_string(), df);
    }

    // Execute layer-specific queries
    let first_spec = &specs[0];
    for (idx, layer) in first_spec.layers.iter().enumerate() {
        if let Some(ref source) = layer.source {
            let layer_query = match source {
                crate::LayerSource::Identifier(name) => format!("SELECT * FROM {}", name),
                crate::LayerSource::FilePath(path) => format!("SELECT * FROM '{}'", path),
            };
            let df = execute_query(&layer_query).map_err(|e| {
                GgsqlError::ReaderError(format!(
                    "Failed to fetch data for layer {} (source: {}): {}",
                    idx + 1,
                    source.as_str(),
                    e
                ))
            })?;
            data_map.insert(format!("__layer_{}__", idx), df);
        }
    }

    // Validate we have some data
    if data_map.is_empty() {
        return Err(GgsqlError::ValidationError(
            "No data sources found. Either provide a SQL query or use MAPPING FROM in layers."
                .to_string(),
        ));
    }

    // For layers without specific sources, ensure global data exists
    let has_layer_without_source = first_spec.layers.iter().any(|l| l.source.is_none());
    if has_layer_without_source && !data_map.contains_key("__global__") {
        return Err(GgsqlError::ValidationError(
            "Some layers use global data but no SQL query was provided.".to_string(),
        ));
    }

    // Resolve global mappings using global data if available, otherwise first layer data
    let resolve_df = data_map
        .get("__global__")
        .or_else(|| data_map.values().next())
        .ok_or_else(|| GgsqlError::InternalError("No data available".to_string()))?;

    let column_names: Vec<&str> = resolve_df
        .get_column_names()
        .iter()
        .map(|s| s.as_str())
        .collect();

    for spec in &mut specs {
        spec.resolve_global_mappings(&column_names)?;
    }

    Ok(PreparedData { data: data_map, specs })
}

/// Build data map from a query using DuckDB reader
///
/// Convenience wrapper around `prepare_data_with_executor` for direct DuckDB reader usage.
#[cfg(feature = "duckdb")]
pub fn prepare_data(query: &str, reader: &DuckDBReader) -> Result<PreparedData> {
    prepare_data_with_executor(query, |sql| reader.execute(sql))
}

#[cfg(test)]
#[cfg(feature = "duckdb")]
mod tests {
    use super::*;

    #[test]
    fn test_prepare_data_global_only() {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let query = "SELECT 1 as x, 2 as y VISUALISE x, y DRAW point";

        let result = prepare_data(query, &reader).unwrap();

        assert!(result.data.contains_key("__global__"));
        assert_eq!(result.specs.len(), 1);
    }

    #[test]
    fn test_prepare_data_no_viz() {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let query = "SELECT 1 as x, 2 as y";

        let result = prepare_data(query, &reader);
        assert!(result.is_err());
    }

    #[test]
    fn test_prepare_data_layer_source() {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();

        // Create a table first
        reader.connection().execute(
            "CREATE TABLE test_data AS SELECT 1 as a, 2 as b",
            duckdb::params![],
        ).unwrap();

        let query = "VISUALISE DRAW point MAPPING a AS x, b AS y FROM test_data";

        let result = prepare_data(query, &reader).unwrap();

        assert!(result.data.contains_key("__layer_0__"));
        assert!(!result.data.contains_key("__global__"));
    }
}
