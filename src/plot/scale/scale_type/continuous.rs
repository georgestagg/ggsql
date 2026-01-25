//! Continuous scale type implementation

use polars::prelude::{ChunkAgg, Column, DataType};

use super::{ScaleTypeKind, ScaleTypeTrait};
use crate::plot::ArrayElement;

/// Continuous scale type - for continuous numeric data
#[derive(Debug, Clone, Copy)]
pub struct Continuous;

impl ScaleTypeTrait for Continuous {
    fn scale_type_kind(&self) -> ScaleTypeKind {
        ScaleTypeKind::Continuous
    }

    fn name(&self) -> &'static str {
        "continuous"
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
        )
    }

    fn resolve_input_range(
        &self,
        user_range: Option<&[ArrayElement]>,
        columns: &[&Column],
    ) -> Result<Option<Vec<ArrayElement>>, String> {
        let computed = compute_numeric_range(columns);

        match user_range {
            None => Ok(computed),
            Some(range) if super::input_range_has_nulls(range) => match computed {
                Some(inferred) => Ok(Some(super::merge_with_inferred(range, &inferred))),
                None => Ok(Some(range.to_vec())),
            },
            Some(range) => Ok(Some(range.to_vec())),
        }
    }

    fn default_output_range(
        &self,
        aesthetic: &str,
        _input_range: Option<&[ArrayElement]>,
    ) -> Option<Vec<ArrayElement>> {
        match aesthetic {
            // TODO: Fill in preferred defaults
            // "size" => Some(vec![...]),
            // "opacity" | "alpha" => Some(vec![...]),
            // "linewidth" => Some(vec![...]),
            _ => None,
        }
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

impl std::fmt::Display for Continuous {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}
