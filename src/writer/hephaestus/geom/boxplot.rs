//! `boxplot` composite geom. ggsql's stat emits one row per (category,
//! component); the component is tagged by the `type` aesthetic. We decompose
//! into: box (`RectGeom`, q1→q3 filling the category band), whiskers
//! (`SegmentGeom`, box edge → fence), median (`SegmentGeom` spanning the band),
//! optional whisker caps (`SegmentGeom`, the `hinge` SETTING), and outliers
//! (`PointGeom`). All components share the `pos1`/`pos2` scales.
//!
//! Which axis carries the categories and which the summary values follows the
//! layer's orientation (`BandAxes`): a transposed boxplot has its categories on
//! `pos2` and its values in the `pos1` family.

use hephaestus::color::rgb8;
use hephaestus::plot::geom::{BuildableGeom, GeomBuilder, Raw};
use hephaestus::plot::{Plot as HPlot, PointGeom, RectGeom, SegmentGeom};

use super::super::channels::{
    aesthetic_column_name, column_to_channel, column_to_f64, column_to_strings,
};
use super::super::scales::RangeKind;
use super::super::wiring::{
    band_edges, band_half_width, constant_number, constant_string, dodge_offsets, resolve_color,
    resolve_material, side_sign, BandAxes, Ctx, LegendKind, MatDefault, MaterialSource,
    MaterialSpec,
};
use super::hinge::{caps, hinge_points};
use crate::{GgsqlError, Result};

/// The layer aesthetics this composite styles, with ggsql's boxplot defaults.
/// Used both to resolve them and to dress the legend keys in the layer's look.
fn material() -> [MaterialSpec; 5] {
    [
        MaterialSpec::new(
            "fill",
            "fill",
            RangeKind::Color,
            MatDefault::Color(rgb8(255, 255, 255)),
        ),
        MaterialSpec::new(
            "stroke",
            "stroke",
            RangeKind::Color,
            MatDefault::Color(rgb8(60, 60, 60)),
        ),
        MaterialSpec::new(
            "linewidth",
            "linewidth",
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
            "opacity",
            "fill_opacity",
            RangeKind::Number,
            MatDefault::None,
        ),
    ]
}

