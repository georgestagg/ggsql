//! Shared geom-wiring: position/material channels, scales, axes, legends, and
//! group keys. Each geom module declares its channel specs; these helpers do the
//! repetitive work, generic over the concrete geom builder.

use std::collections::{HashMap, HashSet};

use hephaestus::color::Color;
use hephaestus::plot::chrome::axis::{Axis, AxisPlacement};
use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
use hephaestus::plot::geom::{BuildableGeom, Geom, GeomBuilder, Raw};
use hephaestus::plot::scale::Scale as HScale;
use hephaestus::plot::Plot as HPlot;
use hephaestus::scales::chrome::{AxisSide, LegendSide};

use super::channels::{
    aesthetic_column_name, build_group_keys, column_to_channel, column_to_colors, column_to_f64,
    column_to_strings,
};
use super::scales::{build_scale, RangeKind};
use crate::plot::ScaleTypeKind;
use crate::{AestheticValue, DataFrame, GgsqlError, Layer, Plot, Result};

/// Read-only context for building one layer's geom.
pub struct Ctx<'a> {
    pub spec: &'a Plot,
    pub layer: &'a Layer,
    pub df: &'a DataFrame,
    /// Whether the layer is in transposed (horizontal) orientation.
    pub transposed: bool,
}

/// Accumulates everything that attaches to the plot/composition while building
/// a geom: scales to register, channel→scale bindings, axes, and legends.
#[derive(Default)]
pub struct Wiring {
    pub registered: Vec<(String, HScale)>,
    pub bindings: Vec<(&'static str, String)>,
    pub axes: Vec<Axis>,
    pub legends: Vec<Legend>,
    /// Channels sharing a `(data source, output kind)` reuse one scale so their
    /// legends collapse (e.g. ggsql's `color` → fill + stroke).
    shared_scales: HashMap<(String, RangeKind), String>,
}

/// Which panel axis a position channel drives.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PanelAxis {
    X,
    Y,
}

impl PanelAxis {
    fn scale_name(self) -> &'static str {
        match self {
            PanelAxis::X => "pos1",
            PanelAxis::Y => "pos2",
        }
    }
    fn side(self) -> AxisSide {
        match self {
            PanelAxis::X => AxisSide::Bottom,
            PanelAxis::Y => AxisSide::Left,
        }
    }
}

/// A position channel: hephaestus `channel` ← ggsql `aesthetic`, on `axis`.
pub struct PositionSpec {
    pub channel: &'static str,
    pub aesthetic: String,
    pub axis: PanelAxis,
}

impl PositionSpec {
    pub fn new(channel: &'static str, aesthetic: impl Into<String>, axis: PanelAxis) -> Self {
        Self {
            channel,
            aesthetic: aesthetic.into(),
            axis,
        }
    }
}

/// A material aesthetic: ggsql `aesthetic` → hephaestus `channel`, producing
/// `kind`, with a fallback `default` applied when the aesthetic isn't mapped.
pub struct MaterialSpec {
    pub aesthetic: &'static str,
    pub channel: &'static str,
    pub kind: RangeKind,
    pub default: MatDefault,
}

impl MaterialSpec {
    pub fn new(
        aesthetic: &'static str,
        channel: &'static str,
        kind: RangeKind,
        default: MatDefault,
    ) -> Self {
        Self {
            aesthetic,
            channel,
            kind,
            default,
        }
    }
}

/// Fallback for an unmapped material channel, so output matches ggsql defaults.
pub enum MatDefault {
    None,
    Color(Color),
    Number(f64),
}

