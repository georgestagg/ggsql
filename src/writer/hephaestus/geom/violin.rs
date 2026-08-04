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
use super::super::scales::RangeKind;
use super::super::wiring::{
    constant_number, dodge_offsets, resolve_color, resolve_material, Ctx, LegendKind,
};
use crate::{GgsqlError, Result};

pub fn build(plot: &mut HPlot, ctx: &Ctx) -> Result<()> {
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

    for (channel, scale) in [
        ("x", ctx.pos1_scale),
        ("x2", ctx.pos1_scale),
        ("y", ctx.pos2_scale),
    ] {
        plot.set_binding(channel, scale);
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
        plot,
        "fill",
        "fill",
        rgb8(255, 255, 255),
        LegendKind::Rect,
    )?;
    let stroke = resolve_color(
        ctx,
        plot,
        "stroke",
        "stroke",
        rgb8(60, 60, 60),
        LegendKind::Rect,
    )?;
    // Outline width + dash pattern, applied to both ribbon edges.
    let linewidth = resolve_material(
        ctx,
        plot,
        "linewidth",
        "linewidth",
        RangeKind::Number,
        LegendKind::Line,
    )?;
    let linetype = resolve_material(
        ctx,
        plot,
        "linetype",
        "linetype",
        RangeKind::Linetype,
        LegendKind::Line,
    )?;
    // The ribbon's two edges share each outline scale (the `2` suffix is the far
    // edge), so a data-mapped stroke/width/dash styles both sides alike.
    for (source, channel) in [
        (Some(&stroke), "stroke2"),
        (linewidth.as_ref(), "linewidth2"),
        (linetype.as_ref(), "linetype2"),
    ] {
        if let Some(name) = source.and_then(|s| s.scale_name()) {
            plot.set_binding(channel, name);
        }
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
    // `RibbonGeom` resolves its outline channels once per mark (from the mark's
    // first row), so a data-mapped width/dash varies per violin, not per vertex.
    for (source, channels) in [
        (linewidth.as_ref(), ["linewidth", "linewidth2"]),
        (linetype.as_ref(), ["linetype", "linetype2"]),
    ] {
        if let Some(source) = source {
            for channel in channels {
                source.apply(&mut b, channel, &order);
            }
        }
    }
    b.set("alpha", constant_number(ctx, "opacity", 1.0));
    plot.add_geom(b.build());

    Ok(())
}

fn require<'a>(layer: &'a crate::Layer, aesthetic: &str) -> Result<&'a str> {
    aesthetic_column_name(layer, aesthetic)
        .ok_or_else(|| GgsqlError::WriterError(format!("violin layer has no {aesthetic} mapping")))
}
