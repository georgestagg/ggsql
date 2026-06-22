//! Translating resolved ggsql scales into hephaestus scales.
//!
//! Phase 1 handles continuous position scales only. ggsql resolves the scale
//! configuration (domain, breaks, formatted labels); we hand those to
//! hephaestus, which performs the value→pixel mapping at draw time.

use hephaestus::plot::scale::{self, Scale as HScale};
use hephaestus::scales::value::Value as HValue;

use crate::Scale;

/// Build a hephaestus continuous scale from a resolved ggsql position scale.
///
/// Uses the ggsql scale's resolved domain and formatted breaks when available,
/// falling back to the supplied data extent (and hephaestus's own break
/// selection) when the scale is absent or carries no real domain.
pub fn build_continuous(scale: Option<&Scale>, data_extent: (f64, f64)) -> HScale {
    let usable = scale.filter(|s| !s.is_dummy());
    let (min, max) = usable
        .and_then(|s| s.numeric_domain())
        .unwrap_or(data_extent);
    let (min, max) = pad_degenerate(min, max);

    let mut hs = scale::continuous(min..=max);
    if let Some(s) = usable {
        let labels = s.break_labels();
        if !labels.is_empty() {
            hs = hs.with_breaks_labeled(
                labels
                    .into_iter()
                    .map(|(pos, label)| (HValue::Number(pos), label))
                    .collect(),
            );
        }
    }
    hs
}

/// Widen a domain that is non-finite or zero-width into something a continuous
/// scale can map without dividing by zero.
fn pad_degenerate(min: f64, max: f64) -> (f64, f64) {
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 1.0);
    }
    if (max - min).abs() < f64::EPSILON {
        return (min - 0.5, max + 0.5);
    }
    (min, max)
}
