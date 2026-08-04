# Plan: a Hephaestus writer for ggsql

A second `Writer` implementation that renders a resolved ggsql `Spec` to raster
output via [`hephaestus`](https://github.com/posit-dev/hephaestus) — a
backend-agnostic 2D scene renderer with a high-level grammar-of-graphics plot
API. Intended as the eventual default writer, replacing the Vega-Lite JSON
writer.

Status: **implemented** behind the non-default `hephaestus` feature — all geoms
except `arrow`, all scale types, multi-layer, faceting (fixed + free), Cartesian
/ Polar / Map projections, spatial, and plot chrome (titles, axes, legends,
strips). This document is the design of record and the phase log; §9 lists the
work deliberately deferred.

## 1. Why this is a good fit

ggsql and hephaestus are architecturally complementary and the seam is clean:

| ggsql produces (resolved `Spec`) | hephaestus consumes |
| --- | --- |
| Per-layer `DataFrame`s with **raw values** in internal columns (`__ggsql_aes_pos1__`, …) | Geoms hold **raw columnar `Value` data** and map through scales at draw time |
| Fully-resolved `Scale`s (type, domain, transform, breaks, formatted labels, palette) | `Scale` with `domain_*`, `with_transform`, `with_breaks_labeled`, `with_format`, `range_colors` |
| Scale-type enum: Continuous/Discrete/Binned/Ordinal/Identity | **Identical** enum: Continuous/Discrete/Binned/Ordinal/Identity |
| Transforms: log/log2/sqrt/square/asinh/pseudo_log/… | **Identical** `TransformKind` set (`PseudoLog` doc'd as "matching ggsql's pseudo_log") |
| Coords: Cartesian / Polar / Map (pre-projected in SQL) | `Projection::Cartesian` / `Polar(PolarProjection)` / `Custom(CustomProjection)` |
| Facets: Wrap / Grid | `composition::{grid, beside, stack}` + `PlotComposition` orchestrator |

Decisive point: **ggsql resolves scale config but does not apply scales** — the
DataFrame still holds raw values, and value→pixel mapping was always the
renderer's job (Vega-Lite did it; hephaestus does it too). We feed hephaestus
the raw columns plus `Scale` objects built from ggsql's resolved config. No
data is recomputed.

Confirmed during investigation:
- hephaestus already implements the **full transform set** ggsql needs
  (`src/scales/transform.rs`: `Log10/Log2/Log/Sqrt/Square/Exp*/Asinh/PseudoLog*`).
  The "Identity-only in v1" note in its scales doc is stale.
- ggsql resolves palettes → colors **in the writer**, not earlier
  (`src/writer/vegalite/encoding.rs:501` via `lookup_palette` in
  `src/plot/scale/palettes.rs`). The hephaestus writer reuses the same helper.
- hephaestus `Scale::with_breaks_labeled(Vec<(Value, String)>)` and
  `with_format(closure)` accept ggsql's resolved breaks + formatted labels
  directly (`src/plot/scale/mod.rs`).
- hephaestus has every geom ggsql needs (Point/Line/Rect/Ribbon/Polygon/
  Segment/Wedge/Ellipse/Geometry/Text + composites).

## 2. Constraints & risks (settle before / during Phase 1)

In priority order:

1. **MSRV conflict — the big one.** ggsql MSRV is **Rust 1.86** (CRAN-locked,
   enforced in CI per root `CLAUDE.md`). hephaestus declares
   `rust-version = "1.88"` and pulls `wgpu`/`vello`. This is the same situation
   the repo already handles for the `adbc` test path and `ggsql-wasm`: the
   feature must be **gated and excluded from the MSRV-enforced build** and out
   of the path the R bindings compile.

2. **GPU + heavy deps.** The only working hephaestus backend today is `vello`
   (wgpu/GPU); `svg`/`pdf`/`blend2d` are declared placeholders with no code.
   Implications:
   - Output is **raster** (RGBA8 → PNG) for the foreseeable future. No SVG/PDF.
   - Needs a GPU adapter at render time; headless CI/containers need a software
     adapter (e.g. `llvmpipe`). Document this operational footgun.
   - Not viable for the R/CRAN target; not the default for `ggsql-wasm` (wasm
     can run wgpu but it is a separate, heavy build).

3. **hephaestus maturity.** `version = "0.0.1"`, `publish = false`. Consume as a
   **git rev or path dependency**; API is pre-1.0 and will churn. Pin a commit.

4. **`text` feature required.** All chrome (axes, legends, titles) and text
   geoms are gated on hephaestus's `text` feature (parley shaper). Turn it on.
   The shaper is "scaffolding, meant to be replaced by the host" — acceptable
   for v1; flag as future work.

**Recommendation:** ship behind a non-default `hephaestus` cargo feature in
`src/Cargo.toml`; mirror the `adbc` CI exemption (build/test with `cargo
+stable`, keep out of the 1.86 job); depend on hephaestus with
`features = ["vello", "png", "text"]`.

**Answers from author**

follow recommendations

## 3. Architecture

The Vega-Lite writer flattens everything into one declarative JSON document and
lets the Vega-Lite runtime do layout + scale application. hephaestus **is** the
runtime, so this writer's job is to build a `PlotComposition` and render it, not
emit a spec.

```
HephaestusWriter::render(&Spec)
  ├─ build_scale_registry(plot.scales)        → one hephaestus Scale per aesthetic ("pos1","pos2","color"…)
  ├─ build_composition(plot.facet, data)      → Composition (Patches) — "our own faceting"
  ├─ for each panel (facet cell):
  │     Plot::new(patch_id)
  │       .bind("x","pos1").bind("y","pos2")…  ← channel→scale-name bindings (projection-aware)
  │     for each ggsql Layer:
  │         GeomRenderer::build(layer, panel_df) → Vec<Box<dyn Geom>> + channel bindings
  │     set projection (Cartesian/Polar/Custom)
  │     add axes / legend / titles from Labels + scales
  ├─ PlotComposition::render(scene, size, dpi)
  └─ VelloRenderer → RGBA8 → PNG bytes
```

### Core trait — analog of the VL `GeomRenderer`

```rust
/// Translates one ggsql Layer (for one facet panel's data slice) into
/// hephaestus geoms plus the channel→scale-name bindings they need.
trait GeomRenderer {
    /// Channel bindings this geom contributes, e.g. [("x","pos1"),("y","pos2"),("fill","color")].
    fn bindings(&self, layer: &Layer, ctx: &RenderCtx) -> Vec<(String, String)>;
    /// Build the concrete geom(s) from the panel DataFrame.
    fn build(&self, layer: &Layer, df: &DataFrame, ctx: &RenderCtx) -> Result<Vec<Box<dyn Geom>>>;
}
```

`RenderCtx` carries the `AestheticContext` (user↔internal name map), the
projection kind, and the theme. One impl per `GeomType`, dispatched from a
factory — mirrors the VL `GeomRenderer` registry.

### Channel-name translation

Scales live in the hephaestus `ScaleRegistry` keyed by the **ggsql aesthetic**
(`pos1`, `color`, …); each `Plot` binds its channel (`x`) to that scale name
(`pos1`). Two panels binding `x→pos1` share one scale — hephaestus's fixed-scale
faceting model.

| ggsql internal | hephaestus channel (Cartesian) | (Polar) |
| --- | --- | --- |
| `pos1` | `x` | `theta` |
| `pos2` | `y` | `radius` |
| `pos1min/max`, `pos2min/max` | `x`/`x2`, `y`/`y2` (ribbon/range) | … |
| `pos1end`, `pos2end` | `x1`/`y1` (segment) | … |
| `color`/`fill`/`stroke`/`size`/`shape`/`linetype` | same | same |

## 4. Component mapping

**Geoms** (`GeomType` → hephaestus geom):

| ggsql | hephaestus | notes |
| --- | --- | --- |
| point | `PointGeom` | direct |
| line, path | `LineGeom` | path ordered by `__ggsql_order__` |
| bar, histogram, tile | `RectGeom` | x/x2/y/y2 from pos extents |
| area, ribbon | `RibbonGeom` | orientation from which of x2/y2 present |
| polygon | `PolygonGeom` | |
| segment, rule | `SegmentGeom` | |
| arrow | `SegmentGeom` + endpoint marker (`ShapeRegistry`) | |
| range | `SegmentGeom` or `RibbonGeom` | |
| boxplot | composite: `RectGeom`+`SegmentGeom`(+`PointGeom` outliers) | multiple geoms, like VL `PreparedData::Composite` |
| violin | `PolygonGeom`/`RibbonGeom` | |
| density, smooth | `LineGeom` (+ `RibbonGeom` for CI) | |
| text | `TextGeom` | needs `text` feature |
| spatial | `GeometryGeom` | WKB/WKT via `geom-*` features |
| (polar bar / pie) | `WedgeGeom` | when projection is Polar |

**Scales:** map `ScaleTypeKind` 1:1; `scale.numeric_domain()`→`domain_continuous`,
discrete categories→`domain_discrete`; `scale.break_labels()`→`with_breaks_labeled`
(bake ggsql's formatted strings in directly — simplest way to preserve ggsql's
label semantics exactly); `OutputRange::Palette` → reuse `lookup_palette()` →
`range_colors`; transforms map by name to `TransformKind`.

**Coords:** Cartesian→`Projection::Cartesian`; Polar→`Projection::Polar`
(`full_circle`/`gauge`/`radar` from properties); Map→`Projection::Custom` fed
the pre-projected `panel_boundary`/`bbox` from `Projection.computed` (same data
the VL `MapProjection` reads — coordinates already projected in SQL).

**Faceting (the "roll our own" piece):** read `FacetLayout`. Group panel rows by
`__ggsql_aes_facet1__`/`facet2__`. Build the `Composition`: `Wrap` →
`grid(nrow, ncol, patches)` (ncol from properties); `Grid` → nested `grid`
indexed by (row-var, col-var). Strip labels via `Slot::StripTop`/`StripLeft`.
`scales: fixed|free_x|free_y|free` decides whether panels share one scale per
aesthetic (fixed) or get per-panel scales (free) in the registry.

**Labels/titles/theme:** `plot.labels` → `Plot::set_title/subtitle`, axis titles
via `Axis::title`, legend titles. ggsql has **no theme concept**, so v1 picks a
hephaestus default theme; expose theme selection later.

## 5. New module layout

```
src/writer/hephaestus/
├── mod.rs           HephaestusWriter (config: size, dpi, bg, theme), Writer impl, render orchestration
├── CLAUDE.md        architecture doc (mirror the vegalite one)
├── scales.rs        ggsql Scale → hephaestus Scale; palette/transform/break mapping
├── composition.rs   FacetLayout → Composition; panel data splitting (our faceting)
├── channels.rs      aesthetic↔channel translation (projection-aware)
├── layer.rs         GeomRenderer trait + factory + RenderCtx
├── geom/            one renderer per GeomType (point.rs, line.rs, rect.rs, ribbon.rs, boxplot.rs, …)
└── projection.rs    Coord → hephaestus Projection (cartesian/polar/custom-map)
```

`HephaestusWriter` is a *configured* writer (width/height/dpi/background/theme) —
hephaestus rendering needs a target size, unlike the resolution-independent VL
JSON. Output type: see Decision 2.

Wire-up: feature `hephaestus` in `src/Cargo.toml`; `pub mod hephaestus` +
`pub use HephaestusWriter` under that gate in `src/writer/mod.rs`; CLI
`--format`/output-extension routing in `ggsql-cli`; CI exemption matching
`adbc`.

## 6. Phased implementation

1. **Spike / skeleton.** Gated dependency + feature. `HephaestusWriter` that
   renders a single-panel, single-`point`-layer, Cartesian, fixed-scale plot to
   PNG. Proves the dep/MSRV/GPU path end-to-end. (`scales.rs` continuous-only,
   `channels.rs`, `geom/point.rs`.)
2. **Scales & axes.** All scale types + transforms + palettes + breaks/labels;
   Cartesian axes, gridlines, axis titles; discrete/binned/ordinal; legends for
   color/size/shape.
3. **Geom coverage.** line/path, rect family (bar/histogram/tile), ribbon/area,
   polygon, segment/rule/arrow/range, text; composite boxplot/violin;
   density/smooth.
4. **Faceting.** Wrap + Grid via `composition`; fixed vs free scales; strip
   labels; composition-level title.
5. **Projections.** Polar (pie/rose/radar), then Map via `Custom` projection +
   pre-projected geometry + clip boundary.
6. **Polish.** Theme defaults, title/subtitle/caption, snapshot (PNG) tests,
   CLAUDE.md, CHANGELOG entry. Decide default-writer switchover criteria.

Each phase ends with `cargo fmt` + `cargo clippy` (per CLAUDE.md) and visual
inspection against the equivalent Vega-Lite output.

## 7. Decisions (resolved)

1. **Dependency mode** — pinned **git rev** of `posit-dev/hephaestus` (now a
   public repo, so CI fetches it with no credentials).
2. **Output type** — `Output = Vec<u8>` (PNG bytes); ggsql encodes hephaestus's
   RGBA buffer via the `png` crate.
3. **First milestone** — the Phase 1 spike (this is implemented; see below).
4. **Default-writer ambition** — land the gated alternative now, promote later.

## Phase 1 — status: implemented

Single-panel, single `point` layer, Cartesian, continuous scales with
bottom/left axes → PNG bytes, behind the non-default `hephaestus` feature.

- Module: `src/writer/hephaestus/{mod.rs, scales.rs, channels.rs, geom/point.rs}`.
- Dep + feature in `src/Cargo.toml`; gate in `src/writer/mod.rs`; stable-only
  CI step (+ lavapipe install) in `.github/workflows/build.yaml`.
- Verified: feature build, default build (hephaestus absent), `cargo +1.86`
  build (MSRV intact), fmt, clippy, tests (render + reject), output eyeballed.

Hephaestus deficiencies noted during Phase 1 (for upstream):
- `png::write_png` is file-only — no in-memory PNG/RGBA-bytes API, so each host
  re-implements byte encoding.
- `render_to_buffer` returns premultiplied RGBA (fine for opaque backgrounds;
  transparent ones need un-premultiplying before PNG encode).
- `src/scales/` docs claim transforms are Identity-only, but `TransformKind`
  already implements the full Log/Sqrt/PseudoLog set — stale doc.

## Phase 2 — status: implemented

All scale types, transforms, material aesthetics, axis titles, and legends for
the single-panel point geom.

- `scales.rs::build_scale` builds continuous/discrete/ordinal/binned scales;
  maps ggsql transforms → hephaestus `TransformKind` (cast/temporal → identity);
  maps resolved `OutputRange::Array` → `range_colors/range_numbers/range_strings`
  (palettes already concrete; hex/name colors parsed via `csscolorparser`).
- `channels.rs` extracts columns as typed channel data (text → category strings,
  numeric → f64) and parses literal colors.
- `mod.rs` discovers mapped aesthetics, registers a scale + binds a channel per
  data-mapped aesthetic, sets constants for `Literal` aesthetics and `Raw`
  per-row constants for identity/annotation columns, falls back to ggsql defaults
  otherwise, and adds axis titles + legends.
- Channels driven by the same `(data source, output kind)` bind to **one**
  shared scale (ggsql's `color` → fill + stroke); their legends then share a
  `domain_scale` and **hephaestus auto-collapses them** — no bespoke legend
  dedup. Generalized via `shared_scales` keyed on `(aesthetic_source, kind)`.
- Continuous domains come straight from ggsql's resolved `numeric_domain` (see
  the Multi-layer section — the writer never computes its own extents).
- hephaestus builtin shape names match ggsql's 1:1 (pass-through).
- Verified: discrete-color (single collapsed legend), continuous-size,
  log-scale (correct log spacing), axis-title tests; output eyeballed; default
  + 1.86 builds, fmt, clippy clean.

Hephaestus deficiency noted in Phase 2 (for upstream):
- No domain expansion / "nice" padding. Edge data points sit exactly on the
  panel boundary and clip; consumers must pre-expand domains by hand, which is
  awkward for non-linear transforms (data-space padding can push a log lower
  bound ≤ 0). A scale-level expansion factor would help.

Deferred to later phases: domain expansion (above); calendar-native temporal
axes (continuous + ggsql formatted breaks used for now); `linetype` material
aesthetic (line geoms, Phase 3); ggsql's degenerate inferred log domain is
worked around here but is worth fixing upstream in ggsql too.

## Phase 3 — status: implemented

All non-composite geoms render through a per-geom dispatch:
point/line/path/smooth, bar/histogram/tile, area/ribbon/density, polygon,
segment/range/rule, text. Single panel, Cartesian.

- **Architecture.** `mod.rs` dispatches on `GeomType` to per-geom modules under
  `geom/` that each declare a `GeomSpec` (position channels + material table +
  raw string/number channels + grouping). `wiring.rs` holds the shared,
  builder-generic helpers (`build_and_add`, `wire_positions`, `wire_material`,
  scale/axis/legend/group-key logic) lifted out of the Phase-2 `mod.rs`.
- **Grouping.** multi-vertex geoms (line/path/area/ribbon/polygon) derive
  hephaestus `keys` from `layer.partition_by` via `channels::build_group_keys`
  (concatenated partition-column values) — so e.g. a line colored by category
  renders as one line per category.
- **Orientation.** bar/histogram/area/ribbon/range consult
  `is_transposed(layer)` and swap the value axis to the pos1-family when
  transposed.
- **Positions** bind to one `pos1`/`pos2` scale per axis (domain = union extent
  of the family columns used); `RangeKind::Linetype` restored for line dashes.
- Verified: 13 render tests (point/grouped-line/bar/histogram/area/ribbon/
  segment/text/polygon/color/size/log + composite-reject); output eyeballed for
  line/bar/area/segment/text; default + 1.86 builds, fmt, clippy clean.

Known limitations (refinements, not blockers):
- No domain expansion → data points/labels on the domain edge clip at the panel
  boundary (same hephaestus gap noted in Phase 2).
- Legend keys are always point glyphs; line/area legends could use line/rect
  keys. (hephaestus supports `LegendKeySpec::line()`/`rect()`.)
- Discrete-tile uses band edges best-effort; continuous/binned tile is exact.

Deferred to Phase 3b: boxplot, violin (composite decomposition).

## Phase 3b — status: implemented

Composite geoms render by decomposing one ggsql layer into several hephaestus
geoms sharing the `pos1`/`pos2` scales.

- **boxplot** (`geom/boxplot.rs`): the stat's `type`-tagged rows are split by
  index into box (`RectGeom`, q1→q3 filling the band), whiskers (`SegmentGeom`,
  box edge→fence), median (`SegmentGeom` spanning the band via `Raw` x_band
  ±0.5), and outliers (`PointGeom`). `fill` is data-mapped (e.g. fill-by-group →
  colored boxes + legend) or a constant white default; stroke is constant.
- **violin** (`geom/violin.rs`): rows grouped by `pos1`, each sorted by `pos2`;
  one vertical `RibbonGeom` band per category — right edge `x_band = +offset`,
  left edge `x2_band = -offset` (the stat's pre-scaled half-width, a band
  fraction), sharing `y = pos2`. One ribbon row per grid sample (no hand-built
  mirrored outline).
- Composites dispatch through a dedicated path in `geom/mod.rs::build_into_plot`
  (not `build_and_add`); shared helpers `wiring::{register_axis, resolve_fill,
  material_legend}` are now `pub`, and `channels::ChannelData::select` subsets
  extracted columns by row index (no DataFrame filtering).
- **hephaestus dep bumped** to rev `79b240de`: the `RibbonGeom` band-channel gap
  found while planning violin (no `x_band`/`x2_band`) was fixed upstream, so
  violin moved from a doubled `PolygonGeom` outline to a `RibbonGeom` band.
- Verified: `renders_boxplot` (with outlier — domain spans it, ggsql breaks),
  `renders_boxplot_fill_by_group` (colored + legend), `renders_violin` (2
  categories); all eyeballed. 16 writer tests pass; default + 1.86 builds, fmt,
  clippy clean.

Known limitations: outliers/edge points clip at the panel boundary (standing
no-expansion gap).

## Gap-closing — status: implemented

Closing the audit gaps in implemented geoms, most-visible first.

- **width + dodge** (bar/histogram, boxplot, violin): the `width` parameter and
  `position = dodge` are honored. `wiring::band_half_width` reads
  `layer.parameters["width"]` (dodge-narrowed via `Layer::adjusted_width`), and
  `wiring::dodge_offsets` reads the `pos1offset`/`pos2offset` columns; bars/boxes
  set per-row `x_band`/`x2_band = offset ± half-width` (via the new
  `GeomSpec::data_channels`), violins add the dodge offset to their band edges.
  Bars now have proper gaps and dodge side-by-side; boxes honor `width`.
- **text aesthetics**: `text` is now a custom builder wiring `fontsize`→size,
  `fontweight`→weight (CSS keyword/numeric parse), `italic`→italic,
  `typeface`→family, `hjust`→anchor_x, `vjust`→anchor_y (flipped to
  hephaestus's top origin), `rotation`→angle (degrees→radians), plus
  data-mapped/constant `fill` and `opacity`. (`column_to_bool` added.)
- **domain expansion (pass ggsql's range through)**: ggsql already expands the
  resolved input range, so the writer just uses `scale.numeric_domain()` for
  untransformed continuous scales (`continuous_domain`, `use_resolved` path)
  instead of the raw data extent — edge marks/labels no longer clip, with no
  writer-side expansion. Under a non-identity transform ggsql's data-space
  expansion can collapse a log lower bound toward `f64::MIN_POSITIVE`, so those
  scales use the data extent + hephaestus's transform-aware breaks (so log
  axes have no expansion yet — minor; tied to ggsql's log-domain expansion).
- **legend key fidelity**: each geom declares a `LegendKind` (point/line/rect)
  threaded through `GeomSpec`/`wire_material`/`resolve_fill`/`material_legend`,
  so line legends show a line swatch, bar/area/box/violin a filled rect, points
  a point — instead of always a point glyph.
- **boxplot/violin styling**: honor a constant `stroke` (box/whisker/median
  outline, both ribbon edges) and `opacity` (box fill / ribbon alpha) via
  `wiring::{constant_color, constant_number}`, instead of hardcoded grey.
- **tile linetype**: `linetype` wired on the rect material (dashed tile borders).
- **diagonal rule (abline)**: a rule with a non-zero `slope` (ggsql sets
  `parameters["diagonal"] = true`) renders as a single `SegmentGeom` spanning the
  position scales' resolved range — `segment::build_diagonal` grabs each pos
  domain from `spec.find_scale("pos1"/"pos2").numeric_domain()`, computes
  `secondary = slope·primary + intercept` over it (intercept from the `pos2`/`pos1`
  literal, slope from the `slope` literal/SETTING), registers both axes from the
  endpoints, and binds x/x2→pos1, y/y2→pos2. The user supplies the ranges via
  `SCALE x/y FROM (..)`; when a scale is unresolved it falls back to 0..1. No
  DRAW/PLACE or multi-layer distinction — the writer just reads the scale ranges.
  Required teaching `wiring::{constant_color, constant_number}` to read bare
  `Literal` aesthetic values (not only annotation columns), since `slope`,
  `stroke`, etc. arrive as `AestheticValue::Literal`.
- **constant (`Literal`) material aesthetics across all geoms**: ggsql delivers
  every geom default *and* every `SETTING` constant (`color => 'red'`,
  `linetype => 'dashed'`, `size => 8`, …) as `AestheticValue::Literal` in the
  layer mappings — **not** as a materialized column. `wire_material` previously
  keyed only off `aesthetic_column_name` (columns), so it silently dropped all
  literals and substituted its own `MatDefault`; e.g. `SETTING color => 'red'`
  rendered black. It now dispatches the three variants exactly like the VL
  writer's `build_encoding_channel`: `Literal` → a constant channel value via
  `set_literal_channel` (color via `parse_color`, shape name string, linetype via
  `map_linetype`→`Value::Linetype`, numbers pass through since hephaestus takes
  points directly), `Column` (non-identity scale) → scaled + legend,
  `Column`(identity)/`AnnotationColumn` → per-row `Raw`. `MatDefault` is now a
  true last-resort fallback. ggsql's own defaults match the old hardcoded ones
  (black fill, opacity 0.8, size 3), so plain geoms are unchanged.
- **composite per-group color + outlier/tile fidelity** (mirrors the VL writer's
  shared-encoding model): `wiring::resolve_color(aesthetic, channel)` generalizes
  the old `resolve_fill` — a data-mapped color registers a scale, binds the
  channel, and adds one legend (returning the column for components to select);
  otherwise the mapped literal (via `constant_color`) or default. Boxplot and
  violin resolve **fill and stroke** this way and apply the same resolved color to
  every component (box/whisker/median/outlier; both ribbon edges), so a
  `stroke AS group` colors the whole mark per group under one collapsed legend.
  Boxplot **outliers** are hollow points (stroke only — matching VL's
  `filled = false`) honoring the `size`/`shape` aesthetics (via `constant_number`
  / new `constant_string`) instead of a hardcoded dot. Discrete **tile**
  `width`/`height` are read from ggsql's per-row band-fraction columns (1.0 = full
  band, like VL's `datum.width * bandwidth`) → per-axis band edges at ±fraction/2.
  (`opacity`-on-stroke was investigated and is **not** a gap: VL retargets
  `opacity → fillOpacity` only for fill-bearing geoms and leaves stroke-only geoms
  on `opacity`, which the writer already mirrors.)
- Verified: `renders_dodged_bar`, `renders_text_styled`, `renders_boxplot_styled`
  (navy outline), `renders_boxplot_stroke_by_group` (per-group blue/orange incl.
  hollow outlier, collapsed legend — eyeballed), `renders_tile_sized` (half-band
  tiles eyeballed), `renders_diagonal_rule`, `renders_constant_aesthetics`,
  grouped-line/dodged-bar legends, expanded point plot eyeballed; 23 writer tests;
  default + 1.86 builds, fmt, clippy clean.

Remaining geom gap: calendar-native temporal axes (numeric axes with ggsql's
formatted break labels work today; date-native ticks are a larger follow-up, not
niche).

## Multi-layer — status: implemented

The writer renders **N layers** into one shared panel (`validate` allows ≥1
layer; non-Cartesian projections supported — see below; faceting landed in the
Phase 4 section). `write` loops over `spec.layers`, building each layer's geom into one
`HPlot`; geoms draw in DRAW order (= z-order).

The enabling change is a principle correction: **the writer never computes its
own scale extents — it uses the domain ggsql reports**, exactly as the VL writer
uses `input_range`. ggsql resolves every `Scale` globally over all layers × the
whole position family, so `numeric_domain()` already spans every layer and
includes pos2end/min/max, fences, tile extents, etc.

The wiring was then refactored to make ggsql the single source of scale truth and
the geoms per-geom-stateless (the old `Wiring` accumulator is **gone**):
- **Scales** are registered once, up front, from `spec.scales` into the
  `PlotComposition` (`write` → `build_scale(scale, kind)`; `build_scale` returns
  `Option`, registering nothing when a scale has no resolved type). No per-geom
  scale registration or extent computation.
- **Geoms write directly to `plot`**: `wire_positions`/`wire_material`/
  `resolve_color` and the composites call `plot.set_binding` (idempotent) and
  `plot.add_legend` (hephaestus collapses compatible legends).
- **Axes** are created per coordinate system in `projection::apply_projection`
  (Cartesian → bottom/left rails; polar → angular + radial rings), so the axis
  kind can depend on the coord.

Supporting ggsql fix: a scale that no layer trains (e.g. a diagonal rule keeps
its position out of training) but that has an explicit `FROM` range is now typed
by `infer_scale_type_from_input_range` (numeric/temporal → continuous, string/
bool → discrete) in `execute/scale.rs`. Previously it stayed untyped, so the
writer registered no scale and the abline couldn't bind; this benefits the VL
writer too.

Projections (Phase 5): `projection.rs` dispatches on `CoordKind` — Cartesian
(clip/aspect-ratio + rails), Polar (`HProj::Polar` with start/end/inner + angular/
radial rings), and Map (implemented in the Phase 5b section below). Polar renders
**truthfully**: pos1→radius, pos2→theta
(matching the VL writer), so a stacked bar becomes a correct pie/donut with the
right slice proportions, fills, and angular axis. `start`/`end` are degrees
clockwise from 12 o'clock (`end` defaults to `start + 360°`, so setting only
`start` rotates a full circle); `inner` opens a donut hole. A synthetic dummy
position scale (`__ggsql_stat_dummy` — a pie's radius, or a bar with no x) is
given no axis, in every projection, via the shared `has_real_axis` predicate,
mirroring the VL writer's `AxisInfo::suppress`.

Two hephaestus fixes were required (landed upstream on `main` as `5997cdd`; ggsql
pins that rev in `src/Cargo.toml`):
1. `angle_channel`/`radius_channel` are now honored by **geom geometry**, not
   just chrome: `project_to_panel_px` / `interpolate_segment_with_t` route a
   geom's positional `[x, y]` to theta/radius via `PolarProjection::theta_r_from_xy`
   (default `x→theta`; ggsql sets `angle_channel="y"` so pos2 drives the angle).
2. RectGeom's zero-size cull now applies only on the linear path — a 180° polar
   wedge whose two diagonal corners share a pixel coordinate is no longer dropped.

Verified (eyeballed): point+line (shared axes), bar+point overlay (z-order),
scatter+abline, two layers colored by one variable, polar pie (180° slice), donut,
rotated + reflex + 4-slice pies. 29 writer tests; fmt, clippy clean.

Minor cosmetic (deferred): a plain pie shows a tiny centre hole because the dummy
bar occupies a `width` band (radius ~0.05–0.95) rather than the full 0–1 radius;
proportions/angles are unaffected.

Resolved since: `color AS <var>` maps **both** fill and stroke to the variable →
two separate scales (`fill`, `stroke`) whose legends used to render twice. This is
now collapsed upstream by hephaestus's `collapse_legends` (see the Faceting
section) — no writer-side shared-source map needed.

**Confirmed shared with the VL writer (not a regression here):** ggsql's range
expansion runs in linear data space then clips to the transform's valid domain,
so a log scale's domain collapses to `[f64::MIN_POSITIVE, max]` and its breaks
explode. The VL writer emits the same squashed domain + breaks (verified via the
CLI). Fixing this belongs in ggsql core (expand in transform space); it will
improve both writers at once.

Known issue (orthogonal, shared with the VL writer): a `bar` mapped to a
**numeric** primary axis is resolved by ggsql as a *continuous* `pos1` scale (no
`pos1end`), so the writer's band-fraction bars get no width — and the VL writer
hits the same wall (`bandwidth('x')` is 0 on a continuous scale). A `bar` is
meant to have a discrete primary axis; this looks like ggsql not coercing a
numeric bar axis to discrete. (`histogram` is unaffected — it carries real
`pos1`/`pos1end` bin edges and renders correctly.)

## Faceting (Phase 4) — status: implemented

`FACET` now renders as small multiples. The writer builds a hephaestus
`Composition` of named panels and attaches one `Plot` per panel to the shared
`PlotComposition`, so all panels resolve through one scale registry (fixed
scales). Works under Cartesian, Polar, and Map projections.

- **New module `facet.rs`.** ggsql resolves faceting fully (layout, `free` bools,
  Wrap `ncol`, per-row `__ggsql_aes_facet1__`/`facet2__`), so the writer only
  lays it out. `build_panels` returns a `(Composition, Vec<Panel>)` — a 1×1 grid +
  one `Panel` when unfaceted, else `composition::grid(nrow, ncol, cells)` (Wrap
  flows row-major into the resolved `ncol`, padding the partial last row with
  `spacer()`; Grid is facet1 rows × facet2 columns). Panel **ordering** mirrors
  the VL writer's `resolve_facet_ordering` (facet scale `input_range`, then
  `reverse`; numeric-aware ascending otherwise). Per-panel **data slicing** reuses
  `DataFrame::take` on the row indices matching the panel's facet value(s); a
  layer with no facet column is used whole. Grid cells with no data (e.g.
  `missing => 'null'`) are skipped, leaving an empty framed panel.
- **The write loop is now panel-based** (`mod.rs`): unfaceted and faceted share
  one path (`Vec<Panel>`, length 1 when there's no FACET). Fixed scales are
  registered once globally; each panel builds one `HPlot`, applies the
  projection, and sets strip labels.
- **Strip labels** via hephaestus `Plot::strip`: Wrap/Grid-column headers on
  `AxisSide::Top`, Grid-row headers on `AxisSide::Right`.
- **Edge-only axes** (ggplot2 look): for a fixed dimension the x-axis is drawn
  only on the bottom-most present panel of each column and the y-axis only on the
  left column (`Panel::{first_col,last_row}`, honored in
  `projection::apply_proj_cartesian`).
- **Free scales** (`free => 'x' | 'y' | ['x','y']`): a **deliberate, scoped
  exception** to "ggsql owns all scale domains" — fixed dimensions still use
  `numeric_domain()`, but a free dimension gets a per-panel scale
  (`pos1__p{idx}`) whose domain the writer computes from that panel's slices
  (`scales::free_position_scale`: family numeric extent for continuous/binned,
  per-panel distinct categories for discrete). `PanelScales` carries the
  per-panel position scale names; `Ctx` threads them so every geom (generic +
  composite) binds positions to the panel scale. A free dimension forces its axis
  onto every panel. The clean long-term home is ggsql resolving per-panel domains,
  which would let the writer drop this computation.
- Verified (eyeballed): 3×2 wrap (partial last row, correct per-column x-axis),
  2×2 grid (top + right strips), free-x/y (each panel its own domain/axes vs the
  squashed fixed comparison), polar facet (one pie per panel, per-panel
  proportions). Tests: `renders_wrap_facet`, `renders_grid_facet`,
  `renders_faceted_bar_with_color`, `renders_free_scale_facet`,
  `renders_polar_facet` (34 writer tests pass); hephaestus-absent build compiles;
  fmt + clippy clean.
- **Single shared legend across panels.** hephaestus `bac7632` added a
  composition-level legend ring (`PlotComposition::add_legend`), so legends live
  on the composition, never on the per-panel plots. Wiring no longer calls
  `Plot::add_legend`: `Ctx` carries a `legends` sink (`Option<&RefCell<
  Vec<Legend>>>`) and `wire_material` / `resolve_color` push through
  `Ctx::push_legend`. `mod.rs` passes the sink only while building the **first**
  panel (every panel produces the same legends — all built from the globally
  resolved scales — and each legend reflects the global scale domain regardless of
  a panel's data slice), then registers the captured set once on the composition
  (which also carries a `shape_registry` for the legend glyphs). This uniformly
  covers the single-panel case (a 1×1 composition legend ring) — no faceted/
  non-faceted branch, no register-then-unregister. Verified by eyeballing a
  3-panel wrap colored by a categorical (one legend beside the whole strip) and
  the single-panel equivalent (unchanged).
- **Equivalent fill+stroke scales collapse to one legend.** A `point` colored by
  a categorical maps `color` onto **two** ggsql scales — `fill` and `stroke` — so
  the writer records two legends. hephaestus `f133825`'s `collapse_legends` (run
  at render) merges legends whose scales are `legend_equivalent_to` (same
  scale_type / transform / input_range / breaks) even when their names differ, and
  overlays the merged keys into one swatch. The two `cat` legends therefore render
  as a single legend with a filled+outlined swatch per category — no writer code
  needed beyond registering both legends (which it already does) with the default
  `merge` flag on.

Known limitations (refinements, for upstream / later): see §9.

## Spatial / Map (Phase 5b) — status: implemented

The `spatial` geom and the **Map** coordinate system render, completing the last
coordinate system (Cartesian + Polar were already done). Full parity with the
Vega-Lite writer: geometry marks, the projected panel boundary, and graticules —
for both a `PROJECT map` and a bare `spatial` geom under Cartesian.

The seam is clean because ggsql's executor does **all** SQL-side projection
renderer-agnostically (`plot/projection/coord/map.rs`, `plot/layer/geom/
spatial.rs`): by the time the writer runs, geometry is WKB in
`__ggsql_aes_geometry__`, and `Projection.computed` holds `panel_boundary` (WKT),
`bbox` (`[xmin,ymin,xmax,ymax]`), and `graticule_lon`/`graticule_lat` (WKT). The
writer only decodes and frames — no data is recomputed. Mirrors the VL
`MapProjection` (identity projection) and `SpatialRenderer`.

- **Dep** (`src/Cargo.toml`): hephaestus `features` gains `geom-wkb` (geometry
  column) + `geom-wkt` (boundary/graticule strings), and the rev is bumped to
  `d343138` — whose `CustomProjection` outline is a `Vec<GeoPolygon>`, so a
  multi-part clip boundary passes through whole (`GeometryGeom` already existed).
- **`channels.rs`**: `column_to_geometry` decodes the WKB `Binary`/`LargeBinary`
  column (and hex-WKB strings, for ODBC/PostGIS parity) via `Geometry::from_wkb`;
  nulls → `Geometry::Empty`. `wkt_to_lines` (graticule polylines) and
  `wkt_to_outline` (boundary → `Vec<GeoPolygon>`: a Polygon → one, a MultiPolygon
  → all parts, each with holes) feed the map projection.
- **`geom/spatial.rs`**: a custom builder (like `text`/`boxplot`) — `GeometryGeom`
  has no x/y channel, so it sets the `geometry` channel and binds x→`pos1`,
  y→`pos2` (the geom's draw resolves each coordinate through those scales).
  fill/stroke go through `resolve_color` (data-mapped → shared scale + one
  collapsed legend, else the mapped literal / ggsql spatial defaults `#747474`
  fill, black stroke, opacity 0.8, linewidth 0.2, solid linetype).
- **`mod.rs`**: a spatial layer has no `pos1`/`pos2` in `spec.scales`, so
  `write` registers continuous `pos1`/`pos2` scales from ggsql's `computed["bbox"]`
  (else the union geometry extent for a bare spatial geom) — the domain still
  comes from ggsql. The panel's **aspect ratio is locked to the bbox**
  (`aspect_mode(Range)`) so the projection isn't distorted (the raster analog of
  VL's uniform projection scale); hephaestus's Cartesian y-flip already puts north
  up, so no `reflectY` equivalent.
  `opacity`/`linewidth`/`linetype` route through the shared `wire_material`
  (made `pub`), so each is honored as the ggsql literal default, a `SETTING`
  constant, **or data-mapped** (scale-bound + legend) — full parity with the VL
  writer's generic encoding path, not fill/stroke-only.
- **`projection.rs`**: `apply_proj_map` clears the Cartesian rails and builds a
  `Projection::Custom` from `computed` — `panel_boundary` → the clip/background
  outline (a full **MultiPolygon**: every part with its holes; needs hephaestus
  ≥ `d343138`, whose `CustomProjection` outline is `Vec<GeoPolygon>`),
  `graticule_lon`/`graticule_lat` → the x/y grid lines. Custom's coordinate math
  equals Cartesian (no reprojection), exactly matching the pre-projected data. A
  map with no boundary (no CRS/clip) falls back to the bbox-framed Cartesian
  identity.
- Verified (eyeballed): orthographic world globe (round — equal aspect held —
  with boundary + graticules), Mercator choropleth by continent (single collapsed
  categorical legend, projected proportions), two-polygon bare `spatial` geom with
  a continuous fill colorbar, and data-mapped `opacity` (per-feature transparency
  + legend). Tests `renders_spatial`, `renders_spatial_mapped_opacity`,
  `renders_map` (feature `spatial`); 37 writer tests; feature build, default
  (hephaestus absent) build, `cargo +1.86` build, fmt, clippy all clean.

Known limitations (deferred, matching VL/PLAN precedent):
- No writer-side reprojection (by design — ggsql projects in SQL).

## Chrome + composite polish — status: implemented

Closing the named feature gaps left by the phases above: plot titles, text
outlines, composite outline styling, and facet strip labelling.

- **Plot title / subtitle / caption** (`mod.rs`, `wiring::plot_label`): read from
  the `LABEL` clause and set on the **`PlotComposition`**, not the per-panel plots
  — one label spans the whole figure, and the unfaceted 1×1 case needs no branch
  (a plot-level title would resolve to the same layout row and be painted over).
  `Some(None)` (`LABEL title => NULL`) suppresses; literal `\n` becomes a real
  newline, mirroring the VL writer's `split_label_on_newlines`. `caption` is new
  capability — the VL writer never implemented it (`doc/syntax/clause/label.qmd`
  updated accordingly).
- **Text outlines** (`geom/text.rs`): ggsql's `stroke` → hephaestus
  `"text_stroke"`, via the new `wiring::resolve_optional_color` so an unmapped
  `stroke` (the ggsql default for text is `Null`) leaves the channel unset and
  draws no outline. Outline *width* is hephaestus's theme default (1pt): ggsql's
  text geom has no `linewidth` aesthetic and neither does VL's text mark, so this
  is exact parity.
- **`MaterialSource`** (`wiring.rs`) generalizes the old `ColorSource` over
  `RangeKind::{Color, Number, Linetype}`, dispatching the same three ways as
  `wire_material` (Literal / data-mapped column + scale + legend / identity-or-
  annotation raw) but returning a value that can be applied to a **row subset** —
  which whole-column `wire_material` can't do, and composites need.
  `resolve_color` is now a thin wrapper over it.
- **boxplot / violin outline styling**: `linewidth` and `linetype` are resolved
  once per layer and applied to every component (box rect, whiskers, median;
  both ribbon edges via the `2`-suffixed far-edge channels), matching the VL
  writer, which puts `strokeWidth`/`strokeDash` in the boxplot's *shared*
  encoding. The median's hardcoded `linewidth = 1.5` is gone — VL's median tick
  carries no explicit thickness, so the resolved `linewidth` (default 1.0) is
  parity. `PointGeom` has no dash channel, so outliers take width only.
  `opacity` stays retargeted to the box's fill, mirroring VL's `opacity` →
  `fillOpacity` for a fill-bearing geom.
- **Facet strip labels** (`facet.rs`) now match the VL writer. `ordered_levels`
  returns a `Level { key, value, is_null, label }`: `key` is the arrow-cast string
  that selects the panel's rows (so `Panel.facet1`/`facet2` and `panel_dataframe`
  are unchanged and self-consistent), `label` is the strip text.
  - *Discrete*: `label_mapping` (`RENAMING`) applied per
    `build_indexed_facet_label_expr` — renamed / suppressed → empty strip /
    absent → raw value. A NULL level keys as the literal `"null"`, so
    `RENAMING null => 'The rest'` works. The domain element is matched by
    data-space string first, then numerically, because `label_mapping` is keyed on
    `to_key_string()` (`"5"`) which can differ from the column's cast text (`"5.0"`).
  - *Binned*: the facet column carries the **bin centre**, so
    `scales::{binned_bins, bin_at_centre}` join it back to its bin and label it
    with the bin's range — `"lower – upper"` (en dash), per-edge `RENAMING`
    overrides, and `"< upper"` / `"≥ lower"` (or `≤` / `>` when
    `closed => 'right'`) for a terminal bin whose outer edge label is suppressed
    by `oob => 'squish'`. Verified byte-identical to the VL writer's `labelExpr`
    for the same queries. The join is numeric (`column_to_f64`, whose arrow cast
    bridges `Date32`/`Timestamp` in exactly `ArrayElement::to_f64`'s units), which
    is why the temporal case works here while **VL silently fails it** (its
    midpoint-string comparison never matches the serialized form).
  - Ordering mirrors VL: a binned facet sorts by bin centre (VL's
    `resolve_facet_ordering` early-returns for Binned), everything else by
    `input_range` then numeric-aware ascending, then `reverse`.
  - A suppressed label is `Some("")`, not `None`, so hephaestus still reserves the
    strip slot and sibling panels stay aligned.
- **Free binned facet dimensions** (`scales::free_binned_scale`): a free binned
  dimension now gets a real `scale::binned` per panel instead of degrading to
  continuous. It keeps **ggsql's** global bin edges, narrowed to the window of bins
  the panel's data occupies, labelled with ggsql's own edge labels for that window.
  Edges and domain narrow *together* because a hephaestus binned scale keeps its
  edges in the output range and derives band width as `1/(edges-1)` — shrinking only
  the domain would leave every bar a global bin-width wide, hanging off the panel.
- **Binned axis ticks stay hephaestus's job.** `apply_breaks` hands a binned scale
  ggsql's break **edges** with ggsql's edge labels — composite `"lower – upper"`
  range labels belong to keyed legends and facet strips, not axes (the same split
  the VL writer makes). Placing an edge break correctly is hephaestus's
  responsibility, and it now does: `486390b` added `Scale::map_break`, which
  positions a binned break linearly in the domain instead of through `binned_map`
  (which still sends *data* to bin centres, as it should). Edge labels now sit on
  their boundaries with no collision.
- Verified: 62 writer tests (10 exact-text `facet_strips_*` assertions that need no
  GPU, 5 `binned_bins`/`bin_at_centre` unit tests, and `renders_*` smoke tests for
  titles, text stroke, composite widths/dashes, binned + free-binned facets);
  eyeballed titles (1×1 and faceted), red/white text outlines, dashed+thick
  boxplot and violin, binned facet strips (`2500 – 3500`, plus a `null` panel),
  free binned panels, and the collision-free fixed binned axis. fmt + clippy clean;
  feature, default (hephaestus absent) and `cargo +1.86` builds all pass.

**hephaestus dep bumped** to rev `9c2462e`, which fixes two bugs this work
surfaced (both reported upstream with standalone repros, both verified here):

1. `486390b` — a binned scale's breaks (its bin **edges**) were positioned through
   `binned_map`, so they rendered at bin *centres* and edges sharing a bin collided
   (for edges `2500…6500` the axis showed `2500 3500 4500 6500`, losing `5500`).
   Fixed by the new `Scale::map_break`, used by axis chrome.
2. `9c2462e` — a `Binned` scale kept its bin edges in the **output range**, so
   attaching a palette destroyed them: `range_colors` made every `map` return
   `Null` (marks silently vanished — `SCALE BINNED color` drew an empty panel) and
   `range_numbers` silently reinterpreted the palette as the edge list. Fixed by
   giving bins their own field (`Scale::{with_bins, set_bins, bins}`), so a binned
   scale is now usable as a material scale, its palette indexed by bin.

3. `a90dc84` — `collapse_legends` only merged `LegendBody::Stack` bodies, so two
   colorbars never collapsed. ggsql maps `color AS <var>` onto both `fill` and
   `stroke`, so every continuous *or* binned color scale drew **two identical
   colorbars**. Now merged when both bodies paint the same gradient
   (`colorbar_gradients_agree` + `Scale::visual_equivalent_to`); genuinely distinct
   legends (e.g. a `color` colorbar beside a `size` legend) still stay separate.
   No writer change needed — collapse runs at render.

- **Non-color legend keys paint.** A hephaestus legend key only draws what it is
  told to draw, so `LegendKeySpec::point().scaled("size", …)` with no color
  rendered *empty swatches* beside correct labels — every `size` / `shape` /
  `linetype` legend was blank (found while verifying the colorbar merge; it long
  predated it). `material_legend` now adds `.fixed("fill", …)` (`"stroke"` for a
  line key) whenever the scaled channel isn't itself a color, using
  `wiring::key_color`: the layer's constant `fill` (or `stroke`), else a neutral
  grey — mirroring hephaestus's own `examples/legends.rs`.
  The subtlety worth remembering: a *data-mapped* color aesthetic must be skipped,
  not read. Its column holds domain values, and `column_to_colors` maps an
  unparseable cell to `Color::BLACK`… except a continuous fill column's first row
  parsed as a **transparent** color (alpha 0), so the key stayed invisible in
  exactly the colorbar-plus-size case. `wiring::is_data_mapped` (extracted from
  `resolve_material`, now shared by both) is the guard.

Newly found, **not** fixed here (see §9): a data-mapped `linewidth` on a
boxplot/violin is rejected by ggsql itself.

## 8. Key source references

ggsql:
- `src/writer/mod.rs` — `Writer` trait (`type Output`, `write`, `render`).
- `src/writer/vegalite/` — the writer to mirror; `layer.rs` `GeomRenderer`,
  `projection/` `ProjectionRenderer`, `encoding.rs:501` palette resolution.
- `src/reader/spec.rs` — `Spec` accessors (`plot()`, `data()`, `layer_data()`).
- `src/naming.rs` — `__ggsql_*__` column conventions; data keys.
- `src/plot/` — `Plot`, `Layer`, `Geom`/`GeomType`, `Scale`/`ScaleTypeKind`,
  `Facet`/`FacetLayout`, `Projection`/`CoordKind`, `Labels`, `AestheticContext`.
- `src/plot/scale/palettes.rs` — `lookup_palette()`.

hephaestus (`~/GitHub/hephaestus`):
- `src/plot/composition.rs` — `PlotComposition` orchestrator, `render`.
- `src/plot/plot.rs` — `Plot`, `bind`, `add_geom`, chrome.
- `src/plot/scale/mod.rs` + `constructors.rs` — `Scale`, `ScaleRegistry`.
- `src/plot/geom/` — concrete geoms + `Geom`/`GeomBuilder` traits.
- `src/plot/chrome/axis.rs` — `Axis`, `AxisPlacement`.
- `src/plot/projection.rs` — `Projection::{Cartesian, Polar, Custom}`.
- `src/composition/` — `Composition`, `Patch`, `grid`/`beside`/`stack`, `Slot`.
- `src/backend/vello/` — `VelloRenderer`; `src/png.rs` — PNG writer.

## 9. Deferred

Everything known to be missing or wrong, deliberately not being worked on. Kept
here so it survives between efforts.

### Release plumbing (blocks publishing)

- **`hephaestus` is a pinned git dep** (`src/Cargo.toml`) on an unpublished
  `0.0.1` crate. crates.io rejects git dependencies **even when optional**, so
  ggsql cannot be published while this dep exists in that form. This is the one
  item that blocks a release rather than polish.
- No `src/writer/hephaestus/CLAUDE.md` (planned in §5). This document is doing
  that job and is a phase log, not an architecture doc.
- `src/CLAUDE.md` is stale: no `hephaestus` row in the feature table, and the
  `writer/` section still says "Only Vega-Lite is implemented today".
- CLI: `--writer` help text only advertises `vegalite`; no output-extension
  routing; no flags for width/height/dpi/background, so only one hardcoded size
  is reachable.
- `doc/` doesn't mention raster output at all.
- Default-writer switchover criteria still undecided (Decision 4).

### Correctness risks

- **Transparent backgrounds are probably wrong.** `HephaestusWriter::background()`
  accepts any color, but `render_to_buffer` returns **premultiplied** RGBA and the
  PNG encode path has no un-premultiply step.
- **Legends are captured from the first panel only**, assuming every panel yields
  identical legends. True under fixed scales; unverified for a free-scale facet
  that also maps a material aesthetic.
- **Log scales get no domain expansion** — under a non-identity transform the
  writer deliberately falls back to the raw data extent (see the ggsql-core item
  below).
- **No axis label thinning or rotation.** hephaestus's `Axis` is
  `rail(scale, placement)` + `title` only, with ticks coming solely from the
  scale, so long tick labels overlap in narrow facet panels (visible with binned
  range labels, and equally with long categorical labels).

### Feature gaps

- `arrow` geom — the only unsupported `GeomType` (deliberate).
- Theming: ggsql has no theme concept; the writer uses hephaestus's default and
  exposes no selection.
- Calendar-native temporal axes (numeric axes with ggsql's formatted break labels
  work today).
- Diagonal rules ignore `linetype` (`geom/segment.rs::build_diagonal` sets only
  stroke/linewidth/opacity).
- **`densified` segment/ribbon under `PROJECT`**: ggsql expands a projected
  segment into per-vertex rows and remaps `pos1end`→`pos1`, which the VL writer
  handles by switching to a `line` mark (`vegalite/layer.rs:1589-1624`). The
  hephaestus path would draw zero-length segments; `area`/`ribbon` have the same
  hole.
- boxplot `side` and `hinge` parameters are unimplemented (the VL writer has both).
- A `linewidth` aesthetic on ggsql's Text geom would let text outline width be set
  (a core + doc change; the outline itself works).

### Architectural debt — writer doing work ggsql should own

The principle is "ggsql owns all scale domains; the writer never computes
extents". Two scoped exceptions remain, both of which would disappear if ggsql
resolved per-panel domains and spatial position scales:

- **Free facet scales**: `scales::{free_position_scale, free_binned_scale}` compute
  per-panel domains (and select the per-panel bin window).
- **Spatial `pos1`/`pos2`**: synthesized in `mod.rs` from `computed["bbox"]` (or
  the geometry extent) because ggsql resolves no position scales for a spatial
  layer.

### Upstream ggsql-core (each also fixes the Vega-Lite writer)

- **Range expansion runs in linear data space then clips** to the transform's
  valid domain, so a log domain collapses to `[f64::MIN_POSITIVE, max]` and its
  breaks explode. Fix: expand in transform space.
- **`bar` on a numeric primary axis stays continuous** (no `pos1end`), so
  band-fraction bars get no width; VL hits the same wall (`bandwidth('x')` is 0).
- **A data-mapped `linewidth` on a boxplot/violin is rejected by ggsql**: the stat
  drops the column, so `linewidth AS w` fails validation for *both* writers
  ("Column `linewidth` … does not exist"). Grouping aesthetics (fill/stroke)
  survive; scalar ones don't.
- `Scale::break_labels()` misses `label_mapping` for numeric discrete/ordinal
  domains (`to_json()` `"5.0"` vs `to_key_string()` `"5"`).
- VL's `build_discrete_facet_label_expr` is unreachable dead code and iterates a
  `HashMap` nondeterministically — deletion candidate.
- VL's DateTime/Time binned facet strips are broken (its midpoint-string
  comparison never matches the serialized data); the hephaestus writer computes
  these from typed values and is correct.

### Upstream hephaestus

- `png::write_png` is file-only — no in-memory encode, so every host
  re-implements byte encoding.
- `render_to_buffer` returns premultiplied RGBA (see Correctness risks).
- No scale-level domain expansion / "nice" padding.
- Binned scales keep bin edges in the output range, so they can't also carry a
  color/size range (see the binned-material bug above).
- Range labels (`"lower – upper"`) would be the right presentation for a binned
  scale driving a **keyed** legend (size / shape), where the writer currently passes
  ggsql's edge labels. Not urgent: binned color renders as a colorbar, where edge
  labels on the band boundaries are correct.
- Chrome text (titles, axis labels, strip labels) can't be outlined:
  `TextElement` has no stroke field; `text_stroke` is a geom channel only.
- The `text` feature's parley shaper is documented as scaffolding "meant to be
  replaced by the host".
- `src/scales/` docs still claim transforms are Identity-only — stale.

### Standing constraints (accepted)

- **Raster only.** `vello`/wgpu is the sole working backend; `svg`/`pdf`/`blend2d`
  are declared placeholders. No vector output.
- **Needs a GPU adapter at render time.** CI installs lavapipe; this operational
  footgun isn't documented anywhere a user would find it.
- **MSRV split** (hephaestus 1.88 vs ggsql's CRAN-locked 1.86) — handled by
  gating, but it means this writer is not viable for the R/CRAN target and is not
  the wasm default.
- hephaestus is pre-1.0; the pinned rev needs periodic bumping.

### Testing

The writer's tests are render-succeeds smoke tests plus exact-text assertions for
strip labels and bin labelling, backed by manual eyeballing. §6 planned
**snapshot PNG tests** and they don't exist — there is no automated protection
against visual regression, which matters with a moving pinned rev.
