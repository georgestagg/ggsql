//! Hephaestus raster writer.
//!
//! Renders a resolved ggsql `Spec` to PNG bytes via the [`hephaestus`] 2D scene
//! renderer.
//!
//! **Phase 1 scope** (see `src/writer/hephaestus/PLAN.md`): a single-panel,
//! single `point` layer in Cartesian coordinates with continuous position
//! scales and basic bottom/left axes. Unsupported specs are rejected by
//! [`HephaestusWriter::validate`] rather than rendered incorrectly. Further
//! geoms, scales, faceting and projections arrive in later phases.
//!
//! Rendering uses hephaestus's Vello (GPU) backend, so a working wgpu adapter
//! (hardware or software, e.g. lavapipe) is required at render time.

mod channels;
mod geom;
mod scales;

use std::collections::HashMap;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::{Plot as HPlot, PlotComposition};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::Renderer;

use crate::plot::layer::geom::GeomType;
use crate::plot::projection::coord::CoordKind;
use crate::plot::types::ParameterValue;
use crate::writer::Writer;
use crate::{AestheticValue, DataFrame, GgsqlError, Layer, Plot, Result};

/// Internal patch id for the single panel rendered in Phase 1.
const PANEL_ID: &str = "ggsql_panel";
/// Fallback marker diameter (pt) when no `size` is supplied.
const DEFAULT_SIZE: f64 = 4.0;

/// Writer that renders a ggsql plot to a PNG image via hephaestus.
///
/// The writer is configured with a target pixel size and DPI because raster
/// rendering needs concrete dimensions, unlike the resolution-independent
/// Vega-Lite JSON writer.
pub struct HephaestusWriter {
    width: u32,
    height: u32,
    dpi: f64,
    background: Color,
}

impl HephaestusWriter {
    /// Create a writer for the given pixel dimensions and DPI, with a white
    /// background.
    pub fn new(width: u32, height: u32, dpi: f64) -> Self {
        Self {
            width,
            height,
            dpi,
            background: rgb8(255, 255, 255),
        }
    }

    /// Set the background color used to clear the canvas before rendering.
    pub fn background(mut self, color: Color) -> Self {
        self.background = color;
        self
    }
}

impl Writer for HephaestusWriter {
    type Output = Vec<u8>;

    fn validate(&self, spec: &Plot) -> Result<()> {
        if spec.facet.is_some() {
            return Err(GgsqlError::WriterError(
                "hephaestus writer (phase 1) does not support FACET yet".into(),
            ));
        }
        if let Some(projection) = &spec.project {
            if projection.coord.coord_kind() != CoordKind::Cartesian {
                return Err(GgsqlError::WriterError(
                    "hephaestus writer (phase 1) supports only Cartesian coordinates".into(),
                ));
            }
        }
        if spec.layers.len() != 1 {
            return Err(GgsqlError::WriterError(format!(
                "hephaestus writer (phase 1) supports exactly one layer, got {}",
                spec.layers.len()
            )));
        }
        let geom_type = spec.layers[0].geom.geom_type();
        if geom_type != GeomType::Point {
            return Err(GgsqlError::WriterError(format!(
                "hephaestus writer (phase 1) supports only the 'point' geom, got '{geom_type}'"
            )));
        }
        Ok(())
    }

    fn write(&self, spec: &Plot, data: &HashMap<String, DataFrame>) -> Result<Self::Output> {
        self.validate(spec)?;

        let layer = &spec.layers[0];
        let df = layer_dataframe(layer, data)?;

        // Raw x / y data columns.
        let x_col = channels::aesthetic_column_name(layer, "pos1")
            .ok_or_else(|| GgsqlError::WriterError("point layer has no x (pos1) mapping".into()))?;
        let y_col = channels::aesthetic_column_name(layer, "pos2")
            .ok_or_else(|| GgsqlError::WriterError("point layer has no y (pos2) mapping".into()))?;
        let xs = channels::column_to_f64(df, x_col)?;
        let ys = channels::column_to_f64(df, y_col)?;

        // Continuous scales, with the data extent as a domain fallback.
        let scale_x = scales::build_continuous(spec.find_scale("pos1"), extent(&xs));
        let scale_y = scales::build_continuous(spec.find_scale("pos2"), extent(&ys));

        // Single-panel composition; the same shape feeds Plot::new and the
        // orchestrator (the orchestrator rebuilds from its own copy).
        let mut plot = HPlot::new(&single_panel(), PANEL_ID)
            .bind("x", "pos1")
            .bind("y", "pos2");
        plot.add_geom(geom::point::build(
            &xs,
            &ys,
            rgb8(70, 120, 220),
            literal_size(layer),
        ));
        plot.add_axis(Axis::rail(
            "pos1",
            AxisPlacement::Cartesian(AxisSide::Bottom),
        ));
        plot.add_axis(Axis::rail("pos2", AxisPlacement::Cartesian(AxisSide::Left)));

        let mut view = PlotComposition::new(single_panel())
            .add_scale("pos1", scale_x)
            .add_scale("pos2", scale_y)
            .with_plot(plot);

        let issues = view.validate();
        if !issues.is_empty() {
            return Err(GgsqlError::WriterError(format!(
                "hephaestus composition validation failed: {issues:?}"
            )));
        }

        render_png(
            &mut view,
            self.width,
            self.height,
            self.dpi,
            self.background,
        )
    }
}

