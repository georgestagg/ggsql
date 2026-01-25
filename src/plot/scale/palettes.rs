//! Named palette definitions for color and shape aesthetics
//!
//! Provides lookup functions to expand palette names to explicit color/shape values.

use crate::plot::ArrayElement;

// =============================================================================
// Categorical Color Palettes
// =============================================================================

/// Tableau 10 - default categorical palette
pub const TABLEAU10: &[&str] = &[
    "#4e79a7", "#f28e2b", "#e15759", "#76b7b2", "#59a14f",
    "#edc948", "#b07aa1", "#ff9da7", "#9c755f", "#bab0ac",
];

/// D3 Category 10
pub const CATEGORY10: &[&str] = &[
    "#1f77b4", "#ff7f0e", "#2ca02c", "#d62728", "#9467bd",
    "#8c564b", "#e377c2", "#7f7f7f", "#bcbd22", "#17becf",
];

/// ColorBrewer Set1
pub const SET1: &[&str] = &[
    "#e41a1c", "#377eb8", "#4daf4a", "#984ea3", "#ff7f00",
    "#ffff33", "#a65628", "#f781bf", "#999999",
];

/// ColorBrewer Set2
pub const SET2: &[&str] = &[
    "#66c2a5", "#fc8d62", "#8da0cb", "#e78ac3", "#a6d854",
    "#ffd92f", "#e5c494", "#b3b3b3",
];

/// ColorBrewer Set3
pub const SET3: &[&str] = &[
    "#8dd3c7", "#ffffb3", "#bebada", "#fb8072", "#80b1d3",
    "#fdb462", "#b3de69", "#fccde5", "#d9d9d9", "#bc80bd",
    "#ccebc5", "#ffed6f",
];

/// ColorBrewer Pastel1
pub const PASTEL1: &[&str] = &[
    "#fbb4ae", "#b3cde3", "#ccebc5", "#decbe4", "#fed9a6",
    "#ffffcc", "#e5d8bd", "#fddaec", "#f2f2f2",
];

/// ColorBrewer Pastel2
pub const PASTEL2: &[&str] = &[
    "#b3e2cd", "#fdcdac", "#cbd5e8", "#f4cae4", "#e6f5c9",
    "#fff2ae", "#f1e2cc", "#cccccc",
];

/// ColorBrewer Dark2
pub const DARK2: &[&str] = &[
    "#1b9e77", "#d95f02", "#7570b3", "#e7298a", "#66a61e",
    "#e6ab02", "#a6761d", "#666666",
];

/// ColorBrewer Paired
pub const PAIRED: &[&str] = &[
    "#a6cee3", "#1f78b4", "#b2df8a", "#33a02c", "#fb9a99",
    "#e31a1c", "#fdbf6f", "#ff7f00", "#cab2d6", "#6a3d9a",
    "#ffff99", "#b15928",
];

/// ColorBrewer Accent
pub const ACCENT: &[&str] = &[
    "#7fc97f", "#beaed4", "#fdc086", "#ffff99", "#386cb0",
    "#f0027f", "#bf5b17", "#666666",
];

// =============================================================================
// Sequential Color Palettes (sampled at 8 points)
// =============================================================================

/// Viridis
pub const VIRIDIS: &[&str] = &[
    "#440154", "#482878", "#3e4a89", "#31688e", "#26828e",
    "#1f9e89", "#35b779", "#6ece58", "#b5de2b", "#fde725",
];

/// Plasma
pub const PLASMA: &[&str] = &[
    "#0d0887", "#46039f", "#7201a8", "#9c179e", "#bd3786",
    "#d8576b", "#ed7953", "#fb9f3a", "#fdca26", "#f0f921",
];

/// Magma
pub const MAGMA: &[&str] = &[
    "#000004", "#180f3d", "#440f76", "#721f81", "#9e2f7f",
    "#cd4071", "#f1605d", "#fd9668", "#feca8d", "#fcfdbf",
];

