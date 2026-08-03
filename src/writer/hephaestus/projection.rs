//! Apply a ggsql `PROJECT` clause (coordinate system) to the hephaestus plot,
//! including the coord-appropriate axes. Cartesian (the default when there is no
//! `PROJECT`) gets bottom/left rails; polar gets angular + radial rings.

use hephaestus::plot::chrome::axis::{Axis, AxisPlacement, PolarRing};
use hephaestus::plot::projection::{PolarProjection, Projection as HProj};
use hephaestus::plot::AspectMode;
use hephaestus::plot::Plot as HPlot;
use hephaestus::scales::chrome::AxisSide;

use super::facet::{Panel, PanelScales};
use super::wiring::aesthetic_label;
use crate::plot::projection::{coord::CoordKind, Projection};
use crate::plot::ParameterValue;
use crate::Plot;

/// Apply the plot's coordinate system to one panel. No `PROJECT` clause is
/// treated as Cartesian.
pub fn apply_projection(plot: HPlot, spec: &Plot, panel: &Panel, ps: &PanelScales) -> HPlot {
    match spec.project.as_ref().map(|p| p.coord.coord_kind()) {
        None | Some(CoordKind::Cartesian) => {
            apply_proj_cartesian(plot, spec.project.as_ref(), spec, panel, ps)
        }
        Some(CoordKind::Polar) => apply_proj_polar(plot, spec.project.as_ref().unwrap(), spec, ps),
        Some(CoordKind::Map) => apply_proj_map(plot, spec.project.as_ref().unwrap()),
    }
}

fn apply_proj_cartesian(
    mut plot: HPlot,
    proj: Option<&Projection>,
    spec: &Plot,
    panel: &Panel,
    ps: &PanelScales,
) -> HPlot {
    if let Some(proj) = proj {
        if let Some(ParameterValue::Boolean(false)) = proj.properties.get("clip") {
            plot = plot.clip(false);
        }
        if let Some(ParameterValue::Number(ratio)) = proj.properties.get("ratio") {
            plot = plot.aspect_ratio(*ratio).aspect_mode(AspectMode::Range);
        }
    }
    // Edge-only axes for fixed scales (ggplot2 look): x on the bottom-most panel
    // of each column, y on the left column. A free dimension has a per-panel
    // domain, so its axis is drawn on every panel.
    if panel.last_row || ps.free_x {
        add_cartesian_axis(&mut plot, spec, "pos1", &ps.pos1, AxisSide::Bottom);
    }
    if panel.first_col || ps.free_y {
        add_cartesian_axis(&mut plot, spec, "pos2", &ps.pos2, AxisSide::Left);
    }
    plot
}

/// Whether a position scale warrants an axis. A missing scale, or a synthetic
/// single-category dummy (`__ggsql_stat_dummy` — e.g. a bar with no x mapped, or
/// a pie's radius), gets none; drawing it would expose the internal placeholder.
/// Mirrors the Vega-Lite writer's `AxisInfo::suppress`.
fn has_real_axis(spec: &Plot, name: &str) -> bool {
    spec.find_scale(name).is_some_and(|s| !s.is_dummy())
}

/// Add one bottom/left rail bound to `scale_name`, titled from the plot's labels
/// (or the first layer's mapped column, keyed by `aesthetic`). Skipped for absent
/// or dummy scales. `aesthetic` is the ggsql position name (`pos1`/`pos2`);
/// `scale_name` is the registered scale the rail reads (they differ only for a
/// free per-panel scale).
fn add_cartesian_axis(
    plot: &mut HPlot,
    spec: &Plot,
    aesthetic: &str,
    scale_name: &str,
    side: AxisSide,
) {
    if !has_real_axis(spec, aesthetic) {
        return;
    }
    let mut rail = Axis::rail(scale_name, AxisPlacement::Cartesian(side));
    if let Some(layer) = spec.layers.first() {
        if let Some(title) = aesthetic_label(spec, layer, aesthetic) {
            rail = rail.title(title);
        }
    }
    plot.add_axis(rail);
}

fn apply_proj_polar(mut plot: HPlot, proj: &Projection, spec: &Plot, ps: &PanelScales) -> HPlot {
    plot.clear_axes();
    if let Some(ParameterValue::Boolean(false)) = proj.properties.get("clip") {
        plot = plot.clip(false);
    }
    // Sweep angles in degrees, clockwise from 12 o'clock (ggplot2 / Vega-Lite
    // pie convention). `start` defaults to 0 (12 o'clock); `end` defaults to a
    // full turn past `start`, so setting only `start` rotates a full circle
    // rather than truncating it (matches the VL writer's `start + 360`).
    let base = PolarProjection::full_circle();
    let num = |k| match proj.properties.get(k) {
        Some(ParameterValue::Number(n)) => Some(*n),
        _ => None,
    };
    let start_deg = num("start").unwrap_or(0.0);
    let end_deg = num("end").unwrap_or(start_deg + 360.0);
    let deg = |d: f64| base.theta_start - d * std::f64::consts::PI / 180.0;
    let start = deg(start_deg);
    let end = deg(end_deg);
    let inner = num("inner").unwrap_or(0.0);
    // ggsql assigns pos1→radius, pos2→theta (as the Vega-Lite writer does), so a
    // value on `y` (pos2) drives the slice angle and `x` (pos1) the radius.
    plot = plot.projection(HProj::Polar(PolarProjection {
        angle_channel: "y".into(),
        radius_channel: "x".into(),
        theta_start: start,
        theta_end: end,
        inner_radius_frac: inner,
        ..base
    }));
    // Suppress an axis whose position scale is a synthetic dummy (e.g. a pie's
    // radius), same as the Cartesian path.
    if has_real_axis(spec, "pos2") {
        plot.add_axis(Axis::rail(
            ps.pos2.as_str(),
            AxisPlacement::PolarAngular(PolarRing::Outer),
        ));
    }
    if has_real_axis(spec, "pos1") {
        plot.add_axis(Axis::rail(
            ps.pos1.as_str(),
            AxisPlacement::PolarRadius { theta_frac: start },
        ));
    }
    plot
}

fn apply_proj_map(plot: HPlot, _proj: &Projection) -> HPlot {
    plot
}
