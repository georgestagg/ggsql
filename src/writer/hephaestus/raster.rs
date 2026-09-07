//! Rasterising a composition to pixels.
//!
//! The one module that names a GPU renderer, and the only part of the writer
//! that needs an adapter at all: the vector and document writers build a scene
//! or a byte string from the same `PlotComposition` and never come through here.
//!
//! The backend is Vello Hybrid — coverage computed on the CPU, a plain render
//! pipeline on the GPU — chosen over vello classic because its buffers are
//! sized to the scene rather than capped, so a dense plot has no draw-count
//! ceiling. Which backend that is stays inside this file.

use std::collections::HashMap;

use hephaestus::backend::hybrid::HybridRenderer;
use hephaestus::backend::MAX_TEXTURE_DIMENSION;
use hephaestus::plot::PlotComposition;
use hephaestus::{Renderer, SceneBuilder};

use super::canvas::Canvas;
use super::compose;
use crate::{DataFrame, GgsqlError, Plot, Result};

/// A GPU renderer held across renders.
///
/// Constructing one creates a wgpu device and builds the rasteriser's
/// pipelines, which is far too expensive to repeat per figure. A host rendering
/// more than one plot — a kernel serving a plot pane, a batch job — should keep
/// one of these and hand it to `render_with`; a one-shot caller can ignore it
/// and let the writer make its own. It is `Send` but not `Sync`, so it moves to
/// a render thread rather than being shared between them.
///
/// Sizing is handled internally: the renderer rebuilds whatever is bound to the
/// frame dimensions when they change, so one of these serves a sequence of
/// differently-sized renders.
pub struct RasterRenderer(HybridRenderer);

impl RasterRenderer {
    /// Initialise the renderer, which requires a working GPU adapter.
    ///
    /// # Errors
    ///
    /// Returns `GgsqlError::WriterError` when no adapter is available, or when
    /// one is but the rasteriser could not be set up on it. The two are worth
    /// telling apart by a caller that falls back to a different output format,
    /// so the message names which happened.
    pub fn new() -> Result<Self> {
        // Not `with_picking`: indexing costs CPU per draw call and nothing here
        // hit-tests. A host that wants picking wants a live scene, not a file.
        HybridRenderer::new().map(Self).map_err(|e| {
            GgsqlError::WriterError(format!("could not initialise the GPU renderer: {e}"))
        })
    }
}

/// Largest canvas dimension a rasterising build can be asked for, in pixels.
///
/// The true ceiling belongs to the GPU: it is the device's own
/// `max_texture_dimension_2d`, and this is the most the renderer will ask a
/// device to grant. A device offering less fails the render with its own
/// limit named, which is why this is a cheap up-front guard against the
/// absurd rather than a promise that anything under it will work.
pub const MAX_RASTER_DIMENSION: u32 = MAX_TEXTURE_DIMENSION;

/// Reject a canvas no GPU could rasterise, before anything is allocated.
///
/// A device with a lower limit than [`MAX_RASTER_DIMENSION`] rejects the frame
/// itself, naming the limit it does have; this catches the sizes no device
/// would take, and is the one place that points at the writers with no ceiling
/// at all.
///
/// # Errors
///
/// Returns `GgsqlError::WriterError` naming the limit and the alternatives.
fn check_size(canvas: &Canvas) -> Result<()> {
    if canvas.width > MAX_RASTER_DIMENSION || canvas.height > MAX_RASTER_DIMENSION {
        return Err(GgsqlError::WriterError(format!(
            "{}x{} is too large to rasterise: no dimension may exceed {} px, and a \
             given GPU may allow less. The svg and pdf writers have no such limit \
             and are resolution independent, so they are the better choice at this size",
            canvas.width, canvas.height, MAX_RASTER_DIMENSION
        )));
    }
    Ok(())
}

/// Draw `view` at the canvas's size and resolution and read the pixels back.
///
/// Returns RGBA8 with straight (un-premultiplied) alpha, `width * height * 4`
/// bytes, which is what every raster encoder here expects.
pub fn render_rgba8(
    view: &mut PlotComposition,
    canvas: &Canvas,
    renderer: &mut RasterRenderer,
) -> Result<Vec<u8>> {
    check_size(canvas)?;
    {
        let scene = renderer.0.scene();
        scene.clear();
        view.render(scene, canvas.size(), canvas.dpi);
    }
    let mut pixels = vec![0u8; (canvas.width as usize) * (canvas.height as usize) * 4];
    renderer
        .0
        .render_to_buffer(canvas.width, canvas.height, canvas.background, &mut pixels)
        .map_err(|e| GgsqlError::WriterError(format!("render failed: {e}")))?;
    Ok(pixels)
}

/// Everything a raster writer does before its encoder: check the plot, compose
/// it, and rasterise it at the canvas's size and resolution.
///
/// The four raster writers differ only in the encoder they hand the result to,
/// so this is the whole of what they share.
///
/// # Errors
///
/// Returns `GgsqlError::WriterError` if the plot cannot be drawn by this
/// renderer, if composing it fails, or if the render does.
pub fn pixels(
    spec: &Plot,
    data: &HashMap<String, DataFrame>,
    canvas: &Canvas,
    renderer: &mut RasterRenderer,
) -> Result<Vec<u8>> {
    compose::validate_plot(spec)?;
    let mut view = compose::build_composition(spec, data)?;
    render_rgba8(&mut view, canvas, renderer)
}
