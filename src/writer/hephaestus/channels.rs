//! Bridging ggsql aesthetic mappings and DataFrame columns to the raw numeric
//! vectors hephaestus geoms consume.

use arrow::array::Float64Array;
use arrow::datatypes::DataType;

use crate::array_util::{as_f64, cast_array};
use crate::{AestheticValue, DataFrame, Layer, Result};

/// The DataFrame column name backing the given internal aesthetic, if it maps
/// to a column (rather than a literal value).
pub fn aesthetic_column_name<'a>(layer: &'a Layer, aesthetic: &str) -> Option<&'a str> {
    match layer.mappings.get(aesthetic)? {
        AestheticValue::Column { name, .. } => Some(name.as_str()),
        AestheticValue::AnnotationColumn { name } => Some(name.as_str()),
        AestheticValue::Literal(_) => None,
    }
}

/// Read a numeric column as `f64`, casting from any numeric/temporal source
/// type and mapping nulls to `NaN`.
pub fn column_to_f64(df: &DataFrame, name: &str) -> Result<Vec<f64>> {
    let array = df.column(name)?;
    let casted;
    let f64_array: &Float64Array = if matches!(array.data_type(), DataType::Float64) {
        as_f64(array)?
    } else {
        casted = cast_array(array, &DataType::Float64)?;
        as_f64(&casted)?
    };
    Ok(f64_array.iter().map(|v| v.unwrap_or(f64::NAN)).collect())
}
