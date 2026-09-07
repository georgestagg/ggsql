//! Turning a frontend's logical size into a canvas.
//!
//! Everything here is pure arithmetic, so it is all directly testable — which
//! matters, because getting it wrong is invisible until a plot arrives blurry
//! or with chrome the wrong size relative to the panel.

/// Dots per inch a device pixel ratio of 1.0 corresponds to.
///
/// One CSS pixel is 1/96 in by definition, so rendering at 96 dpi makes the
/// renderer's point→pixel conversion agree with the browser's.
pub const CSS_DPI: f64 = 96.0;

/// Smallest canvas dimension we will render, in device pixels.
///
/// Positron can transiently report a zero-width pane while it lays out, and a
/// zero-sized GPU target is a hard error rather than an empty picture.
const MIN_PX: u32 = 32;

/// Largest canvas dimension we will render, in device pixels.
///
/// The rasteriser cannot produce a dimension above 4096 — it is
/// `vello_hybrid`'s default intermediate-texture size, which the renderer does
/// not override — and a pane on a large display at 2x reaches that easily. So
/// the ceiling is the raster one even for the vector formats: capping them too
/// costs nothing (they are resolution independent, and the *displayed* size is
/// unaffected) and keeps one number in play instead of two.
const MAX_PX: u32 = 4_096;

/// Device pixel ratios we will honour. Beyond this a frontend is either
/// confused or asking for a texture we should not allocate.
const MIN_RATIO: f64 = 0.5;
const MAX_RATIO: f64 = 4.0;

/// A canvas to render at: device pixels plus the resolution to render them at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub dpi: f64,
}

impl Canvas {
    /// The canvas for a logical size at a device pixel ratio.
    ///
    /// **All three scale together**, which is the part that is easy to get
    /// wrong: the renderer converts the theme's point sizes at render dpi, so
    /// scaling the pixel dimensions alone would render the same chrome into
    /// more pixels (a blurry plot at the right size), and scaling dpi alone
    /// would grow the chrome instead of the resolution. This matches
    /// matplotlib's Positron backend, which scales `figure.dpi` by the ratio
    /// and divides the requested size by the scaled dpi, holding physical size
    /// invariant.
    pub fn from_logical(width: f64, height: f64, pixel_ratio: f64) -> Self {
        let ratio = if pixel_ratio.is_finite() {
            pixel_ratio.clamp(MIN_RATIO, MAX_RATIO)
        } else {
            1.0
        };
        Self {
            width: clamp_px(width * ratio),
            height: clamp_px(height * ratio),
            dpi: CSS_DPI * ratio,
        }
    }

    /// The canvas for a physical size in inches at a given resolution — what
    /// Quarto asks for, and what a print workflow means.
    pub fn from_inches(width: f64, height: f64, dpi: f64) -> Self {
        let dpi = if dpi.is_finite() && dpi > 0.0 {
            dpi
        } else {
            CSS_DPI
        };
        Self {
            width: clamp_px(width * dpi),
            height: clamp_px(height * dpi),
            dpi,
        }
    }

    /// The size a frontend should display this at, in CSS pixels.
    ///
    /// A 2× render has to be shown at half its pixel dimensions or it appears
    /// at twice its intended size. This is what `metadata[mime].width/height`
    /// carries, and what JupyterLab and nbconvert honour.
    pub fn css_size(&self) -> (u32, u32) {
        let scale = CSS_DPI / self.dpi;
        (
            ((self.width as f64) * scale).round().max(1.0) as u32,
            ((self.height as f64) * scale).round().max(1.0) as u32,
        )
    }
}

impl Default for Canvas {
    /// A reasonable figure for a frontend that told us nothing at all.
    fn default() -> Self {
        Self {
            width: 1000,
            height: 618,
            dpi: CSS_DPI,
        }
    }
}

/// Round to whole device pixels and hold the result inside what we will render.
fn clamp_px(value: f64) -> u32 {
    if !value.is_finite() {
        return MIN_PX;
    }
    (value.round().max(0.0) as u32).clamp(MIN_PX, MAX_PX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ratio_of_one_is_the_logical_size_at_css_dpi() {
        let canvas = Canvas::from_logical(800.0, 600.0, 1.0);
        assert_eq!((canvas.width, canvas.height), (800, 600));
        assert_eq!(canvas.dpi, 96.0);
        assert_eq!(canvas.css_size(), (800, 600));
    }

    #[test]
    fn a_retina_ratio_scales_pixels_and_dpi_together() {
        let canvas = Canvas::from_logical(800.0, 600.0, 2.0);
        assert_eq!((canvas.width, canvas.height), (1600, 1200));
        assert_eq!(canvas.dpi, 192.0);
        // Twice the pixels, shown at the original size — so it is sharp rather
        // than twice as big.
        assert_eq!(canvas.css_size(), (800, 600));
    }

    #[test]
    fn a_zero_size_is_clamped_rather_than_rendered() {
        // Positron reports this while laying out a pane, and a zero-sized GPU
        // target is an error rather than an empty picture.
        let canvas = Canvas::from_logical(0.0, 0.0, 1.0);
        assert_eq!((canvas.width, canvas.height), (MIN_PX, MIN_PX));
    }

    #[test]
    fn an_absurd_size_is_clamped_rather_than_allocated() {
        let canvas = Canvas::from_logical(99_999.0, 99_999.0, 1.0);
        assert_eq!((canvas.width, canvas.height), (MAX_PX, MAX_PX));
    }

    #[test]
    fn a_large_pane_at_two_x_stays_within_what_can_be_rendered() {
        // 2400 logical px at 2x is 4800 device px, which the rasteriser
        // refuses outright. Clamping gives a slightly softer plot; not
        // clamping gives a render error and no plot at all.
        let canvas = Canvas::from_logical(2400.0, 1400.0, 2.0);
        assert!(canvas.width <= MAX_PX, "{} px", canvas.width);
        assert_eq!(canvas.width, MAX_PX);
        // The height was within the cap, so it is untouched.
        assert_eq!(canvas.height, 2800);
    }

    #[test]
    fn a_nonsense_ratio_falls_back_rather_than_propagating() {
        for ratio in [f64::NAN, f64::INFINITY] {
            let canvas = Canvas::from_logical(800.0, 600.0, ratio);
            assert_eq!((canvas.width, canvas.height), (800, 600), "{ratio}");
            assert_eq!(canvas.dpi, 96.0);
        }
        // A ratio outside what any display reports is clamped, not honoured.
        assert_eq!(Canvas::from_logical(100.0, 100.0, 99.0).dpi, CSS_DPI * 4.0);
        assert_eq!(Canvas::from_logical(100.0, 100.0, 0.01).dpi, CSS_DPI * 0.5);
    }

    #[test]
    fn inches_go_through_the_requested_resolution() {
        let canvas = Canvas::from_inches(6.0, 4.0, 150.0);
        assert_eq!((canvas.width, canvas.height), (900, 600));
        assert_eq!(canvas.dpi, 150.0);
        // Six inches at 150 dpi is 900 px, shown at 576 CSS px (6 in × 96).
        assert_eq!(canvas.css_size(), (576, 384));
    }

    #[test]
    fn a_bad_dpi_falls_back_to_css_dpi() {
        for dpi in [0.0, -100.0, f64::NAN] {
            let canvas = Canvas::from_inches(6.0, 4.0, dpi);
            assert_eq!(canvas.dpi, CSS_DPI, "{dpi}");
        }
    }
}
