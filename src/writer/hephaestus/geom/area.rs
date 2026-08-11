//! `area`, `ribbon`, and `density` geoms → hephaestus `RibbonGeom` (a filled
//! band between two curves). Orientation-aware: aligned bands run along x with
//! the extent on y (`y`/`y2`); transposed bands run along y with the extent on
//! x (`x`/`x2`).

use hephaestus::color::rgb8;

use super::super::scales::RangeKind;
use super::super::wiring::{
    Ctx, GeomSpec, LegendKind, MatDefault, MaterialSpec, PanelAxis, PositionSpec,
};
use crate::plot::layer::geom::GeomType;

pub fn spec(ctx: &Ctx) -> GeomSpec {
    let ribbon = ctx.layer.geom.geom_type() == GeomType::Ribbon;

    let positions = if !ctx.transposed {
        // Band along x; extent on y. ribbon → [pos2min, pos2max]; area/density
        // → [pos2end (baseline), pos2].
        let (lo, hi) = if ribbon {
            ("pos2min", "pos2max")
        } else {
            ("pos2end", "pos2")
        };
        vec![
            PositionSpec::new("x", "pos1", PanelAxis::X),
            PositionSpec::new("y", lo, PanelAxis::Y),
            PositionSpec::new("y2", hi, PanelAxis::Y),
        ]
    } else {
        // Band along y; extent on x.
        let (lo, hi) = if ribbon {
            ("pos1min", "pos1max")
        } else {
            ("pos1end", "pos1")
        };
        vec![
            PositionSpec::new("y", "pos2", PanelAxis::Y),
            PositionSpec::new("x", lo, PanelAxis::X),
            PositionSpec::new("x2", hi, PanelAxis::X),
        ]
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
            // A ribbon's two edge curves are stroked independently: `stroke`
            // outlines curve A (the baseline / lower edge), `stroke2` curve B
            // (the data curve). Wiring only the first leaves the band's visible
            // silhouette unbordered, so every outline aesthetic is sent to both.
            MaterialSpec::new("stroke", "stroke", RangeKind::Color, MatDefault::None),
            MaterialSpec::new("stroke", "stroke2", RangeKind::Color, MatDefault::None),
            MaterialSpec::new(
                "opacity",
                "alpha",
                RangeKind::Number,
                MatDefault::Number(0.8),
            ),
            MaterialSpec::new(
                "linewidth",
                "linewidth",
                RangeKind::Number,
                MatDefault::None,
            ),
            MaterialSpec::new(
                "linewidth",
                "linewidth2",
                RangeKind::Number,
                MatDefault::None,
            ),
            MaterialSpec::new(
                "linetype",
                "linetype",
                RangeKind::Linetype,
                MatDefault::None,
            ),
            MaterialSpec::new(
                "linetype",
                "linetype2",
                RangeKind::Linetype,
                MatDefault::None,
            ),
        ],
        raw_strings: &[],
        raw_numbers: vec![],
        data_channels: vec![],
        legend_key: LegendKind::Rect,
        grouped: true,
    }
}
