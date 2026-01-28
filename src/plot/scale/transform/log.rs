//! Log transform implementation (parameterized by base)
//!
//! This module provides a unified logarithm transform that supports any base.
//! Common bases (10, 2, e) have named constructors for convenience.

use super::{TransformKind, TransformTrait};
use crate::plot::scale::breaks::{log_breaks, minor_breaks_log};

/// Log transform - logarithm with configurable base
///
/// Domain: (0, +∞) - positive values only
///
/// The base determines which `TransformKind` is returned:
/// - Base 10 → `TransformKind::Log10`
/// - Base 2 → `TransformKind::Log2`
/// - Base e → `TransformKind::Log`
#[derive(Debug, Clone, Copy)]
pub struct Log {
    base: f64,
}

impl Log {
    /// Create a log transform with the given base
    pub fn new(base: f64) -> Self {
        assert!(
            base > 0.0 && base != 1.0,
            "Log base must be positive and not 1"
        );
        Self { base }
    }

    /// Create a base-10 logarithm transform
    pub fn base10() -> Self {
        Self { base: 10.0 }
    }

    /// Create a base-2 logarithm transform
    pub fn base2() -> Self {
        Self { base: 2.0 }
    }

    /// Create a natural logarithm transform (base e)
    pub fn natural() -> Self {
        Self {
            base: std::f64::consts::E,
        }
    }

    /// Get the base of this logarithm
    pub fn base(&self) -> f64 {
        self.base
    }

    /// Check if this is a base-10 log (within floating point tolerance)
    fn is_base10(&self) -> bool {
        (self.base - 10.0).abs() < 1e-10
    }

    /// Check if this is a base-2 log (within floating point tolerance)
    fn is_base2(&self) -> bool {
        (self.base - 2.0).abs() < 1e-10
    }

    /// Check if this is a natural log (within floating point tolerance)
    fn is_natural(&self) -> bool {
        (self.base - std::f64::consts::E).abs() < 1e-10
    }
}

impl TransformTrait for Log {
    fn transform_kind(&self) -> TransformKind {
        if self.is_base10() {
            TransformKind::Log10
        } else if self.is_base2() {
            TransformKind::Log2
        } else {
            // Natural log and any other base map to Log
            TransformKind::Log
        }
    }

    fn name(&self) -> &'static str {
        if self.is_base10() {
            "log"
        } else if self.is_base2() {
            "log2"
        } else {
            "ln"
        }
    }

    fn allowed_domain(&self) -> (f64, f64) {
        (f64::MIN_POSITIVE, f64::INFINITY)
    }

    fn is_value_in_domain(&self, value: f64) -> bool {
        value > 0.0 && value.is_finite()
    }

    fn calculate_breaks(&self, min: f64, max: f64, n: usize, pretty: bool) -> Vec<f64> {
        log_breaks(min, max, n, self.base, pretty)
    }

    fn calculate_minor_breaks(
        &self,
        major_breaks: &[f64],
        n: usize,
        range: Option<(f64, f64)>,
    ) -> Vec<f64> {
        minor_breaks_log(major_breaks, n, self.base, range)
    }

    fn default_minor_break_count(&self) -> usize {
        8 // Similar density to traditional 2-9 pattern on log axes
    }

    fn transform(&self, value: f64) -> f64 {
        value.log(self.base)
    }

    fn inverse(&self, value: f64) -> f64 {
        self.base.powf(value)
    }
}

impl std::fmt::Display for Log {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::E;

    // ==================== Base-10 (Log10) Tests ====================

    #[test]
    fn test_log10_domain() {
        let t = Log::base10();
        let (min, max) = t.allowed_domain();
        assert!(min > 0.0);
        assert!(max.is_infinite());
    }

    #[test]
    fn test_log10_is_value_in_domain() {
        let t = Log::base10();
        assert!(t.is_value_in_domain(1.0));
        assert!(t.is_value_in_domain(0.0001));
        assert!(t.is_value_in_domain(1000000.0));
        assert!(!t.is_value_in_domain(0.0));
        assert!(!t.is_value_in_domain(-1.0));
        assert!(!t.is_value_in_domain(f64::INFINITY));
        assert!(!t.is_value_in_domain(f64::NAN));
    }

    #[test]
    fn test_log10_transform() {
        let t = Log::base10();
        assert!((t.transform(1.0) - 0.0).abs() < 1e-10);
        assert!((t.transform(10.0) - 1.0).abs() < 1e-10);
        assert!((t.transform(100.0) - 2.0).abs() < 1e-10);
        assert!((t.transform(1000.0) - 3.0).abs() < 1e-10);
        assert!((t.transform(0.1) - (-1.0)).abs() < 1e-10);
    }

