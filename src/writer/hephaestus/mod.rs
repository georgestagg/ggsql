//! Hephaestus raster writer.
//!
//! Renders a resolved ggsql `Spec` to PNG bytes via the [`hephaestus`] 2D scene
//! renderer.
//!
//! **Scope** (see `src/writer/hephaestus/PLAN.md`): multi-layer plots under
//! Cartesian, Polar, and Map projections, with `FACET` faceting (Wrap/Grid,
//! fixed + free scales). All geoms (point/line/path/area/ribbon/bar/histogram/
//! tile/polygon/segment/rule/range/text/density/smooth/boxplot/violin), all
//! scale types/transforms, material aesthetics, axis titles, and legends are
//! supported. Unsupported geoms are rejected by [`HephaestusWriter::validate`].
//!
//! Rendering uses hephaestus's Vello (GPU) backend, so a working wgpu adapter
//! (hardware or software, e.g. lavapipe) is required at render time.

mod channels;
mod facet;
mod geom;
mod projection;
mod scales;
mod wiring;

use std::collections::HashMap;

use hephaestus::backend::vello::VelloRenderer;
pub use hephaestus::color::{rgba, Color};
use hephaestus::geometry::Size;
use hephaestus::plot::{scale, AspectMode, Plot as HPlot, PlotComposition};
use hephaestus::scales::chrome::AxisSide;
use hephaestus::shape::ShapeRegistry;
use hephaestus::Renderer;

use crate::naming;
use crate::plot::layer::geom::GeomType;
use crate::plot::layer::is_transposed;
use crate::plot::ParameterValue;
use crate::writer::hephaestus::projection::apply_projection;
use crate::writer::hephaestus::scales::build_scale;
use crate::writer::Writer;
use crate::{DataFrame, GgsqlError, Layer, Plot, Result};

