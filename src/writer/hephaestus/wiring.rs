//! Shared geom-wiring: position/material channels, scales, axes, legends, and
//! group keys. Each geom module declares its channel specs; these helpers do the
//! repetitive work, generic over the concrete geom builder.

use std::collections::HashSet;

use hephaestus::color::{rgb8, Color};
use hephaestus::plot::chrome::legend::{Legend, LegendKeySpec};
use hephaestus::plot::geom::{BuildableGeom, Geom, GeomBuilder, Raw};
use hephaestus::plot::Plot as HPlot;
use hephaestus::scales::chrome::LegendSide;
use hephaestus::scales::value::Value as HValue;

use super::channels::{
    aesthetic_column_name, build_group_keys, column_to_channel, column_to_colors, column_to_f64,
    column_to_strings, ChannelData,
};
use super::scales::{map_linetype, parse_color, RangeKind};
use crate::plot::{ParameterValue, ScaleTypeKind};
use crate::{AestheticValue, DataFrame, GgsqlError, Layer, Plot, Result};

/// Read-only context for building one layer's geom.
pub struct Ctx<'a> {
    pub spec: &'a Plot,
    pub layer: &'a Layer,
    pub df: &'a DataFrame,
    /// Whether the layer is in transposed (horizontal) orientation.
    pub transposed: bool,
    /// Scale name this panel binds `pos1` (x) to: the shared `"pos1"` when fixed,
    /// a per-panel name when the facet dimension is free.
    pub pos1_scale: &'a str,
    /// Scale name this panel binds `pos2` (y) to.
    pub pos2_scale: &'a str,
    /// Sink collecting the legends a geom would draw. Legends are registered once
    /// on the composition (never on the per-panel plot), so faceted plots get a
    /// single shared legend rather than one per panel. `Some` only while building
    /// the first panel — every panel produces the same legends (all built from the
    /// globally resolved scales), so one capture suffices.
    pub legends: Option<&'a std::cell::RefCell<Vec<Legend>>>,
}

impl Ctx<'_> {
    /// The scale name to bind a position channel on `axis` to (panel-aware for
    /// free facet scales).
    pub fn pos_scale(&self, axis: PanelAxis) -> &str {
        match axis {
            PanelAxis::X => self.pos1_scale,
            PanelAxis::Y => self.pos2_scale,
        }
    }

    /// Record a legend for later registration on the composition. A no-op once
    /// the first panel has been captured (`legends` is `None`).
    pub fn push_legend(&self, legend: Legend) {
        if let Some(sink) = self.legends {
            sink.borrow_mut().push(legend);
        }
    }
}

/// Which panel axis a position channel drives.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PanelAxis {
    X,
    Y,
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

/// Build a concrete geom from its spec and attach it to the plot. Bindings are
/// written onto `plot`; legends are recorded on `ctx` for one-shot registration
/// on the composition; scales are registered globally from `spec.scales` (see
/// `HephaestusWriter::write`), and axes are created per coordinate system in
/// `projection`.
pub fn build_and_add<G>(plot: &mut HPlot, spec: GeomSpec, ctx: &Ctx) -> Result<()>
where
    G: BuildableGeom + Geom + 'static,
{
    let mut builder = GeomBuilder::<G>::new();
    if spec.grouped {
        if let Some(keys) = build_group_keys(ctx.df, &ctx.layer.partition_by)? {
            builder.keys(keys);
        }
    }
    // Band channels the geom computes itself (bar/tile edges) already carry the
    // position adjustment, so `wire_positions` must not overwrite them.
    let claimed: Vec<&str> = spec.data_channels.iter().map(|(c, _)| *c).collect();
    wire_positions(&mut builder, &spec.positions, plot, ctx, &claimed)?;
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
    wire_material(&mut builder, &spec.material, plot, ctx, spec.legend_key)?;
    plot.add_geom(builder.build());
    Ok(())
}

