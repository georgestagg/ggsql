//! `segment`, `range`, and `rule` geoms → hephaestus `SegmentGeom`.
//!
//! - segment: an explicit (pos1,pos2)→(pos1end,pos2end) line.
//! - range: a bar-less interval; aligned spans pos2min→pos2max at fixed pos1
//!   (transposed swaps).
//! - rule: a panel-spanning reference line at a fixed pos1 (vertical) or pos2
//!   (horizontal); the free axis uses scale-bypassing 0..1 panel fractions.

use hephaestus::color::rgb8;
use hephaestus::plot::{Plot as HPlot, SegmentGeom};

use super::super::channels::aesthetic_column_name;
use super::super::scales::RangeKind;
use super::super::wiring::{
    constant_color, constant_number, register_axis, Ctx, GeomSpec, LegendKind, MatDefault,
    MaterialSpec, PanelAxis, PositionSpec, Wiring,
};
use crate::plot::layer::geom::GeomType;
use crate::plot::ParameterValue;
use crate::{Layer, Result};

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
        data_channels: vec![],
        legend_key: LegendKind::Line,
        grouped: false,
    }
}

/// Whether this rule is a diagonal (abline): has a non-zero `slope`.
pub fn is_diagonal(layer: &Layer) -> bool {
    matches!(
        layer.parameters.get("diagonal"),
        Some(ParameterValue::Boolean(true))
    )
}

/// A diagonal rule (abline): a single line spanning the position scales'
/// resolved range, with `secondary = slope * primary + intercept`. The range
/// comes straight from the scales (explicit `FROM` or data-trained); when a
/// scale is unresolved it falls back to 0..1 like any continuous scale.
pub fn build_diagonal(plot: &mut HPlot, ctx: &Ctx, w: &mut Wiring) -> Result<()> {
    let slope = slope_value(ctx);

    let (x0, y0, x1, y1, x_extent, y_extent) = if !ctx.transposed {
        // y-intercept (`pos2`); x is the spanning axis.
        let intercept = constant_number(ctx, "pos2", 0.0);
        let (x0, x1) = primary_range(ctx, "pos1");
        let (y0, y1) = (slope * x0 + intercept, slope * x1 + intercept);
        (x0, y0, x1, y1, (x0, x1), (y0.min(y1), y0.max(y1)))
    } else {
        // x-intercept (`pos1`); y is the spanning axis.
        let intercept = constant_number(ctx, "pos1", 0.0);
        let (y0, y1) = primary_range(ctx, "pos2");
        let (x0, x1) = (slope * y0 + intercept, slope * y1 + intercept);
        (x0, y0, x1, y1, (x0.min(x1), x0.max(x1)), (y0, y1))
    };

    register_axis(ctx, w, PanelAxis::X, x_extent);
    register_axis(ctx, w, PanelAxis::Y, y_extent);
    for (channel, scale) in [("x", "pos1"), ("x2", "pos1"), ("y", "pos2"), ("y2", "pos2")] {
        w.bindings.push((channel, scale.to_string()));
    }

    let mut b = SegmentGeom::builder();
    b.set("x", vec![x0]);
    b.set("x2", vec![x1]);
    b.set("y", vec![y0]);
    b.set("y2", vec![y1]);
    b.set("stroke", constant_color(ctx, "stroke", rgb8(0, 0, 0)));
    b.set("linewidth", constant_number(ctx, "linewidth", 1.0));
    b.set("stroke_opacity", constant_number(ctx, "opacity", 1.0));
    plot.add_geom(b.build());
    Ok(())
}

/// Resolved (min, max) for a position scale, or 0..1 when unresolved.
fn primary_range(ctx: &Ctx, aesthetic: &str) -> (f64, f64) {
    ctx.spec
        .find_scale(aesthetic)
        .and_then(|s| s.numeric_domain())
        .unwrap_or((0.0, 1.0))
}

/// Slope from the `slope` aesthetic (literal/annotation) or the SETTING param.
fn slope_value(ctx: &Ctx) -> f64 {
    let param = match ctx.layer.parameters.get("slope") {
        Some(ParameterValue::Number(n)) => *n,
        _ => 0.0,
    };
    constant_number(ctx, "slope", param)
}

/// A non-diagonal rule is a reference line spanning the whole panel on its free
/// axis. The free axis uses raw 0..1 panel fractions, so no scale/axis is
/// created for it.
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
