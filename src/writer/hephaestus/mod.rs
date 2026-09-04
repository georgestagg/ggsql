//! Renderer-backed writers.
//!
//! Every writer here renders a resolved ggsql `Spec` through the [`hephaestus`]
//! 2D scene renderer. Only the writers themselves are public; the renderer
//! behind them is an implementation detail, and this module is private.
//!
//! The work splits three ways, which is what keeps one writer per format small:
//!
//! - [`compose`] turns a `Plot` into a live `PlotComposition`. Format-independent,
//!   and where nearly all the code is.
//! - [`canvas`] carries the size, resolution and background, and parses the
//!   options they come from.
//! - [`raster`] rasterises a composition to pixels. **The only part that needs a
//!   GPU adapter** — a vector writer builds a scene from the same composition
//!   and never comes through here.
//!
//! **Scope**: multi-layer plots under Cartesian, Polar, and Map projections,
//! with `FACET` faceting (Wrap/Grid, fixed + free scales); every geom except
//! `arrow`, which is a stub no writer implements; all scale types and
//! transforms, material aesthetics, plot and axis titles, and legends.
//!
//! Architecture — the abstractions and the invariants they keep — and the
//! inventory of deferred work are documented in
//! `src/writer/hephaestus/CLAUDE.md`.

mod canvas;
mod channels;
mod compose;
mod facet;
mod geom;
mod projection;
mod raster;
mod scales;
mod wiring;

use std::collections::HashMap;

pub use hephaestus::color::{rgba, Color};
#[cfg(feature = "png")]
use hephaestus::png::{encode_png, PngCompression};

pub use canvas::Canvas;
#[cfg(test)]
use canvas::{DEFAULT_DPI, DEFAULT_HEIGHT, DEFAULT_WIDTH};
#[cfg(feature = "raster")]
pub use raster::RasterRenderer;

use crate::writer::{Writer, WriterOptions};
use crate::{DataFrame, GgsqlError, Plot, Result};

/// Option keys [`PngWriter`] adds to the shared canvas set.
const PNG_OPTIONS: &[&str] = &["compression"];

/// How hard the PNG encoder works to make the file small.
const COMPRESSION_VALUES: &[&str] = &["none", "fast", "balanced", "small"];

/// Writer that renders a ggsql plot to a PNG image.
///
/// Configured with a target pixel size and DPI because raster rendering needs
/// concrete dimensions, unlike the resolution-independent Vega-Lite writer.
/// [`PngWriter::from_options`] builds the same configuration from
/// key–value [`WriterOptions`]:
///
/// | Option | Value | Default |
/// | --- | --- | --- |
/// | `width` | Canvas width, in `units` | 1500 px |
/// | `height` | Canvas height, in `units` | 1000 px |
/// | `units` | `px`, `in`, `cm`, `mm`, or `pt` — how `width`/`height` are read | `px` |
/// | `dpi` | Pixels per inch; converts physical sizes, including `units` | 300 |
/// | `background` | Any CSS color, e.g. `white`, `#ff0000`, `transparent` | `white` |
/// | `compression` | `none`, `fast`, `balanced`, or `small` | `balanced` |
///
/// `compression` trades encode time against file size, losslessly either way.
/// `balanced` is what a file wants. `fast` is for a caller on a frame deadline —
/// a host encoding a plot per resize, say — where it costs a fraction of the
/// time for about half again the bytes.
///
/// Rendering requires a working wgpu adapter (hardware or software, e.g.
/// lavapipe) at render time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PngWriter {
    canvas: Canvas,
    compression: PngCompression,
}

impl PngWriter {
    /// Create a writer for the given pixel dimensions and DPI, white background.
    pub fn new(width: u32, height: u32, dpi: f64) -> Self {
        Self {
            canvas: Canvas::new(width, height, dpi),
            compression: PngCompression::Balanced,
        }
    }

    /// Set the background color used to clear the canvas before rendering.
    pub fn background(mut self, color: Color) -> Self {
        self.canvas = self.canvas.background(color);
        self
    }

