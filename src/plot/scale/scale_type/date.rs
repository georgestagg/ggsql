//! Date scale type implementation

use polars::prelude::{ChunkAgg, Column, DataType};

use super::{ScaleTypeKind, ScaleTypeTrait};
use crate::plot::ArrayElement;

/// Date scale type - for date data (maps to temporal type)
#[derive(Debug, Clone, Copy)]
pub struct Date;

impl ScaleTypeTrait for Date {
    fn scale_type_kind(&self) -> ScaleTypeKind {
        ScaleTypeKind::Date
    }

    fn name(&self) -> &'static str {
        "date"
    }

    fn allows_data_type(&self, dtype: &DataType) -> bool {
        matches!(dtype, DataType::Date)
    }

    fn resolve_input_range(
        &self,
        user_range: Option<&[ArrayElement]>,
        columns: &[&Column],
    ) -> Result<Option<Vec<ArrayElement>>, String> {
        let computed = compute_date_range(columns);

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
        _aesthetic: &str,
        _input_range: Option<&[ArrayElement]>,
    ) -> Option<Vec<ArrayElement>> {
        None // Temporal scales don't have output range defaults
    }
}

/// Compute date input range as [min_date, max_date] ISO strings from Columns.
fn compute_date_range(column_refs: &[&Column]) -> Option<Vec<ArrayElement>> {
    let mut global_min: Option<i32> = None;
    let mut global_max: Option<i32> = None;

    for column in column_refs {
        let series = column.as_materialized_series();
        if let Ok(date_ca) = series.date() {
            // Get the underlying physical representation (Int32) for min/max
            let physical = &date_ca.phys;
            if let Some(min) = physical.min() {
                global_min = Some(global_min.map_or(min, |m| m.min(min)));
            }
            if let Some(max) = physical.max() {
                global_max = Some(global_max.map_or(max, |m| m.max(max)));
            }
        }
    }

    match (global_min, global_max) {
        (Some(min_days), Some(max_days)) => {
            let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?;
            let min_date = epoch + chrono::Duration::days(min_days as i64);
            let max_date = epoch + chrono::Duration::days(max_days as i64);
            Some(vec![
                ArrayElement::String(min_date.format("%Y-%m-%d").to_string()),
                ArrayElement::String(max_date.format("%Y-%m-%d").to_string()),
            ])
        }
        _ => None,
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}
