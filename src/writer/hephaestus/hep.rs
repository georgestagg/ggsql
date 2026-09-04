//! The `hep` plot-document writer.

use std::collections::HashMap;

use hephaestus::document::{
    unsupported_items_for, write_composition, UnsupportedItem, WriteOptions,
};

use super::canvas::Canvas;
use super::{compose, CANVAS_HINT_OPTIONS};
use crate::writer::{Writer, WriterOptions};
use crate::{DataFrame, GgsqlError, Plot, Result};

/// Option keys [`HepWriter`] adds to the canvas set.
const HEP_OPTIONS: &[&str] = &["lossy", "embed-fonts"];

/// Writer that captures a ggsql plot as a self-contained **`.hep`** plot
/// document.
///
/// Unlike every other writer here, this one produces no picture. It records the
/// resolved plot — scales, breaks, labels, theme, geometry and data channels —
/// so a consumer can render it *itself*, at whatever size and resolution it has,
/// and re-render on resize without going back to the query. That is what makes
/// it the format for an interactive host: hit-testing a mark or hovering a
/// legend key never crosses a wire.
///
/// **The name is the format's**, not ggsql's. ggsql does not define `.hep`, so
/// calling it anything else would imply a container ggsql owns.
///
/// Needs no GPU adapter, and no encoder: it serialises the same composition the
/// other writers draw.
///
/// [`HepWriter::from_options`] takes:
///
/// | Option | Value | Default |
/// | --- | --- | --- |
/// | `width` | Canvas width **hint**, in `units` | none |
/// | `height` | Canvas height **hint**, in `units` | none |
/// | `units` | `px`, `in`, `cm`, `mm`, or `pt` — how `width`/`height` are read | `px` |
/// | `dpi` | Resolution **hint** | none |
/// | `background` | Background a consumer should paint behind the plot | `white` |
/// | `lossy` | Drop what the format cannot carry instead of refusing | `false` |
/// | `embed-fonts` | Inline the font files the plot's text needs | `false` |
///
/// **The size is a hint, not a canvas.** Any size works — that is the point of
/// the format — so `width`/`height`/`dpi` record what a consumer should default
/// to rather than fixing anything.
///
/// `lossy` decides what happens to a plot the format cannot fully carry.
/// Refusing is the default because silently changing a plot is worse than
/// saying what is wrong; with `lossy` on, the same list comes back as warnings
/// from [`HepWriter::write_reporting`]. Nothing ggsql itself builds should trip
/// it — the writer registers only built-in geoms and gives its scales resolved
/// break labels rather than formatter closures — so a non-empty list is a bug
/// here rather than a limit of the format.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HepWriter {
    canvas: Canvas,
    /// Whether a size was asked for at all, since an unset hint and a hint that
    /// happens to match the canvas default are different things to record.
    sized: bool,
    lossy: bool,
    embed_fonts: bool,
}

impl HepWriter {
    /// A writer recording the given size and resolution as the consumer's
    /// default.
    pub fn new(width: u32, height: u32, dpi: f64) -> Self {
        Self {
            canvas: Canvas::new(width, height, dpi),
            sized: true,
            ..Self::default()
        }
    }

    /// Set the background a consumer should paint behind the plot.
    pub fn background(mut self, color: super::Color) -> Self {
        self.canvas = self.canvas.background(color);
        self
    }

    /// Drop what the format cannot carry instead of refusing to write.
    pub fn lossy(mut self, lossy: bool) -> Self {
        self.lossy = lossy;
        self
    }

    /// Inline the font files the plot's text needs.
    ///
    /// Off by default, and expensively so: a system family is often megabytes.
    /// A consumer that can register its own fonts — a web page already serving
    /// a subsetted font — should.
    pub fn embed_fonts(mut self, embed: bool) -> Self {
        self.embed_fonts = embed;
        self
    }

    /// Write the document, reporting anything the format could not carry.
    ///
    /// With `lossy` off the same list is an error instead, so the report is
    /// non-empty only when the caller asked to degrade.
    ///
    /// # Errors
    ///
    /// Returns `GgsqlError::WriterError` if the plot cannot be composed, if it
    /// carries something the format cannot express and `lossy` is off, or if
    /// serialising fails.
    pub fn write_reporting(
        &self,
        spec: &Plot,
        data: &HashMap<String, DataFrame>,
    ) -> Result<(Vec<u8>, Vec<String>)> {
        compose::validate_plot(spec)?;
        let view = compose::build_composition(spec, data)?;
        let options = self.options();

        // Checked here rather than left to `write_composition` so the error is
        // ggsql's own and names no renderer. The list is the same either way.
        let problems = unsupported_items_for(&view, &options);
        if !problems.is_empty() && !self.lossy {
            return Err(GgsqlError::WriterError(format!(
                "this plot cannot be captured as a document: {}. Pass lossy=true to write it \
                 anyway, dropping what cannot be carried",
                describe(&problems).join("; ")
            )));
        }

        let bytes = write_composition(&view, &options)
            .map_err(|e| GgsqlError::WriterError(format!("hep write failed: {e}")))?;
        Ok((bytes, describe(&problems)))
    }

