//! `violin` composite geom. ggsql's stat emits a KDE grid per category
//! (`pos1` = category, `pos2` = value, `offset` = pre-scaled half-width). We
//! render one vertical `RibbonGeom` band per category: the right edge sits at
//! `+offset` and the left edge at `-offset` of the category band (via the
//! ribbon's per-row `x_band` / `x2_band` channels), sharing `y = pos2`. One row
//! per grid sample — no hand-built outline.

use std::cmp::Ordering;
use std::collections::HashMap;

use hephaestus::color::rgb8;
use hephaestus::plot::{Plot as HPlot, RibbonGeom};

use super::super::channels::{
    aesthetic_column_name, column_to_channel, column_to_f64, column_to_strings,
};
use super::super::wiring::{
    constant_number, dodge_offsets, register_axis, resolve_color, Ctx, LegendKind, PanelAxis,
    Wiring,
};
use crate::{GgsqlError, Result};

pub fn build(plot: &mut HPlot, ctx: &Ctx, w: &mut Wiring) -> Result<()> {
    let (layer, df) = (ctx.layer, ctx.df);

    let pos1 = require(layer, "pos1")?;
    let pos2 = require(layer, "pos2")?;
    let offset = require(layer, "offset")?;

    let p1 = column_to_channel(df, pos1)?;
    let cat = column_to_strings(df, pos1)?; // grouping key per row
    let p2 = column_to_f64(df, pos2)?;
    let off = column_to_f64(df, offset)?;

    // Order rows so each category's band is contiguous and ascending in pos2
    // (RibbonGeom connects a mark's rows in source order).
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for (i, c) in cat.iter().enumerate() {
        let g = *index.entry(c.as_str()).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[g].push(i);
    }
    let mut order: Vec<usize> = Vec::with_capacity(cat.len());
    for rows in &mut groups {
        rows.sort_by(|&a, &b| p2[a].partial_cmp(&p2[b]).unwrap_or(Ordering::Equal));
        order.extend_from_slice(rows);
    }

    register_axis(ctx, w, PanelAxis::X, p1.extent());
    register_axis(ctx, w, PanelAxis::Y, finite_extent(&p2));
    for (channel, scale) in [("x", "pos1"), ("x2", "pos1"), ("y", "pos2")] {
        w.bindings.push((channel, scale.to_string()));
    }

    // One vertical ribbon per category: right edge +offset, left edge -offset,
    // both shifted by the dodge offset (zero when not dodged).
    let dodge = dodge_offsets(df, "pos1offset");
    let keys: Vec<String> = order.iter().map(|&i| cat[i].clone()).collect();
    let x_band: Vec<f64> = order.iter().map(|&i| dodge[i] + off[i]).collect();
    let x2_band: Vec<f64> = order.iter().map(|&i| dodge[i] - off[i]).collect();
    let ys: Vec<f64> = order.iter().map(|&i| p2[i]).collect();

    // Resolve fill + stroke once (data-mapped → shared scale/legend, else
    // constant), mirroring the VL writer's shared-encoding model.
    let fill = resolve_color(
        ctx,
        w,
        "fill",
        "fill",
        rgb8(255, 255, 255),
        LegendKind::Rect,
    )?;
    let stroke = resolve_color(
        ctx,
        w,
        "stroke",
        "stroke",
        rgb8(60, 60, 60),
        LegendKind::Rect,
    )?;
    // The ribbon's two edges share the stroke scale (`stroke2` is the far edge).
    if let Some(name) = stroke.scale_name() {
        w.bindings.push(("stroke2", name.to_string()));
    }

    let mut b = RibbonGeom::builder();
    b.keys(keys);
    p1.select(&order).apply(&mut b, "x");
    p1.select(&order).apply(&mut b, "x2");
    b.set("x_band", x_band);
    b.set("x2_band", x2_band);
    b.set("y", ys);
    fill.apply(&mut b, "fill", &order);
    stroke.apply(&mut b, "stroke", &order);
    stroke.apply(&mut b, "stroke2", &order);
    b.set("alpha", constant_number(ctx, "opacity", 1.0));
    plot.add_geom(b.build());

    Ok(())
}

fn require<'a>(layer: &'a crate::Layer, aesthetic: &str) -> Result<&'a str> {
    aesthetic_column_name(layer, aesthetic)
        .ok_or_else(|| GgsqlError::WriterError(format!("violin layer has no {aesthetic} mapping")))
}

/// Finite (min, max), or `(0, 1)` if no finite values.
fn finite_extent(v: &[f64]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &x in v {
        if x.is_finite() {
            min = min.min(x);
            max = max.max(x);
        }
    }
    if min <= max {
        (min, max)
    } else {
        (0.0, 1.0)
    }
}
