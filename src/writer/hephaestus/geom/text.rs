//! `text` geom → hephaestus `TextGeom`. A custom builder (not the generic
//! position/material path) because several text aesthetics need conversion
//! before they can be set: `vjust`/`hjust` accept keywords and flip for
//! hephaestus's top-origin `anchor_y`, `rotation` is degrees → radians,
//! `fontweight` accepts CSS keywords, and `italic` is boolean. The aesthetics
//! that need no conversion still go through `wire_material`, so a scaled
//! `fontsize` maps through its resolved scale like any other geom's.

use std::f64::consts::PI;

use hephaestus::color::rgb8;
use hephaestus::plot::geom::Raw;
use hephaestus::plot::{Plot as HPlot, TextGeom};

use super::super::channels::{
    aesthetic_column_name, column_to_bool, column_to_channel, column_to_f64, column_to_strings,
};
use super::super::scales::RangeKind;
use super::super::wiring::{
    constant_number, constant_string, wire_material, Ctx, LegendKind, MatDefault, MaterialSpec,
};
use crate::plot::types::{ArrayElement, ParameterValue};
use crate::plot::AestheticValue;
use crate::{GgsqlError, Result};

pub fn build(plot: &mut HPlot, ctx: &Ctx) -> Result<()> {
    let (layer, df) = (ctx.layer, ctx.df);
    let n = df.height();

    let pos1 = require(layer, "pos1")?;
    let pos2 = require(layer, "pos2")?;
    let label = require(layer, "label")?;

    let mut b = TextGeom::builder();

    // Positions: bind to the panel's pos1/pos2 scales (panel-aware for free).
    let p1 = column_to_channel(df, pos1)?;
    let p2 = column_to_channel(df, pos2)?;
    plot.set_binding("x", ctx.pos1_scale);
    plot.set_binding("y", ctx.pos2_scale);
    p1.apply(&mut b, "x");
    p2.apply(&mut b, "y");

    // Label string.
    b.set("text", Raw(column_to_strings(df, label)?));

    // Color, glyph outline, size and opacity: the shared material path, so each
    // is honored whether it arrives as a `SETTING` literal, a scaled column
    // (`SCALE fontsize TO (6, 20)` maps through its resolved scale) or an
    // identity column. `text_stroke` has no default because ggsql's default for
    // a text geom's `stroke` is Null and hephaestus skips the outline pass
    // entirely while the channel is unset; its width is hephaestus's theme
    // default, as ggsql's text geom has no `linewidth` aesthetic.
    wire_material(&mut b, &material(), plot, ctx, LegendKind::Point)?;

    // Aesthetics needing conversion, resolved per row: a mapped column, else the
    // layer's literal repeated, else the ggsql default.
    b.set("anchor_x", Raw(justification(ctx, "hjust")));
    // ggsql vjust: 0 = bottom, 1 = top; hephaestus anchor_y: 0 = top, 1 = bottom.
    let anchor_y: Vec<f64> = justification(ctx, "vjust")
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
    // Only set `family` when the layer actually names one: an empty family is
    // not "use the default", it is a font lookup that misses.
    let families = strings_or(ctx, "typeface", "");
    if families.iter().any(|f| !f.is_empty()) {
        b.set("family", Raw(families));
    }

    // `offset` nudges the label off its anchor point, in points. It is a layer
    // parameter rather than an aesthetic, so it bypasses the material table
    // entirely. hephaestus's offsets are already in points and its y grows up,
    // so both components pass through unchanged — unlike the Vega-Lite writer,
    // which converts to pixels and negates y for VL's downward axis.
    let (dx, dy) = offset(layer);
    if dx != 0.0 || dy != 0.0 {
        b.set("x_offset", Raw(vec![dx; n]));
        b.set("y_offset", Raw(vec![dy; n]));
    }

    plot.add_geom(b.build());
    Ok(())
}

/// The layer aesthetics wired through the shared material path, with ggsql's
/// text defaults. Everything else this geom sets needs conversion first.
fn material() -> [MaterialSpec; 4] {
    [
        MaterialSpec::new(
            "fill",
            "fill",
            RangeKind::Color,
            MatDefault::Color(rgb8(0, 0, 0)),
        ),
        MaterialSpec::new("stroke", "text_stroke", RangeKind::Color, MatDefault::None),
        MaterialSpec::new(
            "fontsize",
            "size",
            RangeKind::Number,
            MatDefault::Number(11.0),
        ),
        MaterialSpec::new(
            "opacity",
            "fill_opacity",
            RangeKind::Number,
            MatDefault::Number(1.0),
        ),
    ]
}

