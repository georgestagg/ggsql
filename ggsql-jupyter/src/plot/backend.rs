//! The render thread.
//!
//! Rendering does not happen on the message loop, for one concrete reason:
//! `kernel.rs` awaits `handle_shell_message` *inline* in its `select!`, so
//! anything blocking there stalls the heartbeat, the control channel and the
//! SIGINT handler alike.
//!
//! # What the cold start actually costs
//!
//! Measured on a release build, Apple GPU, vello-hybrid:
//!
//! | | warm | cold (first process after a build) |
//! | --- | --- | --- |
//! | `RasterRenderer::new()` | 14 ms | ~185 ms |
//! | **first render** | **~85 ms** | **~1.35 s** |
//! | later renders, 3-point plot at 1200×800 | 5 ms | 5 ms |
//! | later renders, 50k points | ~200 ms | ~200 ms |
//!
//! **Constructing the renderer is not the expensive part — the first render
//! is**, and most of that is text: parley/fontique enumerating and loading
//! system faces. Rendering an SVG first (which needs no GPU but does the same
//! text work) drops the first raster render from ~85 ms to ~20 ms, which is
//! what identifies the cost. It is per *process*, not per renderer.
//!
//! So this thread does two things at startup: builds the renderer, and renders
//! a throwaway frame to pay that cost before any user is waiting on it. With
//! the warm-up, the first plot of a session renders in ~14 ms instead of
//! ~85 ms — and in the genuinely cold case, instead of well over a second.
//!
//! The renderer is `Send` but not `Sync`, which is exactly the shape this
//! wants: it moves here once and is never shared.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};

use anyhow::{anyhow, Result};
use ggsql::reader::Spec;

use super::{Format, RenderRequest, RenderTicket};

/// How long to wait for a GPU adapter before deciding there isn't one.
///
/// The probe is worth doing eagerly — a lazy one would leave the *first* plot
/// unable to choose a path — but not worth hanging on. A driver that has not
/// answered in ten seconds is not one to render through.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// A finished render, on its way back to the message loop.
pub struct RenderOutcome {
    pub ticket: Box<RenderTicket>,
    pub result: Result<Vec<u8>>,
}

/// Work for the render thread.
enum Job {
    /// Keep `spec` so it can be re-rendered on demand. This is what lets a
    /// plot be re-drawn at a new size without re-running the query — and it is
    /// why the retained `DataFrame`s live here rather than on the async task.
    Store { comm_id: String, spec: Box<Spec> },
    /// Forget a stored plot, because its comm closed or it was evicted.
    Forget { comm_id: String },
    /// Re-render a stored plot and report the result asynchronously.
    Render {
        comm_id: String,
        request: RenderRequest,
        // Boxed to keep the variants a similar size: a ticket carries a whole
        // Jupyter message, and every job would otherwise be that large.
        ticket: Box<RenderTicket>,
    },
    /// Render a plot we were handed and will not keep, answering `reply`
    /// directly. The one-shot path, for a static output bundle.
    RenderOnce {
        spec: Box<Spec>,
        request: RenderRequest,
        reply: Sender<Result<Vec<u8>>>,
    },
    /// Stop the thread.
    Shutdown,
}

/// A handle to the render thread.
pub struct PlotBackend {
    jobs: Sender<Job>,
    /// Whether a GPU adapter was found at startup, and therefore whether the
    /// raster formats are available at all. Decided once: a machine does not
    /// grow a GPU mid-session.
    raster: bool,
}

impl PlotBackend {
    /// Start the render thread and probe for a GPU adapter.
    ///
    /// Blocks until the probe finishes, so callers know from the first plot
    /// onward whether raster output is possible. Call it after announcing
    /// `starting` status, not before.
    pub fn spawn(outcomes: tokio::sync::mpsc::UnboundedSender<RenderOutcome>) -> Self {
        Self::start(true, Some(outcomes))
    }

    /// A backend that never builds a GPU renderer.
    ///
    /// For tests: the probe is the one slow, machine-dependent part of
    /// starting up, and skipping it makes a test both fast and the same
    /// everywhere. The SVG path it leaves is the one that always works, which
    /// is what a test should be asserting on anyway.
    #[cfg(test)]
    pub fn without_raster() -> Self {
        Self::start(false, None)
    }

    fn start(
        allow_raster: bool,
        outcomes: Option<tokio::sync::mpsc::UnboundedSender<RenderOutcome>>,
    ) -> Self {
        let (jobs, inbox) = mpsc::channel();
        let (probed, probe_result) = mpsc::channel();

        std::thread::Builder::new()
            .name("ggsql-render".to_string())
            .spawn(move || render_loop(inbox, probed, allow_raster, outcomes))
            .expect("failed to spawn the render thread");

        // A thread that died before probing, or a driver that never answered,
        // both mean the same thing to us.
        let raster = probe_result
            .recv_timeout(PROBE_TIMEOUT)
            .unwrap_or_else(|_| {
                tracing::warn!("GPU probe did not finish within {PROBE_TIMEOUT:?}");
                false
            });

        if raster {
            tracing::info!("GPU adapter available; raster plot formats enabled");
        } else {
            tracing::info!("no GPU adapter; plots will render as SVG");
        }

        Self { jobs, raster }
    }