    #[test]
    fn test_log10_inverse() {
        let t = Log::base10();
        assert!((t.inverse(0.0) - 1.0).abs() < 1e-10);
        assert!((t.inverse(1.0) - 10.0).abs() < 1e-10);
        assert!((t.inverse(2.0) - 100.0).abs() < 1e-10);
        assert!((t.inverse(-1.0) - 0.1).abs() < 1e-10);
    }

    #[test]
    fn test_log10_roundtrip() {
        let t = Log::base10();
        for &val in &[0.001, 0.1, 1.0, 10.0, 100.0, 1000.0] {
            let transformed = t.transform(val);
            let back = t.inverse(transformed);
            assert!(
                (back - val).abs() / val < 1e-10,
                "Roundtrip failed for {}",
                val
            );
        }
    }

    #[test]
    fn test_log10_breaks_powers() {
        let t = Log::base10();
        let breaks = t.calculate_breaks(1.0, 10000.0, 10, false);
        assert!(breaks.contains(&1.0));
        assert!(breaks.contains(&10.0));
        assert!(breaks.contains(&100.0));
        assert!(breaks.contains(&1000.0));
        assert!(breaks.contains(&10000.0));
    }

    #[test]
    fn test_log10_breaks_pretty() {
        let t = Log::base10();
        let breaks = t.calculate_breaks(1.0, 100.0, 10, true);
        // Should have 1-2-5 pattern
        assert!(breaks.contains(&1.0));
        assert!(breaks.contains(&10.0));
        assert!(breaks.contains(&100.0));
    }

    #[test]
    fn test_log10_kind_and_name() {
        let t = Log::base10();
        assert_eq!(t.transform_kind(), TransformKind::Log10);
        assert_eq!(t.name(), "log");
    }

    // ==================== Base-2 (Log2) Tests ====================

    #[test]
    fn test_log2_domain() {
        let t = Log::base2();
        let (min, max) = t.allowed_domain();
        assert!(min > 0.0);
        assert!(max.is_infinite());
    }

    #[test]
    fn test_log2_is_value_in_domain() {
        let t = Log::base2();
        assert!(t.is_value_in_domain(1.0));
        assert!(t.is_value_in_domain(0.5));
        assert!(t.is_value_in_domain(1024.0));
        assert!(!t.is_value_in_domain(0.0));
        assert!(!t.is_value_in_domain(-1.0));
    }

    #[test]
    fn test_log2_transform() {
        let t = Log::base2();
        assert!((t.transform(1.0) - 0.0).abs() < 1e-10);
        assert!((t.transform(2.0) - 1.0).abs() < 1e-10);
        assert!((t.transform(4.0) - 2.0).abs() < 1e-10);
        assert!((t.transform(8.0) - 3.0).abs() < 1e-10);
        assert!((t.transform(0.5) - (-1.0)).abs() < 1e-10);
    }

