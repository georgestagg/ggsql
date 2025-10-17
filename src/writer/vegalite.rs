//! Vega-Lite JSON writer implementation
//!
//! Converts VizQL specifications and DataFrames into Vega-Lite JSON format
//! for web-based interactive visualizations.
//!
//! # Mapping Strategy
//!
//! - VizQL Geom → Vega-Lite mark type
//! - VizQL aesthetics → Vega-Lite encoding channels
//! - VizQL layers → Vega-Lite layer composition
//! - Polars DataFrame → Vega-Lite inline data
//!
//! # Example
//!
//! ```rust,ignore
//! use vizql::writer::{Writer, VegaLiteWriter};
//!
//! let writer = VegaLiteWriter::new();
//! let vega_json = writer.write(&spec, &dataframe)?;
//! // Can be rendered in browser with vega-embed
//! ```

use crate::writer::Writer;
use crate::{DataFrame, Result, VizqlError, VizSpec, VizType, Geom, AestheticValue};
use crate::parser::ast::LiteralValue;
use serde_json::{json, Value, Map};
use polars::prelude::*;

/// Vega-Lite JSON writer
///
/// Generates Vega-Lite v5 specifications from VizQL specs and data.
pub struct VegaLiteWriter {
    /// Vega-Lite schema version
    schema: String,
}

impl VegaLiteWriter {
    /// Create a new Vega-Lite writer with default settings
    pub fn new() -> Self {
        Self {
            schema: "https://vega.github.io/schema/vega-lite/v5.json".to_string(),
        }
    }

    /// Convert Polars DataFrame to Vega-Lite data values (array of objects)
    fn dataframe_to_values(&self, df: &DataFrame) -> Result<Vec<Value>> {
        let mut values = Vec::new();
        let height = df.height();
        let column_names = df.get_column_names();

        for row_idx in 0..height {
            let mut row_obj = Map::new();

            for (col_idx, col_name) in column_names.iter().enumerate() {
                let series = df.get_columns().get(col_idx).ok_or_else(|| {
                    VizqlError::WriterError(format!("Failed to get column {}", col_name))
                })?;

                // Get value from series and convert to JSON Value
                let value = self.series_value_at(series, row_idx)?;
                row_obj.insert(col_name.to_string(), value);
            }

            values.push(Value::Object(row_obj));
        }

        Ok(values)
    }

    /// Get a single value from a series at a given index as JSON Value
    fn series_value_at(&self, series: &Series, idx: usize) -> Result<Value> {
        use DataType::*;

        match series.dtype() {
            Int32 => {
                let ca = series.i32().map_err(|e| {
                    VizqlError::WriterError(format!("Failed to cast to i32: {}", e))
                })?;
                Ok(ca.get(idx).map(|v| json!(v)).unwrap_or(Value::Null))
            }
            Int64 => {
                let ca = series.i64().map_err(|e| {
                    VizqlError::WriterError(format!("Failed to cast to i64: {}", e))
                })?;
                Ok(ca.get(idx).map(|v| json!(v)).unwrap_or(Value::Null))
            }
            Float32 => {
                let ca = series.f32().map_err(|e| {
                    VizqlError::WriterError(format!("Failed to cast to f32: {}", e))
                })?;
                Ok(ca.get(idx).map(|v| json!(v)).unwrap_or(Value::Null))
            }
            Float64 => {
                let ca = series.f64().map_err(|e| {
                    VizqlError::WriterError(format!("Failed to cast to f64: {}", e))
                })?;
                Ok(ca.get(idx).map(|v| json!(v)).unwrap_or(Value::Null))
            }
            Boolean => {
                let ca = series.bool().map_err(|e| {
                    VizqlError::WriterError(format!("Failed to cast to bool: {}", e))
                })?;
                Ok(ca.get(idx).map(|v| json!(v)).unwrap_or(Value::Null))
            }
            String => {
                let ca = series.str().map_err(|e| {
                    VizqlError::WriterError(format!("Failed to cast to string: {}", e))
                })?;
                // Try to parse as number if it looks numeric
                if let Some(val) = ca.get(idx) {
                    if let Ok(num) = val.parse::<f64>() {
                        Ok(json!(num))
                    } else {
                        Ok(json!(val))
                    }
                } else {
                    Ok(Value::Null)
                }
            }
            _ => {
                // Fallback: convert to string
                Ok(json!(series.get(idx).map(|v| v.to_string()).unwrap_or_default()))
            }
        }
    }

    /// Map VizQL Geom to Vega-Lite mark type
    fn geom_to_mark(&self, geom: &Geom) -> String {
        match geom {
            Geom::Point => "point",
            Geom::Line => "line",
            Geom::Path => "line",
            Geom::Bar => "bar",
            Geom::Col => "bar",
            Geom::Area => "area",
            Geom::Tile => "rect",
            Geom::Ribbon => "area",
            Geom::Histogram => "bar",
            Geom::Density => "area",
            Geom::Boxplot => "boxplot",
            Geom::Text => "text",
            Geom::Label => "text",
            _ => "point", // Default fallback
        }
        .to_string()
    }