use wiring::Ctx;

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
            background: rgba(1.0, 1.0, 1.0, 1.0),
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
        if spec.layers.is_empty() {
            return Err(GgsqlError::WriterError(
                "hephaestus writer requires at least one layer".into(),
            ));
        }
        for layer in &spec.layers {
            let geom_type = layer.geom.geom_type();
            if !geom::is_supported(geom_type) {
                return Err(GgsqlError::WriterError(format!(
                    "hephaestus writer does not support the '{geom_type}' geom yet"
                )));
            }
        }
        Ok(())
    }

    fn write(&self, spec: &Plot, data: &HashMap<String, DataFrame>) -> Result<Self::Output> {
        self.validate(spec)?;

        // FACET → a grid of named panels (a single panel when unfaceted). Each
        // panel becomes one hephaestus `Plot` sharing the composition's scales.
        let (composition, panels) = facet::build_panels(spec, data)?;
        // The composition owns the shape registry backing composition-level legend
        // glyphs (point markers, line dashes).
        let mut view =
            PlotComposition::new(&composition).shape_registry(ShapeRegistry::with_builtins());

        // Plot title/subtitle/caption from the LABEL clause. These live on the
        // composition, not the per-panel plots, so one label spans the whole
        // figure — which is also correct for the unfaceted 1x1 case (a plot-level
        // title would resolve to the same layout row and be painted over).
        if let Some(text) = wiring::plot_label(spec, "title") {
            view = view.title(text);
        }
        if let Some(text) = wiring::plot_label(spec, "subtitle") {
            view = view.subtitle(text);
        }
        if let Some(text) = wiring::plot_label(spec, "caption") {
            view = view.caption(text);
        }

        // Register the fixed (shared) scales once, globally. Every panel binds
        // its position channels to these names, giving fixed-scale faceting.
        for scale in &spec.scales {
            let kind = match scale.aesthetic.as_str() {
                "fill" | "stroke" | "color" | "colour" => scales::RangeKind::Color,
                "shape" => scales::RangeKind::Shape,
                "linetype" => scales::RangeKind::Linetype,
                _ => {
                    if scale.aesthetic.starts_with("pos") {
                        scales::RangeKind::Position
                    } else {
                        scales::RangeKind::Number
                    }
                }
            };
            if let Some(hs) = build_scale(Some(scale), kind) {
                view.insert_scale(scale.aesthetic.clone(), hs);
            }
        }

        // A spatial layer positions marks by geometry, not by `pos1`/`pos2`
        // columns, so ggsql resolves no position scales for it. Register
        // continuous `pos1`/`pos2` scales spanning the map's bounding box so
        // `GeometryGeom`'s coordinates map into the panel. The bbox comes from
        // ggsql (`computed["bbox"]` when projected, else the geometry extent),
        // keeping the "writer never invents extents" principle.
        let spatial_bbox = spatial_bbox(spec, data)?;
        if let Some((xmin, ymin, xmax, ymax)) = spatial_bbox {
            view.insert_scale(
                "pos1".to_string(),
                scale::continuous(nice_range(xmin, xmax)),
            );
            view.insert_scale(
                "pos2".to_string(),
                scale::continuous(nice_range(ymin, ymax)),
            );
        }

        // Legends are collected from the first panel only and registered once on
        // the composition's own legend ring, so a faceted plot gets a single shared
        // legend rather than one per panel. Every panel produces the same legends
        // (all built from the globally resolved scales), so one capture suffices.
        let legend_sink = std::cell::RefCell::new(Vec::new());
        let mut legends_captured = false;

        for panel in &panels {
            // Slice each layer's data to this panel. Skip panels with no data in
            // any layer (e.g. a Grid cell absent under `missing => 'null'`) so the
            // grid cell stays an empty framed panel rather than erroring.
            let slices: Vec<(&Layer, DataFrame)> = spec
                .layers
                .iter()
                .map(|layer| {
                    Ok((
                        layer,
                        facet::panel_dataframe(layer_dataframe(layer, data)?, panel)?,
                    ))
                })
                .collect::<Result<_>>()?;
            if slices.iter().all(|(_, df)| df.height() == 0) {
                continue;
            }

            // Fixed dimensions bind the shared `pos1`/`pos2`; free dimensions get
            // a per-panel scale whose domain is computed from this panel's slices
            // (the one place the writer computes extents — free facets only).
            let ps = facet::PanelScales::new(spec, panel);
            let layer_dfs: Vec<&DataFrame> = slices.iter().map(|(_, df)| df).collect();
            if ps.free_x {
                if let Some(hs) =
                    scales::free_position_scale(spec.find_scale("pos1"), &layer_dfs, "pos1")
                {
                    view.insert_scale(ps.pos1.clone(), hs);
                }
            }
            if ps.free_y {
                if let Some(hs) =
                    scales::free_position_scale(spec.find_scale("pos2"), &layer_dfs, "pos2")
                {
                    view.insert_scale(ps.pos2.clone(), hs);
                }
            }

            // Build every layer's geom into this panel; geoms bind channels and
            // record legends (first panel only) into `legend_sink`, drawing in
            // layer (DRAW) = z-order.
            let panel_legends = (!legends_captured).then_some(&legend_sink);
            let mut plot = HPlot::new(&composition, panel.id.as_str())
                .shape_registry(ShapeRegistry::with_builtins());
            for (layer, df) in &slices {
                let ctx = Ctx {
                    spec,
                    layer,
                    df,
                    transposed: is_transposed(layer),
                    pos1_scale: &ps.pos1,
                    pos2_scale: &ps.pos2,
                    legends: panel_legends,
                };
                geom::build_into_plot(&mut plot, &ctx)?;
            }
            legends_captured = true;

            // Axes are created per coordinate system, edge-only for fixed scales.
            plot = apply_projection(plot, spec, panel, &ps);

            // Lock a spatial panel's aspect to its bounding box so the projected
            // geometry keeps its proportions (a globe stays round), the raster
            // analog of the Vega-Lite writer's uniform projection scale.
            if let Some((xmin, ymin, xmax, ymax)) = spatial_bbox {
                let (w, h) = (xmax - xmin, ymax - ymin);
                if w > 0.0 && h > 0.0 {
                    plot = plot.aspect_ratio(h / w).aspect_mode(AspectMode::Range);
                }
            }

            // Facet strip labels (Wrap/Grid-column header on top, Grid-row on right).
            if let Some(text) = &panel.strip_top {
                plot = plot.strip(AxisSide::Top, text.clone());
            }
            if let Some(text) = &panel.strip_right {
                plot = plot.strip(AxisSide::Right, text.clone());
            }

            view.attach_plot(plot);
        }

        // One shared legend for the whole composition (see `legend_sink` above).
        for legend in legend_sink.into_inner() {
            view.add_legend(legend);
        }

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

/// The map bounding box `(xmin, ymin, xmax, ymax)` when the plot has a spatial
/// layer, else `None`. Prefers ggsql's resolved `computed["bbox"]` (set under a
/// `PROJECT map`); falls back to the union extent of the geometry data for a
/// bare `spatial` geom with no projection.
fn spatial_bbox(
    spec: &Plot,
    data: &HashMap<String, DataFrame>,
) -> Result<Option<(f64, f64, f64, f64)>> {
    let is_spatial = |layer: &Layer| layer.geom.geom_type() == GeomType::Spatial;
    if !spec.layers.iter().any(is_spatial) {
        return Ok(None);
    }

    if let Some(proj) = &spec.project {
        if let Some(ParameterValue::Array(arr)) = proj.computed.get("bbox") {
            let nums: Vec<f64> = arr.iter().filter_map(|e| e.to_f64()).collect();
            if let [xmin, ymin, xmax, ymax] = nums[..] {
                if [xmin, ymin, xmax, ymax].iter().all(|v| v.is_finite()) {
                    return Ok(Some((xmin, ymin, xmax, ymax)));
                }
            }
        }
    }

    let geom_col = naming::aesthetic_column("geometry");
    let (mut xmin, mut ymin, mut xmax, mut ymax) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for layer in spec.layers.iter().filter(|l| is_spatial(l)) {
        let df = layer_dataframe(layer, data)?;
        if df.column(&geom_col).is_err() {
            continue;
        }
        for g in channels::column_to_geometry(df, &geom_col)? {
            if let Some((x0, y0, x1, y1)) = g.bounds() {
                xmin = xmin.min(x0);
                ymin = ymin.min(y0);
                xmax = xmax.max(x1);
                ymax = ymax.max(y1);
            }
        }
    }
    Ok(
        (xmin.is_finite() && ymin.is_finite() && xmax.is_finite() && ymax.is_finite())
            .then_some((xmin, ymin, xmax, ymax)),
    )
}

/// A non-degenerate inclusive range for a continuous position scale, widening a
/// zero-width or inverted extent so the scale can map it.
fn nice_range(min: f64, max: f64) -> std::ops::RangeInclusive<f64> {
    if max - min > f64::EPSILON {
        min..=max
    } else {
        (min - 0.5)..=(max + 0.5)
    }
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

    /// The panels' `(top, right)` strip labels, in panel order. Exercises the
    /// facet layout and labelling without rendering, so it needs no GPU.
    fn strips(query: &str) -> Vec<(Option<String>, Option<String>)> {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let spec = reader.execute(query).unwrap();
        let (_, panels) = facet::build_panels(spec.plot(), spec.data()).unwrap();
        panels
            .iter()
            .map(|p| (p.strip_top.clone(), p.strip_right.clone()))
            .collect()
    }

    /// Just the top strip labels, in panel order.
    fn top_strips(query: &str) -> Vec<String> {
        strips(query)
            .into_iter()
            .map(|(top, _)| top.unwrap_or_default())
            .collect()
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
    fn renders_shape_legend() {
        // A non-color legend key must be given a color to paint, else the
        // swatches come out empty next to their labels.
        assert_png_or_skip(render(
            "SELECT x, y, g FROM (VALUES (1,2,'a'),(2,3,'b'),(3,1,'c')) t(x,y,g) \
             VISUALISE x AS x, y AS y, g AS shape DRAW point",
        ));
    }

    #[test]
    fn renders_linetype_legend() {
        assert_png_or_skip(render(
            "SELECT x, y, g FROM (VALUES (1,2,'a'),(2,3,'a'),(1,1,'b'),(2,2,'b')) t(x,y,g) \
             VISUALISE x AS x, y AS y, g AS linetype DRAW line",
        ));
    }

    #[test]
    fn renders_colorbar_beside_size_legend() {
        // Two distinct scales: a merged colorbar for `color` plus a keyed size
        // legend whose glyphs fall back to a neutral color (the mapped `fill`
        // column holds domain values, not a constant to borrow).
        assert_png_or_skip(render(
            "SELECT x, y, c, w FROM (VALUES (1,2,10,100),(2,3,50,200),(3,1,90,300)) t(x,y,c,w) \
             VISUALISE x AS x, y AS y, c AS color, w AS size DRAW point",
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
    fn renders_dodged_bar() {
        assert_png_or_skip(render(
            "SELECT x, grp, v FROM (VALUES ('a','p',3),('a','q',5),('b','p',2),('b','q',4)) \
             t(x, grp, v) \
             VISUALISE x AS x, v AS y, grp AS fill DRAW bar SETTING position => 'dodge'",
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
    fn renders_text_styled() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 1 AS y, 'a' AS lab UNION ALL SELECT 2, 2, 'Hello' \
             UNION ALL SELECT 3, 3, 'z' \
             VISUALISE x AS x, y AS y, lab AS label, 30 AS rotation, \
             'bold' AS fontweight, 22 AS fontsize DRAW text",
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
    fn renders_boxplot() {
        assert_png_or_skip(render(
            "SELECT g, y FROM (VALUES ('a',1),('a',5),('a',3),('a',9),('a',2),('a',20), \
             ('b',4),('b',6),('b',5),('b',7),('b',3)) t(g, y) \
             VISUALISE g AS x, y AS y DRAW boxplot",
        ));
    }

    #[test]
    fn renders_boxplot_fill_by_group() {
        assert_png_or_skip(render(
            "SELECT g, y FROM (VALUES ('a',1),('a',5),('a',3),('a',9),('a',2), \
             ('b',4),('b',6),('b',5),('b',7),('b',3)) t(g, y) \
             VISUALISE g AS x, y AS y, g AS fill DRAW boxplot",
        ));
    }

    #[test]
    fn renders_diagonal_rule() {
        assert_png_or_skip(render(
            "SELECT 0 AS i VISUALISE i AS y DRAW rule \
             SETTING slope => 1 SCALE x FROM (0, 10) SCALE y FROM (0, 10)",
        ));
    }

    #[test]
    fn renders_constant_aesthetics() {
        // Constant material values from `SETTING` arrive as `AestheticValue::Literal`
        // and must be honored (color/size on points, linetype/linewidth on a line).
        assert_png_or_skip(render(
            "SELECT * FROM (VALUES (1,1),(2,3),(3,2)) t(a,b) \
             VISUALISE a AS x, b AS y DRAW point SETTING color => 'red', size => 8",
        ));
        assert_png_or_skip(render(
            "SELECT * FROM (VALUES (1,1),(2,3),(3,2)) t(a,b) \
             VISUALISE a AS x, b AS y DRAW line \
             SETTING color => 'steelblue', linetype => 'dashed', linewidth => 2",
        ));
    }

    #[test]
    fn renders_multilayer_point_line() {
        // Two layers share one pair of axes / position scales.
        assert_png_or_skip(render(
            "SELECT * FROM (VALUES (1,2),(2,4),(3,5),(4,4),(5,7)) t(a,b) \
             VISUALISE a AS x, b AS y DRAW point DRAW line",
        ));
    }

    #[test]
    fn renders_multilayer_overlay() {
        // Bar + point overlay (point drawn over bar) over a shared discrete x.
        assert_png_or_skip(render(
            "SELECT g, b FROM (VALUES ('a',2),('b',4),('c',5),('d',3)) t(g,b) \
             VISUALISE g AS x, b AS y DRAW bar DRAW point SETTING color => 'red'",
        ));
    }

    #[test]
    fn renders_multilayer_abline() {
        // A diagonal reference line overlaid on a scatter spans the shared
        // resolved x/y domain.
        assert_png_or_skip(render(
            "SELECT * FROM (VALUES (1,2),(2,4),(3,5),(4,4),(5,7)) t(a,b) \
             VISUALISE a AS x, b AS y DRAW point PLACE rule SETTING slope => 1, y => 0",
        ));
    }

    #[test]
    fn renders_multilayer_shared_legend() {
        // Two layers both colored by the same variable → one collapsed legend.
        assert_png_or_skip(render(
            "SELECT g, a, b FROM (VALUES ('p',1,2),('p',2,4),('q',3,5),('q',4,4)) t(g,a,b) \
             VISUALISE a AS x, b AS y, g AS color DRAW point DRAW line",
        ));
    }

    #[test]
    fn renders_boxplot_styled() {
        assert_png_or_skip(render(
            "SELECT g, y FROM (VALUES ('a',1),('a',5),('a',3),('a',9),('a',2), \
             ('b',4),('b',6),('b',5),('b',7),('b',3)) t(g, y) \
             VISUALISE g AS x, y AS y, 'navy' AS stroke DRAW boxplot",
        ));
    }

    #[test]
    fn renders_boxplot_stroke_by_group() {
        // Data-mapped stroke colors every component (box/whisker/median/outlier)
        // per group and registers one collapsed legend.
        assert_png_or_skip(render(
            "SELECT g, y FROM (VALUES ('a',1),('a',5),('a',3),('a',9),('a',2),('a',40), \
             ('b',4),('b',6),('b',5),('b',7),('b',3)) t(g, y) \
             VISUALISE g AS x, y AS y, g AS stroke DRAW boxplot",
        ));
    }

    #[test]
    fn renders_tile_sized() {
        // `width`/`height` settings shrink discrete tiles within their band.
        assert_png_or_skip(render(
            "SELECT a, b, v FROM (VALUES ('x','p',1),('y','q',2),('x','q',3),('y','p',4)) t(a,b,v) \
             VISUALISE a AS x, b AS y, v AS fill DRAW tile SETTING width => 0.5, height => 0.5",
        ));
    }

    #[test]
    fn renders_violin() {
        assert_png_or_skip(render(
            "SELECT g, y FROM (VALUES ('a',1),('a',5),('a',3),('a',9),('a',2), \
             ('b',4),('b',6),('b',5),('b',7),('b',3)) t(g, y) \
             VISUALISE g AS x, y AS y DRAW violin",
        ));
    }

    #[test]
    fn renders_polar_pie() {
        // A stacked bar under polar becomes a pie: pos2 (count) → theta,
        // pos1 (dummy) → radius. Includes a 180° slice, which exercises the
        // wide-wedge path.
        assert_png_or_skip(render(
            "SELECT c FROM (VALUES ('a'),('a'),('a'),('b'),('b'),('c')) t(c) \
             VISUALISE c AS fill DRAW bar PROJECT TO polar",
        ));
    }

    #[test]
    fn renders_polar_donut() {
        // `inner` opens a centre hole (donut).
        assert_png_or_skip(render(
            "SELECT c FROM (VALUES ('a'),('a'),('a'),('b'),('b'),('c')) t(c) \
             VISUALISE c AS fill DRAW bar PROJECT TO polar SETTING inner => 0.5",
        ));
    }

    #[test]
    fn renders_wrap_facet() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 'a' AS g UNION ALL SELECT 2, 3, 'b' \
             UNION ALL SELECT 3, 1, 'a' UNION ALL SELECT 4, 5, 'c' \
             VISUALISE x AS x, y AS y DRAW point FACET g",
        ));
    }

    #[test]
    fn renders_grid_facet() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 'a' AS r, 'p' AS c UNION ALL SELECT 2, 3, 'b', 'p' \
             UNION ALL SELECT 3, 1, 'a', 'q' UNION ALL SELECT 4, 5, 'b', 'q' \
             VISUALISE x AS x, y AS y DRAW point FACET r BY c",
        ));
    }

    #[test]
    fn renders_faceted_bar_with_color() {
        assert_png_or_skip(render(
            "SELECT g, k FROM (VALUES ('a','x'),('a','y'),('b','x'),('b','y'),('a','x')) t(g, k) \
             VISUALISE k AS x, k AS fill DRAW bar FACET g",
        ));
    }

    #[test]
    fn renders_free_scale_facet() {
        // Panels with very different data ranges: free scales give each panel its
        // own per-panel domain and axes.
        assert_png_or_skip(render(
            "SELECT x, y, g FROM (VALUES (1,1,'a'),(2,2,'a'),(3,3,'a'),\
             (100,100,'b'),(200,200,'b'),(300,300,'b')) t(x,y,g) \
             VISUALISE x AS x, y AS y DRAW point FACET g SETTING free => ['x','y']",
        ));
    }

    #[test]
    fn renders_polar_facet() {
        // A pie per panel, sharing the fill scale; proportions differ per panel.
        assert_png_or_skip(render(
            "SELECT c, panel FROM (VALUES \
             ('a','one'),('a','one'),('b','one'),('c','one'),\
             ('a','two'),('b','two'),('b','two'),('b','two'),('c','two')) t(c, panel) \
             VISUALISE c AS fill DRAW bar PROJECT TO polar FACET panel",
        ));
    }

    #[cfg(feature = "spatial")]
    #[test]
    fn renders_spatial() {
        // A bare `spatial` geom (no PROJECT): two polygons filled by a value,
        // framed to the geometry bbox under Cartesian with equal aspect.
        assert_png_or_skip(render(
            "INSTALL spatial; LOAD spatial; \
             SELECT ST_GeomFromText('POLYGON ((0 0, 1 0, 1 1, 0 1, 0 0))') AS geom, \
             200 AS population \
             UNION ALL SELECT ST_GeomFromText('POLYGON ((1 0, 2 0, 2 1, 1 1, 1 0))'), 150 \
             VISUALISE DRAW spatial MAPPING population AS fill",
        ));
    }

    #[cfg(feature = "spatial")]
    #[test]
    fn renders_spatial_mapped_opacity() {
        // A data-mapped scalar aesthetic (opacity) must vary per feature and
        // register a legend, not collapse to a constant.
        assert_png_or_skip(render(
            "INSTALL spatial; LOAD spatial; \
             SELECT ST_GeomFromText('POLYGON ((0 0, 1 0, 1 1, 0 1, 0 0))') AS geom, \
             10 AS v \
             UNION ALL SELECT ST_GeomFromText('POLYGON ((1 0, 2 0, 2 1, 1 1, 1 0))'), 90 \
             VISUALISE DRAW spatial MAPPING v AS opacity",
        ));
    }

    #[cfg(feature = "spatial")]
    #[test]
    fn renders_map() {
        // A projected world map: pre-projected geometry + Custom projection
        // boundary + graticules from `computed`.
        assert_png_or_skip(render(
            "VISUALISE FROM ggsql:world DRAW spatial PROJECT TO orthographic",
        ));
    }

    /// A 6-row fixture whose `g` is categorical and `v` numeric.
    const FACET_DATA: &str = "SELECT g, v, y FROM (VALUES \
         ('a',5,1),('a',7,2),('b',15,3),('b',18,1),('c',25,2),('c',28,3)) t(g,v,y)";

    #[test]
    fn facet_strips_rename_discrete() {
        assert_eq!(
            top_strips(&format!(
                "{FACET_DATA} VISUALISE v AS x, y AS y DRAW point FACET g \
                 SCALE panel RENAMING 'a' => 'Alpha'"
            )),
            vec!["Alpha", "b", "c"]
        );
    }

    #[test]
    fn facet_strips_suppress_discrete() {
        // A suppressed label leaves an empty strip, keeping panel heights aligned.
        assert_eq!(
            top_strips(&format!(
                "{FACET_DATA} VISUALISE v AS x, y AS y DRAW point FACET g \
                 SCALE panel RENAMING 'b' => NULL"
            )),
            vec!["a", "", "c"]
        );
    }

    #[test]
    fn facet_strips_null_level() {
        // A NULL facet level keys as "null" and is renamable under that key.
        let data = "SELECT g, v FROM (VALUES ('a',1),(NULL,2)) t(g,v)";
        assert_eq!(
            top_strips(&format!(
                "{data} VISUALISE v AS x, v AS y DRAW point FACET g \
                 SCALE panel FROM ('a', null)"
            )),
            vec!["a", "null"]
        );
        assert_eq!(
            top_strips(&format!(
                "{data} VISUALISE v AS x, v AS y DRAW point FACET g \
                 SCALE panel FROM ('a', null) RENAMING null => 'The rest'"
            )),
            vec!["a", "The rest"]
        );
    }

    #[test]
    fn facet_strips_binned_ranges() {
        // A numeric facet is binned; strips show the bin range, not the midpoint.
        assert_eq!(
            top_strips(&format!(
                "{FACET_DATA} VISUALISE v AS x, y AS y DRAW point FACET v \
                 SCALE panel SETTING breaks => (0, 10, 20, 30)"
            )),
            vec!["0 – 10", "10 – 20", "20 – 30"]
        );
    }

    #[test]
    fn facet_strips_binned_squish() {
        // `oob => 'squish'` opens the terminal bins: "< upper" / "≥ lower".
        // Two breaks-interior bins here, both terminal — matches the Vega-Lite
        // writer's labelExpr for the same query.
        assert_eq!(
            top_strips(&format!(
                "{FACET_DATA} VISUALISE v AS x, y AS y DRAW point FACET v \
                 SCALE panel SETTING breaks => (10, 20, 30), oob => 'squish'"
            )),
            vec!["< 20", "≥ 20"]
        );
    }

    #[test]
    fn facet_strips_binned_closed_right() {
        // `closed => 'right'` flips the open-ended terminal symbols.
        assert_eq!(
            top_strips(&format!(
                "{FACET_DATA} VISUALISE v AS x, y AS y DRAW point FACET v \
                 SCALE panel SETTING breaks => (10, 20, 30), oob => 'squish', \
                 closed => 'right'"
            )),
            vec!["≤ 20", "> 20"]
        );
    }

    #[test]
    fn facet_strips_binned_edge_renaming() {
        // RENAMING applies per break edge, before the range label is built.
        assert_eq!(
            top_strips(&format!(
                "{FACET_DATA} VISUALISE v AS x, y AS y DRAW point FACET v \
                 SCALE panel SETTING breaks => (0, 10, 20, 30) RENAMING 20 => 'twenty'"
            )),
            vec!["0 – 10", "10 – twenty", "twenty – 30"]
        );
    }

    #[test]
    fn facet_strips_binned_reverse() {
        assert_eq!(
            top_strips(&format!(
                "{FACET_DATA} VISUALISE v AS x, y AS y DRAW point FACET v \
                 SCALE panel SETTING breaks => (0, 10, 20, 30), reverse => true"
            )),
            vec!["20 – 30", "10 – 20", "0 – 10"]
        );
    }

    #[test]
    fn facet_strips_binned_temporal() {
        // Temporal binned facets label as date ranges. Vega-Lite silently fails
        // this case (its midpoint-string comparison never matches); computing the
        // label from typed values here avoids that whole class of bug.
        let data = "SELECT CAST(d AS DATE) AS d, v FROM (VALUES \
             ('1973-05-04', 1), ('1973-05-20', 2), ('1973-06-08', 3)) t(d, v)";
        assert_eq!(
            top_strips(&format!(
                "{data} VISUALISE v AS x, v AS y DRAW point FACET d \
                 SCALE panel SETTING breaks => 'month'"
            )),
            vec!["1973-05-01 – 1973-06-01", "1973-06-01 – 1973-07-01"]
        );
    }

    #[test]
    fn facet_strips_grid_row_column() {
        // Grid: renamed column labels on the top row only, renamed row labels on
        // the right column only.
        let data = "SELECT r, c, v FROM (VALUES \
             ('r1','c1',1),('r1','c2',2),('r2','c1',3),('r2','c2',4)) t(r,c,v)";
        assert_eq!(
            strips(&format!(
                "{data} VISUALISE v AS x, v AS y DRAW point FACET r BY c \
                 SCALE row RENAMING 'r1' => 'Row one' \
                 SCALE column RENAMING 'c2' => 'Col two'"
            )),
            vec![
                (Some("c1".into()), None),
                (Some("Col two".into()), Some("Row one".into())),
                (None, None),
                (None, Some("r2".into())),
            ]
        );
    }

    #[test]
    fn renders_binned_facet() {
        assert_png_or_skip(render(&format!(
            "{FACET_DATA} VISUALISE v AS x, y AS y DRAW point FACET v \
             SCALE panel SETTING breaks => (0, 10, 20, 30)"
        )));
    }

    #[test]
    fn renders_free_binned_facet() {
        // A free binned position dimension: each panel keeps ggsql's global bin
        // edges but shows only the bins its own data occupies.
        assert_png_or_skip(render(
            "VISUALISE body_mass AS x FROM ggsql:penguins DRAW bar \
             SCALE BINNED x SETTING breaks => (2500, 3500, 4500, 5500, 6500) \
             FACET species SETTING free => 'x'",
        ));
    }

    #[test]
    fn renders_boxplot_linewidth() {
        // `linewidth` thickens box, whiskers and median alike (VL puts
        // strokeWidth in the boxplot's shared encoding).
        assert_png_or_skip(render(
            "SELECT g, v FROM (VALUES ('a',1),('a',2),('a',3),('a',9),\
             ('b',2),('b',3),('b',4),('b',5)) t(g,v) \
             VISUALISE g AS x, v AS y DRAW boxplot SETTING linewidth => 3",
        ));
    }

    #[test]
    fn renders_boxplot_dashed() {
        assert_png_or_skip(render(
            "SELECT g, v FROM (VALUES ('a',1),('a',2),('a',3),('b',2),('b',3),('b',5)) t(g,v) \
             VISUALISE g AS x, v AS y DRAW boxplot \
             SETTING linetype => 'dashed', linewidth => 2",
        ));
    }

    #[test]
    fn renders_violin_linewidth() {
        assert_png_or_skip(render(
            "SELECT g, v FROM (VALUES ('a',1),('a',2),('a',2),('a',3),('a',4),\
             ('b',2),('b',3),('b',3),('b',4),('b',6)) t(g,v) \
             VISUALISE g AS x, v AS y DRAW violin \
             SETTING linewidth => 3, linetype => 'dashed'",
        ));
    }

    #[test]
    fn renders_text_stroke() {
        // A constant `stroke` outlines the glyphs; white-on-dark legibility.
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 'peak' AS lbl UNION ALL SELECT 2, 3, 'trough' \
             VISUALISE x AS x, y AS y, lbl AS label DRAW text \
             SETTING fontsize => 28, fontweight => 'bold', color => 'black', \
             stroke => 'white'",
        ));
    }

    #[test]
    fn renders_text_stroke_by_group() {
        // A data-mapped outline color: one scale + legend, per-row outline.
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 'a' AS lbl, 'one' AS g \
             UNION ALL SELECT 2, 3, 'b', 'two' \
             VISUALISE x AS x, y AS y, lbl AS label, g AS stroke DRAW text \
             SETTING fontsize => 30, fontweight => 'bold'",
        ));
    }

    #[test]
    fn renders_titled_plot() {
        // Title, subtitle and caption all sit on the composition, above/below the
        // single panel.
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y UNION ALL SELECT 2, 3 UNION ALL SELECT 3, 1 \
             VISUALISE x AS x, y AS y DRAW point \
             LABEL title => 'Sales by Region', subtitle => 'FY 2024', \
             caption => 'Source: internal'",
        ));
    }

    #[test]
    fn renders_suppressed_title() {
        // `LABEL title => NULL` suppresses; the subtitle still renders.
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y UNION ALL SELECT 2, 3 \
             VISUALISE x AS x, y AS y DRAW point \
             LABEL title => NULL, subtitle => 'no title above me'",
        ));
    }

    #[test]
    fn renders_titled_facet() {
        // One composition-spanning title over the whole 3-panel strip, not one
        // title per panel.
        assert_png_or_skip(render(
            "SELECT x, y, g FROM (VALUES (1,1,'a'),(2,2,'a'),(1,2,'b'),(2,3,'b'),\
             (1,3,'c'),(2,1,'c')) t(x,y,g) \
             VISUALISE x AS x, y AS y DRAW point FACET g \
             LABEL title => 'One title for all panels'",
        ));
    }

    #[test]
    fn rejects_unsupported_geom() {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let spec = reader
            .execute(
                "SELECT 0 AS x, 0 AS y, 1 AS xend, 1 AS yend \
                 VISUALISE x AS x, y AS y, xend AS xend, yend AS yend DRAW arrow",
            )
            .unwrap();
        let writer = HephaestusWriter::new(320, 240, 96.0);
        assert!(matches!(
            writer.validate(spec.plot()),
            Err(GgsqlError::WriterError(_))
        ));
    }
}