/// Inferno
pub const INFERNO: &[&str] = &[
    "#000004", "#1b0c41", "#4a0c6b", "#781c6d", "#a52c60",
    "#cf4446", "#ed6925", "#fb9b06", "#f7d13d", "#fcffa4",
];

/// Cividis
pub const CIVIDIS: &[&str] = &[
    "#00224e", "#123570", "#3b496c", "#575d6d", "#707173",
    "#8a8678", "#a59c74", "#c3b369", "#e1cc55", "#fdea45",
];

/// Blues
pub const BLUES: &[&str] = &[
    "#f7fbff", "#deebf7", "#c6dbef", "#9ecae1", "#6baed6",
    "#4292c6", "#2171b5", "#08519c", "#08306b",
];

/// Greens
pub const GREENS: &[&str] = &[
    "#f7fcf5", "#e5f5e0", "#c7e9c0", "#a1d99b", "#74c476",
    "#41ab5d", "#238b45", "#006d2c", "#00441b",
];

/// Oranges
pub const ORANGES: &[&str] = &[
    "#fff5eb", "#fee6ce", "#fdd0a2", "#fdae6b", "#fd8d3c",
    "#f16913", "#d94801", "#a63603", "#7f2704",
];

/// Reds
pub const REDS: &[&str] = &[
    "#fff5f0", "#fee0d2", "#fcbba1", "#fc9272", "#fb6a4a",
    "#ef3b2c", "#cb181d", "#a50f15", "#67000d",
];

/// Purples
pub const PURPLES: &[&str] = &[
    "#fcfbfd", "#efedf5", "#dadaeb", "#bcbddc", "#9e9ac8",
    "#807dba", "#6a51a3", "#54278f", "#3f007d",
];

// =============================================================================
// Diverging Color Palettes
// =============================================================================

/// Red-Blue diverging
pub const RDBU: &[&str] = &[
    "#67001f", "#b2182b", "#d6604d", "#f4a582", "#fddbc7",
    "#f7f7f7", "#d1e5f0", "#92c5de", "#4393c3", "#2166ac", "#053061",
];

/// Red-Yellow-Blue diverging
pub const RDYLBU: &[&str] = &[
    "#a50026", "#d73027", "#f46d43", "#fdae61", "#fee090",
    "#ffffbf", "#e0f3f8", "#abd9e9", "#74add1", "#4575b4", "#313695",
];

/// Red-Yellow-Green diverging
pub const RDYLGN: &[&str] = &[
    "#a50026", "#d73027", "#f46d43", "#fdae61", "#fee08b",
    "#ffffbf", "#d9ef8b", "#a6d96a", "#66bd63", "#1a9850", "#006837",
];

/// Spectral diverging
pub const SPECTRAL: &[&str] = &[
    "#9e0142", "#d53e4f", "#f46d43", "#fdae61", "#fee08b",
    "#ffffbf", "#e6f598", "#abdda4", "#66c2a5", "#3288bd", "#5e4fa2",
];

/// Brown-Blue-Green diverging
pub const BRBG: &[&str] = &[
    "#543005", "#8c510a", "#bf812d", "#dfc27d", "#f6e8c3",
    "#f5f5f5", "#c7eae5", "#80cdc1", "#35978f", "#01665e", "#003c30",
];

/// Purple-Green diverging
pub const PRGN: &[&str] = &[
    "#40004b", "#762a83", "#9970ab", "#c2a5cf", "#e7d4e8",
    "#f7f7f7", "#d9f0d3", "#a6dba0", "#5aae61", "#1b7837", "#00441b",
];

/// Pink-Yellow-Green diverging
pub const PIYG: &[&str] = &[
    "#8e0152", "#c51b7d", "#de77ae", "#f1b6da", "#fde0ef",
    "#f7f7f7", "#e6f5d0", "#b8e186", "#7fbc41", "#4d9221", "#276419",
];

// =============================================================================
// Shape Palettes
// =============================================================================

