//! ggsql geom → hephaestus geom dispatch. Each module declares its channel
//! specs; [`build_into_plot`] picks the concrete hephaestus geom and builds it
//! through the shared wiring. Composite geoms (boxplot, violin) are Phase 3b.

mod area;
mod boxplot;
mod line;
mod point;
mod polygon;
mod rect;
mod segment;
mod text;
mod violin;

use hephaestus::plot::{
    LineGeom, Plot as HPlot, PointGeom, PolygonGeom, RectGeom, RibbonGeom, SegmentGeom,
};

use super::wiring::{build_and_add, Ctx, Wiring};
use crate::plot::layer::geom::GeomType;
use crate::{GgsqlError, Result};

/// Build the layer's geom into `plot`, recording its scales/axes/legends in `w`.
pub fn build_into_plot(plot: &mut HPlot, ctx: &Ctx, w: &mut Wiring) -> Result<()> {
    match ctx.layer.geom.geom_type() {
        GeomType::Point => build_and_add::<PointGeom>(plot, point::spec(ctx), ctx, w),
        GeomType::Line | GeomType::Path | GeomType::Smooth => {
            build_and_add::<LineGeom>(plot, line::spec(ctx), ctx, w)
        }
        GeomType::Bar | GeomType::Histogram | GeomType::Tile => {
            build_and_add::<RectGeom>(plot, rect::spec(ctx), ctx, w)
        }
        GeomType::Area | GeomType::Ribbon | GeomType::Density => {
            build_and_add::<RibbonGeom>(plot, area::spec(ctx), ctx, w)
        }
        GeomType::Polygon => build_and_add::<PolygonGeom>(plot, polygon::spec(ctx), ctx, w),
        GeomType::Rule if segment::is_diagonal(ctx.layer) => segment::build_diagonal(plot, ctx, w),
        GeomType::Segment | GeomType::Range | GeomType::Rule => {
            build_and_add::<SegmentGeom>(plot, segment::spec(ctx), ctx, w)
        }
        GeomType::Text => text::build(plot, ctx, w),
        GeomType::Boxplot => boxplot::build(plot, ctx, w),
        GeomType::Violin => violin::build(plot, ctx, w),
        other => Err(GgsqlError::WriterError(format!(
            "hephaestus writer does not support the '{other}' geom yet"
        ))),
    }
}

/// Geoms this writer can render (used by `validate`).
pub fn is_supported(geom: GeomType) -> bool {
    matches!(
        geom,
        GeomType::Point
            | GeomType::Line
            | GeomType::Path
            | GeomType::Smooth
            | GeomType::Bar
            | GeomType::Histogram
            | GeomType::Tile
            | GeomType::Area
            | GeomType::Ribbon
            | GeomType::Density
            | GeomType::Polygon
            | GeomType::Segment
            | GeomType::Range
            | GeomType::Rule
            | GeomType::Text
            | GeomType::Boxplot
            | GeomType::Violin
    )
}
