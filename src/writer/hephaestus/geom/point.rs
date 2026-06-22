//! `point` geom: one marker per row.
//!
//! Phase 2 wires the point geom generically — the writer iterates [`MATERIAL`]
//! and, per entry, either binds a data-mapped scale or sets a constant. The
//! actual `PointGeom` is assembled in the parent module.

use super::super::scales::RangeKind;

/// A material aesthetic the point geom supports: the ggsql aesthetic name, the
/// hephaestus channel it drives, and the kind of output it produces.
pub struct Material {
    pub aesthetic: &'static str,
    pub channel: &'static str,
    pub kind: RangeKind,
}

/// Material aesthetics in priority order. `color`/`colour` are aliases for the
/// `fill` channel (point's primary color aesthetic); when several map to the
/// same channel the first present wins.
pub const MATERIAL: &[Material] = &[
    Material {
        aesthetic: "fill",
        channel: "fill",
        kind: RangeKind::Color,
    },
    Material {
        aesthetic: "color",
        channel: "fill",
        kind: RangeKind::Color,
    },
    Material {
        aesthetic: "colour",
        channel: "fill",
        kind: RangeKind::Color,
    },
    Material {
        aesthetic: "stroke",
        channel: "stroke",
        kind: RangeKind::Color,
    },
    Material {
        aesthetic: "size",
        channel: "size",
        kind: RangeKind::Number,
    },
    Material {
        aesthetic: "opacity",
        channel: "fill_opacity",
        kind: RangeKind::Number,
    },
    Material {
        aesthetic: "linewidth",
        channel: "linewidth",
        kind: RangeKind::Number,
    },
    Material {
        aesthetic: "shape",
        channel: "shape",
        kind: RangeKind::Shape,
    },
];