/// Default point shapes (Vega-Lite shape symbols)
pub const SHAPES: &[&str] = &[
    "circle",
    "square",
    "cross",
    "diamond",
    "triangle-up",
    "triangle-down",
    "triangle-left",
    "triangle-right",
];

// =============================================================================
// Lookup Functions
// =============================================================================

/// Look up a color palette by name.
/// Returns the palette colors as a static slice, or None if not found.
pub fn get_color_palette(name: &str) -> Option<&'static [&'static str]> {
    match name.to_lowercase().as_str() {
        // Categorical
        "tableau10" | "tableau" => Some(TABLEAU10),
        "category10" => Some(CATEGORY10),
        "set1" => Some(SET1),
        "set2" => Some(SET2),
        "set3" => Some(SET3),
        "pastel1" => Some(PASTEL1),
        "pastel2" => Some(PASTEL2),
        "dark2" => Some(DARK2),
        "paired" => Some(PAIRED),
        "accent" => Some(ACCENT),
        // Sequential
        "viridis" => Some(VIRIDIS),
        "plasma" => Some(PLASMA),
        "magma" => Some(MAGMA),
        "inferno" => Some(INFERNO),
        "cividis" => Some(CIVIDIS),
        "blues" => Some(BLUES),
        "greens" => Some(GREENS),
        "oranges" => Some(ORANGES),
        "reds" => Some(REDS),
        "purples" => Some(PURPLES),
        // Diverging
        "rdbu" => Some(RDBU),
        "rdylbu" => Some(RDYLBU),
        "rdylgn" => Some(RDYLGN),
        "spectral" => Some(SPECTRAL),
        "brbg" => Some(BRBG),
        "prgn" => Some(PRGN),
        "piyg" => Some(PIYG),
        _ => None,
    }
}

/// Look up a shape palette by name.
pub fn get_shape_palette(name: &str) -> Option<&'static [&'static str]> {
    match name.to_lowercase().as_str() {
        "shapes" | "default" => Some(SHAPES),
        _ => None,
    }
}

/// Get the default color palette for categorical data.
pub fn default_color_palette() -> &'static [&'static str] {
    TABLEAU10
}

/// Get the default shape palette.
pub fn default_shape_palette() -> &'static [&'static str] {
    SHAPES
}

/// Expand a palette to an array of ArrayElements, sized to match input_range length.
/// Cycles through palette if more values needed than palette size.
pub fn expand_palette(palette: &'static [&'static str], count: usize) -> Vec<ArrayElement> {
    palette
        .iter()
        .cycle()
        .take(count)
        .map(|s| ArrayElement::String(s.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_color_palette() {
        assert!(get_color_palette("viridis").is_some());
        assert!(get_color_palette("VIRIDIS").is_some()); // case insensitive
        assert!(get_color_palette("tableau10").is_some());
        assert!(get_color_palette("unknown").is_none());
    }

    #[test]
    fn test_get_shape_palette() {
        assert!(get_shape_palette("shapes").is_some());
        assert!(get_shape_palette("default").is_some());
        assert!(get_shape_palette("unknown").is_none());
    }

    #[test]
    fn test_expand_palette() {
        let expanded = expand_palette(TABLEAU10, 3);
        assert_eq!(expanded.len(), 3);
        assert_eq!(expanded[0], ArrayElement::String("#4e79a7".to_string()));
        assert_eq!(expanded[1], ArrayElement::String("#f28e2b".to_string()));
        assert_eq!(expanded[2], ArrayElement::String("#e15759".to_string()));
    }

    #[test]
    fn test_expand_palette_cycles() {
        // Test cycling - TABLEAU10 has 10 colors, request 15
        let expanded = expand_palette(TABLEAU10, 15);
        assert_eq!(expanded.len(), 15);
        // Element 10 should be same as element 0 (cycling)
        assert_eq!(expanded[10], expanded[0]);
        assert_eq!(expanded[11], expanded[1]);
    }

    #[test]
    fn test_default_palettes() {
        assert_eq!(default_color_palette().len(), 10);
        assert_eq!(default_shape_palette().len(), 8);
    }
}
