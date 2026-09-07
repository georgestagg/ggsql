//! Display data formatting for Jupyter output
//!
//! This module formats execution results as Jupyter display_data messages
//! with appropriate MIME types for rich rendering.

use crate::executor::ExecutionResult;
use crate::message::MessageHeader;
use crate::plot::{self, Canvas, Delivery, PlotBackend, RenderRequest};
use anyhow::Result;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use clap::ValueEnum;
use ggsql::reader::Spec;
use ggsql::writer::{VegaLiteWriter, Writer};
use ggsql::DataFrame;
use serde_json::{json, Value};

/// What the frontend declared itself to be, via `--session-mode`.
///
/// Only a frontend that knows which kind of session it is launching passes
/// this — in practice the ggsql extension, which knows because it is the one
/// creating the session. Everything else leaves it unset and is classified by
/// the heuristic below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SessionMode {
    /// A Positron console session: plots belong in the Plots pane.
    Console,
    /// A Positron notebook session: plots belong in the cell.
    Notebook,
    /// A Positron background session, attached to no UI at all. Output has
    /// nowhere special to go, so it is treated exactly like a session Positron
    /// is not driving.
    Background,
}

/// Where a plot this kernel produces is meant to end up.
///
/// The distinction is not cosmetic: Positron routes a plot comm to the Plots
/// pane whatever kind of session opened it, so a notebook that used the comm
/// would put its picture in the pane and leave the cell empty. Console and
/// notebook therefore need different output paths, and this is what tells them
/// apart.
///
/// - **`PositronConsole`**: output lands in the Plots pane. The Vega-Lite
///   container upgrades to `100vh` inside `.positron-output-container`, so
///   Vega-Lite's own container observer tracks pane resizes.
/// - **`PositronNotebook`**: inline code-chunk output in an editor view.
///   Rendered into a plain 400px container that watches layout only when the
///   first measurement collapsed, because Positron animates the slot during
///   its reveal transition.
/// - **`Standalone`**: anything else — Jupyter, Quarto, nbconvert, and a
///   Positron *background* session, which is attached to no UI. The HTML
///   embeds in a static document. An outer/inner div wrapper with a 450px
///   design width applies a uniform CSS-transform scale when the viewport is
///   narrower, so the plot shrinks in proportion instead of squashing.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    PositronConsole,
    PositronNotebook,
    #[default]
    Standalone,
}

impl SessionKind {
    /// Classify a session, preferring what the frontend declared.
    ///
    /// `mode` comes from `--session-mode` and is authoritative: a frontend that
    /// passes it knows what it launched. The session-id heuristic is the
    /// fallback for external Jupyter and Quarto, which pass nothing, and for
    /// older versions of the extension that predate the flag.
    pub fn resolve(session: &str, mode: Option<SessionMode>) -> Self {
        match mode {
            Some(SessionMode::Console) => Self::PositronConsole,
            Some(SessionMode::Notebook) => Self::PositronNotebook,
            // A background session has no pane and no cell, so there is no
            // Positron-specific slot to render into.
            Some(SessionMode::Background) => Self::Standalone,
            // Positron's supervisor tags every session it manages with a
            // `ggsql-` prefix; standalone Jupyter/Quarto uses UUIDs without
            // one. A session that is not Positron's is standalone whatever
            // else its id says.
            None if !session.starts_with("ggsql-") => Self::Standalone,
            None if session.contains("notebook") => Self::PositronNotebook,
            None => Self::PositronConsole,
        }
    }

    /// Whether the frontend is Positron, in either of its two shapes.
    pub fn is_positron(self) -> bool {
        matches!(self, Self::PositronConsole | Self::PositronNotebook)
    }

    /// Whether output belongs in a notebook cell rather than a pane.
    pub fn is_notebook(self) -> bool {
        matches!(self, Self::PositronNotebook)
    }
}

/// Frontend-supplied hints about the output rendering slot.
#[derive(Default, Debug, Clone, Copy)]
pub struct RenderHints {
    pub kind: SessionKind,
    /// Width of the output slot in CSS pixels, when the frontend says.
    pub output_width_px: Option<u32>,
    /// Device pixel ratio of the display, when the frontend says.
    pub pixel_ratio: Option<f64>,
}

