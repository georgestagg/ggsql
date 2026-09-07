//! Deciding what a plot should become, and producing it.
//!
//! Three things have to agree before a plot can be rendered: where the output
//! is going ([`SessionKind`](crate::display::SessionKind)), what this build and
//! this machine can produce ([`PlotBackend::raster`]), and what the frontend
//! asked for. [`choose`] is the single place that reconciles them, so the
//! policy is readable in one function rather than spread across the formatting
//! code.

pub mod backend;
pub mod comm;
pub mod quarto;
pub mod sizing;

pub use backend::{PlotBackend, RenderOutcome};
pub use sizing::Canvas;

use crate::display::SessionKind;

/// An output format a plot can be rendered to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Svg,
    Pdf,
    /// Only the plot comm asks for this; nothing else routes to it.
    Tiff,
}

impl Format {
    /// The MIME type this format travels under.
    ///
    /// Copied from Positron's own backend (`plots.py`) so both ends agree.
    pub fn mime(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Svg => "image/svg+xml",
            Self::Pdf => "application/pdf",
            Self::Tiff => "image/tiff",
        }
    }

    /// Whether the output is text rather than bytes, and so travels in a
    /// display bundle unencoded.
    pub fn is_text(self) -> bool {
        matches!(self, Self::Svg)
    }

    /// Whether producing this needs a GPU adapter.
    pub fn needs_raster(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::Tiff)
    }
}

/// What a comm render needs to send its answer back where it came from.
///
/// The render thread treats this as opaque and echoes it with the result. It
/// lives here rather than in `backend` so the thread carries no knowledge of
/// the messaging layer beyond moving this along.
#[derive(Debug, Clone)]
pub struct RenderTicket {
    pub comm_id: String,
    /// The JSON-RPC `id` to answer.
    pub rpc_id: serde_json::Value,
    /// The request being answered, which decides the reply's `mime_type` and
    /// echoed `settings`.
    pub params: comm::RenderParams,
    /// The `comm_msg` this is a reply to, and the socket identities to route
    /// it back along.
    pub parent: crate::message::JupyterMessage,
    pub identities: Vec<Vec<u8>>,
}

/// A plot to render, at a size, in a format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderRequest {
    pub format: Format,
    pub canvas: Canvas,
}

/// Where a rendered plot should be delivered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Delivery {
    /// A static bundle in the cell's output, which is what a notebook, a
    /// Quarto render and plain Jupyter all want.
    Static(RenderRequest),
    /// A `positron.plot` comm, which the kernel opens and then serves render
    /// requests on. **No `execute_result` accompanies it** — the comm alone
    /// creates the pane entry, so an output message as well would show the
    /// plot twice.
    Comm,
    /// The old Vega-Lite HTML payload, routed to Positron's Plots pane.
    ///
    /// No longer chosen, and kept only as an escape hatch: setting
    /// `GGSQL_PLOT_VEGALITE=1` puts a console session back on it. That exists
    /// so a problem with the comm in a real Positron build can be confirmed
    /// against the previous behaviour without rebuilding, and it goes away
    /// with the Vega-Lite plot path itself.
    VegaLite,
}

