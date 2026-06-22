//! Hephaestus raster writer.
//!
//! Renders a resolved ggsql `Spec` to PNG bytes via the [`hephaestus`] 2D scene
//! renderer.
//!
//! **Scope** (see `src/writer/hephaestus/PLAN.md`): single-panel, single-layer,
//! Cartesian plots. All non-composite geoms (point/line/path/area/ribbon/bar/
//! histogram/tile/polygon/segment/rule/range/text/density/smooth), all scale
//! types/transforms, material aesthetics, axis titles, and legends are
//! supported. Faceting, projections, and composite geoms (boxplot/violin)
//! arrive in later phases; unsupported specs are rejected by
//! [`HephaestusWriter::validate`].
//!
//! Rendering uses hephaestus's Vello (GPU) backend, so a working wgpu adapter
//! (hardware or software, e.g. lavapipe) is required at render time.

mod channels;
mod geom;
mod scales;
mod wiring;

use std::collections::HashMap;

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::{Plot as HPlot, PlotComposition};
use hephaestus::shape::ShapeRegistry;
use hephaestus::Renderer;

use crate::plot::layer::is_transposed;
use crate::plot::projection::coord::CoordKind;
use crate::writer::Writer;
use crate::{DataFrame, GgsqlError, Layer, Plot, Result};

use wiring::{Ctx, Wiring};

/// Internal patch id for the single panel.
const PANEL_ID: &str = "ggsql_panel";

/// Writer that renders a ggsql plot to a PNG image via hephaestus.
///
/// Configured with a target pixel size and DPI because raster rendering needs
/// concrete dimensions, unlike the resolution-independent Vega-Lite writer.
pub struct HephaestusWriter {
    width: u32,
    height: u32,
    dpi: f64,
    background: Color,
}