impl RenderHints {
    pub fn from_request(
        header: &MessageHeader,
        content: &Value,
        mode: Option<SessionMode>,
    ) -> Self {
        // Positron puts both of these on the execute request for a notebook
        // or inline cell — see `runtimeNotebookKernel.ts`, which measures the
        // output slot and reads the window's `devicePixelRatio`.
        let positron = content.get("positron");
        let output_width_px = positron
            .and_then(|p| p.get("output_width_px"))
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());
        let pixel_ratio = positron
            .and_then(|p| p.get("output_pixel_ratio"))
            .and_then(|v| v.as_f64())
            .filter(|v| *v > 0.0);
        Self {
            kind: SessionKind::resolve(header.session.as_str(), mode),
            output_width_px,
            pixel_ratio,
        }
    }

    /// The canvas a static render should use.
    ///
    /// **An execute request reports a width but no height**, because the slot
    /// it describes is a cell output — as wide as the cell and as tall as
    /// whatever it is given. So the height is ours to pick, and the golden
    /// ratio is close to ggplot2's own default figure and a better answer than
    /// a square.
    ///
    /// This is not how the Plots pane is sized. A pane reports a **full**
    /// size, through the plot comm's `render` request and through the ui
    /// comm's `did_change_plots_render_settings` — both carrying a required
    /// `{width, height}` plus a pixel ratio — which is why a plot in the pane
    /// fits it exactly. Neither reaches this function: the pane's size arrives
    /// per render, not per execution.
    pub fn canvas(&self) -> Canvas {
        let ratio = self.pixel_ratio.unwrap_or(1.0);
        match self.output_width_px {
            Some(width) if width > 0 => {
                let width = f64::from(width);
                Canvas::from_logical(width, width / 1.618, ratio)
            }
            _ => {
                let default = Canvas::default();
                Canvas::from_logical(f64::from(default.width), f64::from(default.height), ratio)
            }
        }
    }
}

/// Format execution result as Jupyter display_data content
///
/// Returns `Some(Value)` for results that should be displayed, or `None` for
/// empty results (e.g., DDL statements like CREATE TABLE that have no columns).
///
/// Note: A SELECT that returns 0 rows but has columns will still display
/// an empty table with headers. Only truly empty DataFrames (0 columns)
/// from DDL statements return `None`.
///
/// The returned JSON matches the Jupyter display_data message format:
/// ```json
/// {
///   "data": { "mime/type": content, ... },
///   "metadata": { ... },
///   "transient": { ... }
/// }
/// ```
/// What the kernel should do with a formatted result.
pub enum Formatted {
    /// Emit this as the cell's `execute_result`.
    Bundle(Value),
    /// Open a `positron.plot` comm for this plot and emit **no**
    /// `execute_result` — the comm alone creates the pane entry.
    PlotComm(Box<Spec>),
    /// Nothing to show, as for a DDL statement.
    Nothing,
}

pub fn format_display_data(
    result: ExecutionResult,
    hints: &RenderHints,
    backend: &PlotBackend,
) -> Result<Formatted> {
    match result {
        // Rendering happens here rather than at execution time — which is the
        // point: the format is chosen where the destination is known, and so
        // it can also fail here.
        ExecutionResult::Visualization(spec) => {
            match plot::choose(hints.kind, backend.raster(), hints.canvas()) {
                Delivery::Comm => Ok(Formatted::PlotComm(spec)),
                Delivery::VegaLite => Ok(Formatted::Bundle(format_vegalite(&spec, hints)?)),
                Delivery::Static(request) => {
                    Ok(Formatted::Bundle(format_static(spec, request, backend)?))
                }
            }
        }
        ExecutionResult::DataFrame(df) => {
            // DDL statements return DataFrames with 0 columns - don't display anything
            if df.width() == 0 {
                Ok(Formatted::Nothing)
            } else {
                Ok(Formatted::Bundle(format_dataframe(df)))
            }
        }
        ExecutionResult::ConnectionChanged { display_name, .. } => {
            Ok(Formatted::Bundle(format_connection_changed(&display_name)))
        }
    }
}

/// Format a connection-changed message
fn format_connection_changed(display_name: &str) -> Value {
    let text = format!("Connected to {}", display_name);
    json!({
        "data": {
            "text/plain": text
        },
        "metadata": {},
        "transient": {}
    })
}

