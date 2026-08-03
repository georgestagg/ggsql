//! Translating resolved ggsql scales into hephaestus scales.
//!
//! ggsql resolves the scale configuration (type, domain, transform, breaks,
//! formatted labels, and — for material aesthetics — a concrete output range);
//! we build the matching hephaestus `Scale`, which performs the value→output
//! mapping at draw time. Palettes are already resolved to concrete values by
//! ggsql's execution stage, so the output range is always an explicit `Array`.

use std::sync::Arc;

use hephaestus::color::{rgba, Color};
use hephaestus::plot::geom::linetype::{dashdot, dashed, dotted, solid};
use hephaestus::plot::scale::{self, Scale as HScale, TransformKind as HTransform};
use hephaestus::scales::value::{LinetypeStep, Value as HValue};

use super::channels::{column_to_f64, column_to_strings};
use crate::naming;
use crate::plot::scale::TransformKind as GTransform;
use crate::plot::{ArrayElement, OutputRange, Scale as GScale, ScaleTypeKind};
use crate::DataFrame;

/// What kind of visual output a scale's range produces. Selects how a resolved
/// `OutputRange::Array` is mapped onto a hephaestus range.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum RangeKind {
    /// Position scale — no output range; maps to a `[0, 1]` panel fraction.
    Position,
    /// Color-family aesthetic (fill / stroke): hex/name strings → `Color`.
    Color,
    /// Numeric aesthetic (size / linewidth / opacity): numbers passed through.
    Number,
    /// Marker shape: names resolved against the plot's `ShapeRegistry`.
    Shape,
    /// Line dash pattern: names → builtin linetype patterns.
    Linetype,
}

/// Build a hephaestus scale from a resolved ggsql scale.
///
/// `data_extent` is the finite (min, max) of the channel's data, used as the
/// domain fallback when the ggsql scale carries none (continuous scales only).
pub fn build_scale(scale: Option<&GScale>, kind: RangeKind) -> Option<HScale> {
    // No resolved scale type → no scale to register. ggsql is the source of scale
    // truth; the writer never fabricates one.
    let type_kind = scale
        .and_then(|s| s.scale_type.as_ref())
        .map(|st| st.scale_type_kind())?;
    let transform = scale
        .and_then(|s| s.transform.as_ref())
        .map(|t| t.transform_kind());

    let mut hs = match type_kind {
        ScaleTypeKind::Discrete => scale::discrete(domain_values(scale)),
        ScaleTypeKind::Ordinal => scale::ordinal(domain_values(scale)),
        ScaleTypeKind::Identity => scale::identity(),
        ScaleTypeKind::Binned => {
            let h_transform = transform.and_then(map_transform);
            let (min, max) = continuous_domain(scale);
            let breaks = scale.map(|x| x.numeric_breaks()).unwrap_or(vec![min, max]);
            let mut c = scale::binned(min..=max, breaks);
            if let Some(t) = h_transform {
                c = c.with_transform(t);
            }
            c
        }
        ScaleTypeKind::Continuous => {
            let h_transform = transform.and_then(map_transform);
            let (min, max) = continuous_domain(scale);
            let mut c = scale::continuous(min..=max);
            if let Some(t) = h_transform {
                c = c.with_transform(t);
            }
            c
        }
    };

    if kind != RangeKind::Position {
        if let Some(OutputRange::Array(values)) = scale.and_then(|s| s.output_range.as_ref()) {
            hs = apply_output_range(hs, kind, values);
        }
    }

    // Feed ggsql's resolved breaks + formatted labels for every scale (including
    // under a non-identity transform), so axis/legend ticks match ggsql — and the
    // Vega-Lite writer — exactly. ggsql's breaks pair with the same resolved
    // domain hephaestus now uses, so they line up. `apply_breaks` is a no-op when
    // the scale has no resolved breaks.
    if let Some(scale) = scale {
        hs = apply_breaks(hs, scale, Some(type_kind));
    }
    Some(hs)
}