    /// Check if a string column contains numeric values
    fn is_numeric_string_column(&self, series: &Series) -> bool {
        if let Ok(ca) = series.str() {
            // Check first few non-null values to see if they're numeric
            for val in ca.into_iter().flatten().take(5) {
                if val.parse::<f64>().is_err() {
                    return false;
                }
            }
            true
        } else {
            false
        }
    }

    /// Infer Vega-Lite field type from DataFrame column
    fn infer_field_type(&self, df: &DataFrame, field: &str) -> String {
        if let Some(series) = df.column(field).ok() {
            use DataType::*;
            match series.dtype() {
                Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 | Float32
                | Float64 => "quantitative",
                Boolean => "nominal",
                String => {
                    // Check if string column contains numeric values
                    if self.is_numeric_string_column(series) {
                        "quantitative"
                    } else {
                        "nominal"
                    }
                }
                Date | Datetime(_, _) | Time => "temporal",
                _ => "nominal",
            }
            .to_string()
        } else {
            "nominal".to_string()
        }
    }

    /// Build encoding channel from aesthetic mapping
    fn build_encoding_channel(
        &self,
        aesthetic: &str,
        value: &AestheticValue,
        df: &DataFrame,
    ) -> Result<Value> {
        match value {
            AestheticValue::Column(col) => {
                let field_type = self.infer_field_type(df, col);
                let mut encoding = json!({
                    "field": col,
                    "type": field_type,
                });

                // Add titles for positional aesthetics
                if aesthetic == "x" || aesthetic == "y" {
                    encoding["title"] = json!(col);
                }

                Ok(encoding)
            }
            AestheticValue::Literal(lit) => {
                // For literal values, use constant value encoding
                let val = match lit {
                    LiteralValue::String(s) => json!(s),
                    LiteralValue::Number(n) => json!(n),
                    LiteralValue::Boolean(b) => json!(b),
                };
                Ok(json!({"value": val}))
            }
        }
    }

    /// Map VizQL aesthetic name to Vega-Lite encoding channel name
    fn map_aesthetic_name(&self, aesthetic: &str) -> String {
        match aesthetic {
            "fill" => "color",
            "alpha" => "opacity",
            _ => aesthetic,
        }
        .to_string()
    }

    /// Apply guide configurations to encoding channels
    fn apply_guides_to_encoding(&self, encoding: &mut Map<String, Value>, spec: &VizSpec) {
        use crate::parser::ast::{GuideType, GuidePropertyValue};

        for guide in &spec.guides {
            let channel_name = self.map_aesthetic_name(&guide.aesthetic);

            // Skip if this channel doesn't exist in the encoding
            if !encoding.contains_key(&channel_name) {
                continue;
            }

            // Handle guide type
            match &guide.guide_type {
                Some(GuideType::None) => {
                    // Remove legend for this channel
                    if let Some(channel) = encoding.get_mut(&channel_name) {
                        channel["legend"] = json!(null);
                    }
                }
                Some(GuideType::Legend) => {
                    // Apply legend properties
                    if let Some(channel) = encoding.get_mut(&channel_name) {
                        let mut legend = json!({});

                        for (prop_name, prop_value) in &guide.properties {
                            let value = match prop_value {
                                GuidePropertyValue::String(s) => json!(s),
                                GuidePropertyValue::Number(n) => json!(n),
                                GuidePropertyValue::Boolean(b) => json!(b),
                            };

                            // Map property names to Vega-Lite legend properties
                            match prop_name.as_str() {
                                "title" => legend["title"] = value,
                                "position" => legend["orient"] = value,
                                "direction" => legend["direction"] = value,
                                "nrow" => legend["rowPadding"] = value,
                                "ncol" => legend["columnPadding"] = value,
                                "title_position" => legend["titleAnchor"] = value,
                                _ => {
                                    // Pass through other properties
                                    legend[prop_name] = value;
                                }
                            }
                        }

                        if !legend.as_object().unwrap().is_empty() {
                            channel["legend"] = legend;
                        }
                    }
                }
                Some(GuideType::ColorBar) => {
                    // For color bars, similar to legend but with gradient
                    if let Some(channel) = encoding.get_mut(&channel_name) {
                        let mut legend = json!({"type": "gradient"});

                        for (prop_name, prop_value) in &guide.properties {
                            let value = match prop_value {
                                GuidePropertyValue::String(s) => json!(s),
                                GuidePropertyValue::Number(n) => json!(n),
                                GuidePropertyValue::Boolean(b) => json!(b),
                            };

                            match prop_name.as_str() {
                                "title" => legend["title"] = value,
                                "position" => legend["orient"] = value,
                                _ => legend[prop_name] = value,
                            }
                        }

                        channel["legend"] = legend;
                    }
                }
                Some(GuideType::Axis) => {
                    // Apply axis properties
                    if let Some(channel) = encoding.get_mut(&channel_name) {
                        let mut axis = json!({});

                        for (prop_name, prop_value) in &guide.properties {
                            let value = match prop_value {
                                GuidePropertyValue::String(s) => json!(s),
                                GuidePropertyValue::Number(n) => json!(n),
                                GuidePropertyValue::Boolean(b) => json!(b),
                            };

                            // Map property names to Vega-Lite axis properties
                            match prop_name.as_str() {
                                "title" => axis["title"] = value,
                                "text_angle" => axis["labelAngle"] = value,
                                "text_size" => axis["labelFontSize"] = value,
                                _ => axis[prop_name] = value,
                            }
                        }

                        if !axis.as_object().unwrap().is_empty() {
                            channel["axis"] = axis;
                        }
                    }
                }
                None => {
                    // No specific guide type, just apply properties generically
                    if let Some(channel) = encoding.get_mut(&channel_name) {
                        for (prop_name, prop_value) in &guide.properties {
                            let value = match prop_value {
                                GuidePropertyValue::String(s) => json!(s),
                                GuidePropertyValue::Number(n) => json!(n),
                                GuidePropertyValue::Boolean(b) => json!(b),
                            };
                            channel[prop_name] = value;
                        }
                    }
                }
            }
        }
    }

