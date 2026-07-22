//! Bridging ggsql aesthetic mappings and DataFrame columns to the typed data
//! hephaestus geoms consume.

use arrow::array::{Array, ArrayRef, StringArray};
use arrow::datatypes::DataType;

use hephaestus::color::Color;
use hephaestus::plot::geom::{BuildableGeom, GeomBuilder};

use super::scales::parse_color;
use crate::array_util::{as_bool, as_f64, as_str, cast_array, value_to_string};
use crate::{AestheticValue, DataFrame, Layer, Result};

/// A column extracted in the type hephaestus expects for a channel: numeric
/// columns become `f64`s, text columns become category strings.
pub enum ChannelData {
    Floats(Vec<f64>),
    Strings(Vec<String>),
}

impl ChannelData {
    /// Select a subset of rows by index, preserving the channel's value type.
    pub fn select(&self, idx: &[usize]) -> ChannelData {
        match self {
            ChannelData::Floats(v) => ChannelData::Floats(idx.iter().map(|&i| v[i]).collect()),
            ChannelData::Strings(v) => {
                ChannelData::Strings(idx.iter().map(|&i| v[i].clone()).collect())
            }
        }
    }

    /// Set this column on a geom builder under the given channel.
    pub fn apply<G: BuildableGeom>(self, builder: &mut GeomBuilder<G>, channel: &str) {
        match self {
            ChannelData::Floats(values) => {
                builder.set(channel, values);
            }
            ChannelData::Strings(values) => {
                builder.set(channel, values);
            }
        }
    }
}

/// The DataFrame column name backing the given internal aesthetic, if it maps
/// to a column (rather than a literal value).
pub fn aesthetic_column_name<'a>(layer: &'a Layer, aesthetic: &str) -> Option<&'a str> {
    match layer.mappings.get(aesthetic)? {
        AestheticValue::Column { name, .. } => Some(name.as_str()),
        AestheticValue::AnnotationColumn { name } => Some(name.as_str()),
        AestheticValue::Literal(_) => None,
    }
}

/// Extract a column as the channel type implied by its arrow dtype: text →
/// category strings, everything else → `f64`.
pub fn column_to_channel(df: &DataFrame, name: &str) -> Result<ChannelData> {
    let array = df.column(name)?;
    if matches!(array.data_type(), DataType::Utf8 | DataType::LargeUtf8) {
        Ok(ChannelData::Strings(column_to_strings(df, name)?))
    } else {
        Ok(ChannelData::Floats(column_to_f64(df, name)?))
    }
}

/// Read a numeric column as `f64`, casting from any numeric/temporal source
/// type and mapping nulls to `NaN`.
pub fn column_to_f64(df: &DataFrame, name: &str) -> Result<Vec<f64>> {
    let array = df.column(name)?;
    let casted;
    let f64_array = if matches!(array.data_type(), DataType::Float64) {
        as_f64(array)?
    } else {
        casted = cast_array(array, &DataType::Float64)?;
        as_f64(&casted)?
    };
    Ok(f64_array.iter().map(|v| v.unwrap_or(f64::NAN)).collect())
}

/// Read a column as strings, casting non-text columns to text. Nulls become
/// empty strings.
pub fn column_to_strings(df: &DataFrame, name: &str) -> Result<Vec<String>> {
    let array = df.column(name)?;
    let casted;
    let str_array: &StringArray = if matches!(array.data_type(), DataType::Utf8) {
        as_str(array)?
    } else {
        casted = cast_array(array, &DataType::Utf8)?;
        as_str(&casted)?
    };
    Ok((0..str_array.len())
        .map(|i| {
            if str_array.is_null(i) {
                String::new()
            } else {
                str_array.value(i).to_string()
            }
        })
        .collect())
}

/// Build a per-row group key from the layer's partition columns (concatenated
/// values), used as the hephaestus `keys` for multi-vertex geoms. Returns
/// `None` when there are no partition columns (single group).
pub fn build_group_keys(df: &DataFrame, partition_by: &[String]) -> Result<Option<Vec<String>>> {
    if partition_by.is_empty() {
        return Ok(None);
    }
    let arrays: Vec<&ArrayRef> = partition_by
        .iter()
        .map(|c| df.column(c))
        .collect::<Result<_>>()?;
    let keys = (0..df.height())
        .map(|i| {
            let mut key = String::new();
            for arr in &arrays {
                key.push_str(&value_to_string(arr, i));
                key.push('\u{1f}'); // unit separator avoids cross-column collisions
            }
            key
        })
        .collect();
    Ok(Some(keys))
}

/// Read a boolean column (arrow Boolean, or text `true`/`1`). Nulls → false.
pub fn column_to_bool(df: &DataFrame, name: &str) -> Result<Vec<bool>> {
    let array = df.column(name)?;
    if matches!(array.data_type(), DataType::Boolean) {
        let a = as_bool(array)?;
        Ok((0..a.len()).map(|i| !a.is_null(i) && a.value(i)).collect())
    } else {
        Ok(column_to_strings(df, name)?
            .iter()
            .map(|s| matches!(s.to_lowercase().as_str(), "true" | "1"))
            .collect())
    }
}

/// Read a color column (visual-space literal values) as parsed colors,
/// defaulting unparseable entries to black.
pub fn column_to_colors(df: &DataFrame, name: &str) -> Result<Vec<Color>> {
    Ok(column_to_strings(df, name)?
        .iter()
        .map(|s| parse_color(s).unwrap_or(Color::BLACK))
        .collect())
}