    /// Whether this build and this machine can produce raster output.
    pub fn raster(&self) -> bool {
        self.raster
    }

    /// Render a plot once, blocking until the thread answers.
    ///
    /// Deliberately blocking, unlike [`Self::request_render`]. This is the
    /// static path: it runs once per execution, inside a cell that has already
    /// paid for parsing and SQL, and its output has to be ordered between the
    /// `execute_input` and the `execute_reply`. A few milliseconds there buys
    /// a great deal of simplicity.
    ///
    /// # Errors
    ///
    /// Returns an error if the render thread has stopped, or if the render
    /// itself failed.
    pub fn render_once(&self, spec: Box<Spec>, request: RenderRequest) -> Result<Vec<u8>> {
        let (reply, answer) = mpsc::channel();
        self.jobs
            .send(Job::RenderOnce {
                spec,
                request,
                reply,
            })
            .map_err(|_| anyhow!("the render thread has stopped"))?;
        answer
            .recv()
            .map_err(|_| anyhow!("the render thread stopped while rendering"))?
    }

    /// Keep a plot so its comm can re-render it at any size.
    pub fn store(&self, comm_id: String, spec: Box<Spec>) {
        let _ = self.jobs.send(Job::Store { comm_id, spec });
    }

    /// Forget a stored plot.
    pub fn forget(&self, comm_id: &str) {
        let _ = self.jobs.send(Job::Forget {
            comm_id: comm_id.to_string(),
        });
    }

    /// Ask for a stored plot to be re-rendered, and return immediately.
    ///
    /// **This is why the thread exists.** The reply arrives later on the
    /// outcome channel, so a render — up to a couple of hundred milliseconds
    /// on a dense plot, and asked for once per frame while a pane is being
    /// dragged — never blocks the message loop, and with it the heartbeat, the
    /// control channel and the interrupt handler.
    pub fn request_render(&self, comm_id: &str, request: RenderRequest, ticket: RenderTicket) {
        let _ = self.jobs.send(Job::Render {
            comm_id: comm_id.to_string(),
            request,
            ticket: Box::new(ticket),
        });
    }
}

impl Drop for PlotBackend {
    fn drop(&mut self) {
        let _ = self.jobs.send(Job::Shutdown);
    }
}

/// The render thread's body: probe once, then serve jobs until told to stop.
fn render_loop(
    inbox: Receiver<Job>,
    probed: Sender<bool>,
    allow_raster: bool,
    outcomes: Option<tokio::sync::mpsc::UnboundedSender<RenderOutcome>>,
) {
    // One renderer for the whole session. Building it is the expensive part,
    // and it handles a changing frame size internally, so it serves every
    // subsequent render whatever size that render asks for.
    let mut renderer = if allow_raster {
        raster_renderer()
    } else {
        None
    };
    let _ = probed.send(renderer.is_some());

    // Pay the first-render cost now, while nobody is waiting on it. See the
    // module docs: it is mostly font loading, and it is per process.
    warm_up(renderer.as_mut());

    // The retained plots. They live here rather than beside the comm state so
    // the post-stat `DataFrame`s stay off the async task entirely.
    let mut stored: HashMap<String, Box<Spec>> = HashMap::new();

    while let Ok(job) = inbox.recv() {
        match job {
            Job::Shutdown => break,
            Job::Store { comm_id, spec } => {
                stored.insert(comm_id, spec);
            }
            Job::Forget { comm_id } => {
                stored.remove(&comm_id);
            }
            Job::RenderOnce {
                spec,
                request,
                reply,
            } => {
                let result = render_one(&spec, &request, renderer.as_mut());
                // A caller that gave up before we finished is not an error.
                let _ = reply.send(result);
            }
            Job::Render {
                comm_id,
                request,
                ticket,
            } => {
                let result = match stored.get(&comm_id) {
                    Some(spec) => render_one(spec, &request, renderer.as_mut()),
                    // The comm closed, or the plot was evicted, between the
                    // request arriving and us reaching it.
                    None => Err(anyhow!("this plot is no longer available")),
                };
                if let Some(outcomes) = &outcomes {
                    let _ = outcomes.send(RenderOutcome { ticket, result });
                }
            }
        }
    }
}

