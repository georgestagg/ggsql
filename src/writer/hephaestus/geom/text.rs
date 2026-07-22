//! `text` geom → hephaestus `TextGeom`. A custom builder (not the generic
//! position/material path) because several text aesthetics need conversion:
//! `vjust` flips for hephaestus's top-origin `anchor_y`, `rotation` is degrees
//! → radians, `fontweight` accepts CSS keywords, and `italic` is boolean.

use std::f64::consts::PI;

use hephaestus::color::rgb8;
use hephaestus::plot::geom::Raw;
use hephaestus::plot::{Plot as HPlot, TextGeom};

use super::super::channels::{
    aesthetic_column_name, column_to_bool, column_to_channel, column_to_f64, column_to_strings,
};
use super::super::wiring::{resolve_color, Ctx, LegendKind};
use crate::{GgsqlError, Result};

pub fn build(plot: &mut HPlot, ctx: &Ctx) -> Result<()> {
    let (layer, df) = (ctx.layer, ctx.df);
    let n = df.height();

    let pos1 = require(layer, "pos1")?;
    let pos2 = require(layer, "pos2")?;
    let label = require(layer, "label")?;

    let mut b = TextGeom::builder();

    // Positions: bind to the global pos1/pos2 scales.
    let p1 = column_to_channel(df, pos1)?;
    let p2 = column_to_channel(df, pos2)?;
    plot.set_binding("x", "pos1");
    plot.set_binding("y", "pos2");
    p1.apply(&mut b, "x");
    p2.apply(&mut b, "y");

    // Label string.
    b.set("text", Raw(column_to_strings(df, label)?));

    // Color: data-mapped (color-by-group) or constant black.
    resolve_color(ctx, plot, "fill", "fill", rgb8(0, 0, 0), LegendKind::Point)?.apply(
        &mut b,
        "fill",
        &(0..n).collect::<Vec<_>>(),
    );

    // Scalar styling (unscaled visual values).
    b.set("fill_opacity", Raw(numeric_or(ctx, "opacity", 1.0)));
    b.set("size", Raw(numeric_or(ctx, "fontsize", 11.0)));
    b.set("anchor_x", Raw(numeric_or(ctx, "hjust", 0.5)));
    // ggsql vjust: 0 = bottom, 1 = top; hephaestus anchor_y: 0 = top, 1 = bottom.
    let anchor_y: Vec<f64> = numeric_or(ctx, "vjust", 0.5)
        .iter()
        .map(|v| 1.0 - v)
        .collect();
    b.set("anchor_y", Raw(anchor_y));
    // ggsql rotation is in degrees; hephaestus angle is radians (math CCW).
    let angle: Vec<f64> = numeric_or(ctx, "rotation", 0.0)
        .iter()
        .map(|d| d * PI / 180.0)
        .collect();
    b.set("angle", Raw(angle));
    b.set("weight", Raw(weights(ctx, n)?));
    b.set("italic", Raw(italics(ctx, n)?));
    if let Some(col) = aesthetic_column_name(layer, "typeface") {
        b.set("family", Raw(column_to_strings(df, col)?));
    }

    plot.add_geom(b.build());
    Ok(())
}

fn require<'a>(layer: &'a crate::Layer, aesthetic: &str) -> Result<&'a str> {
    aesthetic_column_name(layer, aesthetic)
        .ok_or_else(|| GgsqlError::WriterError(format!("text layer has no {aesthetic} mapping")))
}

/// A per-row numeric aesthetic, or `default` repeated when it isn't mapped.
fn numeric_or(ctx: &Ctx, aesthetic: &str, default: f64) -> Vec<f64> {
    match aesthetic_column_name(ctx.layer, aesthetic) {
        Some(col) => column_to_f64(ctx.df, col).unwrap_or_else(|_| vec![default; ctx.df.height()]),
        None => vec![default; ctx.df.height()],
    }
}

/// Font weights as numeric 100–900 (CSS keywords parsed), default 400.
fn weights(ctx: &Ctx, n: usize) -> Result<Vec<f64>> {
    match aesthetic_column_name(ctx.layer, "fontweight") {
        Some(col) => Ok(column_to_strings(ctx.df, col)?
            .iter()
            .map(|s| parse_weight(s))
            .collect()),
        None => Ok(vec![400.0; n]),
    }
}

/// Parse a CSS font-weight keyword or numeric string to 100–900.
fn parse_weight(value: &str) -> f64 {
    if let Ok(n) = value.parse::<f64>() {
        return n;
    }
    match value.to_lowercase().replace('-', "").as_str() {
        "thin" | "hairline" => 100.0,
        "extralight" | "ultralight" => 200.0,
        "light" => 300.0,
        "medium" => 500.0,
        "semibold" | "demibold" => 600.0,
        "bold" | "bolder" => 700.0,
        "extrabold" | "ultrabold" => 800.0,
        "black" | "heavy" => 900.0,
        _ => 400.0, // normal / regular / unknown
    }
}

/// Italic flags, default false.
fn italics(ctx: &Ctx, n: usize) -> Result<Vec<bool>> {
    match aesthetic_column_name(ctx.layer, "italic") {
        Some(col) => column_to_bool(ctx.df, col),
        None => Ok(vec![false; n]),
    }
}