    /// Validate that all column references in aesthetics exist in the DataFrame
    fn validate_column_references(&self, spec: &VizSpec, data: &DataFrame) -> Result<()> {
        let available_columns: Vec<String> = data
            .get_column_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Check all layers
        for (layer_idx, layer) in spec.layers.iter().enumerate() {
            for (aesthetic, value) in &layer.aesthetics {
                if let AestheticValue::Column(col) = value {
                    if !available_columns.contains(col) {
                        return Err(VizqlError::ValidationError(format!(
                            "Column '{}' referenced in aesthetic '{}' (layer {}) does not exist in the query result.\nAvailable columns: {}",
                            col,
                            aesthetic,
                            layer_idx + 1,
                            available_columns.join(", ")
                        )));
                    }
                }
            }
        }

        // Check facet variables
        if let Some(facet) = &spec.facet {
            use crate::parser::ast::Facet;
            match facet {
                Facet::Wrap { variables, .. } => {
                    for var in variables {
                        if !available_columns.contains(var) {
                            return Err(VizqlError::ValidationError(format!(
                                "Facet variable '{}' does not exist in the query result.\nAvailable columns: {}",
                                var,
                                available_columns.join(", ")
                            )));
                        }
                    }
                }
                Facet::Grid { rows, cols, .. } => {
                    for var in rows.iter().chain(cols.iter()) {
                        if !available_columns.contains(var) {
                            return Err(VizqlError::ValidationError(format!(
                                "Facet variable '{}' does not exist in the query result.\nAvailable columns: {}",
                                var,
                                available_columns.join(", ")
                            )));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

impl Default for VegaLiteWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer for VegaLiteWriter {
    fn write(&self, spec: &VizSpec, data: &DataFrame) -> Result<String> {
        // Only support Plot type for now
        if spec.viz_type != VizType::Plot {
            return Err(VizqlError::WriterError(format!(
                "VegaLiteWriter only supports VizType::Plot, got {:?}",
                spec.viz_type
            )));
        }

        // Validate that all column references exist in the DataFrame
        self.validate_column_references(spec, data)?;

        // Convert DataFrame to Vega-Lite data values
        let data_values = self.dataframe_to_values(data)?;

        // Build the base Vega-Lite spec
        let mut vl_spec = json!({
            "$schema": self.schema,
            "data": {
                "values": data_values
            }
        });

        // Add title if present
        if let Some(labels) = &spec.labels {
            if let Some(title) = labels.labels.get("title") {
                vl_spec["title"] = json!(title);
            }
        }

        // Handle single layer vs multi-layer
        if spec.layers.len() == 1 {
            // Single layer: use flat mark + encoding
            let layer = &spec.layers[0];
            vl_spec["mark"] = json!(self.geom_to_mark(&layer.geom));

            // Build encoding from aesthetics
            let mut encoding = Map::new();
            for (aesthetic, value) in &layer.aesthetics {
                let channel_name = self.map_aesthetic_name(aesthetic);
                let channel_encoding = self.build_encoding_channel(aesthetic, value, data)?;
                encoding.insert(channel_name, channel_encoding);
            }

            // Override axis titles from labels if present
            if let Some(labels) = &spec.labels {
                if let Some(x_label) = labels.labels.get("x") {
                    if let Some(x_enc) = encoding.get_mut("x") {
                        x_enc["title"] = json!(x_label);
                    }
                }
                if let Some(y_label) = labels.labels.get("y") {
                    if let Some(y_enc) = encoding.get_mut("y") {
                        y_enc["title"] = json!(y_label);
                    }
                }
            }

            // Apply guide configurations
            self.apply_guides_to_encoding(&mut encoding, spec);

            vl_spec["encoding"] = Value::Object(encoding);
        } else if spec.layers.len() > 1 {
            // Multi-layer: use layer composition
            let mut layers = Vec::new();

            for layer in &spec.layers {
                let mut layer_spec = json!({
                    "mark": self.geom_to_mark(&layer.geom)
                });

                // Build encoding for this layer
                let mut encoding = Map::new();
                for (aesthetic, value) in &layer.aesthetics {
                    let channel_name = self.map_aesthetic_name(aesthetic);
                    let channel_encoding = self.build_encoding_channel(aesthetic, value, data)?;
                    encoding.insert(channel_name, channel_encoding);
                }

                layer_spec["encoding"] = Value::Object(encoding);
                layers.push(layer_spec);
            }

            vl_spec["layer"] = json!(layers);

            // For multi-layer plots, apply guides at the top level by creating a resolve configuration
            // and encoding the guides in the spec-level encoding (if needed)
            if !spec.guides.is_empty() {
                let mut resolve = json!({"legend": {}, "scale": {}});
                for guide in &spec.guides {
                    let channel = self.map_aesthetic_name(&guide.aesthetic);
                    // Share legends across layers
                    resolve["legend"][&channel] = json!("shared");
                    resolve["scale"][&channel] = json!("shared");
                }
                vl_spec["resolve"] = resolve;
            }
        }

        // Handle faceting if present
        if let Some(facet) = &spec.facet {
            use crate::parser::ast::Facet;
            match facet {
                Facet::Wrap { variables, .. } => {
                    if !variables.is_empty() {
                        let field_type = self.infer_field_type(data, &variables[0]);
                        vl_spec["facet"] = json!({
                            "field": variables[0],
                            "type": field_type,
                        });

                        // Move mark/encoding into spec
                        let mut spec_inner = json!({});
                        if let Some(mark) = vl_spec.get("mark") {
                            spec_inner["mark"] = mark.clone();
                        }
                        if let Some(encoding) = vl_spec.get("encoding") {
                            spec_inner["encoding"] = encoding.clone();
                        }
                        if let Some(layer) = vl_spec.get("layer") {
                            spec_inner["layer"] = layer.clone();
                        }

                        vl_spec["spec"] = spec_inner;
                        vl_spec.as_object_mut().unwrap().remove("mark");
                        vl_spec.as_object_mut().unwrap().remove("encoding");
                        vl_spec.as_object_mut().unwrap().remove("layer");
                    }
                }
                Facet::Grid { rows, cols, .. } => {
                    // Grid faceting: use row and column
                    let mut facet_spec = Map::new();
                    if !rows.is_empty() {
                        let field_type = self.infer_field_type(data, &rows[0]);
                        facet_spec.insert(
                            "row".to_string(),
                            json!({"field": rows[0], "type": field_type}),
                        );
                    }
                    if !cols.is_empty() {
                        let field_type = self.infer_field_type(data, &cols[0]);
                        facet_spec.insert(
                            "column".to_string(),
                            json!({"field": cols[0], "type": field_type}),
                        );
                    }
                    vl_spec["facet"] = Value::Object(facet_spec);

                    // Move mark/encoding into spec
                    let mut spec_inner = json!({});
                    if let Some(mark) = vl_spec.get("mark") {
                        spec_inner["mark"] = mark.clone();
                    }
                    if let Some(encoding) = vl_spec.get("encoding") {
                        spec_inner["encoding"] = encoding.clone();
                    }

                    vl_spec["spec"] = spec_inner;
                    vl_spec.as_object_mut().unwrap().remove("mark");
                    vl_spec.as_object_mut().unwrap().remove("encoding");
                }
            }
        }

        // Serialize to pretty JSON
        serde_json::to_string_pretty(&vl_spec).map_err(|e| {
            VizqlError::WriterError(format!("Failed to serialize Vega-Lite JSON: {}", e))
        })
    }

    fn validate(&self, spec: &VizSpec) -> Result<()> {
        // Check if we support this viz type
        if spec.viz_type != VizType::Plot {
            return Err(VizqlError::ValidationError(format!(
                "VegaLiteWriter only supports VizType::Plot, got {:?}",
                spec.viz_type
            )));
        }

        // Check that we have at least one layer
        if spec.layers.is_empty() {
            return Err(VizqlError::ValidationError(
                "VegaLiteWriter requires at least one layer".to_string(),
            ));
        }

        // Validate each layer has required aesthetics
        for layer in &spec.layers {
            layer.validate_required_aesthetics().map_err(|e| {
                VizqlError::ValidationError(format!("Layer validation failed: {}", e))
            })?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::ast::{Layer, Labels};
    use std::collections::HashMap;

    #[test]
    fn test_geom_to_mark_mapping() {
        let writer = VegaLiteWriter::new();
        assert_eq!(writer.geom_to_mark(&Geom::Point), "point");
        assert_eq!(writer.geom_to_mark(&Geom::Line), "line");
        assert_eq!(writer.geom_to_mark(&Geom::Bar), "bar");
        assert_eq!(writer.geom_to_mark(&Geom::Area), "area");
        assert_eq!(writer.geom_to_mark(&Geom::Tile), "rect");
    }

    #[test]
    fn test_aesthetic_name_mapping() {
        let writer = VegaLiteWriter::new();
        assert_eq!(writer.map_aesthetic_name("x"), "x");
        assert_eq!(writer.map_aesthetic_name("fill"), "color");
        assert_eq!(writer.map_aesthetic_name("alpha"), "opacity");
    }

    #[test]
    fn test_validation_requires_plot_type() {
        let writer = VegaLiteWriter::new();
        let spec = VizSpec::new(VizType::Table);
        assert!(writer.validate(&spec).is_err());
    }

    #[test]
    fn test_validation_requires_layers() {
        let writer = VegaLiteWriter::new();
        let spec = VizSpec::new(VizType::Plot);
        assert!(writer.validate(&spec).is_err());
    }

    #[test]
    fn test_simple_point_spec() {
        let writer = VegaLiteWriter::new();

        // Create a simple spec
        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
        spec.layers.push(layer);

        // Create simple DataFrame
        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
        }
        .unwrap();

        // Generate Vega-Lite JSON
        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        // Verify structure
        assert_eq!(vl_spec["$schema"], writer.schema);
        assert_eq!(vl_spec["mark"], "point");
        assert!(vl_spec["data"]["values"].is_array());
        assert_eq!(vl_spec["data"]["values"].as_array().unwrap().len(), 3);
        assert!(vl_spec["encoding"]["x"].is_object());
        assert!(vl_spec["encoding"]["y"].is_object());
    }

    #[test]
    fn test_with_title() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Line)
            .with_aesthetic("x".to_string(), AestheticValue::Column("date".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("value".to_string()));
        spec.layers.push(layer);

        let mut labels = Labels {
            labels: HashMap::new(),
        };
        labels.labels.insert("title".to_string(), "My Chart".to_string());
        spec.labels = Some(labels);

        let df = df! {
            "date" => &["2024-01-01", "2024-01-02"],
            "value" => &[10, 20],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["title"], "My Chart");
        assert_eq!(vl_spec["mark"], "line");
    }

    #[test]
    fn test_literal_color() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic(
                "color".to_string(),
                AestheticValue::Literal(LiteralValue::String("blue".to_string())),
            );
        spec.layers.push(layer);

        let df = df! {
            "x" => &[1, 2],
            "y" => &[3, 4],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["color"]["value"], "blue");
    }

    #[test]
    fn test_missing_column_error() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("foo".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "x" => &[1, 2],
            "y" => &[3, 4],
        }
        .unwrap();

        let result = writer.write(&spec, &df);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("Column 'foo'"));
        assert!(err_msg.contains("does not exist"));
        assert!(err_msg.contains("Available columns: x, y"));
    }

    #[test]
    fn test_missing_column_in_multi_layer() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);

        // First layer is valid
        let layer1 = Layer::new(Geom::Line)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
        spec.layers.push(layer1);

        // Second layer references non-existent column
        let layer2 = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("missing_col".to_string()));
        spec.layers.push(layer2);

        let df = df! {
            "x" => &[1, 2],
            "y" => &[3, 4],
        }
        .unwrap();

        let result = writer.write(&spec, &df);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(err_msg.contains("Column 'missing_col'"));
        assert!(err_msg.contains("layer 2"));
    }

    // ========================================
    // Comprehensive Grammar Coverage Tests
    // ========================================

    #[test]
    fn test_all_basic_geom_types() {
        let writer = VegaLiteWriter::new();

        let geoms = vec![
            (Geom::Point, "point"),
            (Geom::Line, "line"),
            (Geom::Path, "line"),
            (Geom::Bar, "bar"),
            (Geom::Col, "bar"),
            (Geom::Area, "area"),
            (Geom::Tile, "rect"),
            (Geom::Ribbon, "area"),
        ];

        for (geom, expected_mark) in geoms {
            let mut spec = VizSpec::new(VizType::Plot);
            let layer = Layer::new(geom.clone())
                .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
                .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
            spec.layers.push(layer);

            let df = df! {
                "x" => &[1, 2, 3],
                "y" => &[4, 5, 6],
            }
            .unwrap();

            let json_str = writer.write(&spec, &df).unwrap();
            let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

            assert_eq!(
                vl_spec["mark"].as_str().unwrap(),
                expected_mark,
                "Failed for geom: {:?}",
                geom
            );
        }
    }

    #[test]
    fn test_statistical_geom_types() {
        let writer = VegaLiteWriter::new();

        let geoms = vec![
            (Geom::Histogram, "bar"),
            (Geom::Density, "area"),
            (Geom::Boxplot, "boxplot"),
        ];

        for (geom, expected_mark) in geoms {
            let mut spec = VizSpec::new(VizType::Plot);
            let layer = Layer::new(geom.clone())
                .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
                .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
            spec.layers.push(layer);

            let df = df! {
                "x" => &[1, 2, 3],
                "y" => &[4, 5, 6],
            }
            .unwrap();

            let json_str = writer.write(&spec, &df).unwrap();
            let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

            assert_eq!(vl_spec["mark"].as_str().unwrap(), expected_mark);
        }
    }

    #[test]
    fn test_text_geom_types() {
        let writer = VegaLiteWriter::new();

        for geom in [Geom::Text, Geom::Label] {
            let mut spec = VizSpec::new(VizType::Plot);
            let layer = Layer::new(geom.clone())
                .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
                .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
            spec.layers.push(layer);

            let df = df! {
                "x" => &[1, 2],
                "y" => &[3, 4],
            }
            .unwrap();

            let json_str = writer.write(&spec, &df).unwrap();
            let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

            assert_eq!(vl_spec["mark"].as_str().unwrap(), "text");
        }
    }

    #[test]
    fn test_color_aesthetic_column() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("color".to_string(), AestheticValue::Column("category".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
            "category" => &["A", "B", "A"],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["color"]["field"], "category");
        assert_eq!(vl_spec["encoding"]["color"]["type"], "nominal");
    }

    #[test]
    fn test_size_aesthetic_column() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("size".to_string(), AestheticValue::Column("value".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
            "value" => &[10, 20, 30],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["size"]["field"], "value");
        assert_eq!(vl_spec["encoding"]["size"]["type"], "quantitative");
    }

    #[test]
    fn test_fill_aesthetic_mapping() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Bar)
            .with_aesthetic("x".to_string(), AestheticValue::Column("category".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("value".to_string()))
            .with_aesthetic("fill".to_string(), AestheticValue::Column("region".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "category" => &["A", "B"],
            "value" => &[10, 20],
            "region" => &["US", "EU"],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        // 'fill' should be mapped to 'color' in Vega-Lite
        assert_eq!(vl_spec["encoding"]["color"]["field"], "region");
    }

    #[test]
    fn test_alpha_aesthetic_mapping() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("alpha".to_string(), AestheticValue::Literal(LiteralValue::Number(0.5)));
        spec.layers.push(layer);

        let df = df! {
            "x" => &[1, 2],
            "y" => &[3, 4],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        // 'alpha' should be mapped to 'opacity' in Vega-Lite
        assert_eq!(vl_spec["encoding"]["opacity"]["value"], 0.5);
    }

    #[test]
    fn test_multiple_aesthetics() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("color".to_string(), AestheticValue::Column("category".to_string()))
            .with_aesthetic("size".to_string(), AestheticValue::Column("value".to_string()))
            .with_aesthetic("shape".to_string(), AestheticValue::Column("type".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
            "category" => &["A", "B", "C"],
            "value" => &[10, 20, 30],
            "type" => &["T1", "T2", "T1"],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["x"]["field"], "x");
        assert_eq!(vl_spec["encoding"]["y"]["field"], "y");
        assert_eq!(vl_spec["encoding"]["color"]["field"], "category");
        assert_eq!(vl_spec["encoding"]["size"]["field"], "value");
        assert_eq!(vl_spec["encoding"]["shape"]["field"], "type");
    }

    #[test]
    fn test_literal_number_value() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("size".to_string(), AestheticValue::Literal(LiteralValue::Number(100.0)));
        spec.layers.push(layer);

        let df = df! {
            "x" => &[1, 2],
            "y" => &[3, 4],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["size"]["value"], 100.0);
    }

    #[test]
    fn test_literal_boolean_value() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Line)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("linetype".to_string(), AestheticValue::Literal(LiteralValue::Boolean(true)));
        spec.layers.push(layer);