/// Render a plot to an image and wrap it as a static display bundle.
///
/// **No `output_location`.** That key routes an output to Positron's plot
/// widget, which would show the picture in the Plots pane *as well as* putting
/// it in the cell — one plot arriving twice. A static bundle belongs wherever
/// the cell's output goes and nowhere else.
///
/// `metadata[mime].width/height` carries the size the frontend should display
/// at, in CSS pixels. Without it a 2x render appears at twice its intended
/// size; JupyterLab and nbconvert both honour it.
fn format_static(spec: Box<Spec>, request: RenderRequest, backend: &PlotBackend) -> Result<Value> {
    let metadata = spec.metadata();
    let summary = format!(
        "<ggsql plot: {} layer{}, {} row{}>",
        metadata.layer_count,
        if metadata.layer_count == 1 { "" } else { "s" },
        metadata.rows,
        if metadata.rows == 1 { "" } else { "s" },
    );

    let bytes = backend.render_once(spec, request)?;
    let mime = request.format.mime();
    // SVG is text and travels as itself; everything else is bytes and travels
    // base64-encoded, which is what a display bundle expects for binary data.
    let payload = if request.format.is_text() {
        String::from_utf8(bytes)?
    } else {
        BASE64.encode(&bytes)
    };

    let (css_width, css_height) = request.canvas.css_size();
    Ok(json!({
        "data": {
            mime: payload,
            "text/plain": summary,
        },
        "metadata": {
            mime: { "width": css_width, "height": css_height }
        },
        "transient": {},
    }))
}

/// Render a resolved plot as Vega-Lite and wrap it as display_data.
fn format_vegalite(spec: &Spec, hints: &RenderHints) -> Result<Value> {
    let json = VegaLiteWriter::new().render(spec)?;
    let html = vegalite_html(&json, hints);
    Ok(json!({
        "data": {
            "text/html": html,
            "text/plain": "Vega-Lite visualization".to_string()
        },
        "metadata": {},
        "transient": {},
        "output_location": "plot"
    }))
}

/// Generate the HTML wrapper that embeds a Vega-Lite spec via vega-embed.
pub fn vegalite_html(spec: &str, hints: &RenderHints) -> String {
    let spec_value: Value = serde_json::from_str(spec).unwrap_or_else(|e| {
        tracing::error!("Failed to parse Vega-Lite JSON: {}", e);
        json!({"error": "Invalid Vega-Lite JSON"})
    });

    let spec_json = serde_json::to_string(&spec_value).unwrap_or_else(|_| "{}".to_string());

    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let vis_id = format!("vis-{}", timestamp);

    if hints.kind.is_positron() {
        positron_vegalite_html(&spec_json, &vis_id, hints.kind.is_notebook())
    } else {
        standalone_vegalite_html(&spec_json, &vis_id)
    }
}

