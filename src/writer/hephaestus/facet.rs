//! FACET → multi-panel composition.
//!
//! ggsql resolves faceting fully at execution time: the layout (Wrap/Grid), the
//! `free` bool array, Wrap's `ncol`, and per-row facet assignment materialized in
//! the ordinary aesthetic columns `__ggsql_aes_facet1__` (and `facet2__` for
//! Grid). This module turns that into a hephaestus [`Composition`] of named
//! panels plus a [`Panel`] list the writer loops over — one hephaestus `Plot` per
//! panel, sharing the composition's scale registry.
//!
//! Panel ordering mirrors the Vega-Lite writer's `resolve_facet_ordering`: the
//! facet aesthetic's `SCALE` (its `input_range`, then `reverse`) drives the
//! order, falling back to a numeric-aware ascending sort of the present values.

use std::collections::HashSet;

use arrow::array::UInt32Array;
use hephaestus::composition::{grid, spacer, Composition, Element, Patch};

use super::channels::column_to_strings;
use crate::naming;
use crate::plot::{ArrayElement, FacetLayout, ParameterValue, Scale};
use crate::{DataFrame, Plot, Result};

/// Patch id for the single (unfaceted) panel.
pub const PANEL_ID: &str = "ggsql_panel";

/// One facet cell: which facet values it holds, its grid position (for edge-only
/// axes), and the strip-label text to show.
pub struct Panel {
    /// hephaestus patch id, unique per panel.
    pub id: String,
    /// 0-based panel index (order of enumeration), for per-panel scale names.
    pub index: usize,
    /// Facet1 (Wrap panel / Grid row) value selecting this panel's rows.
    pub facet1: Option<String>,
    /// Facet2 (Grid column) value; `None` for Wrap.
    pub facet2: Option<String>,
    /// Top strip label (Wrap header, or Grid column header on the top row).
    pub strip_top: Option<String>,
    /// Right strip label (Grid row header on the right column).
    pub strip_right: Option<String>,
    /// Whether this panel is in the left column (draws the y-axis when fixed).
    pub first_col: bool,
    /// Whether this panel is the bottom-most present panel in its column (draws
    /// the x-axis when fixed).
    pub last_row: bool,
}

impl Panel {
    /// The unfaceted single panel: draws both axes, no strips.
    fn single() -> Panel {
        Panel {
            id: PANEL_ID.to_string(),
            index: 0,
            facet1: None,
            facet2: None,
            strip_top: None,
            strip_right: None,
            first_col: true,
            last_row: true,
        }
    }
}

/// Build the panel grid for a plot. Returns a single-cell composition + one
/// [`Panel`] when there is no `FACET`, otherwise the faceted grid.
pub fn build_panels(
    spec: &Plot,
    data: &std::collections::HashMap<String, DataFrame>,
) -> Result<(Composition, Vec<Panel>)> {
    let Some(facet) = &spec.facet else {
        let comp = grid(1, 1, vec![Element::from(Patch::new(PANEL_ID))]);
        return Ok((comp, vec![Panel::single()]));
    };
    let layer0 = super::layer_dataframe(&spec.layers[0], data)?;
    match &facet.layout {
        FacetLayout::Wrap { .. } => build_wrap(spec, facet, layer0),
        FacetLayout::Grid { .. } => build_grid(spec, layer0),
    }
}

/// Wrap: N panels flowed row-major into `ncol` columns.
fn build_wrap(
    spec: &Plot,
    facet: &crate::plot::Facet,
    layer0: &DataFrame,
) -> Result<(Composition, Vec<Panel>)> {
    let levels = ordered_levels(spec, layer0, "facet1")?;
    let n = levels.len().max(1);
    let ncol = wrap_ncol(facet, n);
    let nrow = n.div_ceil(ncol);

    let mut panels = Vec::with_capacity(n);
    for (idx, level) in levels.iter().enumerate() {
        let col = idx % ncol;
        // Bottom-most present panel in this column: no panel sits `ncol` cells
        // below it. Governs where the x-axis shows when the last row is partial.
        let last_row = idx + ncol >= n;
        panels.push(Panel {
            id: format!("facet_{idx}"),
            index: idx,
            facet1: Some(level.clone()),
            facet2: None,
            strip_top: Some(level.clone()),
            strip_right: None,
            first_col: col == 0,
            last_row,
        });
    }

    // Cells row-major, padding the trailing slots of a partial last row.
    let mut cells: Vec<Element> = Vec::with_capacity(nrow * ncol);
    for slot in 0..(nrow * ncol) {
        if slot < panels.len() {
            cells.push(Element::from(Patch::new(panels[slot].id.clone())));
        } else {
            cells.push(Element::from(spacer()));
        }
    }
    Ok((grid(nrow, ncol, cells), panels))
}

/// Grid: rows = facet1 levels, columns = facet2 levels. Column strips on the top
/// row, row strips on the right column.
fn build_grid(spec: &Plot, layer0: &DataFrame) -> Result<(Composition, Vec<Panel>)> {
    let rows = ordered_levels(spec, layer0, "facet1")?;
    let cols = ordered_levels(spec, layer0, "facet2")?;
    let nrow = rows.len().max(1);
    let ncol = cols.len().max(1);

    let mut panels = Vec::with_capacity(nrow * ncol);
    let mut cells: Vec<Element> = Vec::with_capacity(nrow * ncol);
    let mut index = 0;
    for (r, rowv) in rows.iter().enumerate() {
        for (c, colv) in cols.iter().enumerate() {
            let id = format!("facet_{r}_{c}");
            panels.push(Panel {
                id: id.clone(),
                index,
                facet1: Some(rowv.clone()),
                facet2: Some(colv.clone()),
                strip_top: (r == 0).then(|| colv.clone()),
                strip_right: (c == ncol - 1).then(|| rowv.clone()),
                first_col: c == 0,
                last_row: r == nrow - 1,
            });
            cells.push(Element::from(Patch::new(id)));
            index += 1;
        }
    }
    Ok((grid(nrow, ncol, cells), panels))
}