    #[test]
    fn test_log2_inverse() {
        let t = Log::base2();
        assert!((t.inverse(0.0) - 1.0).abs() < 1e-10);
        assert!((t.inverse(1.0) - 2.0).abs() < 1e-10);
        assert!((t.inverse(2.0) - 4.0).abs() < 1e-10);
        assert!((t.inverse(3.0) - 8.0).abs() < 1e-10);
        assert!((t.inverse(-1.0) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_log2_roundtrip() {
        let t = Log::base2();
        for &val in &[0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0] {
            let transformed = t.transform(val);
            let back = t.inverse(transformed);
            assert!(
                (back - val).abs() / val < 1e-10,
                "Roundtrip failed for {}",
                val
            );
        }
    }

    #[test]
    fn test_log2_breaks_powers() {
        let t = Log::base2();
        let breaks = t.calculate_breaks(1.0, 16.0, 10, false);
        assert!(breaks.contains(&1.0));
        assert!(breaks.contains(&2.0));
        assert!(breaks.contains(&4.0));
        assert!(breaks.contains(&8.0));
        assert!(breaks.contains(&16.0));
    }

    #[test]
    fn test_log2_kind_and_name() {
        let t = Log::base2();
        assert_eq!(t.transform_kind(), TransformKind::Log2);
        assert_eq!(t.name(), "log2");
    }

    // ==================== Natural Log (base e) Tests ====================

    #[test]
    fn test_log_domain() {
        let t = Log::natural();
        let (min, max) = t.allowed_domain();
        assert!(min > 0.0);
        assert!(max.is_infinite());
    }

    #[test]
    fn test_log_is_value_in_domain() {
        let t = Log::natural();
        assert!(t.is_value_in_domain(1.0));
        assert!(t.is_value_in_domain(E));
        assert!(t.is_value_in_domain(0.0001));
        assert!(!t.is_value_in_domain(0.0));
        assert!(!t.is_value_in_domain(-1.0));
    }

    #[test]
    fn test_log_transform() {
        let t = Log::natural();
        assert!((t.transform(1.0) - 0.0).abs() < 1e-10);
        assert!((t.transform(E) - 1.0).abs() < 1e-10);
        assert!((t.transform(E * E) - 2.0).abs() < 1e-10);
        assert!((t.transform(1.0 / E) - (-1.0)).abs() < 1e-10);
    }

    #[test]
    fn test_log_inverse() {
        let t = Log::natural();
        assert!((t.inverse(0.0) - 1.0).abs() < 1e-10);
        assert!((t.inverse(1.0) - E).abs() < 1e-10);
        assert!((t.inverse(2.0) - E * E).abs() < 1e-10);
    }

    #[test]
    fn test_log_roundtrip() {
        let t = Log::natural();
        for &val in &[0.001, 0.1, 1.0, E, 10.0, 100.0] {
            let transformed = t.transform(val);
            let back = t.inverse(transformed);
            assert!(
                (back - val).abs() / val < 1e-10,
                "Roundtrip failed for {}",
                val
            );
        }
    }

    #[test]
    fn test_log_breaks() {
        let t = Log::natural();
        let breaks = t.calculate_breaks(1.0, 100.0, 10, false);
        assert!(!breaks.is_empty());
    }

    #[test]
    fn test_log_kind_and_name() {
        let t = Log::natural();
        assert_eq!(t.transform_kind(), TransformKind::Log);
        assert_eq!(t.name(), "ln");
    }

    // ==================== General Tests ====================

    #[test]
    fn test_base_accessor() {
        assert!((Log::base10().base() - 10.0).abs() < 1e-10);
        assert!((Log::base2().base() - 2.0).abs() < 1e-10);
        assert!((Log::natural().base() - E).abs() < 1e-10);
    }

    #[test]
    fn test_custom_base() {
        let t = Log::new(5.0);
        // 5^2 = 25, so log_5(25) = 2
        assert!((t.transform(25.0) - 2.0).abs() < 1e-10);
        assert!((t.inverse(2.0) - 25.0).abs() < 1e-10);
        // Custom base maps to TransformKind::Log
        assert_eq!(t.transform_kind(), TransformKind::Log);
        assert_eq!(t.name(), "ln");
    }

    #[test]
    #[should_panic]
    fn test_invalid_base_zero() {
        Log::new(0.0);
    }

    #[test]
    #[should_panic]
    fn test_invalid_base_one() {
        Log::new(1.0);
    }

    #[test]
    #[should_panic]
    fn test_invalid_base_negative() {
        Log::new(-2.0);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", Log::base10()), "log");
        assert_eq!(format!("{}", Log::base2()), "log2");
        assert_eq!(format!("{}", Log::natural()), "ln");
    }

    // ==================== Minor Breaks Tests ====================

    #[test]
    fn test_log10_minor_breaks() {
        let t = Log::base10();
        let majors = vec![1.0, 10.0, 100.0];
        let minors = t.calculate_minor_breaks(&majors, 8, None);
        // 8 minor breaks per decade, 2 decades
        assert_eq!(minors.len(), 16);
        assert!(minors.iter().all(|&x| x > 0.0));
    }

    #[test]
    fn test_log10_minor_breaks_geometric_mean() {
        let t = Log::base10();
        let majors = vec![1.0, 10.0];
        let minors = t.calculate_minor_breaks(&majors, 1, None);
        // Single minor break should be at geometric mean: sqrt(1 * 10) ≈ 3.16
        assert_eq!(minors.len(), 1);
        assert!((minors[0] - (1.0_f64 * 10.0).sqrt()).abs() < 0.01);
    }

    #[test]
    fn test_log10_minor_breaks_with_extension() {
        let t = Log::base10();
        let majors = vec![10.0, 100.0];
        let minors = t.calculate_minor_breaks(&majors, 8, Some((1.0, 1000.0)));
        // Should extend into [1, 10) and (100, 1000]
        assert_eq!(minors.len(), 24); // 8 per decade × 3 decades
    }

    #[test]
    fn test_log10_default_minor_break_count() {
        let t = Log::base10();
        assert_eq!(t.default_minor_break_count(), 8);
    }

    #[test]
    fn test_log2_minor_breaks() {
        let t = Log::base2();
        let majors = vec![1.0, 2.0, 4.0, 8.0];
        let minors = t.calculate_minor_breaks(&majors, 1, None);
        // One midpoint per interval (3 intervals)
        assert_eq!(minors.len(), 3);
        // Geometric means: sqrt(1*2)≈1.41, sqrt(2*4)≈2.83, sqrt(4*8)≈5.66
        assert!((minors[0] - 2.0_f64.sqrt()).abs() < 0.01);
    }
}