    /// [`Self::write_reporting`] from a resolved `Spec`.
    ///
    /// # Errors
    ///
    /// As [`Self::write_reporting`].
    pub fn render_reporting(&self, spec: &crate::reader::Spec) -> Result<(Vec<u8>, Vec<String>)> {
        self.write_reporting(spec.plot(), spec.data())
    }

    /// The write options this writer's settings amount to.
    ///
    /// The canvas becomes hints, which is exactly what those fields are for.
    /// `embed_images` is left off: nothing ggsql builds registers an image, so
    /// the option provably cannot take effect and exposing it would only teach
    /// a user that it exists.
    fn options(&self) -> WriteOptions {
        let mut options = WriteOptions::default();
        options.lossy = self.lossy;
        options.background = self.canvas.vector_background();
        options.size_hint = self
            .sized
            .then_some((self.canvas.width as f64, self.canvas.height as f64));
        options.dpi_hint = self.sized.then_some(self.canvas.dpi);
        options.embed_fonts = self.embed_fonts;
        options.embed_images = false;
        options
    }
}

impl Writer for HepWriter {
    type Output = Vec<u8>;

    fn from_options(options: &WriterOptions) -> Result<Self> {
        let canvas = Canvas::from_options(options, HEP_OPTIONS)?;
        // A hint is only recorded when one was actually asked for.
        let sized = CANVAS_HINT_OPTIONS
            .iter()
            .any(|key| options.get(key).is_some());
        Ok(Self {
            canvas,
            sized,
            lossy: options.boolean("lossy")?.unwrap_or(false),
            embed_fonts: options.boolean("embed-fonts")?.unwrap_or(false),
        })
    }

    fn validate(&self, spec: &Plot) -> Result<()> {
        compose::validate_plot(spec)
    }

    fn write(&self, spec: &Plot, data: &HashMap<String, DataFrame>) -> Result<Self::Output> {
        self.write_reporting(spec, data).map(|(bytes, _)| bytes)
    }
}

/// Put what the format could not carry into ggsql's own words.
///
/// `UnsupportedItem` already `Display`s actionably and names the scale, patch or
/// shape involved, so this only strips the renderer's own vocabulary from the
/// front of it.
fn describe(problems: &[UnsupportedItem]) -> Vec<String> {
    problems.iter().map(ToString::to_string).collect()
}

#[cfg(test)]
impl super::canvas::Canvased for HepWriter {
    fn canvas(&self) -> &Canvas {
        &self.canvas
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;
    use crate::writer::hephaestus::canvas::{
        assert_canvas_semantics, assert_transparent_background,
    };

    fn writer(pairs: &[&str]) -> Result<HepWriter> {
        HepWriter::from_options(&WriterOptions::parse(pairs)?)
    }

    #[test]
    fn canvas_options_behave_as_they_do_for_every_writer() {
        assert_canvas_semantics::<HepWriter>();
        assert_transparent_background::<HepWriter>();
    }

    #[test]
    fn the_default_writer_matches_no_options() {
        let default = HepWriter::default();
        assert_eq!(writer(&[]).unwrap(), default);
        assert!(!default.lossy);
        assert!(!default.embed_fonts);
    }

    #[test]
    fn a_size_is_recorded_only_when_one_was_asked_for() {
        // Any size works, so an unrecorded hint and a hint that happens to
        // equal the default are different things.
        let unset = writer(&[]).unwrap().options();
        assert_eq!(unset.size_hint, None);
        assert_eq!(unset.dpi_hint, None);

        let sized = writer(&["width=1600", "height=900"]).unwrap().options();
        assert_eq!(sized.size_hint, Some((1600.0, 900.0)));
        assert!(sized.dpi_hint.is_some());

        // A physical size resolves to pixels first, as it does everywhere.
        let physical = writer(&["width=6", "units=in", "dpi=100"])
            .unwrap()
            .options();
        assert_eq!(physical.size_hint.map(|(w, _)| w), Some(600.0));
        assert_eq!(physical.dpi_hint, Some(100.0));
    }

    #[test]
    fn the_flags_take_the_boolean_spellings() {
        assert!(writer(&["lossy=true"]).unwrap().lossy);
        assert!(writer(&["lossy=yes"]).unwrap().lossy);
        assert!(writer(&["embed-fonts=1"]).unwrap().embed_fonts);
        assert!(writer(&["embed_fonts=on"]).unwrap().embed_fonts);
        let err = writer(&["lossy=sometimes"]).unwrap_err().to_string();
        assert!(err.contains("'lossy' expects true or false"), "{err}");
    }

    #[test]
    fn images_are_not_an_option_to_ask_for() {
        // Nothing ggsql builds registers an image, so the setting provably
        // cannot take effect and is not offered.
        let err = writer(&["embed-images=true"]).unwrap_err().to_string();
        assert!(err.contains("unknown writer option"), "{err}");
        assert!(!writer(&[]).unwrap().options().embed_images);
    }
}