    /// Set how hard the encoder works to make the file small.
    pub fn compression(mut self, compression: PngCompression) -> Self {
        self.compression = compression;
        self
    }

    /// Render through a renderer the caller keeps, rather than building one.
    ///
    /// Constructing a [`RasterRenderer`] creates a GPU device and compiles the
    /// rasteriser's shaders, so a host rendering more than one figure should
    /// build one once and pass it here.
    ///
    /// # Errors
    ///
    /// Returns `GgsqlError::WriterError` if the plot cannot be composed, the
    /// render fails, or the encode fails.
    pub fn write_with(
        &self,
        spec: &Plot,
        data: &HashMap<String, DataFrame>,
        renderer: &mut RasterRenderer,
    ) -> Result<Vec<u8>> {
        compose::validate_plot(spec)?;
        let mut view = compose::build_composition(spec, data)?;
        let pixels = raster::render_rgba8(&mut view, &self.canvas, renderer)?;
        // `render_to_buffer` hands out straight (un-premultiplied) alpha, which
        // is exactly what PNG stores, so the buffer encodes as-is.
        encode_png(
            self.canvas.width,
            self.canvas.height,
            &pixels,
            self.compression,
            self.canvas.dpi_hint(),
        )
        .map_err(|e| GgsqlError::WriterError(format!("png encode failed: {e}")))
    }

    /// [`Self::write_with`] from a resolved `Spec`.
    ///
    /// # Errors
    ///
    /// As [`Self::write_with`].
    pub fn render_with(
        &self,
        spec: &crate::reader::Spec,
        renderer: &mut RasterRenderer,
    ) -> Result<Vec<u8>> {
        self.write_with(spec.plot(), spec.data(), renderer)
    }
}

impl Default for PngWriter {
    fn default() -> Self {
        Self {
            canvas: Canvas::default(),
            compression: PngCompression::Balanced,
        }
    }
}

impl Writer for PngWriter {
    type Output = Vec<u8>;

    fn from_options(options: &WriterOptions) -> Result<Self> {
        let canvas = Canvas::from_options(options, PNG_OPTIONS)?;
        let compression = match options.one_of("compression", COMPRESSION_VALUES)? {
            Some("none") => PngCompression::None,
            Some("fast") => PngCompression::Fast,
            Some("small") => PngCompression::Small,
            _ => PngCompression::Balanced,
        };
        Ok(Self {
            canvas,
            compression,
        })
    }

    fn validate(&self, spec: &Plot) -> Result<()> {
        compose::validate_plot(spec)
    }