/// Build a per-panel position scale for a **free** facet dimension, computing
/// the domain from this panel's own data slices.
///
/// This is a deliberate, scoped exception to ggsql owning all scale domains
/// (fixed dimensions still pass `numeric_domain()` straight through): only free
/// facet dimensions derive a per-panel domain here. Continuous/binned dimensions
/// take the numeric extent of the position family (`pos1`, `pos1min/max/end`, …)
/// present in the slices; discrete/ordinal take the panel's distinct categories.
/// ggsql's resolved breaks are for the global domain and don't fit a per-panel
/// one, so ticks are left to hephaestus.
pub fn free_position_scale(
    global: Option<&GScale>,
    dfs: &[&DataFrame],
    base: &str,
) -> Option<HScale> {
    let type_kind = global
        .and_then(|s| s.scale_type.as_ref())
        .map(|st| st.scale_type_kind())
        .unwrap_or(ScaleTypeKind::Continuous);
    let transform = global
        .and_then(|s| s.transform.as_ref())
        .map(|t| t.transform_kind());

    match type_kind {
        ScaleTypeKind::Discrete | ScaleTypeKind::Ordinal => {
            let vals: Vec<HValue> = panel_categories(dfs, base)
                .into_iter()
                .map(|s| HValue::String(Arc::from(s.as_str())))
                .collect();
            Some(if matches!(type_kind, ScaleTypeKind::Ordinal) {
                scale::ordinal(vals)
            } else {
                scale::discrete(vals)
            })
        }
        ScaleTypeKind::Identity => Some(scale::identity()),
        ScaleTypeKind::Continuous | ScaleTypeKind::Binned => {
            let (min, max) = panel_extent(dfs, base)?;
            let (min, max) = pad_degenerate(min, max);
            let mut c = scale::continuous(min..=max);
            if let Some(t) = transform.and_then(map_transform) {
                c = c.with_transform(t);
            }
            Some(c)
        }
    }
}