        let df = df! {
            "x" => &[1, 2],
            "y" => &[3, 4],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["linetype"]["value"], true);
    }

    #[test]
    fn test_multi_layer_composition() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);

        // First layer: line
        let layer1 = Layer::new(Geom::Line)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
        spec.layers.push(layer1);

        // Second layer: points
        let layer2 = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("color".to_string(), AestheticValue::Literal(LiteralValue::String("red".to_string())));
        spec.layers.push(layer2);

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        // Should have layer array
        assert!(vl_spec["layer"].is_array());
        let layers = vl_spec["layer"].as_array().unwrap();
        assert_eq!(layers.len(), 2);

        // Check first layer
        assert_eq!(layers[0]["mark"], "line");
        assert_eq!(layers[0]["encoding"]["x"]["field"], "x");
        assert_eq!(layers[0]["encoding"]["y"]["field"], "y");

        // Check second layer
        assert_eq!(layers[1]["mark"], "point");
        assert_eq!(layers[1]["encoding"]["color"]["value"], "red");
    }

    #[test]
    fn test_three_layer_composition() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);

        // Layer 1: area
        spec.layers.push(
            Layer::new(Geom::Area)
                .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
                .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string())),
        );

        // Layer 2: line
        spec.layers.push(
            Layer::new(Geom::Line)
                .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
                .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string())),
        );

        // Layer 3: points
        spec.layers.push(
            Layer::new(Geom::Point)
                .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
                .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string())),
        );

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        let layers = vl_spec["layer"].as_array().unwrap();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0]["mark"], "area");
        assert_eq!(layers[1]["mark"], "line");
        assert_eq!(layers[2]["mark"], "point");
    }

    #[test]
    fn test_label_title() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
        spec.layers.push(layer);

        let mut labels = Labels {
            labels: HashMap::new(),
        };
        labels.labels.insert("title".to_string(), "Test Plot".to_string());
        spec.labels = Some(labels);

        let df = df! {
            "x" => &[1, 2],
            "y" => &[3, 4],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["title"], "Test Plot");
    }

    #[test]
    fn test_label_axis_titles() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Line)
            .with_aesthetic("x".to_string(), AestheticValue::Column("date".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("revenue".to_string()));
        spec.layers.push(layer);

        let mut labels = Labels {
            labels: HashMap::new(),
        };
        labels.labels.insert("x".to_string(), "Date".to_string());
        labels.labels.insert("y".to_string(), "Revenue ($M)".to_string());
        spec.labels = Some(labels);

        let df = df! {
            "date" => &["2024-01", "2024-02", "2024-03"],
            "revenue" => &["100", "150", "200"],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["x"]["title"], "Date");
        assert_eq!(vl_spec["encoding"]["y"]["title"], "Revenue ($M)");
    }

    #[test]
    fn test_label_title_and_axes() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Bar)
            .with_aesthetic("x".to_string(), AestheticValue::Column("category".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("value".to_string()));
        spec.layers.push(layer);

        let mut labels = Labels {
            labels: HashMap::new(),
        };
        labels.labels.insert("title".to_string(), "Sales by Category".to_string());
        labels.labels.insert("x".to_string(), "Product Category".to_string());
        labels.labels.insert("y".to_string(), "Sales Volume".to_string());
        spec.labels = Some(labels);

        let df = df! {
            "category" => &["A", "B", "C"],
            "value" => &[10, 20, 15],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["title"], "Sales by Category");
        assert_eq!(vl_spec["encoding"]["x"]["title"], "Product Category");
        assert_eq!(vl_spec["encoding"]["y"]["title"], "Sales Volume");
    }

    #[test]
    fn test_numeric_type_inference_integers() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["x"]["type"], "quantitative");
        assert_eq!(vl_spec["encoding"]["y"]["type"], "quantitative");
    }

    #[test]
    fn test_nominal_type_inference_strings() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Bar)
            .with_aesthetic("x".to_string(), AestheticValue::Column("category".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("value".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "category" => &["A", "B", "C"],
            "value" => &[10, 20, 30],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["x"]["type"], "nominal");
        assert_eq!(vl_spec["encoding"]["y"]["type"], "quantitative");
    }

    #[test]
    fn test_numeric_string_type_inference() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Line)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "x" => &["1", "2", "3"],
            "y" => &["4.5", "5.5", "6.5"],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        // Numeric strings should be inferred as quantitative
        assert_eq!(vl_spec["encoding"]["x"]["type"], "quantitative");
        assert_eq!(vl_spec["encoding"]["y"]["type"], "quantitative");

        // Values should be converted to numbers in JSON
        let data = vl_spec["data"]["values"].as_array().unwrap();
        assert_eq!(data[0]["x"], 1.0);
        assert_eq!(data[0]["y"], 4.5);
    }

    #[test]
    fn test_data_conversion_all_types() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("int_col".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("float_col".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "int_col" => &[1, 2, 3],
            "float_col" => &[1.5, 2.5, 3.5],
            "string_col" => &["a", "b", "c"],
            "bool_col" => &[true, false, true],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        let data = vl_spec["data"]["values"].as_array().unwrap();
        assert_eq!(data.len(), 3);

        // Check first row
        assert_eq!(data[0]["int_col"], 1);
        assert_eq!(data[0]["float_col"], 1.5);
        assert_eq!(data[0]["string_col"], "a");
        assert_eq!(data[0]["bool_col"], true);
    }

    #[test]
    fn test_empty_dataframe() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
        spec.layers.push(layer);

        let df = df! {
            "x" => &[] as &[i32],
            "y" => &[] as &[i32],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        let data = vl_spec["data"]["values"].as_array().unwrap();
        assert_eq!(data.len(), 0);
    }

    #[test]
    fn test_large_dataset() {
        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()));
        spec.layers.push(layer);

        // Create dataset with 100 rows
        let x_vals: Vec<i32> = (1..=100).collect();
        let y_vals: Vec<i32> = (1..=100).map(|i| i * 2).collect();

        let df = df! {
            "x" => x_vals,
            "y" => y_vals,
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        let data = vl_spec["data"]["values"].as_array().unwrap();
        assert_eq!(data.len(), 100);
        assert_eq!(data[0]["x"], 1);
        assert_eq!(data[0]["y"], 2);
        assert_eq!(data[99]["x"], 100);
        assert_eq!(data[99]["y"], 200);
    }

    // ========================================
    // Guide Tests
    // ========================================

    #[test]
    fn test_guide_none_hides_legend() {
        use crate::parser::ast::{Guide, GuideType};

        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("color".to_string(), AestheticValue::Column("category".to_string()));
        spec.layers.push(layer);

        // Add guide to hide color legend
        spec.guides.push(Guide {
            aesthetic: "color".to_string(),
            guide_type: Some(GuideType::None),
            properties: HashMap::new(),
        });

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
            "category" => &["A", "B", "C"],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["color"]["legend"], json!(null));
    }

    #[test]
    fn test_guide_legend_with_title() {
        use crate::parser::ast::{Guide, GuideType, GuidePropertyValue};

        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("color".to_string(), AestheticValue::Column("category".to_string()));
        spec.layers.push(layer);

        // Add guide with custom title
        let mut properties = HashMap::new();
        properties.insert("title".to_string(), GuidePropertyValue::String("Product Type".to_string()));
        spec.guides.push(Guide {
            aesthetic: "color".to_string(),
            guide_type: Some(GuideType::Legend),
            properties,
        });

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
            "category" => &["A", "B", "C"],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["color"]["legend"]["title"], "Product Type");
    }

    #[test]
    fn test_guide_legend_position() {
        use crate::parser::ast::{Guide, GuideType, GuidePropertyValue};

        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("size".to_string(), AestheticValue::Column("value".to_string()));
        spec.layers.push(layer);

        // Add guide with custom position
        let mut properties = HashMap::new();
        properties.insert("position".to_string(), GuidePropertyValue::String("bottom".to_string()));
        spec.guides.push(Guide {
            aesthetic: "size".to_string(),
            guide_type: Some(GuideType::Legend),
            properties,
        });

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
            "value" => &[10, 20, 30],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        // position maps to orient in Vega-Lite
        assert_eq!(vl_spec["encoding"]["size"]["legend"]["orient"], "bottom");
    }

    #[test]
    fn test_guide_colorbar() {
        use crate::parser::ast::{Guide, GuideType, GuidePropertyValue};

        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("color".to_string(), AestheticValue::Column("temperature".to_string()));
        spec.layers.push(layer);

        // Add colorbar guide
        let mut properties = HashMap::new();
        properties.insert("title".to_string(), GuidePropertyValue::String("Temperature (°C)".to_string()));
        spec.guides.push(Guide {
            aesthetic: "color".to_string(),
            guide_type: Some(GuideType::ColorBar),
            properties,
        });

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
            "temperature" => &[20, 25, 30],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["color"]["legend"]["type"], "gradient");
        assert_eq!(vl_spec["encoding"]["color"]["legend"]["title"], "Temperature (°C)");
    }

    #[test]
    fn test_guide_axis() {
        use crate::parser::ast::{Guide, GuideType, GuidePropertyValue};

        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Bar)
            .with_aesthetic("x".to_string(), AestheticValue::Column("category".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("value".to_string()));
        spec.layers.push(layer);

        // Add axis guide for x
        let mut properties = HashMap::new();
        properties.insert("title".to_string(), GuidePropertyValue::String("Product Category".to_string()));
        properties.insert("text_angle".to_string(), GuidePropertyValue::Number(45.0));
        spec.guides.push(Guide {
            aesthetic: "x".to_string(),
            guide_type: Some(GuideType::Axis),
            properties,
        });

        let df = df! {
            "category" => &["A", "B", "C"],
            "value" => &[10, 20, 30],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["x"]["axis"]["title"], "Product Category");
        assert_eq!(vl_spec["encoding"]["x"]["axis"]["labelAngle"], 45.0);
    }

    #[test]
    fn test_multiple_guides() {
        use crate::parser::ast::{Guide, GuideType, GuidePropertyValue};

        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Point)
            .with_aesthetic("x".to_string(), AestheticValue::Column("x".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("y".to_string()))
            .with_aesthetic("color".to_string(), AestheticValue::Column("category".to_string()))
            .with_aesthetic("size".to_string(), AestheticValue::Column("value".to_string()));
        spec.layers.push(layer);

        // Add guide for color
        let mut color_props = HashMap::new();
        color_props.insert("title".to_string(), GuidePropertyValue::String("Category".to_string()));
        color_props.insert("position".to_string(), GuidePropertyValue::String("right".to_string()));
        spec.guides.push(Guide {
            aesthetic: "color".to_string(),
            guide_type: Some(GuideType::Legend),
            properties: color_props,
        });

        // Add guide for size
        let mut size_props = HashMap::new();
        size_props.insert("title".to_string(), GuidePropertyValue::String("Value".to_string()));
        spec.guides.push(Guide {
            aesthetic: "size".to_string(),
            guide_type: Some(GuideType::Legend),
            properties: size_props,
        });

        let df = df! {
            "x" => &[1, 2, 3],
            "y" => &[4, 5, 6],
            "category" => &["A", "B", "C"],
            "value" => &[10, 20, 30],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        assert_eq!(vl_spec["encoding"]["color"]["legend"]["title"], "Category");
        assert_eq!(vl_spec["encoding"]["color"]["legend"]["orient"], "right");
        assert_eq!(vl_spec["encoding"]["size"]["legend"]["title"], "Value");
    }

    #[test]
    fn test_guide_fill_maps_to_color() {
        use crate::parser::ast::{Guide, GuideType, GuidePropertyValue};

        let writer = VegaLiteWriter::new();

        let mut spec = VizSpec::new(VizType::Plot);
        let layer = Layer::new(Geom::Bar)
            .with_aesthetic("x".to_string(), AestheticValue::Column("category".to_string()))
            .with_aesthetic("y".to_string(), AestheticValue::Column("value".to_string()))
            .with_aesthetic("fill".to_string(), AestheticValue::Column("region".to_string()));
        spec.layers.push(layer);

        // Add guide for fill (should map to color)
        let mut properties = HashMap::new();
        properties.insert("title".to_string(), GuidePropertyValue::String("Region".to_string()));
        spec.guides.push(Guide {
            aesthetic: "fill".to_string(),
            guide_type: Some(GuideType::Legend),
            properties,
        });

        let df = df! {
            "category" => &["A", "B"],
            "value" => &[10, 20],
            "region" => &["US", "EU"],
        }
        .unwrap();

        let json_str = writer.write(&spec, &df).unwrap();
        let vl_spec: Value = serde_json::from_str(&json_str).unwrap();

        // fill should be mapped to color channel
        assert_eq!(vl_spec["encoding"]["color"]["field"], "region");
        assert_eq!(vl_spec["encoding"]["color"]["legend"]["title"], "Region");
    }
}