/// Positron template: plain 400px container. Console sessions additionally
/// upgrade the container to `100vh` when it lives inside
/// `.positron-output-container`, letting Vega-Lite's own container observer
/// keep the Plots pane responsive. Notebook sessions skip that override and
/// keep a stable 400px box.
///
/// Container sizing compiles to `width`/`height` signals that re-read
/// `containerSize()` only on `window:resize`, and `isFinite(0)` holds, so a plot
/// measured before Positron lays the slot out stays zero-sized with no error.
/// `recoverIfCollapsed` fixes that, and two details are load-bearing: it gates
/// on `view.width()` rather than the container's `clientWidth`, which can
/// already be non-zero while the view is not, and it dispatches `resize` rather
/// than calling `view.resize()`, which re-lays out from the same zero. It
/// watches only a collapsed plot and stops once the view has a size, so it
/// cannot re-lay out on every frame of Positron's reveal transition.
fn positron_vegalite_html(spec_json: &str, vis_id: &str, is_notebook: bool) -> String {
    let pane_override_js = if is_notebook {
        ""
    } else {
        "var container = document.getElementById(visId);\n\
         if (container && container.closest('.positron-output-container')) {\n\
         container.style.height = '100vh';\n\
         }\n"
    };

    format!(
        r#"<div id="{vis_id}" style="width: 100%; height: 400px;"></div>
<script type="text/javascript">
(function() {{
var spec = {spec_json};
var visId = '{vis_id}';
var options = {{"actions": true, "renderer": "svg"}};
function recoverIfCollapsed(result) {{
var el = document.getElementById(visId);
if (!el || !result || !result.view) {{ return; }}
if (typeof ResizeObserver === 'undefined') {{ return; }}
if (result.view.width() > 0 && result.view.height() > 0) {{ return; }}
var lastWidth = -1;
var ro = new ResizeObserver(function() {{
if (result.view.width() > 0 && result.view.height() > 0) {{ ro.disconnect(); return; }}
var w = el.clientWidth;
if (w === 0 || w === lastWidth) {{ return; }}
lastWidth = w;
window.dispatchEvent(new Event('resize'));
}});
ro.observe(el);
}}
{pane_override_js}
if (typeof window.requirejs !== 'undefined') {{
window.requirejs.config({{
paths: {{
'dom-ready': 'https://cdn.jsdelivr.net/npm/domready@1/ready.min',
'vega': 'https://cdn.jsdelivr.net/npm/vega@6/build/vega.min',
'vega-lite': 'https://cdn.jsdelivr.net/npm/vega-lite@6.4.1/build/vega-lite.min',
'vega-embed': 'https://cdn.jsdelivr.net/npm/vega-embed@7/build/vega-embed.min'
}}
}});
function docReady(fn) {{
if (document.readyState === 'complete') fn();
else window.addEventListener("load", function() {{ fn(); }});
}}
docReady(function() {{
window.requirejs(["dom-ready", "vega", "vega-embed"], function(domReady, vega, vegaEmbed) {{
domReady(function () {{
vegaEmbed('#' + visId, spec, options).then(recoverIfCollapsed).catch(console.error);
}});
}});
}});
}} else {{
function loadScript(src) {{
return new Promise(function(resolve, reject) {{
var script = document.createElement('script');
script.src = src;
script.onload = resolve;
script.onerror = reject;
document.head.appendChild(script);
}});
}}
Promise.all([
loadScript('https://cdn.jsdelivr.net/npm/vega@6'),
loadScript('https://cdn.jsdelivr.net/npm/vega-lite@6.4.1'),
loadScript('https://cdn.jsdelivr.net/npm/vega-embed@7')
])
.then(function() {{ return vegaEmbed('#' + visId, spec, options); }})
.then(recoverIfCollapsed)
.catch(function(err) {{
console.error('Failed to load Vega libraries:', err);
}});
}}
}})();
</script>
"#,
        vis_id = vis_id,
        spec_json = spec_json,
        pane_override_js = pane_override_js
    )
}

/// Standalone template: outer/inner div wrapper driving a uniform
/// scale-to-fit. The inner div holds a 450px design width; when the outer
/// container measures narrower, a CSS transform scales the inner block
/// proportionally and the outer height follows the scaled content. A
/// `ResizeObserver` on the outer div keeps the transform current as the
/// document viewport resizes.
fn standalone_vegalite_html(spec_json: &str, vis_id: &str) -> String {
    format!(
        r#"<div id="{vis_id}-outer" style="width: 100%; overflow: hidden;">
<div id="{vis_id}" style="width: 100%; min-width: 450px; height: 400px;"></div>
</div>
<script type="text/javascript">
(function() {{
var spec = {spec_json};
var visId = '{vis_id}';
var minWidth = 450;
var inner = document.getElementById(visId);
var outer = document.getElementById(visId + '-outer');
var options = {{"actions": true, "renderer": "svg"}};
function scaleToFit(o, i) {{
var available = o.clientWidth;
if (available < minWidth) {{
var scale = available / minWidth;
i.style.transform = 'scale(' + scale + ')';
i.style.transformOrigin = 'top left';
o.style.height = (i.scrollHeight * scale) + 'px';
}} else {{
i.style.transform = '';
o.style.height = '';
}}
}}
function onRendered() {{
scaleToFit(outer, inner);
var ro = new ResizeObserver(function() {{ scaleToFit(outer, inner); }});
ro.observe(outer);
}}
if (typeof window.requirejs !== 'undefined') {{
window.requirejs.config({{
paths: {{
'dom-ready': 'https://cdn.jsdelivr.net/npm/domready@1/ready.min',
'vega': 'https://cdn.jsdelivr.net/npm/vega@6/build/vega.min',
'vega-lite': 'https://cdn.jsdelivr.net/npm/vega-lite@6.4.1/build/vega-lite.min',
'vega-embed': 'https://cdn.jsdelivr.net/npm/vega-embed@7/build/vega-embed.min'
}}
}});
function docReady(fn) {{
if (document.readyState === 'complete') fn();
else window.addEventListener("load", function() {{ fn(); }});
}}
docReady(function() {{
window.requirejs(["dom-ready", "vega", "vega-embed"], function(domReady, vega, vegaEmbed) {{
domReady(function () {{
vegaEmbed('#' + visId, spec, options).then(onRendered).catch(console.error);
}});
}});
}});
}} else {{
function loadScript(src) {{
return new Promise(function(resolve, reject) {{
var script = document.createElement('script');
script.src = src;
script.onload = resolve;
script.onerror = reject;
document.head.appendChild(script);
}});
}}
Promise.all([
loadScript('https://cdn.jsdelivr.net/npm/vega@6'),
loadScript('https://cdn.jsdelivr.net/npm/vega-lite@6.4.1'),
loadScript('https://cdn.jsdelivr.net/npm/vega-embed@7')
])
.then(function() {{ return vegaEmbed('#' + visId, spec, options); }})
.then(onRendered)
.catch(function(err) {{
console.error('Failed to load Vega libraries:', err);
}});
}}
}})();
</script>
"#,
        vis_id = vis_id,
        spec_json = spec_json
    )
}

