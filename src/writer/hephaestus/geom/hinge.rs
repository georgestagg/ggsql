//! End caps — the `hinge` SETTING shared by `boxplot` (whisker caps, default
//! off) and `range` (interval caps, default 10pt).
//!
//! A cap is a short `SegmentGeom` drawn across the banded axis at an interval
//! endpoint. Its length is given in **points**, not in data or band units, so it
//! keeps a fixed size at any panel width — the raster analog of the Vega-Lite
//! writer's `tick` mark with a pixel `size`. Under `side != 'both'` the cap is
//! halved and drawn on the chosen side only, like the box it belongs to.

use hephaestus::plot::geom::GeomBuilder;
use hephaestus::plot::SegmentGeom;

use super::super::channels::ChannelData;
use super::super::wiring::{band_edges, side_sign, BandAxes, Ctx};
use crate::plot::ParameterValue;
use crate::Layer;

/// The `hinge` SETTING (cap width in points), or `None` when it is `null` (a
/// boxplot's default) or zero — both meaning "no caps".
pub fn hinge_points(layer: &Layer) -> Option<f64> {
    match layer.parameters.get("hinge")? {
        ParameterValue::Number(pts) if *pts > 0.0 => Some(*pts),
        _ => None,
    }
}

/// A cap per row: a segment centred on the row's banded-axis position (`band`,
/// plus its per-row dodge `offsets`) at value-axis position `values`, spanning
/// `hinge` points across the band.
///
/// Returns the builder with only its positions set, so the caller can style the
/// caps' stroke to match the mark they belong to (a composite's pre-resolved
/// material, or the generic material table) before adding it to the plot.
pub fn caps(
    ctx: &Ctx,
    axes: BandAxes,
    band: ChannelData,
    values: Vec<f64>,
    offsets: Vec<f64>,
    hinge: f64,
) -> GeomBuilder<SegmentGeom> {
    let (band_ch, band_ch2) = axes.band_channels();
    let (frac_ch, frac_ch2) = axes.band_fraction_channels();
    let (offset_ch, offset_ch2) = axes.band_offset_channels();
    let (value_ch, value_ch2) = axes.value_channels();
    // A cap spans `hinge` points across the band, halved and pushed to one side
    // when `side` selects a half-band.
    let (near, far) = band_edges(hinge / 2.0, side_sign(ctx.layer));

    let mut b = SegmentGeom::builder();
    band.clone().apply(&mut b, band_ch);
    band.apply(&mut b, band_ch2);
    b.set(frac_ch, offsets.clone());
    b.set(frac_ch2, offsets);
    b.set(offset_ch, near);
    b.set(offset_ch2, far);
    b.set(value_ch, values.clone());
    b.set(value_ch2, values);
    b
}
