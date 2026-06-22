//! Translating resolved ggsql scales into hephaestus scales.
//!
//! ggsql resolves the scale configuration (type, domain, transform, breaks,
//! formatted labels, and — for material aesthetics — a concrete output range);
//! we build the matching hephaestus `Scale`, which performs the value→output
//! mapping at draw time. Palettes are already resolved to concrete values by
//! ggsql's execution stage, so the output range is always an explicit `Array`.

use std::sync::Arc;

use hephaestus::color::{rgba, Color};
use hephaestus::plot::scale::{self, Scale as HScale, TransformKind as HTransform};
use hephaestus::scales::value::Value as HValue;

use crate::plot::scale::TransformKind as GTransform;
use crate::plot::{ArrayElement, OutputRange, Scale as GScale, ScaleTypeKind};

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
}

/// Build a hephaestus scale from a resolved ggsql scale.
///
/// `data_extent` is the finite (min, max) of the channel's data, used as the
/// domain fallback when the ggsql scale carries none (continuous scales only).
pub fn build_scale(scale: Option<&GScale>, data_extent: (f64, f64), kind: RangeKind) -> HScale {
    let usable = scale.filter(|s| !s.is_dummy());
    let type_kind = usable
        .and_then(|s| s.scale_type.as_ref())
        .map(|st| st.scale_type_kind());
    let transform = usable
        .and_then(|s| s.transform.as_ref())
        .map(|t| t.transform_kind());

    let mut hs = match type_kind {
        Some(ScaleTypeKind::Discrete) => scale::discrete(domain_values(usable)),
        Some(ScaleTypeKind::Ordinal) => scale::ordinal(domain_values(usable)),
        // Continuous, Binned, Identity, or unknown: a continuous mapper.
        _ => {
            let (min, max) = continuous_domain(usable, data_extent);
            let mut c = scale::continuous(min..=max);
            if let Some(t) = transform.and_then(map_transform) {
                c = c.with_transform(t);
            }
            c
        }
    };

    if kind != RangeKind::Position {
        if let Some(OutputRange::Array(values)) = usable.and_then(|s| s.output_range.as_ref()) {
            hs = apply_output_range(hs, kind, values);
        }
    }

    // Feed ggsql's resolved breaks for categorical scales and for
    // identity-transform continuous scales (preserving ggsql's formatting).
    // Under a non-identity transform ggsql's breaks are computed in transform
    // space against a possibly-loose domain, so let hephaestus derive its own
    // transform-aware breaks instead.
    let feed_breaks = matches!(
        type_kind,
        Some(ScaleTypeKind::Discrete) | Some(ScaleTypeKind::Ordinal) | Some(ScaleTypeKind::Binned)
    ) || matches!(transform, None | Some(GTransform::Identity));
    if feed_breaks {
        if let Some(s) = usable {
            hs = apply_breaks(hs, s, type_kind);
        }
    }
    hs
}

/// Domain for a continuous scale. ggsql's resolved domain is authoritative only
/// when the user set it explicitly (`FROM`); inferred domains can be degenerate
/// (e.g. a log scale's lower bound collapses to `f64::MIN_POSITIVE`), so fall
/// back to the actual data extent.
fn continuous_domain(scale: Option<&GScale>, data_extent: (f64, f64)) -> (f64, f64) {
    let domain = scale
        .filter(|s| s.explicit_input_range)
        .and_then(|s| s.numeric_domain())
        .unwrap_or(data_extent);
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
        RangeKind::Position => hs,
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