/// Format DataFrame as HTML table
fn format_dataframe(df: DataFrame) -> Value {
    let html = dataframe_to_html(&df);
    let text = dataframe_to_text(&df);

    json!({
        "data": {
            "text/html": html,
            "text/plain": text
        },
        "metadata": {},
        "transient": {}
    })
}

/// Convert DataFrame to HTML table
fn dataframe_to_html(df: &DataFrame) -> String {
    use ggsql::array_util::value_to_string;

    let mut html = String::from("<table border=\"1\" class=\"dataframe\">\n<thead><tr>");

    // Header row
    for col in df.get_column_names() {
        html.push_str(&format!("<th>{}</th>", escape_html(&col)));
    }
    html.push_str("</tr></thead>\n<tbody>\n");

    // Data rows (limit to first 100 for performance)
    let row_limit = df.height().min(100);
    for i in 0..row_limit {
        html.push_str("<tr>");
        for col in df.get_columns() {
            let value = value_to_string(col, i);
            html.push_str(&format!("<td>{}</td>", escape_html(&value)));
        }
        html.push_str("</tr>\n");
    }

    if df.height() > row_limit {
        html.push_str(&format!(
            "<tr><td colspan='{}' style='text-align: center;'>... {} more rows</td></tr>\n",
            df.width(),
            df.height() - row_limit
        ));
    }

    html.push_str("</tbody>\n</table>");
    html
}

/// Convert DataFrame to plain-text summary (shape + column names + first rows).
fn dataframe_to_text(df: &ggsql::DataFrame) -> String {
    use ggsql::array_util::value_to_string;

    let mut s = format!("shape: ({}, {})\n", df.height(), df.width());
    let names = df.get_column_names();
    s.push_str(&names.join("\t"));
    s.push('\n');
    let row_limit = df.height().min(10);
    for i in 0..row_limit {
        let row: Vec<String> = df
            .get_columns()
            .iter()
            .map(|c| value_to_string(c, i))
            .collect();
        s.push_str(&row.join("\t"));
        s.push('\n');
    }
    s
}

