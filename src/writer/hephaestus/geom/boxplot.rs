//! `boxplot` composite geom. ggsql's stat emits one row per (category,
//! component); the component is tagged by the `type` aesthetic. We decompose
//! into: box (`RectGeom`, q1→q3 filling the category band), whiskers
//! (`SegmentGeom`, box edge → fence), median (`SegmentGeom` spanning the band),
//! and outliers (`PointGeom`). All components share the `pos1`/`pos2` scales.

use hephaestus::color::rgb8;
use hephaestus::plot::{Plot as HPlot, PointGeom, RectGeom, SegmentGeom};

use super::super::channels::{
    aesthetic_column_name, column_to_channel, column_to_f64, column_to_strings,
};
use super::super::wiring::{
    band_half_width, constant_number, constant_string, dodge_offsets, resolve_color, Ctx,
    LegendKind,
};
use crate::{GgsqlError, Result};

pub fn build(plot: &mut HPlot, ctx: &Ctx) -> Result<()> {
    let (layer, df) = (ctx.layer, ctx.df);
    let n = df.height();

    let pos1 = require(layer, "pos1")?;
    let type_col = require(layer, "type")?;
    let value = require(layer, "pos2")?;
    let value2 = require(layer, "pos2end")?;

    let p1 = column_to_channel(df, pos1)?;
    let p2 = column_to_f64(df, value)?;
    let p2e = column_to_f64(df, value2)?;
    let types = column_to_strings(df, type_col)?;

    let rows_of = |t: &str| -> Vec<usize> { (0..n).filter(|&i| types[i] == t).collect() };
    let box_i = rows_of("box");
    let med_i = rows_of("median");
    let whisk_i: Vec<usize> = (0..n)
        .filter(|&i| types[i] == "lower_whisker" || types[i] == "upper_whisker")
        .collect();
    let out_i = rows_of("outlier");

    // Bind the position channels (panel-aware for free facet scales).
    for (channel, scale) in [
        ("x", ctx.pos1_scale),
        ("x2", ctx.pos1_scale),
        ("y", ctx.pos2_scale),
        ("y2", ctx.pos2_scale),
    ] {
        plot.set_binding(channel, scale);
    }

    // Resolve fill + stroke once (data-mapped → shared scale/legend, else
    // constant), mirroring the VL writer's shared-encoding model: every
    // component draws with the same resolved fill/stroke.
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
    let alpha = constant_number(ctx, "opacity", 1.0);
    // Box width (band fraction, dodge-aware) + per-row dodge offsets.
    let offsets = dodge_offsets(df, "pos1offset");
    let half = band_half_width(layer, 0.75);

    // Box: a rect from q1 to q3 occupying `width` of the band (dodge-offset).
    if !box_i.is_empty() {
        let mut b = RectGeom::builder();
        p1.select(&box_i).apply(&mut b, "x");
        p1.select(&box_i).apply(&mut b, "x2");
        b.set("y", pick(&p2, &box_i));
        b.set("y2", pick(&p2e, &box_i));
        b.set("x_band", shift(&offsets, &box_i, -half));
        b.set("x2_band", shift(&offsets, &box_i, half));
        fill.apply(&mut b, "fill", &box_i);
        stroke.apply(&mut b, "stroke", &box_i);
        b.set("fill_opacity", alpha);
        plot.add_geom(b.build());
    }

    // Whiskers: vertical segments at the band centre, box edge → fence.
    if !whisk_i.is_empty() {
        let mut b = SegmentGeom::builder();
        p1.select(&whisk_i).apply(&mut b, "x");
        p1.select(&whisk_i).apply(&mut b, "x2");
        b.set("y", pick(&p2, &whisk_i));
        b.set("y2", pick(&p2e, &whisk_i));
        b.set("x_band", shift(&offsets, &whisk_i, 0.0));
        b.set("x2_band", shift(&offsets, &whisk_i, 0.0));
        stroke.apply(&mut b, "stroke", &whisk_i);
        plot.add_geom(b.build());
    }

    // Median: a horizontal segment spanning the band at the median value.
    if !med_i.is_empty() {
        let mut b = SegmentGeom::builder();
        p1.select(&med_i).apply(&mut b, "x");
        p1.select(&med_i).apply(&mut b, "x2");
        b.set("y", pick(&p2, &med_i));
        b.set("y2", pick(&p2, &med_i));
        b.set("x_band", shift(&offsets, &med_i, -half));
        b.set("x2_band", shift(&offsets, &med_i, half));
        stroke.apply(&mut b, "stroke", &med_i);
        b.set("linewidth", 1.5_f64);
        plot.add_geom(b.build());
    }

    // Outliers: hollow points (stroke only, matching VL's `filled = false`)
    // at their value, honoring the `size`/`shape` aesthetics.
    if !out_i.is_empty() {
        let mut b = PointGeom::builder();
        p1.select(&out_i).apply(&mut b, "x");
        b.set("y", pick(&p2, &out_i));
        b.set("x_band", shift(&offsets, &out_i, 0.0));
        stroke.apply(&mut b, "stroke", &out_i);
        b.set("size", constant_number(ctx, "size", 3.0));
        b.set("shape", constant_string(ctx, "shape", "circle"));
        plot.add_geom(b.build());
    }

    Ok(())
}

fn require<'a>(layer: &'a crate::Layer, aesthetic: &str) -> Result<&'a str> {
    aesthetic_column_name(layer, aesthetic)
        .ok_or_else(|| GgsqlError::WriterError(format!("boxplot layer has no {aesthetic} mapping")))
}

/// Select rows by index.
fn pick(v: &[f64], idx: &[usize]) -> Vec<f64> {
    idx.iter().map(|&i| v[i]).collect()
}

/// Per-row band offsets for the selected rows, shifted by `delta` (e.g. ±half
/// the box width for the two edges, 0 for a centered line/point).
fn shift(offsets: &[f64], idx: &[usize], delta: f64) -> Vec<f64> {
    idx.iter().map(|&i| offsets[i] + delta).collect()
}