/// Whether the temporary Vega-Lite escape hatch is set.
fn vegalite_override() -> bool {
    std::env::var("GGSQL_PLOT_VEGALITE")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Decide what to do with a plot.
///
/// The rules, in the order they apply:
///
/// 1. **A Positron console session opens a plot comm** — but only if raster
///    output is possible. Positron asks the comm for `png`, and answering a
///    `png` request with SVG bytes would have it write SVG into a file named
///    `.png`; so with no adapter the console takes the static SVG path
///    instead, and gets a picture rather than a broken one.
/// 2. **Quarto is obeyed.** If `QUARTO_FIG_FORMAT` and friends are set, the
///    document has told us exactly what figure it wants, including its size
///    in inches. Only for a standalone session — Quarto never drives a
///    Positron console.
/// 3. **Raster where possible, SVG where not.** Everything else gets a PNG at
///    the frontend's size, falling back to SVG when there is no GPU adapter or
///    the build has no raster formats. SVG is the right fallback precisely
///    because it needs neither: it carries the same resolved scales, breaks
///    and labels the raster path would.
pub fn choose(kind: SessionKind, backend_raster: bool, canvas: Canvas) -> Delivery {
    if kind == SessionKind::PositronConsole {
        if vegalite_override() {
            return Delivery::VegaLite;
        }
        if backend_raster {
            return Delivery::Comm;
        }
        // No adapter: fall through to a static SVG bundle rather than open a
        // comm we cannot serve a `png` request on.
        return Delivery::Static(RenderRequest {
            format: Format::Svg,
            canvas,
        });
    }

    if kind == SessionKind::Standalone {
        if let Some(figure) = quarto::from_env() {
            // A document asking for a raster figure on a machine with no
            // adapter still gets a figure, just a vector one.
            let format = if figure.format.needs_raster() && !backend_raster {
                Format::Svg
            } else {
                figure.format
            };
            return Delivery::Static(RenderRequest {
                format,
                canvas: figure.canvas,
            });
        }
    }

    Delivery::Static(RenderRequest {
        format: if backend_raster {
            Format::Png
        } else {
            Format::Svg
        },
        canvas,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANVAS: Canvas = Canvas {
        width: 800,
        height: 600,
        dpi: 96.0,
    };

    #[test]
    fn a_console_session_opens_a_plot_comm() {
        assert_eq!(
            choose(SessionKind::PositronConsole, true, CANVAS),
            Delivery::Comm
        );
    }

    #[test]
    fn a_console_without_an_adapter_gets_a_picture_rather_than_a_comm() {
        // Positron asks a comm for `png`. Answering that with SVG bytes would
        // display fine and then save SVG into a `.png`, so with no adapter the
        // console takes the static path instead.
        let Delivery::Static(request) = choose(SessionKind::PositronConsole, false, CANVAS) else {
            panic!("expected a static bundle");
        };
        assert_eq!(request.format, Format::Svg);
    }

    #[test]
    fn a_notebook_gets_a_static_image() {
        // A plot comm would put the picture in the Plots pane and leave the
        // cell empty, so a notebook needs a bundle whatever else is true.
        let Delivery::Static(request) = choose(SessionKind::PositronNotebook, true, CANVAS) else {
            panic!("a notebook should get a static bundle");
        };
        assert_eq!(request.format, Format::Png);
        assert_eq!(request.canvas, CANVAS);
    }

    #[test]
    fn svg_is_the_fallback_wherever_raster_is_unavailable() {
        for kind in [SessionKind::PositronNotebook, SessionKind::Standalone] {
            let Delivery::Static(request) = choose(kind, false, CANVAS) else {
                panic!("{kind:?} should get a static bundle");
            };
            assert_eq!(request.format, Format::Svg, "{kind:?}");
            // The fallback keeps the size it was asked for; only the format
            // changes.
            assert_eq!(request.canvas, CANVAS, "{kind:?}");
        }
    }

    #[test]
    fn every_format_has_positrons_mime_type() {
        // Copied from `plots.py`; both ends have to agree or a frontend
        // decodes the wrong thing.
        assert_eq!(Format::Png.mime(), "image/png");
        assert_eq!(Format::Jpeg.mime(), "image/jpeg");
        assert_eq!(Format::Svg.mime(), "image/svg+xml");
        assert_eq!(Format::Pdf.mime(), "application/pdf");
        assert_eq!(Format::Tiff.mime(), "image/tiff");
    }

    #[test]
    fn only_svg_travels_as_text() {
        assert!(Format::Svg.is_text());
        for format in [Format::Png, Format::Jpeg, Format::Pdf, Format::Tiff] {
            assert!(!format.is_text(), "{format:?}");
        }
    }

    #[test]
    fn the_vector_formats_need_no_adapter() {
        assert!(!Format::Svg.needs_raster());
        assert!(!Format::Pdf.needs_raster());
        for format in [Format::Png, Format::Jpeg, Format::Tiff] {
            assert!(format.needs_raster(), "{format:?}");
        }
    }
}
