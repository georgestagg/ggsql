//! Binned scale type implementation

use std::collections::HashMap;

use polars::prelude::{ChunkAgg, Column, DataType};

use super::{ScaleTypeKind, ScaleTypeTrait, TransformKind};
use crate::plot::{ArrayElement, ParameterValue};

/// Binned scale type - for binned/bucketed data
#[derive(Debug, Clone, Copy)]
pub struct Binned;

impl ScaleTypeTrait for Binned {
    fn scale_type_kind(&self) -> ScaleTypeKind {
        ScaleTypeKind::Binned
    }

    fn name(&self) -> &'static str {
        "binned"
    }

    fn allowed_transforms(&self) -> &'static [TransformKind] {
        &[
            TransformKind::Identity,
            TransformKind::Log10,
            TransformKind::Log2,
            TransformKind::Log,
            TransformKind::Sqrt,
            TransformKind::Asinh,
            TransformKind::PseudoLog,
            // Temporal transforms for date/datetime/time data
            TransformKind::Date,
            TransformKind::DateTime,
            TransformKind::Time,
        ]
    }

    fn default_transform(&self, aesthetic: &str, column_dtype: Option<&DataType>) -> TransformKind {
        // First check column data type for temporal transforms
        if let Some(dtype) = column_dtype {
            match dtype {
                DataType::Date => return TransformKind::Date,
                DataType::Datetime(_, _) => return TransformKind::DateTime,
                DataType::Time => return TransformKind::Time,
                _ => {}
            }
        }

        // Fall back to aesthetic-based defaults
        match aesthetic {
            "size" => TransformKind::Sqrt, // Area-proportional scaling
            _ => TransformKind::Identity,
        }
    }

    fn allowed_properties(&self, aesthetic: &str) -> &'static [&'static str] {
        if super::is_positional_aesthetic(aesthetic) {
            &["expand", "oob", "reverse", "breaks", "pretty", "closed"]
        } else {
            &["oob", "reverse", "breaks", "pretty", "closed"]
        }
    }

    fn get_property_default(&self, aesthetic: &str, name: &str) -> Option<ParameterValue> {
        match name {
            "expand" if super::is_positional_aesthetic(aesthetic) => {
                Some(ParameterValue::Number(super::DEFAULT_EXPAND_MULT))
            }
            "oob" => Some(ParameterValue::String(
                super::default_oob(aesthetic).to_string(),
            )),
            "reverse" => Some(ParameterValue::Boolean(false)),
            "breaks" => Some(ParameterValue::Number(
                super::super::breaks::DEFAULT_BREAK_COUNT as f64,
            )),
            "pretty" => Some(ParameterValue::Boolean(true)),
            // "left" means bins are [lower, upper), "right" means (lower, upper]
            "closed" => Some(ParameterValue::String("left".to_string())),
            _ => None,
        }
    }

    fn allows_data_type(&self, dtype: &DataType) -> bool {
        matches!(
            dtype,
            DataType::Int8
                | DataType::Int16
                | DataType::Int32
                | DataType::Int64
                | DataType::UInt8
                | DataType::UInt16
                | DataType::UInt32
                | DataType::UInt64
                | DataType::Float32
                | DataType::Float64
                // Temporal types supported via temporal transforms
                | DataType::Date
                | DataType::Datetime(_, _)
                | DataType::Time
        )
    }

    fn resolve_input_range(
        &self,
        user_range: Option<&[ArrayElement]>,
        columns: &[&Column],
        properties: &HashMap<String, ParameterValue>,
    ) -> Result<Option<Vec<ArrayElement>>, String> {
        let computed = compute_numeric_range(columns);
        let (mult, add) = super::get_expand_factors(properties);

        // Apply expansion to computed range
        let expanded = computed.map(|range| super::expand_numeric_range(&range, mult, add));

        match user_range {
            None => Ok(expanded),
            Some(range) if super::input_range_has_nulls(range) => {
                // User provided partial range with nulls - merge with expanded computed
                match expanded {
                    Some(inferred) => Ok(Some(super::merge_with_inferred(range, &inferred))),
                    None => Ok(Some(range.to_vec())),
                }
            }
            Some(range) => {
                // User provided explicit full range - still apply expansion
                Ok(Some(super::expand_numeric_range(range, mult, add)))
            }
        }
    }

    fn default_output_range(
        &self,
        aesthetic: &str,
        _input_range: Option<&[ArrayElement]>,
    ) -> Result<Option<Vec<ArrayElement>>, String> {
        use super::super::palettes;

        match aesthetic {
            // Note: "color"/"colour" already split to fill/stroke before scale resolution
            "stroke" | "fill" => {
                let palette = palettes::get_color_palette("sequential")
                    .ok_or_else(|| "Default color palette 'ggsql' not found".to_string())?;
                Ok(Some(
                    palette
                        .iter()
                        .map(|col: &&str| ArrayElement::String(col.to_string()))
                        .collect(),
                ))
            }
            "size" | "linewidth" => Ok(Some(vec![
                ArrayElement::Number(1.0),
                ArrayElement::Number(6.0),
            ])),
            "opacity" => Ok(Some(vec![
                ArrayElement::Number(0.1),
                ArrayElement::Number(1.0),
            ])),
            _ => Ok(None),
        }
    }

    /// Generate SQL for pre-stat binning transformation.
    ///
    /// Uses the resolved breaks to compute bin boundaries via CASE WHEN,
    /// mapping each value to its bin center. Supports arbitrary (non-evenly-spaced) breaks.
    ///
    /// The `closed` property controls which side of the bin is closed:
    /// - `"left"` (default): bins are `[lower, upper)`, last bin is `[lower, upper]`
    /// - `"right"`: bins are `(lower, upper]`, first bin is `[lower, upper]`
    ///
    /// This ensures:
    /// - Values are grouped into bins defined by break boundaries
    /// - Each bin is represented by its center value `(lower + upper) / 2`
    /// - Boundary values are not lost (edge bins include endpoints)
    /// - Data is binned BEFORE any stat transforms are applied
    fn pre_stat_transform_sql(
        &self,
        column_name: &str,
        scale: &super::super::Scale,
    ) -> Option<String> {
        // Get breaks from scale properties (calculated in resolve)
        // breaks should be an Array after resolution
        let breaks = match scale.properties.get("breaks") {
            Some(ParameterValue::Array(arr)) => arr,
            _ => return None,
        };

        if breaks.len() < 2 {
            return None;
        }

        // Extract numeric break values
        let break_values: Vec<f64> = breaks
            .iter()
            .filter_map(|e| match e {
                ArrayElement::Number(v) => Some(*v),
                _ => None,
            })
            .collect();

        if break_values.len() < 2 {
            return None;
        }

        // Get closed property: "left" (default) or "right"
        let closed_left = match scale.properties.get("closed") {
            Some(ParameterValue::String(s)) => s != "right",
            _ => true, // default to left-closed
        };

        // Build CASE WHEN clauses for each bin
        let num_bins = break_values.len() - 1;
        let mut cases = Vec::with_capacity(num_bins);

        for i in 0..num_bins {
            let lower = break_values[i];
            let upper = break_values[i + 1];
            let center = (lower + upper) / 2.0;

            let is_first = i == 0;
            let is_last = i == num_bins - 1;

            // Build the condition based on closed side
            // closed="left": [lower, upper) except last bin which is [lower, upper]
            // closed="right": (lower, upper] except first bin which is [lower, upper]
            let condition = if closed_left {
                if is_last {
                    // Last bin: [lower, upper] (inclusive on both ends)
                    format!("{col} >= {lower} AND {col} <= {upper}", col = column_name)
                } else {
                    // Normal bin: [lower, upper)
                    format!("{col} >= {lower} AND {col} < {upper}", col = column_name)
                }
            } else {
                // closed="right"
                if is_first {
                    // First bin: [lower, upper] (inclusive on both ends)
                    format!("{col} >= {lower} AND {col} <= {upper}", col = column_name)
                } else {
                    // Normal bin: (lower, upper]
                    format!("{col} > {lower} AND {col} <= {upper}", col = column_name)
                }
            };

            cases.push(format!("WHEN {} THEN {}", condition, center));
        }

        // Build final CASE expression
        Some(format!("(CASE {} ELSE NULL END)", cases.join(" ")))
    }
}