/// Render a throwaway frame so the first real plot does not pay for the
/// process's font enumeration and pipeline setup.
///
/// Uses a **throwaway in-memory database of its own**, never the session's
/// reader: executing a query through that would materialise ggsql's internal
/// views in the user's session, and a warm-up must leave no trace. It also
/// keeps this self-contained — nothing has to be wired in from the kernel.
///
/// Rendered tiny, since what is being paid for is not proportional to area.
/// Best-effort throughout: a warm-up that fails costs a slower first plot and
/// nothing else, so it is logged at debug and forgotten.
fn warm_up(renderer: Option<&mut Renderer>) {
    const QUERY: &str = "SELECT 1 AS x, 1 AS y VISUALISE x AS x, y AS y DRAW point";

    let started = std::time::Instant::now();
    let spec = match crate::executor::create_reader("duckdb://memory")
        .and_then(|reader| reader.execute(QUERY).map_err(Into::into))
    {
        Ok(spec) => spec,
        Err(e) => {
            tracing::debug!("renderer warm-up skipped: {e}");
            return;
        }
    };

    let request = RenderRequest {
        // SVG warms the text stack whether or not there is a GPU, and the text
        // stack is where most of the cost is. A raster warm-up on top would
        // save a further ~15 ms, which is not worth a second pass.
        format: Format::Svg,
        canvas: super::Canvas {
            width: 64,
            height: 64,
            dpi: 96.0,
        },
    };
    match render_one(&spec, &request, renderer) {
        Ok(_) => tracing::debug!("renderer warmed up in {:?}", started.elapsed()),
        Err(e) => tracing::debug!("renderer warm-up failed: {e}"),
    }
}

/// Build the GPU renderer, or report that there isn't one.
#[cfg(feature = "raster-plots")]
fn raster_renderer() -> Option<ggsql::writer::RasterRenderer> {
    match ggsql::writer::RasterRenderer::new() {
        Ok(renderer) => Some(renderer),
        Err(e) => {
            tracing::info!("no GPU renderer: {e}");
            None
        }
    }
}

/// Without the feature there is nothing to build, and the probe is a constant.
#[cfg(not(feature = "raster-plots"))]
fn raster_renderer() -> Option<Never> {
    None
}

/// Stands in for the renderer in a build that has none, so `render_one` keeps
/// one signature. Uninhabited, so the raster arms are unreachable rather than
/// merely unused.
#[cfg(not(feature = "raster-plots"))]
pub enum Never {}

#[cfg(feature = "raster-plots")]
type Renderer = ggsql::writer::RasterRenderer;
#[cfg(not(feature = "raster-plots"))]
type Renderer = Never;

/// Render one plot in whichever format was asked for.
fn render_one(
    spec: &Spec,
    request: &RenderRequest,
    renderer: Option<&mut Renderer>,
) -> Result<Vec<u8>> {
    let canvas = request.canvas;
    match request.format {
        Format::Svg => {
            let writer = ggsql::writer::SvgWriter::new(canvas.width, canvas.height, canvas.dpi);
            let (svg, warnings) = writer.render_reporting(spec)?;
            report(&warnings, "svg");
            Ok(svg.into_bytes())
        }
        Format::Pdf => {
            let writer = ggsql::writer::PdfWriter::new(canvas.width, canvas.height, canvas.dpi);
            let (pdf, warnings) = writer.render_reporting(spec)?;
            report(&warnings, "pdf");
            Ok(pdf)
        }
        #[cfg(feature = "raster-plots")]
        Format::Png | Format::Jpeg | Format::Tiff => {
            let renderer = renderer.ok_or_else(|| {
                anyhow!("this plot needs a GPU adapter, and none was found at startup")
            })?;
            match request.format {
                Format::Png => {
                    Ok(
                        ggsql::writer::PngWriter::new(canvas.width, canvas.height, canvas.dpi)
                            // The interactive path re-encodes on every resize, so trade
                            // bytes for latency; a static figure is written once and the
                            // difference is imperceptible either way.
                            .compression(ggsql::writer::PngCompression::Fast)
                            .render_with(spec, renderer)?,
                    )
                }
                Format::Jpeg => {
                    Ok(
                        ggsql::writer::JpegWriter::new(canvas.width, canvas.height, canvas.dpi)
                            .render_with(spec, renderer)?,
                    )
                }
                Format::Tiff => {
                    Ok(
                        ggsql::writer::TiffWriter::new(canvas.width, canvas.height, canvas.dpi)
                            .render_with(spec, renderer)?,
                    )
                }
                Format::Svg | Format::Pdf => unreachable!("handled above"),
            }
        }
        #[cfg(not(feature = "raster-plots"))]
        Format::Png | Format::Jpeg | Format::Tiff => {
            let _ = renderer;
            Err(anyhow!(
                "this build has no raster plot formats; rebuild with --features raster-plots"
            ))
        }
    }
}

/// Put anything a format could not express in front of a human.
///
/// Not behind a verbosity flag: a dropped gradient is a defect in the figure a
/// document is about to embed. The list is empty for everything ggsql draws,
/// so anything here is worth reading.
fn report(warnings: &[String], format: &str) {
    for warning in warnings {
        tracing::warn!("{format}: {warning}");
    }
}
