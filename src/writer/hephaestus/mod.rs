//! Hephaestus raster writer.
//!
//! Renders a resolved ggsql `Spec` to PNG bytes via the [`hephaestus`] 2D scene
//! renderer.
//!
//! **Scope** (see `src/writer/hephaestus/PLAN.md`): single-panel, single `point`
//! layer in Cartesian coordinates. All scale types, transforms, and material
//! aesthetics (fill/stroke/size/shape/opacity/linewidth) are supported, with
//! axis titles and legends. Faceting, projections, and other geoms arrive in
//! later phases; unsupported specs are rejected by [`HephaestusWriter::validate`].
//!
//! Rendering uses hephaestus's Vello (GPU) backend, so a working wgpu adapter
//! (hardware or software, e.g. lavapipe) is required at render time.

mod channels;
mod geom;
mod scales;

use std::collections::{HashMap, HashSet};

use hephaestus::backend::vello::VelloRenderer;
use hephaestus::color::{rgb8, Color};
use hephaestus::composition::{Composition, Patch, Span};
use hephaestus::geometry::Size;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
use hephaestus::plot::geom::Raw;
use hephaestus::plot::scale::Scale as HScale;
use hephaestus::plot::{Plot as HPlot, PlotComposition, PointGeom};
use hephaestus::scales::chrome::{AxisSide, LegendSide};
use hephaestus::shape::ShapeRegistry;
use hephaestus::Renderer;

use crate::plot::layer::geom::GeomType;
use crate::plot::projection::coord::CoordKind;
use crate::plot::ScaleTypeKind;
use crate::writer::Writer;
use crate::{AestheticValue, DataFrame, GgsqlError, Layer, Plot, Result};

use channels::{
    aesthetic_column_name, column_to_channel, column_to_colors, column_to_f64, column_to_strings,
};
use scales::{build_scale, RangeKind};

