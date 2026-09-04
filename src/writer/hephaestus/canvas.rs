//! The canvas configuration every renderer-backed writer carries.
//!
//! Raster and vector output both need concrete dimensions and a resolution —
//! unlike the resolution-independent Vega-Lite writer — so the size, DPI and
//! background live here rather than being restated by each writer. A writer adds
//! only the keys its own format has: a JPEG quality, a TIFF compression.

use hephaestus::color::{rgba, Color};
use hephaestus::geometry::Size;

use super::scales::parse_color;
use crate::writer::WriterOptions;
use crate::{GgsqlError, Result};

/// Default canvas width in pixels.
pub(super) const DEFAULT_WIDTH: u32 = 1500;
/// Default canvas height in pixels.
pub(super) const DEFAULT_HEIGHT: u32 = 1000;
/// Default resolution. DPI converts the theme's physical sizes (text, stroke
/// widths, spacing — all in points) to pixels, so it sets how large the chrome
/// is relative to the canvas as well as the print size of a physical figure.
pub(super) const DEFAULT_DPI: f64 = 300.0;

/// Largest canvas dimension accepted, in pixels. Far beyond any real figure, but
/// small enough that a slipped unit conversion fails with a message instead of
/// exhausting GPU memory.
const MAX_DIMENSION: f64 = 32_768.0;

/// Option keys every renderer-backed writer understands.
///
/// Concatenated ahead of a writer's own keys when rejecting unknown options, so
/// the shared ones lead the "supported options" list in the error.
pub const CANVAS_OPTIONS: &[&str] = &["width", "height", "units", "dpi", "background"];

/// Units a `width` / `height` option may be given in.
const UNITS: &[&str] = &["px", "in", "cm", "mm", "pt"];

/// Size, resolution and background for one rendered figure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub dpi: f64,
    pub background: Color,
    /// Whether the dimensions were given in a physical unit rather than pixels.
    ///
    /// Only the vector backends consult it: a file asked for in inches should
    /// declare a physical size so it prints at the size it was asked for, while
    /// one asked for in pixels should stay in pixels.
    pub physical: bool,
}

impl Canvas {
    /// A canvas of the given pixel dimensions and DPI, on white.
    pub fn new(width: u32, height: u32, dpi: f64) -> Self {
        Self {
            width,
            height,
            dpi,
            background: rgba(1.0, 1.0, 1.0, 1.0),
            physical: false,
        }
    }

    /// Set the background painted before anything is drawn.
    pub fn background(mut self, color: Color) -> Self {
        self.background = color;
        self
    }

    /// Parse the shared keys, rejecting anything outside them or `extra` first.
    ///
    /// `extra` is the calling writer's own option names. Rejection happens
    /// before any value is read, so a mistyped key is reported rather than
    /// silently ignored.
    ///
    /// # Errors
    ///
    /// Returns `GgsqlError::WriterError` for an unknown key, an unusable value,
    /// or a dimension outside the renderable range.
    pub fn from_options(options: &WriterOptions, extra: &[&str]) -> Result<Self> {
        let known: Vec<&str> = CANVAS_OPTIONS.iter().chain(extra).copied().collect();
        options.reject_unknown(&known)?;

        let dpi = match options.number("dpi")? {
            Some(dpi) if dpi > 0.0 => dpi,
            Some(dpi) => {
                return Err(GgsqlError::WriterError(format!(
                    "writer option 'dpi' expects a positive number, got '{dpi}'"
                )))
            }
            None => DEFAULT_DPI,
        };
        // `units` interprets the dimensions the caller supplies; the defaults are
        // pixel counts, so they stand whatever the unit is.
        let units = options.one_of("units", UNITS)?.unwrap_or("px");
        let width = match options.number("width")? {
            Some(width) => to_pixels(width, units, dpi, "width")?,
            None => DEFAULT_WIDTH,
        };
        let height = match options.number("height")? {
            Some(height) => to_pixels(height, units, dpi, "height")?,
            None => DEFAULT_HEIGHT,
        };

        let mut canvas = Self::new(width, height, dpi);
        canvas.physical = units != "px";
        if let Some(raw) = options.get("background") {
            // `none` is a familiar spelling of a transparent canvas that CSS
            // itself doesn't accept as a color.
            let color = match raw.trim().to_lowercase().as_str() {
                "none" => rgba(0.0, 0.0, 0.0, 0.0),
                _ => parse_color(raw).ok_or_else(|| {
                    GgsqlError::WriterError(format!(
                        "writer option 'background' expects a CSS color, got '{raw}'"
                    ))
                })?,
            };
            canvas = canvas.background(color);
        }
        Ok(canvas)
    }

    /// The canvas as a hephaestus size, for `PlotComposition::render`.
    pub fn size(&self) -> Size {
        Size::new(self.width as f64, self.height as f64)
    }

    /// The resolution to record in an output that can carry one.
    ///
    /// A file that declares nothing is read as 72 dpi by whatever opens it, so
    /// an image rendered at a higher resolution would claim the wrong physical
    /// size.
    pub fn dpi_hint(&self) -> Option<f64> {
        Some(self.dpi)
    }
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new(DEFAULT_WIDTH, DEFAULT_HEIGHT, DEFAULT_DPI)
    }
}

/// Convert a canvas dimension given in `units` to whole pixels at `dpi`.
///
/// A physical unit goes through inches, so the same figure grows with DPI; `px`
/// is already the canvas unit, where DPI only scales the chrome.
fn to_pixels(value: f64, units: &str, dpi: f64, key: &str) -> Result<u32> {
    let per_inch = match units {
        "in" => 1.0,
        "cm" => 2.54,
        "mm" => 25.4,
        "pt" => 72.0,
        _ => return whole_pixels(value, key),
    };
    whole_pixels(value / per_inch * dpi, key)
}

/// Round a pixel count and reject one outside the renderable range.
fn whole_pixels(pixels: f64, key: &str) -> Result<u32> {
    let rounded = pixels.round();
    if !(1.0..=MAX_DIMENSION).contains(&rounded) {
        return Err(GgsqlError::WriterError(format!(
            "writer option '{key}' resolves to {rounded} px, outside the supported range 1–{MAX_DIMENSION} px"
        )));
    }
    Ok(rounded as u32)
}
