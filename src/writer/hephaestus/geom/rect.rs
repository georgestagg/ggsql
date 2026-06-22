//! `bar`, `histogram`, and `tile` geoms → hephaestus `RectGeom`.
//!
//! Bars fill their category band (RectGeom's discrete `x_band` defaults ±0.5);
//! histograms span explicit bin edges; tiles span min/max extents. Bars and
//! histograms are orientation-aware (transposed swaps the value axis to x).

use hephaestus::color::rgb8;

use super::super::channels::aesthetic_column_name;
use super::super::scales::RangeKind;
use super::super::wiring::{Ctx, GeomSpec, MatDefault, MaterialSpec, PanelAxis, PositionSpec};
use crate::plot::layer::geom::GeomType;

pub fn spec(ctx: &Ctx) -> GeomSpec {
    let (positions, raw_numbers) = match ctx.layer.geom.geom_type() {
        GeomType::Bar => (bar(ctx.transposed), vec![]),
        GeomType::Histogram => (histogram(ctx.transposed), vec![]),
        GeomType::Tile => tile(ctx),
        _ => (Vec::new(), vec![]),
    };

    GeomSpec {
        positions,
        material: vec![
            MaterialSpec::new(
                "fill",
                "fill",
                RangeKind::Color,
                MatDefault::Color(rgb8(0, 0, 0)),
            ),
            MaterialSpec::new("color", "fill", RangeKind::Color, MatDefault::None),
            MaterialSpec::new("colour", "fill", RangeKind::Color, MatDefault::None),
            MaterialSpec::new("stroke", "stroke", RangeKind::Color, MatDefault::None),
            MaterialSpec::new(
                "opacity",
                "fill_opacity",
                RangeKind::Number,
                MatDefault::Number(0.8),
            ),
            MaterialSpec::new(
                "linewidth",
                "linewidth",
                RangeKind::Number,
                MatDefault::None,
            ),
        ],
        raw_strings: &[],
        raw_numbers,
        grouped: false,
    }
}

/// Categorical bar: the main axis is a band (x and x2 share the category
/// column; band defaults fill the cell); the value axis runs baseline→value.
fn bar(transposed: bool) -> Vec<PositionSpec> {
    if !transposed {
        vec![
            PositionSpec::new("x", "pos1", PanelAxis::X),
            PositionSpec::new("x2", "pos1", PanelAxis::X),
            PositionSpec::new("y", "pos2end", PanelAxis::Y),
            PositionSpec::new("y2", "pos2", PanelAxis::Y),
        ]
    } else {
        vec![
            PositionSpec::new("y", "pos2", PanelAxis::Y),
            PositionSpec::new("y2", "pos2", PanelAxis::Y),
            PositionSpec::new("x", "pos1end", PanelAxis::X),
            PositionSpec::new("x2", "pos1", PanelAxis::X),
        ]
    }
}

/// Histogram: bins span explicit edges on the main axis, value runs baseline→count.
fn histogram(transposed: bool) -> Vec<PositionSpec> {
    if !transposed {
        vec![
            PositionSpec::new("x", "pos1", PanelAxis::X),
            PositionSpec::new("x2", "pos1end", PanelAxis::X),
            PositionSpec::new("y", "pos2end", PanelAxis::Y),
            PositionSpec::new("y2", "pos2", PanelAxis::Y),
        ]
    } else {
        vec![
            PositionSpec::new("y", "pos2", PanelAxis::Y),
            PositionSpec::new("y2", "pos2end", PanelAxis::Y),
            PositionSpec::new("x", "pos1end", PanelAxis::X),
            PositionSpec::new("x2", "pos1", PanelAxis::X),
        ]
    }
}

/// Tile/heatmap: continuous tiles span min/max extents; discrete tiles fill the
/// category band on both axes.
fn tile(ctx: &Ctx) -> (Vec<PositionSpec>, Vec<(&'static str, f64)>) {
    if aesthetic_column_name(ctx.layer, "pos1min").is_some() {
        (
            vec![
                PositionSpec::new("x", "pos1min", PanelAxis::X),
                PositionSpec::new("x2", "pos1max", PanelAxis::X),
                PositionSpec::new("y", "pos2min", PanelAxis::Y),
                PositionSpec::new("y2", "pos2max", PanelAxis::Y),
            ],
            vec![],
        )
    } else {
        // Discrete tile: x band defaults (±0.5) fill x; add y bands for y.
        (
            vec![
                PositionSpec::new("x", "pos1", PanelAxis::X),
                PositionSpec::new("x2", "pos1", PanelAxis::X),
                PositionSpec::new("y", "pos2", PanelAxis::Y),
                PositionSpec::new("y2", "pos2", PanelAxis::Y),
            ],
            vec![("y_band", -0.5), ("y2_band", 0.5)],
        )
    }
}
