//! `text` geom → hephaestus `TextGeom`. The `label` aesthetic carries the
//! string (set raw, unscaled); position + color/size map as usual.

use hephaestus::color::rgb8;

use super::super::scales::RangeKind;
use super::super::wiring::{Ctx, GeomSpec, MatDefault, MaterialSpec, PanelAxis, PositionSpec};

pub fn spec(_ctx: &Ctx) -> GeomSpec {
    GeomSpec {
        positions: vec![
            PositionSpec::new("x", "pos1", PanelAxis::X),
            PositionSpec::new("y", "pos2", PanelAxis::Y),
        ],
        material: vec![
            MaterialSpec::new(
                "fill",
                "fill",
                RangeKind::Color,
                MatDefault::Color(rgb8(0, 0, 0)),
            ),
            MaterialSpec::new("color", "fill", RangeKind::Color, MatDefault::None),
            MaterialSpec::new("colour", "fill", RangeKind::Color, MatDefault::None),
            MaterialSpec::new("fontsize", "size", RangeKind::Number, MatDefault::None),
            MaterialSpec::new(
                "opacity",
                "fill_opacity",
                RangeKind::Number,
                MatDefault::None,
            ),
        ],
        raw_strings: &[("text", "label")],
        raw_numbers: vec![],
        grouped: false,
    }
}