/// Escape HTML special characters
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resolved plot, from a real query — the display layer renders it now,
    /// so a hand-written Vega-Lite string is no longer a stand-in for one.
    fn a_spec() -> Spec {
        use ggsql::reader::{DuckDBReader, Reader};
        DuckDBReader::from_connection_string("duckdb://memory")
            .unwrap()
            .execute("SELECT 1 AS x, 2 AS y VISUALISE x, y DRAW point")
            .unwrap()
    }

    fn render(hints: &RenderHints) -> Value {
        match format_display_data(
            ExecutionResult::Visualization(Box::new(a_spec())),
            hints,
            &backend(),
        )
        .expect("rendering should succeed")
        {
            Formatted::Bundle(bundle) => bundle,
            Formatted::PlotComm(_) => panic!("expected a bundle, not a comm"),
            Formatted::Nothing => panic!("expected output"),
        }
    }

    #[test]
    fn test_console_without_an_adapter_falls_back_to_a_picture() {
        // This test's backend has no GPU, so the console cannot open a comm
        // Positron would ask `png` of — it gets a static SVG instead.
        let display = render(&positron_console());
        assert!(display["data"]["image/svg+xml"].is_string());
        assert!(display.get("output_location").is_none());
    }

    #[test]
    fn test_the_vegalite_escape_hatch_still_works() {
        // A temporary way back to the previous behaviour, so a problem with
        // the comm can be confirmed against it without a rebuild. Goes away
        // with the Vega-Lite plot path.
        let spec = a_spec();
        let bundle = format_vegalite(&spec, &positron_console()).unwrap();
        assert_eq!(bundle["output_location"], "plot");
        let html = bundle["data"]["text/html"].as_str().unwrap();
        assert!(html.contains("vega-embed"), "{html:.200}");
    }

    #[test]
    fn test_a_notebook_gets_a_static_image_in_its_cell() {
        let display = render(&positron_notebook());

        // SVG, because this backend has no GPU — and the fallback is the point:
        // a plot still arrives.
        let svg = display["data"]["image/svg+xml"].as_str().unwrap();
        assert!(svg.starts_with("<svg"), "{svg:.80}");
        assert!(display["data"]["text/html"].is_null(), "no CDN payload");

        // A plain-text summary for a frontend that renders neither.
        let text = display["data"]["text/plain"].as_str().unwrap();
        assert!(text.contains("layer"), "{text}");

        // **No `output_location`.** That key would route this to the Plots
        // pane as well as the cell, and the plot would arrive twice.
        assert!(
            display.get("output_location").is_none(),
            "a static bundle must not claim a plot slot"
        );
    }

    #[test]
    fn test_a_retina_notebook_renders_at_its_own_ratio() {
        // Positron puts `output_pixel_ratio` on the execute request next to
        // `output_width_px`, so a retina cell gets a sharp plot rather than an
        // upscaled one.
        let hints = RenderHints::from_request(
            &header("ggsql-notebook-abc"),
            &json!({"positron": {"output_width_px": 600, "output_pixel_ratio": 2.0}}),
            None,
        );
        assert_eq!(hints.pixel_ratio, Some(2.0));

        let canvas = hints.canvas();
        // Twice the device pixels, at twice the dpi...
        assert_eq!(canvas.width, 1200);
        assert_eq!(canvas.dpi, 192.0);
        // ...displayed at the size the cell actually is.
        assert_eq!(canvas.css_size(), (600, 371));
    }

    #[test]
    fn test_a_missing_ratio_falls_back_to_one() {
        // Plain Jupyter reports nothing, and an older Positron reports only a
        // width. Rendering at 1x is soft on a retina display; assuming 2x
        // would waste four times the pixels on every plot everywhere else.
        let hints = RenderHints::from_request(
            &header("abcd-1234"),
            &json!({"positron": {"output_width_px": 600}}),
            None,
        );
        assert_eq!(hints.pixel_ratio, None);
        assert_eq!(hints.canvas().dpi, 96.0);
        assert_eq!(hints.canvas().width, 600);
    }

    #[test]
    fn test_a_nonsense_ratio_is_ignored_rather_than_used() {
        let hints = RenderHints::from_request(
            &header("ggsql-notebook-abc"),
            &json!({"positron": {"output_width_px": 600, "output_pixel_ratio": 0}}),
            None,
        );
        assert_eq!(hints.pixel_ratio, None);
    }

    #[test]
    fn test_a_static_bundle_declares_the_size_to_show_it_at() {
        // 589 CSS px wide, as the notebook hints report.
        let display = render(&positron_notebook());
        let metadata = &display["metadata"]["image/svg+xml"];
        assert_eq!(metadata["width"], 589);
        assert_eq!(metadata["height"], 364);
    }

    #[test]
    fn test_standalone_gets_a_static_image_and_needs_no_network() {
        // The headline change: a plain Jupyter or Quarto render no longer
        // reaches for a CDN, so a plot works offline and in CI.
        let display = render(&RenderHints::default());
        assert!(display["data"]["image/svg+xml"].is_string());
        let bundle = serde_json::to_string(&display).unwrap();
        assert!(
            !bundle.contains("jsdelivr") && !bundle.contains("vega-embed"),
            "a static bundle should carry no CDN reference"
        );
    }

    #[test]
    fn test_empty_dataframe_returns_none() {
        // DDL statements return DataFrames with 0 columns
        let df = DataFrame::empty();
        let result = ExecutionResult::DataFrame(df);
        let display = format_display_data(result, &RenderHints::default(), &backend()).unwrap();

        assert!(
            matches!(display, Formatted::Nothing),
            "Empty DataFrame (0 columns) should produce nothing"
        );
    }

    #[test]
    fn test_empty_rows_dataframe_returns_some() {
        use arrow::array::{ArrayRef, Int32Array};
        use std::sync::Arc;

        // SELECT with 0 rows but columns should still display
        let empty: ArrayRef = Arc::new(Int32Array::from(Vec::<i32>::new()));
        let df = DataFrame::new(vec![("x", empty)]).unwrap();
        let result = ExecutionResult::DataFrame(df);
        let display = format_display_data(result, &RenderHints::default(), &backend()).unwrap();

        assert!(
            matches!(display, Formatted::Bundle(_)),
            "DataFrame with columns but 0 rows should produce a bundle"
        );
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(
            escape_html("<script>alert('xss')</script>"),
            "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"
        );
    }

    /// A render backend with no GPU, so tests are fast and identical
    /// everywhere. The SVG path it leaves is the one that always works.
    fn backend() -> PlotBackend {
        PlotBackend::without_raster()
    }

    fn positron_console() -> RenderHints {
        RenderHints {
            kind: SessionKind::PositronConsole,
            output_width_px: None,
            pixel_ratio: None,
        }
    }

    fn positron_notebook() -> RenderHints {
        RenderHints {
            kind: SessionKind::PositronNotebook,
            output_width_px: Some(589),
            pixel_ratio: None,
        }
    }

    #[test]
    fn test_positron_html_has_no_observer_feedback_loop() {
        // Positron animates the output slot during reveal, so the templates
        // must not watch layout once the plot has drawn. The only observer is
        // the collapsed-render recovery, and it disconnects as soon as the
        // view has a size.
        for hints in [positron_console(), positron_notebook()] {
            let html = vegalite_html(r#"{"mark": "point"}"#, &hints);
            assert!(
                !html.contains("scaleToFit"),
                "Positron HTML must not include scaleToFit (hints={:?})",
                hints
            );
            assert_eq!(
                html.matches("new ResizeObserver").count(),
                1,
                "the collapsed-render recovery is the only observer (hints={:?})",
                hints
            );
            assert!(
                html.contains("ro.disconnect();"),
                "the observer must disconnect once the view has a size (hints={:?})",
                hints
            );
        }
    }

    #[test]
    fn test_positron_recovery_gates_on_rendered_view_size() {
        // The container can report a width while the view is still laid out at
        // the zero it captured earlier, so gating on `clientWidth` here would
        // skip a plot that did collapse.
        for hints in [positron_console(), positron_notebook()] {
            let html = vegalite_html(r#"{"mark": "point"}"#, &hints);
            assert!(
                html.contains(
                    "if (result.view.width() > 0 && result.view.height() > 0) { return; }"
                ),
                "recovery must gate on the size the view rendered at (hints={:?})",
                hints
            );
        }
    }

    #[test]
    fn test_positron_recovery_dispatches_a_resize_event() {
        // Container sizing compiles to width/height signals that re-read
        // `containerSize()` only on `window:resize`. `view.resize()` re-lays out
        // from the current signal value, which is still zero, so it cannot
        // recover a collapsed plot.
        for hints in [positron_console(), positron_notebook()] {
            let html = vegalite_html(r#"{"mark": "point"}"#, &hints);
            assert!(
                html.contains("window.dispatchEvent(new Event('resize'));"),
                "recovery must dispatch the event the signal listens for (hints={:?})",
                hints
            );
            assert!(
                !html.contains("view.resize()"),
                "view.resize() does not re-read containerSize (hints={:?})",
                hints
            );
        }
    }

    #[test]
    fn test_positron_html_recovers_on_both_load_paths() {
        // vega-embed is reached either through requirejs or through direct
        // script loading; a plot that collapsed must recover either way.
        for hints in [positron_console(), positron_notebook()] {
            let html = vegalite_html(r#"{"mark": "point"}"#, &hints);
            assert_eq!(
                html.matches("recoverIfCollapsed").count(),
                3,
                "recovery must be defined once and wired into both load paths (hints={:?})",
                hints
            );
        }
    }

    #[test]
    fn test_console_html_fills_positron_plots_pane() {
        let html = vegalite_html(r#"{"mark": "point"}"#, &positron_console());
        assert!(
            html.contains(".positron-output-container"),
            "HTML must detect Positron's plots pane for responsive height"
        );
        assert!(
            html.contains("100vh"),
            "HTML must scale to 100vh inside the plots pane"
        );
        assert!(
            html.contains("height: 400px"),
            "HTML must set a 400px baseline height for console output"
        );
    }

    #[test]
    fn test_notebook_html_skips_pane_override() {
        let html = vegalite_html(r#"{"mark": "point"}"#, &positron_notebook());
        assert!(
            html.contains("height: 400px"),
            "notebook container uses the shared 400px baseline"
        );
        assert!(
            !html.contains(".positron-output-container"),
            "notebook HTML must not carry the plots-pane 100vh override"
        );
        assert!(
            !html.contains("100vh"),
            "notebook HTML must not reach for 100vh"
        );
    }

    #[test]
    fn test_standalone_html_uses_scale_to_fit() {
        // Standalone (Jupyter/Quarto) renders into a static document and
        // wraps the plot in the outer/inner div + min-width scale-to-fit so
        // narrow viewports shrink the plot proportionally.
        let html = vegalite_html(r#"{"mark": "point"}"#, &RenderHints::default());
        assert!(
            html.contains("min-width: 450px"),
            "standalone HTML must use the 450px design width"
        );
        assert!(
            html.contains("scaleToFit"),
            "standalone HTML must uniformly scale narrow viewports"
        );
        assert!(
            html.contains("new ResizeObserver"),
            "standalone HTML must observe container resizes"
        );
        assert!(
            html.contains("-outer"),
            "standalone HTML must wrap the inner div in an overflow-hidden outer div"
        );
        assert!(
            !html.contains(".positron-output-container"),
            "standalone HTML must not carry the Positron plots-pane branch"
        );
    }

    fn header(session: &str) -> MessageHeader {
        MessageHeader {
            msg_id: String::new(),
            session: session.to_string(),
            username: String::new(),
            date: String::new(),
            msg_type: String::new(),
            version: String::new(),
        }
    }

    fn kind(session: &str, mode: Option<SessionMode>) -> SessionKind {
        RenderHints::from_request(&header(session), &json!({}), mode).kind
    }

    #[test]
    fn test_from_request_detects_positron_sessions() {
        // The fallback path, for a frontend that passes no `--session-mode`.
        assert_eq!(kind("ggsql-c2a5a97b", None), SessionKind::PositronConsole);
        assert_eq!(
            kind("ggsql-notebook-abc", None),
            SessionKind::PositronNotebook
        );
        assert_eq!(kind("abcd-efgh-1234", None), SessionKind::Standalone);
    }

    #[test]
    fn test_session_mode_overrides_the_heuristic() {
        // A frontend that declares itself is believed, whatever its session id
        // happens to look like — the id is a guess, the flag is a statement.
        assert_eq!(
            kind("abcd-efgh-1234", Some(SessionMode::Console)),
            SessionKind::PositronConsole
        );
        assert_eq!(
            kind("ggsql-c2a5a97b", Some(SessionMode::Notebook)),
            SessionKind::PositronNotebook
        );
        assert_eq!(
            kind("ggsql-notebook-abc", Some(SessionMode::Console)),
            SessionKind::PositronConsole
        );
    }

    #[test]
    fn test_a_background_session_has_no_positron_slot() {
        // It is Positron's session, but attached to no UI — so the heuristic's
        // answer (console, from the `ggsql-` prefix) would aim output at a
        // pane that is not showing it.
        assert_eq!(
            kind("ggsql-bg-4471", Some(SessionMode::Background)),
            SessionKind::Standalone
        );
        assert_eq!(kind("ggsql-bg-4471", None), SessionKind::PositronConsole);
    }

    #[test]
    fn test_a_non_positron_session_is_standalone_whatever_its_id_says() {
        // The old heuristic set `is_notebook` from the id alone, so a
        // standalone session whose id happened to contain "notebook" carried a
        // flag the standalone template never read. Now there is one answer.
        assert_eq!(kind("jupyter-notebook-9f2c", None), SessionKind::Standalone);
        assert!(!SessionKind::Standalone.is_positron());
        assert!(!SessionKind::Standalone.is_notebook());
    }

    #[test]
    fn test_the_two_positron_kinds_pick_different_templates() {
        let spec = r#"{"mark":"point"}"#;
        let console = vegalite_html(spec, &positron_console());
        let notebook = vegalite_html(spec, &positron_notebook());
        // The console template alone reaches for the Plots pane.
        assert!(console.contains("positron-output-container"));
        assert!(!notebook.contains("positron-output-container"));
    }
}