    fn write(&self, spec: &Plot, data: &HashMap<String, DataFrame>) -> Result<Self::Output> {
        let mut renderer = RasterRenderer::new()?;
        self.write_with(spec, data, &mut renderer)
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;

    fn writer(pairs: &[&str]) -> Result<PngWriter> {
        PngWriter::from_options(&WriterOptions::parse(pairs)?)
    }

    /// The writer's canvas as `(width, height, dpi)`.
    fn canvas(pairs: &[&str]) -> (u32, u32, f64) {
        let writer = writer(pairs).unwrap();
        let c = writer.canvas;
        (c.width, c.height, c.dpi)
    }

    #[test]
    fn no_options_gives_the_defaults() {
        assert_eq!(canvas(&[]), (DEFAULT_WIDTH, DEFAULT_HEIGHT, DEFAULT_DPI));
        let default = PngWriter::default();
        let dc = default.canvas;
        assert_eq!(canvas(&[]), (dc.width, dc.height, dc.dpi));
        // White, as `new()` sets it.
        let background = writer(&[]).unwrap().canvas.background;
        assert_eq!(background.components, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn pixel_dimensions_are_taken_verbatim() {
        assert_eq!(canvas(&["width=1600", "height=1200"]).0, 1600);
        assert_eq!(canvas(&["width=1600", "height=1200"]).1, 1200);
        // `units=px` is the default, and DPI does not rescale a pixel canvas.
        assert_eq!(
            canvas(&["width=800", "units=px", "dpi=72"]),
            (800, 1000, 72.0)
        );
    }

    #[test]
    fn physical_dimensions_scale_with_dpi() {
        assert_eq!(
            canvas(&["width=8", "height=6", "units=in", "dpi=100"]).0,
            800
        );
        assert_eq!(
            canvas(&["width=8", "height=6", "units=in", "dpi=100"]).1,
            600
        );
        // 2.54 cm = 1 in; 25.4 mm = 1 in; 72 pt = 1 in.
        assert_eq!(canvas(&["width=2.54", "units=cm", "dpi=96"]).0, 96);
        assert_eq!(canvas(&["width=25.4", "units=mm", "dpi=96"]).0, 96);
        assert_eq!(canvas(&["width=72", "units=pt", "dpi=96"]).0, 96);
        // Defaults stay pixel counts even when the caller works in inches.
        assert_eq!(
            canvas(&["width=5", "units=in", "dpi=200"]).1,
            DEFAULT_HEIGHT
        );
    }

    #[test]
    fn background_accepts_css_colors() {
        let red = writer(&["background=#ff0000"]).unwrap().canvas.background;
        assert_eq!(red.components, [1.0, 0.0, 0.0, 1.0]);
        for spelling in ["background=transparent", "background=none"] {
            let clear = writer(&[spelling]).unwrap().canvas.background;
            assert_eq!(
                clear.components[3], 0.0,
                "{spelling} should be fully transparent"
            );
        }
        assert!(writer(&["background=rgb(0, 0, 255)"]).is_ok());
    }

    #[test]
    fn bad_values_are_reported_per_option() {
        let cases = [
            ("units=furlongs", "'units' expects"),
            ("dpi=0", "'dpi' expects a positive number"),
            ("dpi=high", "'dpi' expects a number"),
            ("width=0", "'width' resolves to 0 px"),
            ("width=-4", "'width' resolves to -4 px"),
            ("height=1e9", "'height' resolves to"),
            ("background=nope", "'background' expects a CSS color"),
        ];
        for (option, expected) in cases {
            let err = writer(&[option]).unwrap_err().to_string();
            assert!(err.contains(expected), "{option}: {err}");
        }
    }

    #[test]
    fn unknown_options_are_rejected() {
        let err = writer(&["with=1600"]).unwrap_err().to_string();
        assert!(err.contains("unknown writer option 'with'"), "{err}");
        assert!(err.contains("supported options: width, height"), "{err}");
    }
}

#[cfg(all(test, feature = "duckdb"))]
mod tests {
    use super::*;
    use crate::reader::{DuckDBReader, Reader};
    use hephaestus::scales::chrome::AxisSide;

    fn render(query: &str) -> Result<Vec<u8>> {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let spec = reader.execute(query).unwrap();
        PngWriter::new(640, 480, 96.0).render(&spec)
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

    /// The figure's composition-level axis titles. Needs no GPU.
    fn axis_titles(query: &str) -> Vec<(AxisSide, String)> {
        let reader = DuckDBReader::from_connection_string("duckdb://memory").unwrap();
        let spec = reader.execute(query).unwrap();
        projection::composition_axis_titles(spec.plot())
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

    /// An identity column is a per-row literal, so a `linetype` column holds ggsql
    /// names or hex patterns and must go through `map_linetype` exactly as the
    /// literal does — the channel takes dash patterns, not strings, so passing the
    /// names through drew a solid line.
    #[test]
    fn renders_identity_linetype() {
        assert_png_or_skip(render(
            "SELECT x, y, lt FROM (VALUES (1,2,'dashed'),(2,3,'dashed'),(1,1,'dotted'),(2,2,'dotted')) t(x,y,lt) \
             VISUALISE x AS x, y AS y, lt AS linetype DRAW line SCALE IDENTITY linetype",
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

    /// A scaled `fontsize` on a layer whose face is set: the legend key is
    /// dressed from the same material table the glyphs are, so `family` /
    /// `weight` / `italic` / `angle` all have to reach it.
    #[test]
    fn renders_text_font_legend() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 1 AS y, 'a' AS lab, 10 AS sz UNION ALL SELECT 2, 2, 'b', 20 \
             UNION ALL SELECT 3, 3, 'c', 30 \
             VISUALISE x AS x, y AS y, lab AS label, sz AS fontsize \
             DRAW text SETTING typeface => 'Times New Roman', fontweight => 'bold', \
             italic => true, rotation => 20 SCALE fontsize TO (10, 30)",
        ));
    }

    /// A label carrying markdown: `parse` defaults on, so the row goes through
    /// hephaestus's rich-text shaper rather than being drawn with its markers.
    #[test]
    fn renders_text_markdown() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 1 AS y, '**bold** and {.red red}' AS lab \
             UNION ALL SELECT 2, 2, '`code` and ~~strike~~' \
             VISUALISE x AS x, y AS y, lab AS label DRAW text",
        ));
    }

    /// `SETTING parse => false` opts the layer out, drawing the markers literally.
    #[test]
    fn renders_text_markdown_off() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 1 AS y, '**bold** and {.red red}' AS lab \
             VISUALISE x AS x, y AS y, lab AS label DRAW text SETTING parse => false",
        ));
    }

    /// The glyph outline survives the markdown path: hephaestus folds the row's
    /// `text_stroke` onto the rich sheet's root selector rather than dropping it.
    #[test]
    fn renders_text_markdown_with_stroke() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 1 AS y, '**bold**' AS lab \
             VISUALISE x AS x, y AS y, lab AS label \
             DRAW text SETTING fontsize => 30, stroke => 'red', rotation => 20",
        ));
    }

    /// Markdown chrome: a `LABEL` string is rich text too, so the title, subtitle,
    /// caption and axis titles all shape through the rich pipeline.
    #[test]
    fn renders_markdown_chrome() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y UNION ALL SELECT 2, 3 \
             VISUALISE x AS x, y AS y DRAW point \
             LABEL title => 'A **bold** title', subtitle => '{.red red} subtitle', \
             caption => '*italic* caption', x => 'axis *italic*'",
        ));
    }

    /// The same aesthetics as *columns*, which take the identity path rather than
    /// the literal one: strings, booleans and degrees, each converted per row.
    #[test]
    fn renders_text_mapped_font() {
        assert_png_or_skip(render(
            "SELECT 1 AS x, 1 AS y, 'a' AS lab, 'Times New Roman' AS face, 'bold' AS wt, \
             true AS it, 0 AS rot \
             UNION ALL SELECT 2, 2, 'b', 'Helvetica', 'light', false, 45 \
             VISUALISE x AS x, y AS y, lab AS label, face AS typeface, wt AS fontweight, \
             it AS italic, rot AS rotation DRAW text",
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
        // The dash pattern is honored on the computed segment.
        assert_png_or_skip(render(
            "SELECT 0 AS i VISUALISE i AS y DRAW rule \
             SETTING slope => 1, linetype => 'dashed', linewidth => 2 \
             SCALE x FROM (0, 10) SCALE y FROM (0, 10)",
        ));
        // One line per row: three intercepts → three parallel lines.
        assert_png_or_skip(render(
            "SELECT * FROM (VALUES (0),(2),(4)) t(i) VISUALISE i AS y DRAW rule \
             SETTING slope => 1 SCALE x FROM (0, 10) SCALE y FROM (0, 15)",
        ));
    }

    #[test]
    fn renders_multiple_diagonal_rules() {
        // Per-row slope + intercept + a data-mapped material aesthetic: three
        // differently-sloped, differently-colored ablines over a scatter (the
        // Vega-Lite writer's `test_rule_renderer_multiple_diagonal_lines` query).
        assert_png_or_skip(render(
            "WITH points AS (SELECT * FROM (VALUES (0, 5), (5, 15), (10, 25)) t(x, y)), \
                  lines AS (SELECT * FROM (VALUES (2, 5, 'A'), (1, 10, 'B'), (3, 0, 'C')) \
                            t(slope, y, line_id)) \
             SELECT * FROM points VISUALISE \
             DRAW point MAPPING x AS x, y AS y \
             DRAW rule MAPPING slope AS slope, y AS y, line_id AS color FROM lines",
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
    fn renders_tile_mixed_discrete_and_continuous_axes() {
        // The tile stat parameterises each direction on its own, so a tile can be
        // banded on one axis and spanned by extents on the other.
        let data =
            "SELECT c, n, v FROM (VALUES ('x',1.0,1),('y',2.0,2),('x',2.0,3),('y',1.0,4)) t(c,n,v)";
        assert_png_or_skip(render(&format!(
            "{data} VISUALISE c AS x, n AS y, v AS fill DRAW tile"
        )));
        assert_png_or_skip(render(&format!(
            "{data} VISUALISE n AS x, c AS y, v AS fill DRAW tile"
        )));
    }

    #[test]
    fn renders_text_keyword_justification_column() {
        // A `vjust` column of keywords is read as keywords: casting it to numbers
        // first would silently make every anchor NaN.
        assert_png_or_skip(render(
            "SELECT x, y, l, j FROM (VALUES (1,1,'one','top'),(2,2,'two','bottom')) t(x,y,l,j) \
             VISUALISE x AS x, y AS y, l AS label, j AS vjust DRAW text",
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
    fn renders_sparse_grid_facet() {
        // A grid whose row × column combinations are not all present: the absent
        // cells are still drawn — framed, gridded, axed and strip-labelled — so the
        // grid stays rectangular. `('b','q')` has no rows here.
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 'a' AS r, 'p' AS c UNION ALL SELECT 2, 3, 'b', 'p' \
             UNION ALL SELECT 3, 1, 'a', 'q' \
             VISUALISE x AS x, y AS y DRAW point FACET r BY c",
        ));
    }

    #[test]
    fn renders_sparse_grid_facet_free() {
        // An empty cell has no extent of its own, so a free dimension falls back to
        // the shared scale there — the axis and channel bindings must still resolve.
        assert_png_or_skip(render(
            "SELECT 1 AS x, 2 AS y, 'a' AS r, 'p' AS c UNION ALL SELECT 2, 3, 'b', 'p' \
             UNION ALL SELECT 3, 1, 'a', 'q' \
             VISUALISE x AS x, y AS y DRAW point FACET r BY c SETTING free => ['x','y']",
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

    /// Under a map `PROJECT`, ggsql expands these layers into per-vertex rows and
    /// remaps the extent aesthetics onto `pos1`/`pos2`, so each must draw as a
    /// polyline or a polygon rather than as its usual mark — otherwise a segment
    /// is zero-length, a ribbon zero-height, a rule a fan of straight lines, and
    /// a tile a box per vertex.
    #[cfg(feature = "spatial")]
    #[test]
    fn renders_densified_segment() {
        assert_png_or_skip(render(
            "INSTALL spatial; LOAD spatial; \
             SELECT * FROM (VALUES (-100,30,20,60),(-50,-20,100,10)) t(x1,y1,x2,y2) \
             VISUALISE x1 AS x, y1 AS y, x2 AS xend, y2 AS yend DRAW segment \
             SETTING stroke => 'firebrick', linewidth => 2 PROJECT x, y TO robinson",
        ));
    }

    #[cfg(feature = "spatial")]
    #[test]
    fn renders_densified_ribbon() {
        assert_png_or_skip(render(
            "INSTALL spatial; LOAD spatial; \
             SELECT * FROM (VALUES (-160,-20,20),(-80,0,40),(0,10,50),(80,-10,30)) t(x,lo,hi) \
             VISUALISE x AS x, lo AS ymin, hi AS ymax DRAW ribbon \
             SETTING fill => 'steelblue' PROJECT x, y TO robinson",
        ));
    }

    #[cfg(feature = "spatial")]
    #[test]
    fn renders_densified_rule() {
        // A rule spans the clip bbox, so its meridians curve with the projection.
        assert_png_or_skip(render(
            "INSTALL spatial; LOAD spatial; \
             SELECT * FROM (VALUES (-100),(0),(100)) t(x) VISUALISE x AS x DRAW rule \
             SETTING stroke => 'darkgreen', linetype => 'dashed' PROJECT x, y TO robinson",
        ));
    }

    #[cfg(feature = "spatial")]
    #[test]
    fn renders_densified_tile() {
        assert_png_or_skip(render(
            "INSTALL spatial; LOAD spatial; \
             SELECT * FROM (VALUES (-120,-30,5),(-40,20,9),(40,-10,3)) t(x,y,v) \
             VISUALISE x AS x, y AS y, v AS fill DRAW tile \
             SETTING width => 40, height => 30 PROJECT x, y TO robinson",
        ));
    }

    #[cfg(feature = "spatial")]
    #[test]
    fn renders_map_over_spatial_base() {
        // A non-spatial layer over a spatial base map: both must frame to ggsql's
        // bbox so the segments land on the boundary, not on their own extent.
        assert_png_or_skip(render(
            "WITH routes AS (SELECT * FROM (VALUES (-74,40,2,48,'a'),(151,-34,18,-34,'b')) \
             t(x1,y1,x2,y2,route)) \
             VISUALISE \
             DRAW spatial MAPPING * FROM ggsql:world \
             DRAW segment MAPPING x1 AS x, y1 AS y, x2 AS xend, y2 AS yend, route AS stroke \
             FROM routes \
             PROJECT x, y TO robinson",
        ));
    }

    /// A 6-row fixture whose `g` is categorical and `v` numeric.
    const FACET_DATA: &str = "SELECT g, v, y FROM (VALUES \
         ('a',5,1),('a',7,2),('b',15,3),('b',18,1),('c',25,2),('c',28,3)) t(g,v,y)";

    #[test]
    fn axis_titles_are_one_per_dimension() {
        // Axis titles are outer chrome: exactly one per dimension for the whole
        // figure, however many panels there are and whether or not a dimension
        // is free (a free dimension draws its rail on every panel, but still
        // gets a single centred title).
        let expected = vec![
            (AxisSide::Bottom, "v".to_string()),
            (AxisSide::Left, "y".to_string()),
        ];
        for facet in [
            "",
            "FACET g",
            "FACET g SETTING free => ('x', 'y')",
            "FACET g BY y",
        ] {
            let query = format!("{FACET_DATA} VISUALISE v AS x, y AS y DRAW point {facet}");
            assert_eq!(axis_titles(&query), expected, "{facet}");
        }
    }

    #[test]
    fn axis_titles_follow_labels() {
        assert_eq!(
            axis_titles(&format!(
                "{FACET_DATA} VISUALISE v AS x, y AS y DRAW point FACET g \
                 LABEL x => 'Value', y => 'Count'"
            )),
            vec![
                (AxisSide::Bottom, "Value".to_string()),
                (AxisSide::Left, "Count".to_string()),
            ]
        );
    }

    #[test]
    fn axis_titles_skip_untitled_axes() {
        // A polar coord has no Cartesian rails to title, and a synthetic dummy
        // position scale has no axis at all.
        assert!(axis_titles(&format!(
            "{FACET_DATA} VISUALISE v AS y, g AS fill DRAW bar PROJECT x, y TO polar"
        ))
        .is_empty());
        assert_eq!(
            axis_titles(&format!("{FACET_DATA} VISUALISE v AS y DRAW bar")),
            vec![(AxisSide::Left, "v".to_string())]
        );
    }

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
    fn facet_strips_null_and_empty_are_separate_panels() {
        // `column_to_strings` renders both a NULL and an empty category as "",
        // so they need the null flag to stay apart — the Vega-Lite writer gives
        // them a panel each.
        let data = "SELECT g, v FROM (VALUES ('', 1), (NULL, 2), ('a', 3)) t(g, v)";
        assert_eq!(
            top_strips(&format!(
                "{data} VISUALISE v AS x, v AS y DRAW point FACET g"
            )),
            vec!["", "a", "null"]
        );
    }

    #[test]
    fn facet_over_empty_data_is_one_panel() {
        // No levels to lay out: both layouts collapse to the unfaceted single
        // panel rather than building a grid of zero cells.
        let empty = "SELECT g, h, v FROM (VALUES ('a','b',1)) t(g,h,v) WHERE false";
        for query in [
            format!("{empty} VISUALISE v AS x, v AS y DRAW point FACET g"),
            format!("{empty} VISUALISE v AS x, v AS y DRAW point FACET g BY h"),
        ] {
            assert_eq!(strips(&query), vec![(None, None)], "for: {query}");
        }
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
    fn renders_binned_size_legend() {
        // A binned *keyed* legend: one key per bin, sized at the bin's midpoint,
        // with ggsql's edge labels on the rail between keys.
        assert_png_or_skip(render(
            "VISUALISE bill_len AS x, bill_dep AS y, body_mass AS size \
             FROM ggsql:penguins DRAW point \
             SCALE BINNED size SETTING breaks => (2500, 3500, 4500, 5500, 6500)",
        ));
    }

    #[test]
    fn renders_binned_color_legend() {
        // The same ladder driving color: a stepped colorbar, one block per bin.
        assert_png_or_skip(render(
            "VISUALISE bill_len AS x, bill_dep AS y, body_mass AS color \
             FROM ggsql:penguins DRAW point \
             SCALE BINNED color SETTING breaks => (2500, 3500, 4500, 5500, 6500)",
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
    fn renders_boxplot_hinge() {
        // `hinge` caps the whiskers with a fixed-size (pt) tick at each fence.
        assert_png_or_skip(render(
            "SELECT g, v FROM (VALUES ('a',1),('a',2),('a',3),('a',9),\
             ('b',2),('b',3),('b',4),('b',5)) t(g,v) \
             VISUALISE g AS x, v AS y DRAW boxplot SETTING hinge => 20",
        ));
    }

    #[test]
    fn renders_boxplot_side() {
        // `side` halves the box, median and caps onto one side of the band,
        // leaving whiskers and outliers on the centreline.
        assert_png_or_skip(render(
            "SELECT g, v FROM (VALUES ('a',1),('a',2),('a',3),('a',9),\
             ('b',2),('b',3),('b',4),('b',5)) t(g,v) \
             VISUALISE g AS x, v AS y DRAW boxplot \
             SETTING side => 'right', hinge => 20",
        ));
    }

    #[test]
    fn renders_transposed_boxplot() {
        // A horizontal boxplot: ggsql flips the position columns, so the
        // categories are on `pos2` and the summary values in the `pos1` family.
        assert_png_or_skip(render(
            "SELECT g, v FROM (VALUES ('a',1),('a',2),('a',3),('a',9),\
             ('b',2),('b',3),('b',4),('b',5)) t(g,v) \
             VISUALISE v AS x, g AS y DRAW boxplot SETTING hinge => 15",
        ));
    }

    #[test]
    fn renders_half_violin_with_half_boxplot() {
        // Opposite `side` values pair the two composites on one band, the
        // documented raincloud-style layout (transposed, so top/bottom).
        assert_png_or_skip(render(
            "SELECT g, v FROM (VALUES ('a',1),('a',2),('a',2),('a',3),('a',4),\
             ('b',2),('b',3),('b',3),('b',4),('b',6)) t(g,v) \
             VISUALISE v AS x, g AS y \
             DRAW violin SETTING side => 'top' \
             DRAW boxplot SETTING side => 'bottom', width => 0.3",
        ));
    }

    #[test]
    fn renders_jittered_points() {
        // `position => 'jitter'` spreads the points across their category band;
        // `side` (folded into the offsets by ggsql) keeps them on one half.
        assert_png_or_skip(render(
            "VISUALISE species AS x, bill_len AS y FROM ggsql:penguins DRAW point \
             SETTING position => 'jitter'",
        ));
        assert_png_or_skip(render(
            "VISUALISE species AS x, bill_len AS y FROM ggsql:penguins DRAW point \
             SETTING position => 'jitter', side => 'right'",
        ));
    }

    #[test]
    fn renders_dodged_points() {
        // Dodge on a geom that doesn't derive its own band edges: the offsets
        // reach the point's band channel.
        assert_png_or_skip(render(
            "SELECT x, g, v FROM (VALUES ('a','p',3),('a','q',5),('b','p',2),('b','q',4)) \
             t(x,g,v) \
             VISUALISE x AS x, v AS y, g AS color DRAW point SETTING position => 'dodge'",
        ));
    }

    #[test]
    fn renders_dodged_range_with_hinges() {
        // A dodged interval and its end caps share one offset, so they stay
        // aligned in the dodge slot.
        assert_png_or_skip(render(
            "SELECT g, s, lo, hi FROM (VALUES ('a','p',1,5),('a','q',2,6),('b','p',2,7)) \
             t(g,s,lo,hi) \
             VISUALISE g AS x, lo AS ymin, hi AS ymax, s AS stroke DRAW range \
             SETTING position => 'dodge'",
        ));
    }

    #[test]
    fn renders_jitter_with_half_boxplot() {
        // The documented raincloud layout: a one-sided jitter above the
        // centreline, a half-boxplot below it.
        assert_png_or_skip(render(
            "VISUALISE bill_len AS x, species AS y FROM ggsql:penguins \
             DRAW point SETTING position => 'jitter', side => 'top', width => 0.4 \
             DRAW boxplot SETTING side => 'bottom', width => 0.4",
        ));
    }

    #[test]
    fn renders_range_hinges() {
        // A range carries 10pt end caps by default; `hinge => null` drops them.
        assert_png_or_skip(render(
            "SELECT g, lo, hi FROM (VALUES ('a',1,5),('b',2,7)) t(g,lo,hi) \
             VISUALISE g AS x, lo AS ymin, hi AS ymax DRAW range",
        ));
        assert_png_or_skip(render(
            "SELECT g, lo, hi FROM (VALUES ('a',1,5),('b',2,7)) t(g,lo,hi) \
             VISUALISE g AS y, lo AS xmin, hi AS xmax DRAW range \
             SETTING hinge => 40",
        ));
        assert_png_or_skip(render(
            "SELECT g, lo, hi FROM (VALUES ('a',1,5),('b',2,7)) t(g,lo,hi) \
             VISUALISE g AS x, lo AS ymin, hi AS ymax DRAW range \
             SETTING hinge => null",
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
    fn renders_dodged_violin() {
        // Two fill groups per category: each must be its own contour (keyed on the
        // category *and* the partition columns), not one merged blob.
        assert_png_or_skip(render(
            "SELECT g, f, v FROM (VALUES ('a','x',1),('a','x',2),('a','x',3),\
             ('a','y',5),('a','y',6),('a','y',7),\
             ('b','x',2),('b','x',3),('b','x',4),('b','y',6),('b','y',7),('b','y',8)) t(g,f,v) \
             VISUALISE g AS x, v AS y, f AS fill DRAW violin",
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
    fn map_range_pads_like_vegalite() {
        // 10% of the span, split evenly around the centre — the same framing
        // Vega-Lite's projection fit produces from `span * 1.1`.
        let r = compose::map_range(0.0, 10.0);
        assert_eq!(*r.start(), -0.5);
        assert_eq!(*r.end(), 10.5);
        assert_eq!((r.end() - r.start()) / 10.0, 1.1);
    }

    #[test]
    fn map_range_widens_a_degenerate_extent() {
        // A single point has no span to pad, so it is widened to a mappable one.
        let r = compose::map_range(3.0, 3.0);
        assert_eq!(*r.start(), 2.5);
        assert_eq!(*r.end(), 3.5);
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
        let writer = PngWriter::new(320, 240, 96.0);
        assert!(matches!(
            writer.validate(spec.plot()),
            Err(GgsqlError::WriterError(_))
        ));
    }
}