/// The layer's `offset` parameter as `(dx, dy)` in points. A bare number offsets
/// both axes; a two-element array gives them separately.
fn offset(layer: &crate::Layer) -> (f64, f64) {
    match layer.parameters.get("offset") {
        Some(ParameterValue::Number(n)) => (*n, *n),
        Some(ParameterValue::Array(a)) if a.len() == 2 => {
            let at = |i: usize| match a[i] {
                ArrayElement::Number(n) => n,
                _ => 0.0,
            };
            (at(0), at(1))
        }
        _ => (0.0, 0.0),
    }
}

fn require<'a>(layer: &'a crate::Layer, aesthetic: &str) -> Result<&'a str> {
    aesthetic_column_name(layer, aesthetic)
        .ok_or_else(|| GgsqlError::WriterError(format!("text layer has no {aesthetic} mapping")))
}

/// A per-row numeric aesthetic. Falls back to the layer's constant — a `SETTING`
/// literal, which is how every fixed value arrives — and only then to `default`.
fn numeric_or(ctx: &Ctx, aesthetic: &str, default: f64) -> Vec<f64> {
    match aesthetic_column_name(ctx.layer, aesthetic) {
        Some(col) => column_to_f64(ctx.df, col).unwrap_or_else(|_| vec![default; ctx.df.height()]),
        None => vec![constant_number(ctx, aesthetic, default); ctx.df.height()],
    }
}

/// A per-row string aesthetic, falling back to the layer's constant literal.
fn strings_or(ctx: &Ctx, aesthetic: &str, default: &str) -> Vec<String> {
    match aesthetic_column_name(ctx.layer, aesthetic) {
        Some(col) => column_to_strings(ctx.df, col)
            .unwrap_or_else(|_| vec![default.to_string(); ctx.df.height()]),
        None => vec![constant_string(ctx, aesthetic, default); ctx.df.height()],
    }
}

/// A justification aesthetic (`hjust` / `vjust`) as a 0–1 fraction. ggsql accepts
/// either a number or a keyword, so the keywords are mapped the way the
/// Vega-Lite writer's `convert_hjust` / `convert_vjust` map them to `align` /
/// `baseline`, and anything unrecognised centres.
fn justification(ctx: &Ctx, aesthetic: &str) -> Vec<f64> {
    let n = ctx.df.height();
    if let Some(col) = aesthetic_column_name(ctx.layer, aesthetic) {
        if let Ok(values) = column_to_f64(ctx.df, col) {
            return values;
        }
        if let Ok(names) = column_to_strings(ctx.df, col) {
            return names.iter().map(|s| parse_justification(s)).collect();
        }
        return vec![0.5; n];
    }
    // `SETTING vjust => 'top'` is a string literal, which `constant_number`
    // cannot read; try it as a number first, then as a keyword.
    let value = match ctx.layer.mappings.aesthetics.get(aesthetic) {
        Some(AestheticValue::Literal(ParameterValue::String(s))) => parse_justification(s),
        _ => constant_number(ctx, aesthetic, 0.5),
    };
    vec![value; n]
}

/// A justification keyword (or numeric string) as a 0–1 fraction; 0 is
/// left/bottom, 1 is right/top.
fn parse_justification(value: &str) -> f64 {
    if let Ok(n) = value.parse::<f64>() {
        return n;
    }
    match value.to_lowercase().as_str() {
        "left" | "bottom" => 0.0,
        "right" | "top" => 1.0,
        _ => 0.5, // centre / center / middle / unknown
    }
}

/// Font weights as numeric 100–900 (CSS keywords parsed), default 400.
fn weights(ctx: &Ctx, n: usize) -> Result<Vec<f64>> {
    match aesthetic_column_name(ctx.layer, "fontweight") {
        Some(col) => Ok(column_to_strings(ctx.df, col)?
            .iter()
            .map(|s| parse_weight(s))
            .collect()),
        None => Ok(vec![
            parse_weight(&constant_string(
                ctx,
                "fontweight",
                "normal"
            ));
            n
        ]),
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

/// Italic flags, from a mapped column or the layer's `SETTING italic => true`,
/// default false.
fn italics(ctx: &Ctx, n: usize) -> Result<Vec<bool>> {
    match aesthetic_column_name(ctx.layer, "italic") {
        Some(col) => column_to_bool(ctx.df, col),
        None => {
            let italic = matches!(
                ctx.layer.mappings.aesthetics.get("italic"),
                Some(AestheticValue::Literal(ParameterValue::Boolean(true)))
            );
            Ok(vec![italic; n])
        }
    }
}