/// Set position channels on the builder and bind them to the `pos1`/`pos2`
/// scales. `set_binding` is idempotent, so repeated bindings across layers are
/// harmless. Axis chrome is created later, per coordinate system, in `projection`.
///
/// Each position also picks up the layer's position adjustment: `dodge` and
/// `jitter` are resolved by ggsql into per-row band fractions on the adjusted
/// axis, which map onto the geom's matching `_band` channel. Channels listed in
/// `claimed` are skipped — a geom that derives its own band edges (bar, tile) has
/// already folded the same offsets in.
fn wire_positions<G: BuildableGeom>(
    builder: &mut GeomBuilder<G>,
    positions: &[PositionSpec],
    plot: &mut HPlot,
    ctx: &Ctx,
    claimed: &[&str],
) -> Result<()> {
    let offsets = AxisOffsets::new(ctx.df);
    for p in positions {
        let col = aesthetic_column_name(ctx.layer, &p.aesthetic).ok_or_else(|| {
            GgsqlError::WriterError(format!(
                "{} layer has no {} mapping",
                ctx.layer.geom.geom_type(),
                p.aesthetic
            ))
        })?;
        let data = column_to_channel(ctx.df, col)?;
        data.apply(builder, p.channel);
        plot.set_binding(p.channel, ctx.pos_scale(p.axis));

        if let Some(values) = offsets.for_axis(p.axis) {
            let band = format!("{}_band", p.channel);
            if !claimed.contains(&band.as_str()) {
                builder.set(band, values.clone());
            }
        }
    }
    Ok(())
}

/// The per-row band-fraction offsets ggsql resolved for a position adjustment,
/// per panel axis. `None` for an axis the layer wasn't adjusted along — which is
/// every axis for `position => 'identity'`, and the value axis always (`stack`
/// rewrites the value columns instead of offsetting).
struct AxisOffsets {
    x: Option<Vec<f64>>,
    y: Option<Vec<f64>>,
}

impl AxisOffsets {
    fn new(df: &DataFrame) -> Self {
        Self {
            x: offset_column(df, "pos1offset"),
            y: offset_column(df, "pos2offset"),
        }
    }

    fn for_axis(&self, axis: PanelAxis) -> Option<&Vec<f64>> {
        match axis {
            PanelAxis::X => self.x.as_ref(),
            PanelAxis::Y => self.y.as_ref(),
        }
    }
}

/// Set material channels: data-mapped → bind channel to its (globally
/// registered) scale + record a legend; literal → constant visual value; identity/
/// annotation → `Raw` per-row values; unmapped → the spec's default. Public so
/// custom-builder geoms (e.g. `spatial`) can wire their scalar aesthetics through
/// the same data-mapped-capable path the generic geoms use.
pub fn wire_material<G: BuildableGeom>(
    builder: &mut GeomBuilder<G>,
    material: &[MaterialSpec],
    plot: &mut HPlot,
    ctx: &Ctx,
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
            data.apply(builder, m.channel);
            // Bind the channel to the aesthetic's scale (registered globally) and
            // record a legend. hephaestus collapses compatible legends, so repeated
            // records across layers for the same scale merge at registration.
            plot.set_binding(m.channel, m.aesthetic);
            ctx.push_legend(material_legend(
                ctx,
                m.aesthetic,
                m.channel,
                m.kind,
                type_kind,
                aesthetic_label(ctx.spec, ctx.layer, m.aesthetic),
                legend_kind,
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

/// The axis roles of a banded geom (boxplot / violin / range): its categories —
/// or, for a range, its fixed positions — sit on one axis and its values on the
/// other. ggsql flips the position columns of a transposed (horizontal) layer, so
/// the banded axis becomes `pos2` and the values land in the `pos1` family;
/// every channel name follows from that.
///
/// Hephaestus channels are named per panel axis (`x`, `y_band`, …), so a geom
/// asks this for the channel that drives its banded or value axis instead of
/// hardcoding `x`/`y`. The `pos1`/`pos2` *bindings* need no swap: a channel
/// always belongs to the same panel axis.
#[derive(Clone, Copy)]
pub struct BandAxes {
    transposed: bool,
}

impl BandAxes {
    pub fn new(ctx: &Ctx) -> Self {
        Self {
            transposed: ctx.transposed,
        }
    }

    /// The aesthetic family holding the banded-axis positions (`"pos1"`).
    pub fn band(&self) -> &'static str {
        if self.transposed {
            "pos2"
        } else {
            "pos1"
        }
    }

    /// The aesthetic family holding the values (`"pos2"`).
    pub fn value(&self) -> &'static str {
        if self.transposed {
            "pos1"
        } else {
            "pos2"
        }
    }

    /// The aesthetic carrying position-adjustment (dodge) offsets on the banded
    /// axis.
    pub fn dodge(&self) -> &'static str {
        if self.transposed {
            "pos2offset"
        } else {
            "pos1offset"
        }
    }

    /// The two banded-axis position channels (`("x", "x2")`).
    pub fn band_channels(&self) -> (&'static str, &'static str) {
        if self.transposed {
            ("y", "y2")
        } else {
            ("x", "x2")
        }
    }

    /// The two value-axis position channels (`("y", "y2")`).
    pub fn value_channels(&self) -> (&'static str, &'static str) {
        if self.transposed {
            ("x", "x2")
        } else {
            ("y", "y2")
        }
    }

    /// The banded axis's band-fraction offset channels (`("x_band", "x2_band")`),
    /// which shift a mark's two edges within the category band.
    pub fn band_fraction_channels(&self) -> (&'static str, &'static str) {
        if self.transposed {
            ("y_band", "y2_band")
        } else {
            ("x_band", "x2_band")
        }
    }

    /// The banded axis's absolute-pt offset channels (`("x_offset",
    /// "x2_offset")`), for marks sized in points rather than band fractions
    /// (hinge caps).
    pub fn band_offset_channels(&self) -> (&'static str, &'static str) {
        if self.transposed {
            ("y_offset", "y2_offset")
        } else {
            ("x_offset", "x2_offset")
        }
    }
}

