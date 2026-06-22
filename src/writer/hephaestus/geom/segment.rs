//! `segment`, `range`, and `rule` geoms → hephaestus `SegmentGeom`.
//!
//! - segment: an explicit (pos1,pos2)→(pos1end,pos2end) line.
//! - range: a bar-less interval; aligned spans pos2min→pos2max at fixed pos1
//!   (transposed swaps).
//! - rule: a panel-spanning reference line at a fixed pos1 (vertical) or pos2
//!   (horizontal); the free axis uses scale-bypassing 0..1 panel fractions.

use hephaestus::color::rgb8;

use super::super::channels::aesthetic_column_name;
use super::super::scales::RangeKind;
use super::super::wiring::{Ctx, GeomSpec, MatDefault, MaterialSpec, PanelAxis, PositionSpec};
use crate::plot::layer::geom::GeomType;

pub fn spec(ctx: &Ctx) -> GeomSpec {
    let (positions, raw_numbers) = match ctx.layer.geom.geom_type() {
        GeomType::Segment => (
            vec![
                PositionSpec::new("x", "pos1", PanelAxis::X),
                PositionSpec::new("y", "pos2", PanelAxis::Y),
                PositionSpec::new("x2", "pos1end", PanelAxis::X),
                PositionSpec::new("y2", "pos2end", PanelAxis::Y),
            ],
            vec![],
        ),
        GeomType::Range if !ctx.transposed => (
            vec![
                PositionSpec::new("x", "pos1", PanelAxis::X),
                PositionSpec::new("x2", "pos1", PanelAxis::X),
                PositionSpec::new("y", "pos2min", PanelAxis::Y),
                PositionSpec::new("y2", "pos2max", PanelAxis::Y),
            ],
            vec![],
        ),
        GeomType::Range => (
            vec![
                PositionSpec::new("y", "pos2", PanelAxis::Y),
                PositionSpec::new("y2", "pos2", PanelAxis::Y),
                PositionSpec::new("x", "pos1min", PanelAxis::X),
                PositionSpec::new("x2", "pos1max", PanelAxis::X),
            ],
            vec![],
        ),
        GeomType::Rule => rule(ctx),
        _ => (Vec::new(), vec![]),
    };

    GeomSpec {
        positions,
        material: vec![
            MaterialSpec::new(
                "stroke",
                "stroke",
                RangeKind::Color,
                MatDefault::Color(rgb8(0, 0, 0)),
            ),
            MaterialSpec::new("color", "stroke", RangeKind::Color, MatDefault::None),
            MaterialSpec::new("colour", "stroke", RangeKind::Color, MatDefault::None),
            MaterialSpec::new(
                "linewidth",
                "linewidth",
                RangeKind::Number,
                MatDefault::Number(1.0),
            ),
            MaterialSpec::new(
                "opacity",
                "stroke_opacity",
                RangeKind::Number,
                MatDefault::None,
            ),
            MaterialSpec::new(
                "linetype",
                "linetype",
                RangeKind::Linetype,
                MatDefault::None,
            ),
        ],
        raw_strings: &[],
        raw_numbers,
        grouped: false,
    }
}

/// A rule is a reference line spanning the whole panel on its free axis.
/// Best-effort: the free axis uses raw 0..1 panel fractions, so no scale/axis
/// is created for it.
fn rule(ctx: &Ctx) -> (Vec<PositionSpec>, Vec<(&'static str, f64)>) {
    if aesthetic_column_name(ctx.layer, "pos1").is_some() {
        // Vertical line at x = pos1, spanning full height.
        (
            vec![
                PositionSpec::new("x", "pos1", PanelAxis::X),
                PositionSpec::new("x2", "pos1", PanelAxis::X),
            ],
            vec![("y", 0.0), ("y2", 1.0)],
        )
    } else {
        // Horizontal line at y = pos2, spanning full width.
        (
            vec![
                PositionSpec::new("y", "pos2", PanelAxis::Y),
                PositionSpec::new("y2", "pos2", PanelAxis::Y),
            ],
            vec![("x", 0.0), ("x2", 1.0)],
        )
    }
}
