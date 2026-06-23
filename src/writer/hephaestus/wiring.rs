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
use hephaestus::scales::value::Value as HValue;

use super::channels::{
    aesthetic_column_name, build_group_keys, column_to_channel, column_to_colors, column_to_f64,
    column_to_strings, ChannelData,
};
use super::scales::{build_scale, map_linetype, parse_color, RangeKind};
use crate::plot::{ParameterValue, ScaleTypeKind};
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

/// The legend swatch a geom's data-mapped scales use, so the key matches the
/// mark (a colored line for line geoms, a filled rect for bars/areas, etc.).
#[derive(Clone, Copy)]
pub enum LegendKind {
    Point,
    Line,
    Rect,
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
    /// Per-row unscaled channel data the geom computes itself (e.g. bar band
    /// edges from width/dodge): (hephaestus channel, one value per row).
    pub data_channels: Vec<(&'static str, Vec<f64>)>,
    /// Legend swatch style for this geom's data-mapped scales.
    pub legend_key: LegendKind,
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
    for (channel, values) in spec.data_channels {
        builder.set(channel, values);
    }
    wire_material(&mut builder, &spec.material, ctx, w, spec.legend_key)?;
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

/// Register one panel axis's scale and axis chrome. Composite geoms call this
/// directly to set up shared `pos1`/`pos2` scales before building components.
///
/// Idempotent across layers: a position scale is registered (and its axis added)
/// exactly once, since ggsql's resolved domain is global — every layer that uses
/// the axis shares the same scale. `extent` is only a fallback for an unresolved
/// scale, so the first layer's value is authoritative.
pub fn register_axis(ctx: &Ctx, w: &mut Wiring, axis: PanelAxis, extent: (f64, f64)) {
    let name = axis.scale_name();
    if w.registered.iter().any(|(n, _)| n == name) {
        return;
    }
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
    legend_kind: LegendKind,
) -> Result<()> {
    let mut handled: HashSet<&str> = HashSet::new();

    for m in material {
        if handled.contains(m.channel) {
            continue;
        }
        // ggsql delivers material values three ways (mirroring the Vega-Lite
        // writer's `build_encoding_channel`): a bare `Literal` (a fixed value,
        // from a geom default or `SETTING`), a data-mapped `Column` (scaled +
        // legend), or an identity `AnnotationColumn` (per-row constant, from
        // PLACE).
        if let Some(AestheticValue::Literal(lit)) = ctx.layer.mappings.get(m.aesthetic) {
            if set_literal_channel(builder, m.channel, m.kind, lit) {
                handled.insert(m.channel);
            }
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
            let (scale_name, is_new) = shared_scale_name(w, ctx, m, extent);
            data.apply(builder, m.channel);
            w.bindings.push((m.channel, scale_name.clone()));
            // One legend per scale: skip when an earlier channel/layer already
            // registered it (hephaestus collapses compatible stack legends but
            // not colorbars, so duplicates must not be pushed).
            if is_new {
                w.legends.push(material_legend(
                    &scale_name,
                    m.channel,
                    m.kind,
                    type_kind,
                    aesthetic_label(ctx.spec, ctx.layer, m.aesthetic),
                    legend_kind,
                ));
            }
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

/// Set a material channel to a constant from a `Literal` aesthetic value,
/// converting by the channel's `RangeKind` (mirrors the Vega-Lite writer's
/// `build_literal_encoding`). Hephaestus takes sizes/widths in the same units
/// ggsql resolves them to (points), so numbers pass through unscaled. Returns
/// whether the value was applicable (an unparseable color / type mismatch is
/// left to the geom's default).
fn set_literal_channel<G: BuildableGeom>(
    builder: &mut GeomBuilder<G>,
    channel: &str,
    kind: RangeKind,
    lit: &ParameterValue,
) -> bool {
    match (kind, lit) {
        (RangeKind::Color, ParameterValue::String(s)) => match parse_color(s) {
            Some(c) => {
                builder.set(channel, c);
                true
            }
            None => false,
        },
        (RangeKind::Shape, ParameterValue::String(s)) => {
            builder.set(channel, s.clone());
            true
        }
        (RangeKind::Linetype, ParameterValue::String(s)) => {
            builder.set(channel, HValue::Linetype(map_linetype(s)));
            true
        }
        (RangeKind::Number, ParameterValue::Number(n)) if n.is_finite() => {
            builder.set(channel, *n);
            true
        }
        _ => false,
    }
}

/// Reuse (or create) the shared scale for a material channel's `(source, kind)`,
/// returning `(scale_name, is_new)` — `is_new` is `true` only when this call
/// registered the scale, so the caller adds its legend exactly once.
fn shared_scale_name(
    w: &mut Wiring,
    ctx: &Ctx,
    m: &MaterialSpec,
    extent: (f64, f64),
) -> (String, bool) {
    let key = (aesthetic_source(ctx.layer, m.aesthetic), m.kind);
    if let Some(existing) = w.shared_scales.get(&key) {
        return (existing.clone(), false);
    }
    let scale = build_scale(ctx.spec.find_scale(m.aesthetic), extent, m.kind);
    w.registered.push((m.aesthetic.to_string(), scale));
    w.shared_scales.insert(key, m.aesthetic.to_string());
    (m.aesthetic.to_string(), true)
}

/// Half the band width a banded geom (bar/box/violin) occupies: the
/// dodge-narrowed width if set, else the `width` parameter (or `default`).
pub fn band_half_width(layer: &Layer, default: f64) -> f64 {
    let width = layer
        .parameters
        .get("width")
        .and_then(|v| match v {
            ParameterValue::Number(n) => Some(*n),
            _ => None,
        })
        .unwrap_or(default);
    layer.adjusted_width.unwrap_or(width).abs() / 2.0
}

/// A constant color from an aesthetic, or `default` when unmapped. Reads an
/// annotation column first, then a bare `Literal` string (e.g. `stroke =>
/// 'black'`). Composite geoms use this for uniform styling like a box outline.
pub fn constant_color(ctx: &Ctx, aesthetic: &str, default: Color) -> Color {
    if let Some(c) = aesthetic_column_name(ctx.layer, aesthetic)
        .and_then(|c| column_to_colors(ctx.df, c).ok())
        .and_then(|v| v.first().copied())
    {
        return c;
    }
    if let Some(AestheticValue::Literal(ParameterValue::String(s))) =
        ctx.layer.mappings.aesthetics.get(aesthetic)
    {
        if let Some(c) = super::scales::parse_color(s) {
            return c;
        }
    }
    default
}

/// A constant number from an aesthetic, or `default` when unmapped. Reads an
/// annotation column first, then a bare `Literal` number (e.g. `slope => 1`).
pub fn constant_number(ctx: &Ctx, aesthetic: &str, default: f64) -> f64 {
    if let Some(n) = aesthetic_column_name(ctx.layer, aesthetic)
        .and_then(|c| column_to_f64(ctx.df, c).ok())
        .and_then(|v| v.first().copied())
        .filter(|x| x.is_finite())
    {
        return n;
    }
    if let Some(AestheticValue::Literal(ParameterValue::Number(n))) =
        ctx.layer.mappings.aesthetics.get(aesthetic)
    {
        if n.is_finite() {
            return *n;
        }
    }
    default
}

/// A constant string from an aesthetic, or `default` when unmapped. Reads an
/// annotation column first, then a bare `Literal` string (e.g. `shape =>
/// 'circle'`). Used for constant shape names on composite geom components.
pub fn constant_string(ctx: &Ctx, aesthetic: &str, default: &str) -> String {
    if let Some(s) = aesthetic_column_name(ctx.layer, aesthetic)
        .and_then(|c| column_to_strings(ctx.df, c).ok())
        .and_then(|v| v.first().cloned())
    {
        return s;
    }
    if let Some(AestheticValue::Literal(ParameterValue::String(s))) =
        ctx.layer.mappings.aesthetics.get(aesthetic)
    {
        return s.clone();
    }
    default.to_string()
}

/// A dodge offset column (per-row band fractions), or zeros when not dodged.
pub fn dodge_offsets(df: &DataFrame, aesthetic: &str) -> Vec<f64> {
    let name = crate::naming::aesthetic_column(aesthetic);
    if df.column(&name).is_ok() {
        column_to_f64(df, &name).unwrap_or_else(|_| vec![0.0; df.height()])
    } else {
        vec![0.0; df.height()]
    }
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

/// A resolved color aesthetic for a composite geom, mirroring the Vega-Lite
/// writer's shared-encoding model: either a data-mapped color column (scaled
/// through a registered scale that is bound + legended once, carrying that
/// scale's name) or a constant. Components select the rows they cover and apply
/// it to a channel.
pub enum ColorSource {
    Data { data: ChannelData, scale: String },
    Constant(Color),
}

impl ColorSource {
    /// Set `channel` for the `idx` rows: the scaled data subset, or the constant.
    pub fn apply<G: BuildableGeom>(
        &self,
        builder: &mut GeomBuilder<G>,
        channel: &str,
        idx: &[usize],
    ) {
        match self {
            ColorSource::Data { data, .. } => data.select(idx).apply(builder, channel),
            ColorSource::Constant(c) => {
                builder.set(channel, *c);
            }
        }
    }

    /// The registered scale name, when data-mapped (for binding extra channels,
    /// e.g. a ribbon's far edge, to the same scale).
    pub fn scale_name(&self) -> Option<&str> {
        match self {
            ColorSource::Data { scale, .. } => Some(scale),
            ColorSource::Constant(_) => None,
        }
    }
}

/// Resolve a color aesthetic (`fill`, `stroke`, …) for a composite geom. A
/// data-mapped non-identity scale reuses (or registers) the shared color scale
/// for its `(source, kind)`, binds `channel` to it, and adds one legend on first
/// registration; the full color-domain column is returned for components to
/// select. Otherwise the constant value (the mapped literal, else `default`).
pub fn resolve_color(
    ctx: &Ctx,
    w: &mut Wiring,
    aesthetic: &str,
    channel: &'static str,
    default: Color,
    legend_kind: LegendKind,
) -> Result<ColorSource> {
    let scale = ctx.spec.find_scale(aesthetic);
    let kind = scale
        .and_then(|s| s.scale_type.as_ref())
        .map(|st| st.scale_type_kind());
    let col = aesthetic_column_name(ctx.layer, aesthetic);
    let data_mapped = col.is_some() && scale.is_some() && kind != Some(ScaleTypeKind::Identity);
    if !data_mapped {
        return Ok(ColorSource::Constant(constant_color(
            ctx, aesthetic, default,
        )));
    }
    // Reuse the shared color scale for this source (collapsing fill/stroke and
    // cross-layer duplicates), registering + legending it only on first sight.
    let key = (aesthetic_source(ctx.layer, aesthetic), RangeKind::Color);
    let scale_name = match w.shared_scales.get(&key) {
        Some(existing) => existing.clone(),
        None => {
            let hs = build_scale(scale, (0.0, 1.0), RangeKind::Color);
            w.registered.push((aesthetic.to_string(), hs));
            w.shared_scales.insert(key, aesthetic.to_string());
            w.legends.push(material_legend(
                aesthetic,
                channel,
                RangeKind::Color,
                kind,
                aesthetic_label(ctx.spec, ctx.layer, aesthetic),
                legend_kind,
            ));
            aesthetic.to_string()
        }
    };
    w.bindings.push((channel, scale_name.clone()));
    Ok(ColorSource::Data {
        data: column_to_channel(ctx.df, col.unwrap())?,
        scale: scale_name,
    })
}

/// Build a legend for a data-mapped material scale. Continuous color uses a
/// colorbar; everything else a keyed legend (swatch per `legend_kind`) at the
/// scale's breaks.
pub fn material_legend(
    scale_name: &str,
    channel: &str,
    kind: RangeKind,
    type_kind: Option<ScaleTypeKind>,
    title: Option<String>,
    legend_kind: LegendKind,
) -> Legend {
    let continuous_color = kind == RangeKind::Color
        && matches!(
            type_kind,
            Some(ScaleTypeKind::Continuous) | Some(ScaleTypeKind::Binned)
        );
    let mut legend = if continuous_color {
        Legend::colorbar(scale_name).side(LegendSide::Right)
    } else {
        let key = match legend_kind {
            LegendKind::Point => LegendKeySpec::point(),
            LegendKind::Line => LegendKeySpec::line(),
            LegendKind::Rect => LegendKeySpec::rect(),
        };
        Legend::new(scale_name)
            .side(LegendSide::Right)
            .key(key.scaled(channel, scale_name))
    };
    if let Some(title) = title {
        legend = legend.title(title);
    }
    legend
}