pub fn build(plot: &mut HPlot, ctx: &Ctx) -> Result<()> {
    let (layer, df) = (ctx.layer, ctx.df);
    let n = df.height();

    let axes = BandAxes::new(ctx);
    let cat_aes = axes.band();
    let value_aes = axes.value();
    let value2_aes = format!("{value_aes}end");

    let cat_col = require(layer, cat_aes)?;
    let type_col = require(layer, "type")?;
    let value_col = require(layer, value_aes)?;
    let value2_col = require(layer, &value2_aes)?;

    let cat = column_to_channel(df, cat_col)?;
    let v1 = column_to_f64(df, value_col)?;
    let v2 = column_to_f64(df, value2_col)?;
    let types = column_to_strings(df, type_col)?;

    let rows_of = |t: &str| -> Vec<usize> { (0..n).filter(|&i| types[i] == t).collect() };
    let box_i = rows_of("box");
    let med_i = rows_of("median");
    let whisk_i: Vec<usize> = (0..n)
        .filter(|&i| types[i] == "lower_whisker" || types[i] == "upper_whisker")
        .collect();
    let out_i = rows_of("outlier");

    // Bind the position channels (panel-aware for free facet scales). A channel
    // always drives the same panel axis, whatever the orientation.
    for (channel, scale) in [
        ("x", ctx.pos1_scale),
        ("x2", ctx.pos1_scale),
        ("y", ctx.pos2_scale),
        ("y2", ctx.pos2_scale),
    ] {
        plot.set_binding(channel, scale);
    }

    // What this composite styles, in one table: the ggsql defaults a legend key
    // should wear when nothing is mapped, and the aliasing each resolve below
    // uses. A composite has no `GeomSpec`, so it declares the same table itself.
    let material = material();

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
        &material,
    )?;
    let stroke = resolve_color(
        ctx,
        plot,
        "stroke",
        "stroke",
        rgb8(60, 60, 60),
        LegendKind::Rect,
        &material,
    )?;
    // Outline width + dash pattern, resolved the same way and applied to every
    // component — the Vega-Lite writer puts `strokeWidth`/`strokeDash` in the
    // boxplot's shared encoding, so all five marks pick them up.
    let linewidth = resolve_material(
        ctx,
        plot,
        "linewidth",
        "linewidth",
        RangeKind::Number,
        LegendKind::Line,
        &material,
    )?;
    let linetype = resolve_material(
        ctx,
        plot,
        "linetype",
        "linetype",
        RangeKind::Linetype,
        LegendKind::Line,
        &material,
    )?;
    // `opacity` retargets to the box's fill, mirroring the Vega-Lite writer
    // (`opacity` → `fillOpacity` for a fill-bearing geom); the stroke-only
    // components have no fill to fade.
    let alpha = constant_number(ctx, "opacity", 1.0);
    // Box width (band fraction, dodge-aware) + per-row dodge offsets. `side`
    // narrows the box to one half of the band; the full `width` is kept for the
    // dodge calculation (ggsql already applied it), so a half-box pairs cleanly
    // with a half-violin on the same band.
    let offsets = dodge_offsets(df, axes.dodge());
    let (near, far) = band_edges(band_half_width(layer, 0.75), side_sign(layer));

    let (band_ch, band_ch2) = axes.band_channels();
    let (frac_ch, frac_ch2) = axes.band_fraction_channels();
    let (value_ch, value_ch2) = axes.value_channels();

    // Box: a rect from q1 to q3 occupying `width` of the band (dodge-offset).
    if !box_i.is_empty() {
        let mut b = RectGeom::builder();
        cat.select(&box_i).apply(&mut b, band_ch);
        cat.select(&box_i).apply(&mut b, band_ch2);
        b.set(value_ch, pick(&v1, &box_i));
        b.set(value_ch2, pick(&v2, &box_i));
        b.set(frac_ch, shift(&offsets, &box_i, near));
        b.set(frac_ch2, shift(&offsets, &box_i, far));
        fill.apply(&mut b, "fill", &box_i);
        stroke.apply(&mut b, "stroke", &box_i);
        outline(&mut b, &linewidth, linetype.as_ref(), &box_i);
        b.set("fill_opacity", Raw(alpha));
        plot.add_geom(b.build());
    }

    // Whiskers: segments at the band centre, box edge → fence. They stay on the
    // centreline under `side`, like the outliers.
    if !whisk_i.is_empty() {
        let mut b = SegmentGeom::builder();
        cat.select(&whisk_i).apply(&mut b, band_ch);
        cat.select(&whisk_i).apply(&mut b, band_ch2);
        b.set(value_ch, pick(&v1, &whisk_i));
        b.set(value_ch2, pick(&v2, &whisk_i));
        b.set(frac_ch, shift(&offsets, &whisk_i, 0.0));
        b.set(frac_ch2, shift(&offsets, &whisk_i, 0.0));
        stroke.apply(&mut b, "stroke", &whisk_i);
        outline(&mut b, &linewidth, linetype.as_ref(), &whisk_i);
        plot.add_geom(b.build());
    }

    // Median: a segment spanning the box's half of the band at the median value.
    if !med_i.is_empty() {
        let mut b = SegmentGeom::builder();
        cat.select(&med_i).apply(&mut b, band_ch);
        cat.select(&med_i).apply(&mut b, band_ch2);
        b.set(value_ch, pick(&v1, &med_i));
        b.set(value_ch2, pick(&v1, &med_i));
        b.set(frac_ch, shift(&offsets, &med_i, near));
        b.set(frac_ch2, shift(&offsets, &med_i, far));
        stroke.apply(&mut b, "stroke", &med_i);
        outline(&mut b, &linewidth, linetype.as_ref(), &med_i);
        plot.add_geom(b.build());
    }

    // Whisker caps at the fence ends, `hinge` points wide (absent by default).
    if let (Some(hinge), false) = (hinge_points(layer), whisk_i.is_empty()) {
        let mut b = caps(
            ctx,
            axes,
            cat.select(&whisk_i),
            pick(&v2, &whisk_i),
            shift(&offsets, &whisk_i, 0.0),
            hinge,
        );
        stroke.apply(&mut b, "stroke", &whisk_i);
        outline(&mut b, &linewidth, linetype.as_ref(), &whisk_i);
        plot.add_geom(b.build());
    }

    // Outliers: hollow points (stroke only, matching VL's `filled = false`)
    // at their value, honoring the `size`/`shape` aesthetics.
    if !out_i.is_empty() {
        let mut b = PointGeom::builder();
        cat.select(&out_i).apply(&mut b, band_ch);
        b.set(value_ch, pick(&v1, &out_i));
        b.set(frac_ch, shift(&offsets, &out_i, 0.0));
        stroke.apply(&mut b, "stroke", &out_i);
        // `PointGeom` has no dash pattern — a marker outline can't be dashed.
        outline(&mut b, &linewidth, None, &out_i);
        b.set("size", Raw(constant_number(ctx, "size", 3.0)));
        b.set("shape", Raw(constant_string(ctx, "shape", "circle")));
        plot.add_geom(b.build());
    }

    Ok(())
}

/// Apply the layer's resolved outline width and dash pattern to one component's
/// rows. `linetype` is `None` for geoms with no dash channel.
fn outline<G: BuildableGeom>(
    b: &mut GeomBuilder<G>,
    linewidth: &Option<MaterialSource>,
    linetype: Option<&MaterialSource>,
    idx: &[usize],
) {
    if let Some(lw) = linewidth {
        lw.apply(b, "linewidth", idx);
    }
    if let Some(lt) = linetype {
        lt.apply(b, "linetype", idx);
    }
}

fn require<'a>(layer: &'a crate::Layer, aesthetic: &str) -> Result<&'a str> {
    aesthetic_column_name(layer, aesthetic)
        .ok_or_else(|| GgsqlError::WriterError(format!("boxplot layer has no {aesthetic} mapping")))
}

/// Select rows by index.
fn pick(v: &[f64], idx: &[usize]) -> Vec<f64> {
    idx.iter().map(|&i| v[i]).collect()
}

/// Per-row band offsets for the selected rows, shifted by `delta` (e.g. the box
/// width's two edges, 0 for a centred line/point).
fn shift(offsets: &[f64], idx: &[usize], delta: f64) -> Vec<f64> {
    idx.iter().map(|&i| offsets[i] + delta).collect()
}