/// Internal patch id for the single panel.
const PANEL_ID: &str = "ggsql_panel";
/// ggsql point geom defaults (mirrors `plot/layer/geom/point.rs`), applied when
/// a channel isn't otherwise set so output matches ggsql.
const DEFAULT_SIZE: f64 = 3.0;
const DEFAULT_OPACITY: f64 = 0.8;

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
        if geom_type != GeomType::Point {
            return Err(GgsqlError::WriterError(format!(
                "hephaestus writer supports only the 'point' geom, got '{geom_type}'"
            )));
        }
        Ok(())
    }

    fn write(&self, spec: &Plot, data: &HashMap<String, DataFrame>) -> Result<Self::Output> {
        self.validate(spec)?;

        let layer = &spec.layers[0];
        let df = layer_dataframe(layer, data)?;

        let mut builder = PointGeom::builder();
        let mut registered: Vec<(String, HScale)> = Vec::new();
        let mut bindings: Vec<(&'static str, String)> = Vec::new();
        let mut axes: Vec<Axis> = Vec::new();
        let mut legends: Vec<Legend> = Vec::new();

        // ── Positions ────────────────────────────────────────────────────
        for (aesthetic, channel, side) in [
            ("pos1", "x", AxisSide::Bottom),
            ("pos2", "y", AxisSide::Left),
        ] {
            let col = aesthetic_column_name(layer, aesthetic).ok_or_else(|| {
                GgsqlError::WriterError(format!("point layer has no {aesthetic} mapping"))
            })?;
            let data = column_to_channel(df, col)?;
            let extent = data.extent();
            let scale = build_scale(spec.find_scale(aesthetic), extent, RangeKind::Position);
            data.apply(&mut builder, channel);
            registered.push((aesthetic.to_string(), scale));
            bindings.push((channel, aesthetic.to_string()));

            let mut axis = Axis::rail(aesthetic, AxisPlacement::Cartesian(side));
            if let Some(title) = aesthetic_label(spec, layer, aesthetic) {
                axis = axis.title(title);
            }
            axes.push(axis);
        }

        // ── Material aesthetics ──────────────────────────────────────────
        let mut handled: HashSet<&str> = HashSet::new();
        // Channels driven by the same data source and output kind share one
        // scale (e.g. ggsql's `color` → fill + stroke); their legends then
        // share a `domain_scale` and hephaestus collapses them.
        let mut shared_scales: HashMap<(String, RangeKind), String> = HashMap::new();
        for material in geom::point::MATERIAL {
            if handled.contains(material.channel) {
                continue;
            }
            let Some(col) = aesthetic_column_name(layer, material.aesthetic) else {
                continue;
            };
            handled.insert(material.channel);

            let scale = spec.find_scale(material.aesthetic);
            let type_kind = scale
                .and_then(|s| s.scale_type.as_ref())
                .map(|st| st.scale_type_kind());
            let data_mapped = scale.is_some() && type_kind != Some(ScaleTypeKind::Identity);

            if data_mapped {
                let channel_data = column_to_channel(df, col)?;
                let extent = channel_data.extent();

                let source = aesthetic_source(layer, material.aesthetic);
                let scale_name = shared_scales
                    .entry((source, material.kind))
                    .or_insert_with(|| {
                        let hs = build_scale(scale, extent, material.kind);
                        registered.push((material.aesthetic.to_string(), hs));
                        material.aesthetic.to_string()
                    })
                    .clone();

                channel_data.apply(&mut builder, material.channel);
                bindings.push((material.channel, scale_name.clone()));
                legends.push(material_legend(
                    &scale_name,
                    material.channel,
                    material.kind,
                    type_kind,
                    aesthetic_label(spec, layer, material.aesthetic),
                ));
            } else {
                // Identity / literal: the column holds visual-space values.
                match material.kind {
                    RangeKind::Color => {
                        builder.set(material.channel, Raw(column_to_colors(df, col)?));
                    }
                    RangeKind::Shape => {
                        builder.set(material.channel, Raw(column_to_strings(df, col)?));
                    }
                    _ => {
                        builder.set(material.channel, Raw(column_to_f64(df, col)?));
                    }
                }
            }
        }

        // ── ggsql defaults for unset channels ────────────────────────────
        if !handled.contains("fill") {
            builder.set("fill", rgb8(0, 0, 0));
        }
        if !handled.contains("size") {
            builder.set("size", DEFAULT_SIZE);
        }
        if !handled.contains("fill_opacity") {
            builder.set("fill_opacity", DEFAULT_OPACITY);
        }

        // ── Assemble plot + composition ──────────────────────────────────
        let mut plot =
            HPlot::new(&single_panel(), PANEL_ID).shape_registry(ShapeRegistry::with_builtins());
        for (channel, scale_name) in &bindings {
            plot.set_binding(*channel, scale_name.clone());
        }
        plot.add_geom(builder.build());
        for axis in axes {
            plot.add_axis(axis);
        }
        for legend in legends {
            plot.add_legend(legend);
        }

        let mut view = PlotComposition::new(single_panel());
        for (name, scale) in registered {
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

/// Resolve a label for an aesthetic: an explicit `LABEL` wins (`None`
/// suppresses), otherwise the original mapped column name is the default.
fn aesthetic_label(spec: &Plot, layer: &Layer, aesthetic: &str) -> Option<String> {
    if let Some(labels) = &spec.labels {
        if let Some(entry) = labels.labels.get(aesthetic) {
            return entry.clone();
        }
    }
    match layer.mappings.get(aesthetic) {
        Some(AestheticValue::Column {
            original_name: Some(name),
            ..
        }) => Some(name.clone()),
        _ => None,
    }
}

/// Identify a mapping's underlying data source — the original column name when
/// known, else the internal column name. Lets color-family channels that share
/// a source (ggsql's `color` → fill + stroke) collapse to one scale + legend.
fn aesthetic_source(layer: &Layer, aesthetic: &str) -> String {
    match layer.mappings.get(aesthetic) {
        Some(AestheticValue::Column {
            original_name: Some(name),
            ..
        }) => name.clone(),
        Some(AestheticValue::Column { name, .. }) => name.clone(),
        Some(AestheticValue::AnnotationColumn { name }) => name.clone(),
        _ => aesthetic.to_string(),
    }
}

/// Build a legend for a data-mapped material scale. Continuous color uses a
/// colorbar; everything else a keyed point legend at the scale's breaks.
fn material_legend(
    scale_name: &str,
    channel: &str,
    kind: RangeKind,
    type_kind: Option<ScaleTypeKind>,
    title: Option<String>,
) -> Legend {
    let continuous_color = kind == RangeKind::Color
        && matches!(
            type_kind,
            Some(ScaleTypeKind::Continuous) | Some(ScaleTypeKind::Binned)
        );
    let mut legend = if continuous_color {
        Legend::colorbar(scale_name).side(LegendSide::Right)
    } else {
        Legend::new(scale_name)
            .side(LegendSide::Right)
            .key(LegendKeySpec::point().scaled(channel, scale_name))
    };
    if let Some(title) = title {
        legend = legend.title(title);
    }
    legend
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