/// The `side` SETTING as a signed direction along the banded axis: `None` for
/// `'both'` (a full-width mark centred on the band), else the sign of the half
/// the mark occupies.
///
/// Hephaestus band offsets are positive-right on x and positive-up on y, so
/// `'top'`/`'right'` are positive in either orientation — which reproduces the
/// Vega-Lite writer's visual outcome (there the sign flips with orientation
/// because Vega-Lite's y offsets point down).
pub fn side_sign(layer: &Layer) -> Option<f64> {
    match layer.parameters.get("side")? {
        ParameterValue::String(s) => match s.as_str() {
            "top" | "right" => Some(1.0),
            "bottom" | "left" => Some(-1.0),
            _ => None,
        },
        _ => None,
    }
}

/// The two edges of a banded mark, as offsets from the band centre: the full
/// `±half` band for `side => 'both'`, else the centreline → `±half` half-band.
/// Used for band fractions (box, median, violin edge) and for pt-sized marks
/// (hinge caps) alike.
pub fn band_edges(half: f64, side: Option<f64>) -> (f64, f64) {
    match side {
        None => (-half, half),
        Some(sign) => (0.0, sign * half),
    }
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

/// A position-adjustment offset column (per-row band fractions), or zeros when
/// the layer wasn't adjusted along that axis. For geoms that derive their own band
/// edges and so need the offsets as numbers; the generic path wires the same
/// column onto a `_band` channel in [`wire_positions`].
pub fn dodge_offsets(df: &DataFrame, aesthetic: &str) -> Vec<f64> {
    offset_column(df, aesthetic).unwrap_or_else(|| vec![0.0; df.height()])
}

/// ggsql's resolved offset column for one axis, when the layer carries one.
fn offset_column(df: &DataFrame, aesthetic: &str) -> Option<Vec<f64>> {
    let name = crate::naming::aesthetic_column(aesthetic);
    df.column(&name).ok()?;
    column_to_f64(df, &name).ok()
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

/// Resolve a plot-level label (`title`, `subtitle`, `caption`) from the `LABEL`
/// clause. `None` covers both "not set" and `LABEL <key> => NULL` (suppressed).
///
/// Literal `\n` in the SQL string literal becomes a real newline, matching the
/// Vega-Lite writer's `split_label_on_newlines`.
pub fn plot_label(spec: &Plot, key: &str) -> Option<String> {
    let text = spec.labels.as_ref()?.labels.get(key)?.as_ref()?;
    Some(text.replace("\\n", "\n"))
}

/// A resolved material aesthetic for a composite geom, mirroring the Vega-Lite
/// writer's shared-encoding model: either a data-mapped column (scaled through a
/// registered scale that is bound + legended once, carrying that scale's name) or
/// a constant visual value. Components select the rows they cover and apply it to
/// a channel, so one resolved aesthetic styles every part of the composite.
pub enum MaterialSource {
    Data { data: ChannelData, scale: String },
    Constant(HValue),
}

impl MaterialSource {
    /// Set `channel` for the `idx` rows: the scaled data subset, or the constant.
    pub fn apply<G: BuildableGeom>(
        &self,
        builder: &mut GeomBuilder<G>,
        channel: &str,
        idx: &[usize],
    ) {
        match self {
            MaterialSource::Data { data, .. } => data.select(idx).apply(builder, channel),
            MaterialSource::Constant(v) => {
                builder.set(channel, v.clone());
            }
        }
    }

    /// The registered scale name, when data-mapped (for binding extra channels,
    /// e.g. a ribbon's far edge, to the same scale).
    pub fn scale_name(&self) -> Option<&str> {
        match self {
            MaterialSource::Data { scale, .. } => Some(scale),
            MaterialSource::Constant(_) => None,
        }
    }
}

/// Resolve a color aesthetic (`fill`, `stroke`, …) for a composite geom, falling
/// back to `default` when unmapped. See [`resolve_material`].
pub fn resolve_color(
    ctx: &Ctx,
    plot: &mut HPlot,
    aesthetic: &'static str,
    channel: &'static str,
    default: Color,
    legend_kind: LegendKind,
) -> Result<MaterialSource> {
    Ok(
        resolve_material(ctx, plot, aesthetic, channel, RangeKind::Color, legend_kind)?
            .unwrap_or(MaterialSource::Constant(HValue::Color(default))),
    )
}

/// Like [`resolve_color`] but with no fallback. For aesthetics whose ggsql
/// default is `Null` (e.g. a text geom's `stroke`), where "unmapped" must leave
/// the channel unset rather than substitute a color.
pub fn resolve_optional_color(
    ctx: &Ctx,
    plot: &mut HPlot,
    aesthetic: &'static str,
    channel: &'static str,
    legend_kind: LegendKind,
) -> Result<Option<MaterialSource>> {
    resolve_material(ctx, plot, aesthetic, channel, RangeKind::Color, legend_kind)
}

/// Resolve a material aesthetic for a composite geom, dispatching the same three
/// ways as [`wire_material`] does for simple geoms, but returning a value the
/// caller can apply to a row subset (which `wire_material`, being whole-column,
/// cannot).
///
/// A data-mapped non-identity scale binds `channel` to the aesthetic's (globally
/// registered) scale and records one legend; the full column is returned for
/// components to select. Otherwise the constant visual value: an identity /
/// annotation column's first value, else the mapped literal (`SETTING linewidth
/// => 3`). `None` when the aesthetic isn't mapped at all.
///
/// hephaestus collapses compatible legends, so repeated binds across a
/// composite's components merge at registration.
pub fn resolve_material(
    ctx: &Ctx,
    plot: &mut HPlot,
    aesthetic: &'static str,
    channel: &'static str,
    kind: RangeKind,
    legend_kind: LegendKind,
) -> Result<Option<MaterialSource>> {
    let type_kind = ctx
        .spec
        .find_scale(aesthetic)
        .and_then(|s| s.scale_type.as_ref())
        .map(|st| st.scale_type_kind());
    if is_data_mapped(ctx, aesthetic) {
        let col = aesthetic_column_name(ctx.layer, aesthetic);
        plot.set_binding(channel, aesthetic);
        ctx.push_legend(material_legend(
            ctx,
            aesthetic,
            channel,
            kind,
            type_kind,
            aesthetic_label(ctx.spec, ctx.layer, aesthetic),
            legend_kind,
        ));
        return Ok(Some(MaterialSource::Data {
            data: column_to_channel(ctx.df, col.unwrap())?,
            scale: aesthetic.to_string(),
        }));
    }
    Ok(constant_material(ctx, aesthetic, kind).map(MaterialSource::Constant))
}

/// Whether an aesthetic maps a data column through a scale that actually
/// transforms it — i.e. it is scaled and legended, rather than carrying
/// visual-space values (an identity scale / annotation column) or a constant.
fn is_data_mapped(ctx: &Ctx, aesthetic: &str) -> bool {
    let scale = ctx.spec.find_scale(aesthetic);
    let type_kind = scale
        .and_then(|s| s.scale_type.as_ref())
        .map(|st| st.scale_type_kind());
    aesthetic_column_name(ctx.layer, aesthetic).is_some()
        && scale.is_some()
        && type_kind != Some(ScaleTypeKind::Identity)
}

/// The constant visual value of an unscaled material aesthetic: an identity /
/// annotation column's first value, else a bare `Literal`, converted by the
/// channel's `RangeKind` (hephaestus takes widths in points, as ggsql resolves
/// them, so numbers pass through). `None` when unmapped or inapplicable.
fn constant_material(ctx: &Ctx, aesthetic: &str, kind: RangeKind) -> Option<HValue> {
    let col = aesthetic_column_name(ctx.layer, aesthetic);
    let literal = match ctx.layer.mappings.aesthetics.get(aesthetic) {
        Some(AestheticValue::Literal(lit)) => Some(lit),
        _ => None,
    };
    match kind {
        RangeKind::Color => {
            if let Some(c) = col
                .and_then(|c| column_to_colors(ctx.df, c).ok())
                .and_then(|v| v.first().copied())
            {
                return Some(HValue::Color(c));
            }
            match literal {
                Some(ParameterValue::String(s)) => parse_color(s).map(HValue::Color),
                _ => None,
            }
        }
        RangeKind::Number | RangeKind::Position => {
            if let Some(n) = col
                .and_then(|c| column_to_f64(ctx.df, c).ok())
                .and_then(|v| v.first().copied())
                .filter(|x| x.is_finite())
            {
                return Some(HValue::Number(n));
            }
            match literal {
                Some(ParameterValue::Number(n)) if n.is_finite() => Some(HValue::Number(*n)),
                _ => None,
            }
        }
        RangeKind::Linetype => {
            let name = col
                .and_then(|c| column_to_strings(ctx.df, c).ok())
                .and_then(|v| v.first().cloned())
                .or_else(|| match literal {
                    Some(ParameterValue::String(s)) => Some(s.clone()),
                    _ => None,
                })?;
            Some(HValue::Linetype(map_linetype(&name)))
        }
        RangeKind::Shape => {
            let name = col
                .and_then(|c| column_to_strings(ctx.df, c).ok())
                .and_then(|v| v.first().cloned())
                .or_else(|| match literal {
                    Some(ParameterValue::String(s)) => Some(s.clone()),
                    _ => None,
                })?;
            Some(HValue::String(name.into()))
        }
    }
}

/// Build a legend for a data-mapped material scale. Continuous color uses a
/// colorbar; everything else a keyed legend (swatch per `legend_kind`) at the
/// scale's breaks.
pub fn material_legend(
    ctx: &Ctx,
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
        let mut key = match legend_kind {
            LegendKind::Point => LegendKeySpec::point(),
            LegendKind::Line => LegendKeySpec::line(),
            LegendKind::Rect => LegendKeySpec::rect(),
        }
        .scaled(channel, scale_name);
        // A key only paints what it is told to paint, so when the scaled channel
        // isn't itself a color the glyph needs one — otherwise the swatch is
        // invisible next to its label. Use the layer's constant color, matching
        // the marks the legend describes.
        if kind != RangeKind::Color {
            let body = match legend_kind {
                LegendKind::Line => "stroke",
                LegendKind::Point | LegendKind::Rect => "fill",
            };
            key = key.fixed(body, HValue::Color(key_color(ctx, legend_kind)));
        }
        Legend::new(scale_name).side(LegendSide::Right).key(key)
    };
    if let Some(title) = title {
        legend = legend.title(title);
    }
    legend
}

/// The color a non-color legend key paints its glyph with: the layer's constant
/// color for the aesthetic carrying the glyph's body, the other color aesthetic
/// as a fallback (a stroke-only geom has no fill, and vice versa), else a neutral
/// grey. A data-mapped color aesthetic has no single constant, so it falls through
/// to the grey — that scale gets its own legend anyway.
fn key_color(ctx: &Ctx, legend_kind: LegendKind) -> Color {
    let order = match legend_kind {
        LegendKind::Line => ["stroke", "fill"],
        LegendKind::Point | LegendKind::Rect => ["fill", "stroke"],
    };
    for aesthetic in order {
        // A data-mapped color has no constant to borrow — its column holds domain
        // values, not colors — and it carries its own legend anyway.
        if is_data_mapped(ctx, aesthetic) {
            continue;
        }
        if let Some(HValue::Color(c)) = constant_material(ctx, aesthetic, RangeKind::Color) {
            return c;
        }
    }
    rgb8(64, 64, 64)
}