/// The single-panel composition Phase 1 renders into.
fn single_panel() -> Composition {
    Composition::empty(1, 1).place(1, 1, Span::cell(), Patch::new(PANEL_ID))
}

/// Look up the DataFrame backing a layer by its execution-assigned data key.
fn layer_dataframe<'a>(
    layer: &Layer,
    data: &'a HashMap<String, DataFrame>,
) -> Result<&'a DataFrame> {
    let key = layer.data_key.as_deref().unwrap_or("__ggsql_layer_0__");
    data.get(key)
        .ok_or_else(|| GgsqlError::WriterError(format!("no data found for layer key '{key}'")))
}

/// A literal numeric `size` aesthetic, or the Phase 1 default.
fn literal_size(layer: &Layer) -> f64 {
    match layer.mappings.get("size") {
        Some(AestheticValue::Literal(ParameterValue::Number(n))) => *n,
        _ => DEFAULT_SIZE,
    }
}

/// Finite (min, max) of the data, or `(0, 1)` when there are no finite values.
fn extent(values: &[f64]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &v in values {
        if v.is_finite() {
            min = min.min(v);
            max = max.max(v);
        }
    }
    if min <= max {
        (min, max)
    } else {
        (0.0, 1.0)
    }
}

/// Render the composition to an RGBA8 buffer and encode it as PNG bytes.
fn render_png(
    view: &mut PlotComposition,
    width: u32,
    height: u32,
    dpi: f64,
    background: Color,
) -> Result<Vec<u8>> {
    let mut renderer = VelloRenderer::new().map_err(|e| {
        GgsqlError::WriterError(format!("could not initialise hephaestus GPU renderer: {e}"))
    })?;
    {
        let scene = renderer.scene();
        scene.clear();
        view.render(scene, Size::new(width as f64, height as f64), dpi);
    }
    let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
    renderer
        .render_to_buffer(width, height, background, &mut pixels)
        .map_err(|e| GgsqlError::WriterError(format!("hephaestus render failed: {e}")))?;

    encode_png(width, height, &pixels)
}

/// Encode a premultiplied RGBA8 buffer as PNG bytes.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut header = encoder
            .write_header()
            .map_err(|e| GgsqlError::WriterError(format!("PNG header write failed: {e}")))?;
        header
            .write_image_data(rgba)
            .map_err(|e| GgsqlError::WriterError(format!("PNG data write failed: {e}")))?;
    }
    Ok(buf)
}

#[cfg(all(test, feature = "duckdb"))]
mod tests {
    use super::*;
    use crate::reader::{DuckDBReader, Reader};

    #[test]
    fn renders_point_plot_to_png() {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let spec = reader
            .execute(
                "SELECT 1 AS x, 2 AS y UNION ALL SELECT 2, 3 UNION ALL SELECT 3, 1 \
                 VISUALISE x AS x, y AS y DRAW point",
            )
            .unwrap();
        let writer = HephaestusWriter::new(640, 480, 96.0);
        match writer.render(&spec) {
            Ok(png) => assert!(
                png.starts_with(&[0x89, b'P', b'N', b'G']),
                "output should carry the PNG signature"
            ),
            Err(GgsqlError::WriterError(msg)) if msg.contains("GPU renderer") => {
                // No wgpu adapter available (e.g. headless CI without a software
                // rasteriser); the spec→composition path still exercised. Skip
                // the render assertion rather than failing the suite.
                eprintln!("skipping render assertion: {msg}");
            }
            Err(e) => panic!("unexpected error rendering point plot: {e}"),
        }
    }

    #[test]
    fn rejects_unsupported_geom() {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let spec = reader
            .execute(
                "SELECT 1 AS x, 2 AS y UNION ALL SELECT 2, 3 \
                 VISUALISE x AS x, y AS y DRAW line",
            )
            .unwrap();
        let writer = HephaestusWriter::new(320, 240, 96.0);
        assert!(matches!(
            writer.validate(spec.plot()),
            Err(GgsqlError::WriterError(_))
        ));
    }
}