/// The resolved Wrap column count (ggsql computes it during resolution); falls
/// back to a single row if somehow absent.
fn wrap_ncol(facet: &crate::plot::Facet, n: usize) -> usize {
    match facet.properties.get("ncol") {
        Some(ParameterValue::Number(c)) if *c >= 1.0 => (*c as usize).min(n).max(1),
        _ => n.max(1),
    }
}

/// Distinct facet levels present in the data, ordered per the facet scale.
fn ordered_levels(spec: &Plot, df: &DataFrame, internal_aes: &str) -> Result<Vec<String>> {
    let col = naming::aesthetic_column(internal_aes);
    let values = column_to_strings(df, &col)?;
    let mut seen = HashSet::new();
    let mut distinct: Vec<String> = Vec::new();
    for v in values {
        if seen.insert(v.clone()) {
            distinct.push(v);
        }
    }
    Ok(order_by_scale(distinct, spec.find_scale(internal_aes)))
}

/// Order distinct facet values by the scale's `input_range` (then any
/// present-but-unlisted values, sorted), or a numeric-aware ascending sort when
/// there is no scale/range. Reversed when the scale sets `reverse => true`.
fn order_by_scale(mut distinct: Vec<String>, scale: Option<&Scale>) -> Vec<String> {
    let reverse = scale
        .map(|s| {
            matches!(
                s.properties.get("reverse"),
                Some(ParameterValue::Boolean(true))
            )
        })
        .unwrap_or(false);

    let mut ordered = match scale.and_then(|s| s.input_range.as_ref()) {
        Some(range) => {
            let order: Vec<String> = range.iter().map(element_to_string).collect();
            let mut ranked: Vec<String> = order
                .iter()
                .filter(|o| distinct.contains(o))
                .cloned()
                .collect();
            let mut extra: Vec<String> = distinct
                .into_iter()
                .filter(|d| !order.contains(d))
                .collect();
            sort_values(&mut extra);
            ranked.extend(extra);
            ranked
        }
        None => {
            sort_values(&mut distinct);
            distinct
        }
    };
    if reverse {
        ordered.reverse();
    }
    ordered
}

/// Numeric-aware ascending sort: numeric when every value parses as `f64`,
/// otherwise lexical.
fn sort_values(values: &mut [String]) {
    if values.iter().all(|s| s.parse::<f64>().is_ok()) {
        values.sort_by(|a, b| {
            a.parse::<f64>()
                .unwrap()
                .partial_cmp(&b.parse::<f64>().unwrap())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else {
        values.sort();
    }
}

/// Render an `input_range` element to the string form the facet column carries
/// (whole numbers as integers, matching an integer column's cast to text).
fn element_to_string(element: &ArrayElement) -> String {
    match element {
        ArrayElement::String(s) => s.clone(),
        ArrayElement::Number(n) if n.fract() == 0.0 && n.is_finite() => format!("{}", *n as i64),
        ArrayElement::Number(n) => n.to_string(),
        ArrayElement::Boolean(b) => b.to_string(),
        ArrayElement::Null => String::new(),
        other => format!("{other:?}"),
    }
}

/// The scale names a panel binds its position channels to, and whether each
/// dimension is free. For fixed dimensions the name is the shared `pos1`/`pos2`;
/// for free dimensions it is a per-panel name (`pos1__p{index}`), so each panel
/// resolves through its own domain.
pub struct PanelScales {
    pub pos1: String,
    pub pos2: String,
    pub free_x: bool,
    pub free_y: bool,
}

impl PanelScales {
    pub fn new(spec: &Plot, panel: &Panel) -> Self {
        let free_x = spec.facet.as_ref().is_some_and(|f| f.is_free("pos1"));
        let free_y = spec.facet.as_ref().is_some_and(|f| f.is_free("pos2"));
        PanelScales {
            pos1: if free_x {
                format!("pos1__p{}", panel.index)
            } else {
                "pos1".to_string()
            },
            pos2: if free_y {
                format!("pos2__p{}", panel.index)
            } else {
                "pos2".to_string()
            },
            free_x,
            free_y,
        }
    }
}

/// The rows of `df` belonging to `panel`, sliced via `DataFrame::take`. A layer
/// with no facet column (annotation/global layers) is used whole for every panel.
pub fn panel_dataframe(df: &DataFrame, panel: &Panel) -> Result<DataFrame> {
    let Some(want1) = &panel.facet1 else {
        return Ok(df.clone());
    };
    let f1 = naming::aesthetic_column("facet1");
    if df.column(&f1).is_err() {
        return Ok(df.clone());
    }
    let c1 = column_to_strings(df, &f1)?;
    let c2 = match &panel.facet2 {
        Some(_) => Some(column_to_strings(df, &naming::aesthetic_column("facet2"))?),
        None => None,
    };

    let mut idx: Vec<u32> = Vec::new();
    for i in 0..df.height() {
        if &c1[i] != want1 {
            continue;
        }
        if let (Some(c2), Some(want2)) = (&c2, &panel.facet2) {
            if &c2[i] != want2 {
                continue;
            }
        }
        idx.push(i as u32);
    }
    df.take(&UInt32Array::from(idx))
}
