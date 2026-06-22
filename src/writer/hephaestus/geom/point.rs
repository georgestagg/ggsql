//! `point` geom: one marker per row.

use hephaestus::color::Color;
use hephaestus::plot::PointGeom;

/// Build a `PointGeom` from x/y panel data with a constant fill and size.
///
/// Phase 1 treats `fill`/`size` as constants; data- and literal-mapped visual
/// channels arrive in Phase 2.
pub fn build(xs: &[f64], ys: &[f64], fill: Color, size: f64) -> PointGeom {
    PointGeom::builder()
        .set("x", xs.to_vec())
        .set("y", ys.to_vec())
        .set("fill", fill)
        .set("size", size)
        .build()
}