impl HephaestusWriter {
    /// Create a writer for the given pixel dimensions and DPI, white background.
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
                "hephaestus writer does not support FACET yet".into(),
            ));
        }
        if let Some(projection) = &spec.project {
            if projection.coord.coord_kind() != CoordKind::Cartesian {
                return Err(GgsqlError::WriterError(
                    "hephaestus writer supports only Cartesian coordinates".into(),
                ));
            }
        }
        if spec.layers.len() != 1 {
            return Err(GgsqlError::WriterError(format!(
                "hephaestus writer supports exactly one layer, got {}",
                spec.layers.len()
            )));
        }
        let geom_type = spec.layers[0].geom.geom_type();
        if !geom::is_supported(geom_type) {
            return Err(GgsqlError::WriterError(format!(
                "hephaestus writer does not support the '{geom_type}' geom yet"
            )));
        }
        Ok(())
    }

    fn write(&self, spec: &Plot, data: &HashMap<String, DataFrame>) -> Result<Self::Output> {
        self.validate(spec)?;

        let layer = &spec.layers[0];
        let df = layer_dataframe(layer, data)?;
        let ctx = Ctx {
            spec,
            layer,
            df,
            transposed: is_transposed(layer),
        };

        // Build the geom (+ its scales/axes/legends) through the shared wiring.
        let mut plot =
            HPlot::new(&single_panel(), PANEL_ID).shape_registry(ShapeRegistry::with_builtins());
        let mut w = Wiring::default();
        geom::build_into_plot(&mut plot, &ctx, &mut w)?;

        for (channel, scale_name) in &w.bindings {
            plot.set_binding(*channel, scale_name.clone());
        }
        for axis in w.axes {
            plot.add_axis(axis);
        }
        for legend in w.legends {
            plot.add_legend(legend);
        }

        let mut view = PlotComposition::new(single_panel());
        for (name, scale) in w.registered {
            view.insert_scale(name, scale);
        }
        view.attach_plot(plot);

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

/// The single-panel composition the writer renders into.
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

    fn render(query: &str) -> Result<Vec<u8>> {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let spec = reader.execute(query).unwrap();
        HephaestusWriter::new(640, 480, 96.0).render(&spec)
    }

    /// Assert a PNG was produced, tolerating headless CI with no GPU adapter.
    fn assert_png_or_skip(result: Result<Vec<u8>>) {
        match result {
            Ok(png) => assert!(
                png.starts_with(&[0x89, b'P', b'N', b'G']),
                "output should carry the PNG signature"
            ),
            Err(GgsqlError::WriterError(msg)) if msg.contains("GPU renderer") => {
                eprintln!("skipping render assertion: {msg}");
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn renders_basic_point_plot() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y UNION ALL SELECT 2, 3 UNION ALL SELECT 3, 1 \
             VISUALISE x AS x, y AS y DRAW point",
        ));
    }

    #[test]
    fn renders_categorical_color_with_legend() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 'a' AS grp UNION ALL SELECT 2, 3, 'b' \
             UNION ALL SELECT 3, 1, 'a' \
             VISUALISE x AS x, y AS y, grp AS color DRAW point",
        ));
    }

    #[test]
    fn renders_continuous_size() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 10 AS w UNION ALL SELECT 2, 3, 40 \
             UNION ALL SELECT 3, 1, 90 \
             VISUALISE x AS x, y AS y, w AS size DRAW point",
        ));
    }

    #[test]
    fn renders_log_scale() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y UNION ALL SELECT 10, 3 UNION ALL SELECT 100, 1 \
             VISUALISE x AS x, y AS y DRAW point SCALE x VIA log",
        ));
    }

    #[test]
    fn renders_grouped_line() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 'a' AS g UNION ALL SELECT 2, 3, 'a' \
             UNION ALL SELECT 1, 1, 'b' UNION ALL SELECT 2, 2, 'b' \
             VISUALISE x AS x, y AS y, g AS color DRAW line",
        ));
    }

    #[test]
    fn renders_bar() {
        assert_png_or_skip(render(
            "SELECT 'a' AS cat, 3 AS v UNION ALL SELECT 'b', 5 UNION ALL SELECT 'c', 2 \
             VISUALISE cat AS x, v AS y DRAW bar",
        ));
    }

    #[test]
    fn renders_histogram() {
        assert_png_or_skip(render(
            "SELECT x FROM (VALUES (1),(2),(2),(3),(3),(3),(4),(4),(5)) t(x) \
             VISUALISE x AS x DRAW histogram",
        ));
    }

    #[test]
    fn renders_area() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y UNION ALL SELECT 2, 4 UNION ALL SELECT 3, 3 \
             VISUALISE x AS x, y AS y DRAW area",
        ));
    }

    #[test]
    fn renders_ribbon() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 1 AS lo, 3 AS hi UNION ALL SELECT 2, 2, 5 \
             UNION ALL SELECT 3, 1, 4 \
             VISUALISE x AS x, lo AS ymin, hi AS ymax DRAW ribbon",
        ));
    }

    #[test]
    fn renders_segment() {
        assert_png_or_skip(render(
            "SELECT 0 AS x, 0 AS y, 1 AS xend, 2 AS yend UNION ALL SELECT 1, 1, 2, 0 \
             VISUALISE x AS x, y AS y, xend AS xend, yend AS yend DRAW segment",
        ));
    }

    #[test]
    fn renders_text() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 'hi' AS lab UNION ALL SELECT 2, 3, 'there' \
             VISUALISE x AS x, y AS y, lab AS label DRAW text",
        ));
    }

    #[test]
    fn renders_polygon() {
        assert_png_or_skip(render(
            "SELECT x, y FROM (VALUES (0,0),(2,0),(1,2)) t(x, y) \
             VISUALISE x AS x, y AS y DRAW polygon",
        ));
    }

    #[test]
    fn rejects_composite_geom() {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let spec = reader
            .execute(
                "SELECT 1 AS g, 2 AS y UNION ALL SELECT 1, 5 UNION ALL SELECT 1, 3 \
                 VISUALISE g AS x, y AS y DRAW boxplot",
            )
            .unwrap();
        let writer = HephaestusWriter::new(320, 240, 96.0);
        assert!(matches!(
            writer.validate(spec.plot()),
            Err(GgsqlError::WriterError(_))
        ));
    }
}
