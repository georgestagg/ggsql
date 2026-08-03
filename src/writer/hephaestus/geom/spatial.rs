//! `spatial` geom → hephaestus `GeometryGeom`. A custom builder (not the generic
//! position/material path) because the geom carries no `x`/`y` columns: each row
//! is a single `Geometry` value whose coordinates resolve through the plot's
//! bound `x`/`y` scales at draw time. Under a `PROJECT map` the plot's projection
//! (a Custom clip surface built from `projection.rs`) shapes those coordinates;
//! with no `PROJECT` the geometry draws in raw data space under Cartesian.

use hephaestus::color::rgb8;
use hephaestus::plot::{GeometryGeom, Plot as HPlot};

use super::super::channels::column_to_geometry;
use super::super::scales::RangeKind;
use super::super::wiring::{
    resolve_color, wire_material, Ctx, LegendKind, MatDefault, MaterialSpec,
};
use crate::naming;
use crate::Result;

pub fn build(plot: &mut HPlot, ctx: &Ctx) -> Result<()> {
    let df = ctx.df;
    let n = df.height();

    // The geometry aesthetic is always materialised to the internal WKB column.
    let geoms = column_to_geometry(df, &naming::aesthetic_column("geometry"))?;

    let mut b = GeometryGeom::builder();
    b.set("geometry", geoms);

    // Coordinates map through the panel's pos1/pos2 scales (bbox-framed; see
    // `HephaestusWriter::write`). GeometryGeom has no x/y channel, but its draw
    // resolves each coordinate against these bound scales.
    plot.set_binding("x", ctx.pos1_scale);
    plot.set_binding("y", ctx.pos2_scale);

    // fill/stroke: data-mapped (choropleth) → shared scale + legend, else the
    // mapped literal or the ggsql spatial defaults.
    let all: Vec<usize> = (0..n).collect();
    resolve_color(
        ctx,
        plot,
        "fill",
        "fill",
        rgb8(0x74, 0x74, 0x74),
        LegendKind::Rect,
    )?
    .apply(&mut b, "fill", &all);
    resolve_color(
        ctx,
        plot,
        "stroke",
        "stroke",
        rgb8(0, 0, 0),
        LegendKind::Rect,
    )?
    .apply(&mut b, "stroke", &all);

    // opacity/linewidth/linetype: routed through the shared material path so
    // each is honored whether it's the ggsql literal default, a `SETTING`
    // constant, or data-mapped (scale-bound + legended). Mirrors the generic
    // geoms; ggsql's spatial defaults (opacity 0.8, linewidth 0.2, solid) arrive
    // as literals and set the fallback.
    let material = [
        MaterialSpec::new(
            "opacity",
            "fill_opacity",
            RangeKind::Number,
            MatDefault::Number(0.8),
        ),
        MaterialSpec::new(
            "linewidth",
            "linewidth",
            RangeKind::Number,
            MatDefault::Number(0.2),
        ),
        MaterialSpec::new(
            "linetype",
            "linetype",
            RangeKind::Linetype,
            MatDefault::None,
        ),
    ];
    wire_material(&mut b, &material, plot, ctx, LegendKind::Rect)?;

    plot.add_geom(b.build());
    Ok(())
}