/// What a geom needs wired: its position channels, material table, any raw
/// (unscaled) string channels (e.g. text labels), and whether it groups rows.
pub struct GeomSpec {
    pub positions: Vec<PositionSpec>,
    pub material: Vec<MaterialSpec>,
    /// Unscaled string channels set from a mapped aesthetic (e.g. text labels):
    /// (hephaestus channel, ggsql aesthetic).
    pub raw_strings: &'static [(&'static str, &'static str)],
    /// Constant panel-space channel values, scale-bypassing (e.g. a rule's
    /// 0..1 span, discrete-tile band edges): (hephaestus channel, value).
    pub raw_numbers: Vec<(&'static str, f64)>,
    pub grouped: bool,
}

/// Build a concrete geom from its spec and attach it to the plot, recording its
/// scales/bindings/axes/legends in `w`.
pub fn build_and_add<G>(plot: &mut HPlot, spec: GeomSpec, ctx: &Ctx, w: &mut Wiring) -> Result<()>
where
    G: BuildableGeom + Geom + 'static,
{
    let mut builder = GeomBuilder::<G>::new();
    if spec.grouped {
        if let Some(keys) = build_group_keys(ctx.df, &ctx.layer.partition_by)? {
            builder.keys(keys);
        }
    }
    wire_positions(&mut builder, &spec.positions, ctx, w)?;
    for (channel, aesthetic) in spec.raw_strings {
        if let Some(col) = aesthetic_column_name(ctx.layer, aesthetic) {
            builder.set(*channel, Raw(column_to_strings(ctx.df, col)?));
        }
    }
    for (channel, value) in &spec.raw_numbers {
        builder.set(*channel, Raw(*value));
    }
    wire_material(&mut builder, &spec.material, ctx, w)?;
    plot.add_geom(builder.build());
    Ok(())
}

/// Set position channels and register the `pos1`/`pos2` scales + axes. Each
/// axis scale's domain is the union extent of the position columns on that axis.
fn wire_positions<G: BuildableGeom>(
    builder: &mut GeomBuilder<G>,
    positions: &[PositionSpec],
    ctx: &Ctx,
    w: &mut Wiring,
) -> Result<()> {
    let mut x_extent: Option<(f64, f64)> = None;
    let mut y_extent: Option<(f64, f64)> = None;

    for p in positions {
        let col = aesthetic_column_name(ctx.layer, &p.aesthetic).ok_or_else(|| {
            GgsqlError::WriterError(format!(
                "{} layer has no {} mapping",
                ctx.layer.geom.geom_type(),
                p.aesthetic
            ))
        })?;
        let data = column_to_channel(ctx.df, col)?;
        let extent = data.extent();
        match p.axis {
            PanelAxis::X => merge_extent(&mut x_extent, extent),
            PanelAxis::Y => merge_extent(&mut y_extent, extent),
        }
        data.apply(builder, p.channel);
        w.bindings
            .push((p.channel, p.axis.scale_name().to_string()));
    }

    if let Some(extent) = x_extent {
        register_axis(ctx, w, PanelAxis::X, extent);
    }
    if let Some(extent) = y_extent {
        register_axis(ctx, w, PanelAxis::Y, extent);
    }
    Ok(())
}

/// Register one panel axis's scale and axis chrome.
fn register_axis(ctx: &Ctx, w: &mut Wiring, axis: PanelAxis, extent: (f64, f64)) {
    let name = axis.scale_name();
    let scale = build_scale(ctx.spec.find_scale(name), extent, RangeKind::Position);
    w.registered.push((name.to_string(), scale));

    let mut rail = Axis::rail(name, AxisPlacement::Cartesian(axis.side()));
    if let Some(title) = aesthetic_label(ctx.spec, ctx.layer, name) {
        rail = rail.title(title);
    }
    w.axes.push(rail);
}

/// Set material channels: data-mapped → scale + binding + legend; identity/
/// literal → `Raw` visual values; unmapped → the spec's default.
fn wire_material<G: BuildableGeom>(
    builder: &mut GeomBuilder<G>,
    material: &[MaterialSpec],
    ctx: &Ctx,
    w: &mut Wiring,
) -> Result<()> {
    let mut handled: HashSet<&str> = HashSet::new();

    for m in material {
        if handled.contains(m.channel) {
            continue;
        }
        let Some(col) = aesthetic_column_name(ctx.layer, m.aesthetic) else {
            continue;
        };
        handled.insert(m.channel);

        let scale = ctx.spec.find_scale(m.aesthetic);
        let type_kind = scale
            .and_then(|s| s.scale_type.as_ref())
            .map(|st| st.scale_type_kind());
        let data_mapped = scale.is_some() && type_kind != Some(ScaleTypeKind::Identity);

        if data_mapped {
            let data = column_to_channel(ctx.df, col)?;
            let extent = data.extent();
            let scale_name = shared_scale_name(w, ctx, m, extent);
            data.apply(builder, m.channel);
            w.bindings.push((m.channel, scale_name.clone()));
            w.legends.push(material_legend(
                &scale_name,
                m.channel,
                m.kind,
                type_kind,
                aesthetic_label(ctx.spec, ctx.layer, m.aesthetic),
            ));
        } else {
            match m.kind {
                RangeKind::Color => {
                    builder.set(m.channel, Raw(column_to_colors(ctx.df, col)?));
                }
                RangeKind::Shape => {
                    builder.set(m.channel, Raw(column_to_strings(ctx.df, col)?));
                }
                _ => {
                    builder.set(m.channel, Raw(column_to_f64(ctx.df, col)?));
                }
            }
        }
    }

    // Defaults for channels no spec mapped.
    for m in material {
        if handled.contains(m.channel) {
            continue;
        }
        match m.default {
            MatDefault::Color(c) => {
                builder.set(m.channel, c);
                handled.insert(m.channel);
            }
            MatDefault::Number(n) => {
                builder.set(m.channel, n);
                handled.insert(m.channel);
            }
            MatDefault::None => {}
        }
    }
    Ok(())
}

/// Reuse (or create) the shared scale for a material channel's `(source, kind)`,
/// returning the scale name to bind/legend against.
fn shared_scale_name(w: &mut Wiring, ctx: &Ctx, m: &MaterialSpec, extent: (f64, f64)) -> String {
    let key = (aesthetic_source(ctx.layer, m.aesthetic), m.kind);
    if let Some(existing) = w.shared_scales.get(&key) {
        return existing.clone();
    }
    let scale = build_scale(ctx.spec.find_scale(m.aesthetic), extent, m.kind);
    w.registered.push((m.aesthetic.to_string(), scale));
    w.shared_scales.insert(key, m.aesthetic.to_string());
    m.aesthetic.to_string()
}

/// Union of two data extents.
fn merge_extent(slot: &mut Option<(f64, f64)>, e: (f64, f64)) {
    *slot = Some(match *slot {
        Some((min, max)) => (min.min(e.0), max.max(e.1)),
        None => e,
    });
}

/// Resolve a label for an aesthetic: explicit `LABEL` wins (`None` suppresses),
/// else the original mapped column name is the default.
pub fn aesthetic_label(spec: &Plot, layer: &Layer, aesthetic: &str) -> Option<String> {
    if let Some(labels) = &spec.labels {
        if let Some(entry) = labels.labels.get(aesthetic) {
            return entry.clone();
        }
    }
    match layer.mappings.get(aesthetic) {
        Some(AestheticValue::Column {
            original_name: Some(name),
            ..
        }) => Some(name.clone()),
        _ => None,
    }
}

/// A mapping's underlying data source — original column name when known, else
/// the internal column name. Lets color-family channels sharing a source
/// collapse to one scale + legend.
fn aesthetic_source(layer: &Layer, aesthetic: &str) -> String {
    match layer.mappings.get(aesthetic) {
        Some(AestheticValue::Column {
            original_name: Some(name),
            ..
        }) => name.clone(),
        Some(AestheticValue::Column { name, .. }) => name.clone(),
        Some(AestheticValue::AnnotationColumn { name }) => name.clone(),
        _ => aesthetic.to_string(),
    }
}

/// Build a legend for a data-mapped material scale. Continuous color uses a
/// colorbar; everything else a keyed point legend at the scale's breaks.
fn material_legend(
    scale_name: &str,
    channel: &str,
    kind: RangeKind,
    type_kind: Option<ScaleTypeKind>,
    title: Option<String>,
) -> Legend {
    let continuous_color = kind == RangeKind::Color
        && matches!(
            type_kind,
            Some(ScaleTypeKind::Continuous) | Some(ScaleTypeKind::Binned)
        );
    let mut legend = if continuous_color {
        Legend::colorbar(scale_name).side(LegendSide::Right)
    } else {
        Legend::new(scale_name)
            .side(LegendSide::Right)
            .key(LegendKeySpec::point().scaled(channel, scale_name))
    };
    if let Some(title) = title {
        legend = legend.title(title);
    }
    legend
}