/// Compute numeric input range as [min, max] from Columns.
fn compute_numeric_range(column_refs: &[&Column]) -> Option<Vec<ArrayElement>> {
    let mut global_min: Option<f64> = None;
    let mut global_max: Option<f64> = None;

    for column in column_refs {
        let series = column.as_materialized_series();
        if let Ok(ca) = series.cast(&DataType::Float64) {
            if let Ok(f64_series) = ca.f64() {
                if let Some(min) = f64_series.min() {
                    global_min = Some(global_min.map_or(min, |m| m.min(min)));
                }
                if let Some(max) = f64_series.max() {
                    global_max = Some(global_max.map_or(max, |m| m.max(max)));
                }
            }
        }
    }

    match (global_min, global_max) {
        (Some(min), Some(max)) => Some(vec![ArrayElement::Number(min), ArrayElement::Number(max)]),
        _ => None,
    }
}

impl std::fmt::Display for Binned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::scale::Scale;

    #[test]
    fn test_pre_stat_transform_sql_even_breaks() {
        let binned = Binned;
        let mut scale = Scale::new("x");
        scale.properties.insert(
            "breaks".to_string(),
            ParameterValue::Array(vec![
                ArrayElement::Number(0.0),
                ArrayElement::Number(10.0),
                ArrayElement::Number(20.0),
                ArrayElement::Number(30.0),
            ]),
        );

        let sql = binned.pre_stat_transform_sql("value", &scale).unwrap();

        // Should produce CASE WHEN with bin centers 5, 15, 25
        assert!(sql.contains("CASE"));
        assert!(sql.contains("WHEN value >= 0 AND value < 10 THEN 5"));
        assert!(sql.contains("WHEN value >= 10 AND value < 20 THEN 15"));
        // Last bin should be inclusive on both ends
        assert!(sql.contains("WHEN value >= 20 AND value <= 30 THEN 25"));
        assert!(sql.contains("ELSE NULL END"));
    }

    #[test]
    fn test_pre_stat_transform_sql_uneven_breaks() {
        let binned = Binned;
        let mut scale = Scale::new("x");
        // Non-evenly-spaced breaks: [0, 10, 25, 100]
        scale.properties.insert(
            "breaks".to_string(),
            ParameterValue::Array(vec![
                ArrayElement::Number(0.0),
                ArrayElement::Number(10.0),
                ArrayElement::Number(25.0),
                ArrayElement::Number(100.0),
            ]),
        );

        let sql = binned.pre_stat_transform_sql("x", &scale).unwrap();

        // Bin centers: (0+10)/2=5, (10+25)/2=17.5, (25+100)/2=62.5
        assert!(sql.contains("THEN 5")); // center of [0, 10)
        assert!(sql.contains("THEN 17.5")); // center of [10, 25)
        assert!(sql.contains("THEN 62.5")); // center of [25, 100]
    }

    #[test]
    fn test_pre_stat_transform_sql_closed_left_default() {
        let binned = Binned;
        let mut scale = Scale::new("x");
        scale.properties.insert(
            "breaks".to_string(),
            ParameterValue::Array(vec![
                ArrayElement::Number(0.0),
                ArrayElement::Number(10.0),
                ArrayElement::Number(20.0),
            ]),
        );
        // No explicit closed property, should default to "left"

        let sql = binned.pre_stat_transform_sql("col", &scale).unwrap();

        // closed="left": [lower, upper) except last which is [lower, upper]
        assert!(sql.contains("col >= 0 AND col < 10"));
        assert!(sql.contains("col >= 10 AND col <= 20")); // last bin inclusive
    }

    #[test]
    fn test_pre_stat_transform_sql_closed_right() {
        let binned = Binned;
        let mut scale = Scale::new("x");
        scale.properties.insert(
            "breaks".to_string(),
            ParameterValue::Array(vec![
                ArrayElement::Number(0.0),
                ArrayElement::Number(10.0),
                ArrayElement::Number(20.0),
            ]),
        );
        scale.properties.insert(
            "closed".to_string(),
            ParameterValue::String("right".to_string()),
        );

        let sql = binned.pre_stat_transform_sql("col", &scale).unwrap();

        // closed="right": first bin is [lower, upper], rest are (lower, upper]
        assert!(sql.contains("col >= 0 AND col <= 10")); // first bin inclusive
        assert!(sql.contains("col > 10 AND col <= 20"));
    }

    #[test]
    fn test_pre_stat_transform_sql_insufficient_breaks() {
        let binned = Binned;
        let mut scale = Scale::new("x");

        // Only one break - not enough to form a bin
        scale.properties.insert(
            "breaks".to_string(),
            ParameterValue::Array(vec![ArrayElement::Number(0.0)]),
        );

        assert!(binned.pre_stat_transform_sql("x", &scale).is_none());
    }

    #[test]
    fn test_pre_stat_transform_sql_no_breaks() {
        let binned = Binned;
        let scale = Scale::new("x");
        // No breaks property at all

        assert!(binned.pre_stat_transform_sql("x", &scale).is_none());
    }

    #[test]
    fn test_pre_stat_transform_sql_number_breaks_returns_none() {
        let binned = Binned;
        let mut scale = Scale::new("x");
        // breaks is still a Number (count), not resolved to Array yet
        scale
            .properties
            .insert("breaks".to_string(), ParameterValue::Number(5.0));

        // Should return None because breaks hasn't been resolved to Array
        assert!(binned.pre_stat_transform_sql("x", &scale).is_none());
    }

    #[test]
    fn test_closed_property_default() {
        let binned = Binned;
        let default = binned.get_property_default("x", "closed");
        assert_eq!(default, Some(ParameterValue::String("left".to_string())));
    }

    #[test]
    fn test_closed_property_allowed() {
        let binned = Binned;
        let allowed = binned.allowed_properties("x");
        assert!(allowed.contains(&"closed"));
    }
}