/// The finite numeric extent of a position family across the given slices.
fn panel_extent(dfs: &[&DataFrame], base: &str) -> Option<(f64, f64)> {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for df in dfs {
        for suffix in ["", "min", "max", "end"] {
            let name = naming::aesthetic_column(&format!("{base}{suffix}"));
            if df.column(&name).is_ok() {
                if let Ok(values) = column_to_f64(df, &name) {
                    for v in values.into_iter().filter(|v| v.is_finite()) {
                        lo = lo.min(v);
                        hi = hi.max(v);
                    }
                }
            }
        }
    }
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

/// The distinct category values of a position column across the given slices,
/// in first-seen order.
fn panel_categories(dfs: &[&DataFrame], base: &str) -> Vec<String> {
    let name = naming::aesthetic_column(base);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for df in dfs {
        if df.column(&name).is_ok() {
            if let Ok(values) = column_to_strings(df, &name) {
                for v in values {
                    if seen.insert(v.clone()) {
                        out.push(v);
                    }
                }
            }
        }
    }
    out
}

/// Domain for a continuous scale. ggsql's resolved `numeric_domain` is
/// authoritative — it carries ggsql's global, expanded, transform-aware training
/// over every layer and the whole position family — so pass it straight through,
/// exactly as the Vega-Lite writer uses `input_range`.
fn continuous_domain(scale: Option<&GScale>) -> (f64, f64) {
    let domain = scale
        .and_then(|s| s.numeric_domain())
        .filter(|(min, max)| min.is_finite() && max.is_finite())
        .unwrap_or((0.0, 1.0));
    pad_degenerate(domain.0, domain.1)
}

/// Category domain for a discrete/ordinal scale, as hephaestus values.
fn domain_values(scale: Option<&GScale>) -> Vec<HValue> {
    scale
        .and_then(|s| s.input_range.as_ref())
        .map(|range| range.iter().map(array_element_to_value).collect())
        .unwrap_or_default()
}

/// Attach the resolved output range to a material scale.
fn apply_output_range(hs: HScale, kind: RangeKind, values: &[ArrayElement]) -> HScale {
    match kind {
        RangeKind::Color => hs.range_colors(values.iter().filter_map(array_element_to_color)),
        RangeKind::Number => hs.range_numbers(values.iter().filter_map(|e| e.to_f64())),
        RangeKind::Shape => {
            hs.range_strings(values.iter().map(|e| Arc::from(e.to_key_string().as_str())))
        }
        RangeKind::Linetype => {
            hs.range_linetypes(values.iter().map(|e| map_linetype(&e.to_key_string())))
        }
        RangeKind::Position => hs,
    }
}

/// Map a ggsql linetype name to a hephaestus dash pattern; unknown → solid.
pub fn map_linetype(name: &str) -> Arc<[LinetypeStep]> {
    match name {
        "dashed" | "longdash" => dashed(),
        "dotted" => dotted(),
        "dotdash" | "dashdot" | "twodash" => dashdot(),
        _ => solid(),
    }
}

/// Feed ggsql's resolved breaks + formatted labels into the hephaestus scale so
/// axis/legend ticks match ggsql exactly (including RENAMING overrides).
fn apply_breaks(hs: HScale, scale: &GScale, type_kind: Option<ScaleTypeKind>) -> HScale {
    let labels = scale.break_labels();
    if labels.is_empty() {
        return hs;
    }
    match type_kind {
        Some(ScaleTypeKind::Discrete) | Some(ScaleTypeKind::Ordinal) => {
            // Pair each category value with its (possibly renamed) label.
            let Some(range) = scale.input_range.as_ref() else {
                return hs;
            };
            let pairs: Vec<(HValue, String)> = range
                .iter()
                .map(array_element_to_value)
                .zip(labels.into_iter().map(|(_, l)| l))
                .collect();
            hs.with_breaks_labeled(pairs)
        }
        _ => hs.with_breaks_labeled(
            labels
                .into_iter()
                .map(|(pos, label)| (HValue::Number(pos), label))
                .collect(),
        ),
    }
}

/// Map a ggsql transform to its hephaestus equivalent. Cast/temporal transforms
/// have no spacing effect (values arrive already projected to f64), so they map
/// to identity (`None` — hephaestus defaults to identity).
fn map_transform(kind: GTransform) -> Option<HTransform> {
    match kind {
        GTransform::Log10 => Some(HTransform::Log10),
        GTransform::Log2 => Some(HTransform::Log2),
        GTransform::Log => Some(HTransform::Log),
        GTransform::Sqrt => Some(HTransform::Sqrt),
        GTransform::Square => Some(HTransform::Square),
        GTransform::Exp10 => Some(HTransform::Exp10),
        GTransform::Exp2 => Some(HTransform::Exp2),
        GTransform::Exp => Some(HTransform::Exp),
        GTransform::Asinh => Some(HTransform::Asinh),
        GTransform::PseudoLog => Some(HTransform::PseudoLog),
        GTransform::Identity
        | GTransform::Date
        | GTransform::DateTime
        | GTransform::Time
        | GTransform::String
        | GTransform::Bool
        | GTransform::Integer => None,
    }
}

/// Convert a ggsql array element to a hephaestus domain value.
pub fn array_element_to_value(element: &ArrayElement) -> HValue {
    match element {
        ArrayElement::String(s) => HValue::String(Arc::from(s.as_str())),
        ArrayElement::Number(n) => HValue::Number(*n),
        ArrayElement::Boolean(b) => HValue::Bool(*b),
        ArrayElement::Date(d) => HValue::Date(*d),
        ArrayElement::DateTime(dt) => HValue::DateTime(*dt),
        ArrayElement::Time(t) => HValue::Time(*t),
        ArrayElement::Null => HValue::Null,
    }
}

/// Parse a color output-range element (hex or CSS name) into a hephaestus color.
fn array_element_to_color(element: &ArrayElement) -> Option<Color> {
    match element {
        ArrayElement::String(s) => parse_color(s),
        _ => None,
    }
}

/// Parse a CSS color string (hex, name, rgb(), …) into a hephaestus color.
pub fn parse_color(value: &str) -> Option<Color> {
    csscolorparser::parse(value)
        .ok()
        .map(|c| rgba(c.r, c.g, c.b, c.a))
}

/// Widen a domain that is non-finite or zero-width so a continuous scale can map
/// it without dividing by zero.
fn pad_degenerate(min: f64, max: f64) -> (f64, f64) {
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 1.0);
    }
    if (max - min).abs() < f64::EPSILON {
        return (min - 0.5, max + 0.5);
    }
    (min, max)
}
