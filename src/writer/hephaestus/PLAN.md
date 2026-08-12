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
work deliberately deferred. The *architecture* — abstractions, invariants, how to
extend them — lives in [`CLAUDE.md`](CLAUDE.md).

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
  `wiring`'s constant-value helpers, instead of hardcoded grey.
- **tile linetype**: `linetype` wired on the rect material (dashed tile borders).
- **diagonal rule (abline)**: a rule with a non-zero `slope` (ggsql sets
  `parameters["diagonal"] = true`) renders as `SegmentGeom`s spanning the
  position scales' resolved range — `segment::build_diagonal` grabs each pos
  domain from `spec.find_scale("pos1"/"pos2").numeric_domain()`, computes
  `secondary = slope·primary + intercept` over it (intercept from the `pos2`/`pos1`
  mapping, slope from the `slope` mapping/SETTING), registers both axes from the
  endpoints, and binds x/x2→pos1, y/y2→pos2. The user supplies the ranges via
  `SCALE x/y FROM (..)`; when a scale is unresolved it falls back to 0..1. No
  DRAW/PLACE or multi-layer distinction — the writer just reads the scale ranges.
  Required teaching `wiring::constant_number` to read bare `Literal` aesthetic
  values (not only annotation columns), since `slope`, `stroke`, etc. arrive as
  `AestheticValue::Literal`. (Per-row slopes/intercepts and full material styling
  landed later — see the Chrome + composite polish section.)
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
  otherwise the mapped literal (via `constant_material`) or default. Boxplot and
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
- **Diagonal rules are per-row, and styled like any segment**
  (`geom/segment.rs::build_diagonal`). The writer used to collapse an abline to a
  *single* segment carrying the first row's slope/intercept and only
  stroke/linewidth/opacity, so `linetype` was dropped and `MAPPING slope AS slope,
  y AS y` (N lines) drew one. It now mirrors the Vega-Lite writer, whose
  `calculate` transforms evaluate `secondary = slope · primary + intercept` **per
  row**: `slope_values`/`intercept_values` read the mapped column when there is one
  (else the literal / SETTING, repeated), so N rows give N lines, each with its own
  slope and intercept. Materials go through the shared `wiring::wire_material` with
  the segment family's now-extracted `material()` table, so a data-mapped `stroke` /
  `linetype` / `linewidth` is scaled + legended exactly as on a plain segment —
  verified byte-for-byte against VL's `strokeDash` scale range for the same query.
  Positions are still computed (the spanning range comes from the pos scales'
  resolved domain), which is why this keeps its own builder rather than
  `build_and_add`. An empty data slice draws nothing, matching VL's zero-row layer.
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

## Densified geoms under `PROJECT` — status: implemented

Projecting a straight edge onto a curved surface bends it, so ggsql densifies
the edge **in SQL** rather than leaving each writer to approximate it: each
original row becomes a run of vertex rows along the projected edge, the extent
aesthetics are remapped onto plain `pos1`/`pos2`, `__ggsql_densify_id__` is
appended to the layer's `partition_by` to tie one row's vertices together, and
the layer is flagged `parameters["densified"] = true`. Four geoms do this
(`apply_projection` in `plot/layer/geom/{segment,rule,ribbon,tile}.rs`):

| geom | expansion | remap | shape |
| --- | --- | --- | --- |
| `segment` | 2 endpoints → N vertices | `pos1end`/`pos2end` → the `pos1`/`pos2` **columns** | open |
| `rule` | spans the clip bbox → N vertices | synthesizes the missing axis; forces `orientation => aligned` | open |
| `ribbon` | upper edge forward + lower edge backward → 2n vertices | `pos2min`/`pos2max` → the `pos2` column | closed |
| `tile` (continuous) | 4 corners → N vertices | drops `pos1min/max`,`pos2min/max`; adds `pos1`/`pos2` | closed |

Because the remap points the extent aesthetic at the *same* column rather than
deleting it, each geom failed silently in its own way instead of erroring: a
segment drew `x2 == x` (zero-length), a ribbon `y == y2` (zero-height), a rule a
fan of straight full-height lines (one per vertex), and a tile fell into
`rect::tile`'s discrete branch and drew a full-band rect per vertex. (`area` was
listed in §9 as having the same hole; it does not — it has no `apply_projection`
and never densifies.)

- **`geom/densified.rs`** dispatches ahead of the `GeomType` match in
  `geom/mod.rs::build_into_plot` (as the Vega-Lite renderers check `densified`
  first, so a densified rule takes this path over the diagonal-abline one). The
  vertices are exactly what `line` and `polygon` already draw — same columns,
  same `partition_by` grouping, same material tables — so `line::spec` /
  `polygon::spec` are reused whole rather than duplicated: `LineGeom` for the
  open pair, `PolygonGeom` for the closed pair. The VL writer makes the same
  swap, to a `line` mark with `interpolate: linear-closed` when closed. Vertex
  order is source order in both writers (VL's `order` is the row index).
- **Map framing is no longer spatial-only** (`mod.rs`): `spatial_bbox` became
  `map_bbox`, which takes ggsql's `computed["bbox"]` for **any** map projection
  and only falls back to the geometry extent for a bare `spatial` geom. Under a
  map every mark, the clip boundary and the graticules share one pre-projected
  data space, and hephaestus's `CustomProjection` outline is in *data space* — so
  with the old gate a non-spatial map (`DRAW segment … PROJECT TO robinson`) framed
  its position scales to the marks' own extent and got no aspect lock: the data
  drifted off the boundary and the map was stretched. The VL writer frames from
  the bbox unconditionally.
- Verified (eyeballed): curved firebrick segments on Robinson; a filled ribbon
  whose edges follow the meridians; dashed curving rule meridians; four
  projection-following tile quads under one colorbar; and a `ggsql:world` base map
  with route segments landing on the right countries. The pre-existing spatial /
  orthographic-globe renders are unchanged. Tests `renders_densified_{segment,
  ribbon,rule,tile}` and `renders_map_over_spatial_base` (feature `spatial`); 71
  writer tests; feature, default (hephaestus absent) and `cargo +1.86` builds,
  fmt, clippy all clean.

## `side` + `hinge` (banded geoms) — status: implemented

The two boxplot parameters §9 listed as missing, plus the orientation handling
they turned out to depend on. `hinge` and `side` are both *banded-axis* concepts —
they measure across the axis a box/violin/interval sits on — so the work is shared
in `wiring::BandAxes` and `geom/hinge.rs` rather than per geom.

- **`BandAxes`** (`wiring.rs`) names the channels of a banded geom by role instead
  of by axis: the aesthetic family holding the categories (`pos1`, or `pos2` when
  ggsql flipped the layer), the value family, the dodge column, and the
  banded-axis position / `_band` / `_offset` channels. Bindings need no swap — a
  hephaestus channel always drives the same panel axis; only the column feeding it
  moves. This closed a gap §9 never listed: **transposed composites were broken**
  — a horizontal `boxplot` errored outright (`no pos2end mapping`) and a horizontal
  `violin` silently rendered an empty panel (it read the category column as
  numbers). Both now render, caps and all.
- **`hinge`** (`geom/hinge.rs`) draws a `SegmentGeom` cap across the band via the
  segment's absolute **pt** offset channels (`x_offset`/`y_offset`), so a cap keeps
  its size at any panel width — the raster analog of the Vega-Lite writer's `tick`
  of `size` px. `boxplot` caps its two whisker fences (default `null` = no caps);
  `range` caps both interval endpoints (default 10pt, `hinge => null` to hide),
  which had been silently missing for every range the writer drew. Caps take the
  mark's resolved stroke/width/dash, so a per-group `stroke` colors them too.
- **`side`** (`wiring::{side_sign, band_edges}`) halves a mark onto one side of the
  band: box, median and caps span centreline → `±half`, while whiskers and
  outliers stay centred (matching VL). The violin's ribbon collapses one edge onto
  the centreline the same way. hephaestus band offsets are positive-right on x and
  positive-up on y, so `'top'`/`'right'` are positive in **either** orientation —
  one predicate, where the VL writer flips the sign with orientation because
  Vega-Lite's y offsets point down. The visual outcome is the same, including the
  documented half-violin + half-boxplot pairing. The full `width` is left to
  ggsql's dodge calculation, so a half-box still occupies its dodge slot.
- **Cap geometry is byte-equivalent to VL**, verified against its emitted spec for
  `hinge => 20`: VL's both-sides `tick` has `size = 26.67px` (= 20pt × 96/72),
  centred, so it spans `±13.33px`; the half-side tick has `size = 13.33px` shifted
  by `+6.67px`, so it spans `[0, 13.33px]` — half the length, starting exactly on
  the centreline. `band_edges(hinge / 2.0, …)` produces the same two edges
  (`±10pt`, or `0 → 10pt`). The only unit caveat is writer-wide, not hinge-specific:
  VL bakes points into CSS px at 96/72 while hephaestus converts at the render DPI,
  as it does for every absolute size (`linewidth`, point `size`, dash lengths).
- **Position adjustments reach every geom** (`wiring::wire_positions`). ggsql
  resolves `dodge` and `jitter` into per-row band fractions in
  `__ggsql_aes_pos1offset__`/`pos2offset` — and folds jitter's own `side` in there,
  so that third `side` consumer needs no writer logic at all, only delivery. Only
  the geoms that read `dodge_offsets` themselves (bar/histogram/tile, boxplot,
  violin) were consuming those columns, so a jittered `point` layer drew every
  point on the category centreline and a dodged one overplotted — the Vega-Lite
  writer has always bound the column to `xOffset`/`yOffset` for any layer carrying
  it. `wire_positions` now maps the offsets onto each position's matching `_band`
  channel, skipping channels the geom claims in `data_channels` (bar/tile compute
  edges that already include the offsets). This is also what the range-dodge gap
  needed, so `segment::build_hinges` reads the same offsets and a dodged interval's
  caps travel with it. `position => 'stack'` is untouched — it rewrites the value
  columns in data and produces no offset column.
- Verified (eyeballed): vertical + horizontal boxplots with caps, `side` left/
  right/top/bottom, dodged boxes with caps, per-group stroke caps, a dummy-axis
  (single) boxplot, faceted half-boxes, half-violin, the documented violin/boxplot
  pairing, range caps in both orientations plus `hinge => null`, jittered points
  (full band and one-sided), dodged points/text/ranges, the raincloud layout, and —
  unchanged against a pre-change build — dodged bars, boxes, tiles. Tests
  `renders_boxplot_hinge`, `renders_boxplot_side`, `renders_transposed_boxplot`,
  `renders_half_violin_with_half_boxplot`, `renders_range_hinges`,
  `renders_jittered_points`, `renders_dodged_points`,
  `renders_dodged_range_with_hinges`, `renders_jitter_with_half_boxplot`; 80 writer
  tests; feature, default (hephaestus absent) and `cargo +1.86` builds, fmt, clippy
  clean.

## Violin grouping — status: implemented

A violin's `RibbonGeom` keys now come from the **category and the layer's
`partition_by`**, not the category alone (`geom/violin.rs`), and rows are ordered
by the value axis within that composite group. ggsql keeps position aesthetics out
of `partition_by`, so neither key is sufficient by itself: the category alone
merged every group in a category into one contour (three islands of one species
drew as a single blob, in either orientation), while `partition_by` alone would
merge the categories. The Vega-Lite writer composes its `detail` encoding the same
way — `build_detail_encoding(partition_by)` plus the categorical position field.
This covers `PARTITION BY` as well as dodge, which the original report didn't
mention: the group *positions* were always right, only their identity was wrong.

An audit of the other geoms for the same fault found none. Only marks spanning
several rows can mis-group, which is `LineGeom` (line/path/smooth), `RibbonGeom`
(area/ribbon/density), `PolygonGeom` (polygon) and the densified pair — all of
which take their keys from `partition_by` through `wiring::build_and_add`
(`GeomSpec::grouped`). Every other geom (point, the rect family, segment/range/
rule, hinge caps, text, spatial, and each boxplot component) draws one mark per
row, where grouping cannot apply. Violin was the only geom building its own keys.

- Verified (eyeballed): dodged violins vertical + horizontal (one contour per
  island, correct fills), `side => 'right'` dodged, `PARTITION BY sex`, faceted
  violins, a `color`-mapped violin, and — unchanged — the single-group, dummy-axis,
  `intensity`, half-violin and ridgeline (`SCALE ORDINAL y`) renders. Test
  `renders_dodged_violin`; 81 writer tests; fmt, clippy clean.

## Temporal axes — status: implemented

Temporal axes and legends were labelled with the epoch integer their position
projects to (`1208`, or `106358400000000` for a timestamp) rather than the date,
and a `RENAMING` on a temporal scale was dropped entirely. Tick *positions* were
always ggsql's, so this was a labelling fault, not a placement one. Two causes:

- `Scale::break_labels()` (default impl in `plot/scale/scale_type/mod.rs`) built
  each label as `format!("{v}")` over `numeric_breaks()`, discarding the break's
  `ArrayElement` variant. It now labels each break from the element itself via
  `to_key_string()` — which is also how `label_mapping` is keyed, so the label
  template and `RENAMING` overrides are found instead of missed. Numeric labels
  are unchanged (`format_number` agrees with the old `format!("{v}")` on every
  break value), and the discrete/ordinal override is untouched.
- The writer built every continuous scale with `scale::continuous`, so hephaestus
  never learned the calendar unit even though the resolved ggsql scale names it
  (`transform` is auto-set to `Date`/`DateTime`/`Time` from the column dtype).
  `scales::temporal_scale` now builds `scale::temporal` in the unit the transform
  names — days / µs since epoch, ns since midnight, matching `ArrayElement` and
  therefore what a temporal column projects to f64 as — and `apply_breaks` hands
  a continuous temporal scale its breaks as `Value::Date`/`DateTime`/`Time`.
  Mapping is unaffected: hephaestus maps `Temporal` exactly as `Continuous`.

Free temporal facet dimensions keep ggsql's global break labels **narrowed to the
panel** (`free_continuous_scale`), the treatment `free_binned_scale` already gives
bin edges and what the Vega-Lite writer does with a free temporal axis. Letting
hephaestus pick per-panel calendar ticks instead — which it now can — invents
breaks ggsql didn't resolve and puts five full ISO labels in a panel ~130 px wide.
A panel that no global break falls inside keeps hephaestus's own ticks rather than
a bare axis; they read as dates either way, because the scale carries the unit.

- Verified (eyeballed, against the Vega-Lite render of the same query): a fixed
  `Date` axis, the same with `RENAMING * => '{:time %b %d}'`, a `TIMESTAMP` axis, a
  `Date`-mapped colourbar, and a free-scale faceted `Date` axis. Tests
  `temporal_scale_labels_ggsql_breaks_as_dates`, `temporal_scale_is_calendar_aware`,
  `test_temporal_break_labels_are_iso_strings`,
  `test_temporal_break_labels_honour_mapping`; full `--features hephaestus` suite;
  fmt, clippy clean.

## hephaestus bump — status: implemented

Dep bumped to rev `5aada7c`, which lands four of the deficiencies this writer
reported upstream:

1. **In-memory PNG encode.** `png::{encode_png, write_png_to}` sit beside the
   file-only `write_png`, so `render_png` calls `encode_png` instead of driving
   the `png` crate itself — the `png` dependency is gone from `src/Cargo.toml`.
   The same change settled the alpha question the Phase 1 notes left open:
   `render_to_buffer` hands out **straight** (un-premultiplied) alpha, which is
   what PNG stores, so no conversion is needed on a transparent background
   either.
2. **Chrome text can be outlined.** `TextElement` gained
   `text_stroke` / `text_linewidth_pt`, mirroring the text geoms' channels, and
   every chrome path (axis and legend tick labels, titles, polar) draws the
   stroke pass behind the fill. Nothing to wire here yet — ggsql has no theme
   concept — but the gap is closed upstream.
3. **Binned keyed legends reserve what they draw.** The measure pass now sizes
   one row per *bin* at the bin's midpoint, matching the renderer.
4. Stale `src/scales/` docs (transforms "Identity-only") and the `text` module's
   "scaffolding" framing of its parley shaper are both corrected.

**Binned legends now use hephaestus's binned mode.** `material_legend` calls
`.binned()` whenever the scale type is `Binned`, which was the fix §9 had wrong.
The old §9 entry wanted compound `"lower – upper"` range labels from hephaestus;
they aren't needed, because a binned legend puts the edge labels on a tick rail
*between* the keys, so each edge is labelled once at the boundary it names. What
was actually missing was the writer asking for that mode. Before: a keyed legend
drew one key per *edge* — five keys for a four-bin ladder, each sized at an edge
value, implying five categories and a mark of exactly that value. After: four keys
sampled at bin midpoints with `2500 … 6500` on the rail between them; on a
colorbar the same call gives four constant-color blocks instead of a gradient.
Vega-Lite has no between-keys rail, so it takes the other route — `determine_legend_style`
sends non-color binned aesthetics to a **symbol** legend, whose per-key labels
*are* Vega-generated ranges (`"2500 – 3500"`), which is why `encoding.rs` needs
`build_symbol_legend_label_mapping` to re-derive those strings before a `RENAMING`
can match them. hephaestus needs none of that machinery.
`open_lower()` / `open_upper()` remain unused: ggsql marks a suppressed terminal
label as `Some(None)`, which `break_labels()` turns into an empty string, so the
outer boundary renders as a bare tick rather than no tick.

Verified: full `--features hephaestus` suite (85 tests) passes, including
`renders_binned_size_legend` and `renders_binned_color_legend`; both eyeballed
against the pre-change render. fmt, clippy clean.

## Free-panel expansion — status: implemented

A free facet dimension is the one domain the writer computes itself, and it was a
raw data extent: marks at a panel's extremes were drawn *half outside* the panel
(a point at the maximum lost its top half to the clip) while a fixed axis got the
usual 5%. `SETTING expand` also silently stopped applying to a dimension once it
was freed.

Fixed by reading the policy off the scale instead of padding in the writer:

- `Scale::expand_range(min, max)` (`plot/scale/types.rs`) applies the scale's
  resolved `expand` factors to a caller-supplied range and clips to the transform's
  allowed domain — the same two steps, in the same order, as `resolve_common_steps`.
  It sits next to `numeric_domain()` / `break_labels()` because it is the same kind
  of thing: a resolved fact a consumer reads rather than re-derives. `Scale` was
  already the writer's only channel for scale truth, so no new surface was needed
  in `scales.rs`.
- `resolve_common_steps` now writes the factors it applied back into
  `properties["expand"]`, normalised to `[mult, add]`. Without this the writer would
  read the *requested* expansion and miss `ScaleDataContext::default_expand`, which
  is visible only during resolution — a polar full-circle theta resolves to zero
  expansion, and a free theta panel padded by 5% would open a gap in the pie. This
  matches how `properties["breaks"]` already carries a resolved value, and is
  idempotent: re-resolving reads back the same factors it wrote.
- `free_continuous_scale` calls `expand_range` on the panel extent. Break labels are
  filtered against the *padded* bounds, so a global break just outside a panel's
  data extent but inside its panel now draws, as it would on a fixed axis.
- `free_binned_scale` deliberately does **not** expand: a bar's band width is
  `1 / (edges - 1)`, which assumes the domain spans exactly the edges, so padding
  the domain would desynchronise bar width from bin width.

Verified: `FACET species SETTING free => 'y'` on penguins, eyeballed before/after —
every extreme mark is now whole and inside its panel, with break labels unchanged.
Four `expand_range` unit tests (default, `SETTING expand` as scalar and as
`[mult, add]`, zero, log clip); full suite 1782 tests + 24 doctests pass; fmt,
clippy clean.

## Minor breaks — status: implemented

Break positions are ggsql's to own, **minor as well as major**. The writer supplied
majors and left minors to be generated from the domain, so a sparse major set — a
fixed temporal axis narrowed to one break in a facet panel — got sub-unit minors and
read as a dotted rail. Fixed on both sides of the boundary; dep bumped to `5e9a060`
for the upstream half.

- **hephaestus** gained a minor-break override: `MinorBreaksSpec`
  (`Explicit` / `CountBetween` / `NumericInterval` / `TemporalInterval`) with
  `with_minor_breaks` / `with_minor_count` / `with_minor_interval` /
  `with_minor_temporal_interval` / `clear_minor_breaks`, independent of
  `breaks_spec`, falling back to the automatic algorithm when unset or when the
  variant doesn't match the scale type.
- **ggsql** already owned the *algorithms* —
  `TransformTrait::calculate_minor_breaks` per transform, plus a
  `default_minor_break_count` (1 for identity/sqrt, 8 for the log family, 3 for
  temporal) — but nothing resolved them: no callers outside
  `plot/scale/transform/` and `plot/scale/breaks.rs`. They had never been reachable,
  because Vega-Lite has no minor-tick concept and there was no other writer, so with
  one that draws them the whole thing became worth exposing rather than merely
  wiring: **`minor_breaks` is now a continuous-scale `SETTING`** mirroring `breaks` —
  a count, an array of positions, or a temporal interval string — resolved in place
  into an array of positions in step 5b of the default `resolve()` and read back via
  `Scale::numeric_minor_breaks()`. Documented in
  [`doc/syntax/scale/type/continuous.qmd`](../../../doc/syntax/scale/type/continuous.qmd).
  Three details worth keeping:
  - **The count is per major interval**, not a target for the whole axis the way
    `breaks => n` is. Subdividing an interval shouldn't depend on how many breaks the
    scale ended up with. `minor_breaks => 0` means none.
  - **`Some(vec![])` and `None` must stay distinct.** The first is "resolved to no
    minors", which a writer has to honour; the second is "not resolved", which leaves
    a writer free to fall back on its own. Hence `numeric_minor_breaks()` returns an
    `Option`, unlike `numeric_breaks()`.
  - Minors are filtered to the domain by comparing `to_f64()`, not through
    `filter_breaks_to_range`, which only filters `Number` elements and so cannot
    constrain a temporal break at all — a gap the majors path still has (see the
    ggsql-core list).

  `Binned` overrides `resolve` and has its own settings list, so it neither derives
  minors nor accepts the setting — a binned axis's ticks are its bin edges, with
  nothing to subdivide.
- **The writer** pins them through `apply_minor_breaks` (fixed scales) and the
  narrowed-to-panel list in `free_continuous_scale` (free dimensions), wrapping each
  position as the transform's value variant exactly as the majors are. Both go
  through `apply_pinned_minors`, which passes `None` straight through (keeping
  hephaestus's automatic minors) but pins an empty list as an empty list, so
  `minor_breaks => 0` reaches `with_minor_breaks(vec![])` and draws nothing. A free
  panel that no global major lands in keeps the whole tick set automatic rather than
  mixing ggsql minors with hephaestus majors.

Verified: a `DATE` line facetted with `free => 'x'`, eyeballed before/after — roughly
ten crowded weekly minors per panel become four on ggsql's own grid, majors unchanged;
plus `SETTING minor_breaks => 3` (three gridlines per interval), `=> 0` (none), and
`=> -1` rejected at validation. Eleven new tests across the accessor, the resolution
of each setting form, and the rejection; full suite 1793 tests + 24 doctests pass;
fmt, clippy clean.

## Writer options — status: implemented

The writer's canvas was only reachable through Rust: the CLI hardcoded
`HephaestusWriter::new(1500, 1000, 300.0)` with a transparent background, so no
user could pick a size, a resolution, or a background. Fixed generically rather
than with four hephaestus-specific flags, because "a writer takes settings" is a
property of the `Writer` abstraction, not of this writer.

- **`WriterOptions`** (`src/writer/options.rs`) is a normalised key–value bag —
  keys trimmed, lowercased, `-` folded to `_` — built by `parse` from
  `key=value` strings and read through accessors that produce the user-facing
  error themselves: `number`, `one_of`, `get`, and `reject_unknown`. One string
  may carry several options separated by **`;`**, so a caller can spell them out
  or collapse them, and mix the two. `;` is the only separator: `,` is common
  *inside* a value and `background=rgba(0,0,0,0)` has to survive. Values
  otherwise keep everything after the first `=`.
- **`Writer::from_options`** is a required trait method, so every writer answers
  the question and a frontend needs no compile-time knowledge of which one was
  chosen. Vega-Lite's implementation is `reject_unknown(&[])` — its output is
  resolution-independent, so size, DPI and background belong to whoever renders
  the spec.
- **hephaestus's** implementation takes `width`, `height`, `units`, `dpi`, and
  `background`, documented in [`CLAUDE.md`](CLAUDE.md#configuration). Two
  decisions worth keeping: `units` interprets only the dimensions the caller
  supplies (the defaults stay pixel counts, so `units=in` with no `height` is
  coherent), and the default background is now **white**, matching the writer's
  own `new()` and `ggsave`'s default rather than the CLI's old transparent
  canvas — `background=transparent` (or `none`) gets it back.
- **The CLI** collects a repeatable `--writer-option key=value` on `exec` and
  `run` — short `-D`, visible alias `--writer-options` — into a
  `WriterSpec { name, options }`, so the writer name and its settings travel
  together through `cmd_exec` → `exec_with_reader` → `render_spec`. An option
  error exits non-zero with the message; nothing is ignored. `-D` follows the
  gcc / java / cmake convention for a pass-through key=value, which also leaves
  `-r`, `-w` and `-o` to mean `--reader`, `--writer` and `--output` — the shorts
  a single letter next to those flags would otherwise be misread as, and all
  three now exist.

Verified: `width`/`height` in each unit render at the expected pixel dimensions
(6×4 in at 150 dpi → 900×600, 2.54 cm / 25.4 mm / 72 pt at 96 dpi → 96 px),
`background` accepts hex, names, `rgb()`, `hsl()`, `transparent`; unknown keys,
non-numeric values, a zero or absurd dimension, a bad unit and a bad color each
report the offending option. The collapsed form renders identically to the
spelled-out one under single quotes, double quotes and `\;`, and mixes with
repeated flags; unquoted in zsh the shell splits the command, which is why the
docs lead with the quoting. Eighteen new tests (twelve on `WriterOptions`, six on
`from_options`, neither needing a reader or a GPU); fmt and clippy clean.

## Visual test harness — status: implemented

Every phase above ended in "eyeballed", one query at a time, against whatever
cases the work happened to touch. That is how the late-phase omissions kept
surfacing: nothing ever rendered the *whole* feature surface at once. The docs
already contain a curated corpus that does — every executable ```` ```{ggsql} ````
cell in [`/doc/`](../../../doc/) — so the harness renders that corpus instead of
inventing a new one.

[`/ggsql-cli/examples/visual_test.rs`](../../../ggsql-cli/examples/visual_test.rs),
run as `cargo run -p ggsql-cli --features hephaestus --example visual_test`, writes
`target/visual-test/index.html`: one HTML page pairing each query with its render,
with `--compare` putting the Vega-Lite render of the **same `Spec`** beside it.
It is a developer tool, not a shipped feature — `[[example]]`'s `required-features`
keeps it out of `cargo test --workspace`. Implementation notes live in
[`/ggsql-cli/CLAUDE.md`](../../../ggsql-cli/CLAUDE.md); three properties matter
here:

- **The corpus runs like the docs run.** One reader per source file, cells in
  document order, so a page that builds a table in one cell and plots it in the
  next behaves as written. A cell with no `VISUALISE` runs as setup.
- **Nothing aborts the run.** An execution error, a render error, or a *panic*
  inside a writer is captured against its cell, so one report inventories every
  problem at once. A harness that stops at the first failure would answer the
  question this one exists to answer only for the first cell.
- **It is a display harness, not a snapshot suite.** There is still no baseline
  and no automated pass/fail on pixels (§9 Testing) — the judgement stays human.
  What changed is the cost of exercising it: 190 queries in one pass instead of
  one `--output /tmp/out.png` at a time.

The report labels each cell with its source file and line, so anything it turns
up points straight back at the query that produced it.

Verified: 191 cells from 33 files under `doc/syntax/` in ~160 s (185 plots, 6
setup cells), each rendered beside its Vega-Lite twin and eyeballed. The first
full run found the two constant-channel bugs fixed in the section below —
exactly the class of omission that one-query-at-a-time eyeballing had been
missing.

## Constant channels — status: implemented

The first full harness run turned up two bugs with one root: **the writer set
values it had resolved itself as plain hephaestus channel constants**, and a
`Channel::Constant` is *scale-applied* (`resolve.rs::resolve_value`; only the
`Raw*` variants bypass). A binding, meanwhile, belongs to the plot **channel**,
not to the geom that set it. So a constant only behaved as intended while no
other layer bound the same channel.

- **A non-diagonal `rule` panicked.** `SegmentGeom::build: "x" must be data, not
  constant — positions vary per row`. `segment::rule` spans the free axis with a
  0..1 panel fraction through `GeomSpec::raw_numbers`, which `build_and_add` set
  as a scalar; hephaestus's `require_data_column` rejects a constant on any
  position channel of a geom whose geometry varies per row. Now materialised one
  value per row. (The diagonal abline never hit this — `build_diagonal` already
  computes per-row endpoints.) Four cells in `doc/syntax/layer/type/rule.qmd`.
- **A constant material vanished next to a data-mapped sibling.** `DRAW line …
  DRAW rule MAPPING label AS colour` bound `stroke` to a categorical scale for
  the *whole panel*, so the line's constant black was looked up in
  `{Critical, Target, Warning}`, resolved to `Null`, and the line simply wasn't
  drawn — silently, in a plot that otherwise looked right. Every writer-resolved
  constant is now `Raw`: `set_literal_channel` (literals and `SETTING`s),
  `MatDefault` (geom defaults), `MaterialSource::Constant` (composites), and the
  composites' own `size`/`shape`/`fill_opacity`. Per-row band fractions
  and offsets are untouched — no scale is ever bound to a `_band` channel.

The rule that falls out, now stated in [`CLAUDE.md`](CLAUDE.md): **only a
ggsql-mapped column goes through a scale; everything the writer resolves itself
is `Raw`.** For symmetry `wire_positions` gained `constant_position`, so a
position arriving as a bare `Literal` is materialised per row and still travels
through its position scale instead of erroring — every geom now accepts every
form ggsql delivers an aesthetic in.

Verified: all five `rule` forms render (horizontal, vertical, N data-driven lines
with a collapsed legend, `aggregate => 'max'`, diagonal abline), the line+rule
overlay draws both layers, 91 writer tests pass, and a full harness re-run over
`doc/syntax/` is clean — 191 cells, 0 problems, all eyeballed against the
Vega-Lite renders. fmt and clippy clean.

## hephaestus bump `a353698` → `aec4e1b` — status: implemented

Upstream closed every item §9's "Upstream hephaestus" list had accumulated. The
bump is mostly *subtraction* on this side: three of the seven were writer
workarounds that now have nothing to work around.

Required by the bump (`aec4e1b` panics on an unknown channel rather than ignoring
it, so these were hard failures, not cosmetic):

- **The `alpha` channel is gone**; opacity is `fill_opacity` / `stroke_opacity`
  everywhere, geoms and legend keys alike. `area` and `violin` were the last two
  users — both `RibbonGeom`, both now on `fill_opacity`, which is what every other
  fill-bearing geom in the writer already mapped ggsql's `opacity` to.

Enabled by the bump:

- **`UNPINNABLE_CHANNELS` deleted.** hephaestus now sizes each swatch cell from
  the key it holds (`render_keys::swatch_dim_for`), insets a rect's border, and
  reserves a line key's cap body. All three reasons the writer withheld `size` /
  `linewidth` / `shape` from a key are gone, so `pin_constants` pins them like
  anything else: `SETTING shape => 'star'` now puts stars in the legend, and
  `SETTING size => 12` gets a cell that fits the marker instead of a disc painted
  across the legend.
- **The partial-opacity translation deleted.** `ResolvedKey` grew separate
  `fill_opacity` / `stroke_opacity` matching the geom channels 1:1, so a zero
  opacity is *pinned* rather than reverse-engineered into "leave the colour
  channel unset". `partial_opacity_target` and `suppressed_channels` are gone;
  `opacity => 0` on a point layer still draws open circles in both panel and key.
- **`LegendKind::Text`** added, and the `text` geom switched to it from `Point`.
  A scaled `fontsize` legend now draws letters at each size.

Fixed upstream with no writer change — verified by re-rendering each repro:

- A **shape scale** draws its marks (`ctx.scale_for("shape")`), and its keys draw
  the glyph at a sensible size with no swallowing outline disc.
- **`SCALE linewidth TO (0, 30)`** renders the polyline, tapering from zero.
- A **radar's closing edge** wraps forward across the 1.0/0.0 seam instead of
  retracing the interior.
- **Text chrome is measured with the width it is drawn at**, fixing both symptoms
  at once: a wrapped facet strip reserves all its lines rather than clipping to
  one, and an axis tick label containing a space centres on the whole label rather
  than on its first word.

Verified: 191 cells over `doc/syntax/` in ~164 s, 0 problems, eyeballed against
the Vega-Lite renders; the seven repros above re-rendered individually; 91 writer
tests pass; fmt and clippy clean.

## Area baselines — status: implemented

An `area` or `density` outlined its baseline as heavily as its curve, so every
one of them sat on a rule along `y = 0` — clearly wrong with `stroke => 'red'`,
and a visible black hem on the default stack. The ribbon-edge wiring sends each
outline aesthetic to *both* curves, which is right for `ribbon` (both edges are
data) and wrong here, where curve A is usually the axis.

Not right by geom, though, which is what §9 originally proposed: a **centred**
stack's bottom band rides on `-total/2`, and that baseline is the figure's lower
silhouette — dropping its outline would leave a streamgraph unbordered along the
bottom. The rule is therefore about the data, per mark: **a baseline that holds
one value is the axis; one that wanders is silhouette.** `area::baseline_outline`
groups the resolved baseline column by the same `partition_by` keys
`build_and_add` marks with, and emits a per-row `stroke_opacity` for curve A —
1.0 where the baseline wanders, 0.0 where it doesn't. Opacity is the gate because
hephaestus strokes curve A whenever its channel is *bound*, and a binding belongs
to the whole geom; `stroke_opacity` is per-mark (resolved at the mark's first
row), unmapped by ggsql, and free for the writer.

What that yields, each eyeballed: a plain area and a `density` outline only their
curve; a normal or `total`-normalised stack loses the hem at zero but keeps every
band boundary; a centred stack is unchanged from the fully-outlined render it
already had; `ribbon` is untouched. A transposed area is the same rule on
`pos1end`. Interior boundaries in a stack are still drawn twice — once as the
lower band's curve, once as the upper band's baseline — which is invisible while
the stroke is one colour, and takes the upper band's colour when `stroke` is
data-mapped.

Verified: five `silhouette_opacity` unit tests (no GPU) cover flat, wandering,
normal-stack, centred-stack and null baselines; 96 writer tests pass; the seven
renders above; fmt and clippy clean.

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
- CLI: `--writer hephaestus` plus `--writer-option` reach the writer and
  configure it, but there is still no output-extension routing — `--output
  chart.png` does not by itself select the raster writer.
- `doc/` covers raster output only in the CLI page's writer-options section; the
  gallery and the rest of the site are Vega-Lite throughout.
- Default-writer switchover criteria still undecided (Decision 4).

### Correctness risks

- **Legends are captured from the first panel only**, assuming every panel yields
  identical legends. True under fixed scales; unverified for a free-scale facet
  that also maps a material aesthetic.
- **A row whose scaled *numeric* value is null is drawn unpainted rather than
  dropped.** `column_to_f64` maps a null to `NaN`, the geom draws the mark anyway,
  and the channel resolves to nothing — so `doc/syntax/layer/position/jitter.qmd:67`
  renders penguins' two NULL-`body_mass` rows as white circles that Vega-Lite
  omits entirely. Note the rule differs by scale type and the categorical half is
  already correct: a **categorical** null is a trained level with its own colour
  and legend key (see `channels::NULL_CATEGORY`), a **continuous or binned** null
  is missing data and the row should not be drawn at all.
- **A map frames tighter than Vega-Lite does.** Both writers frame to the data
  bbox and now agree on proportions, but VL pads the fitted extent by 10%
  (`vegalite/projection/map.rs:135-147`, `dx = (xmax - xmin) * 1.1`) while
  `map_bbox`/`nice_range` pad not at all — `nice_range` only widens a degenerate
  span. Matching that 10% is the writer half. Whether framing should key off the
  projection's own extent instead of the data's is a separate core question in
  `resolve_final_bbox`, and `doc/syntax/coord/crs.qmd:204-213` currently documents
  data-framing as intended.
- **A log scale whose expanded lower bound crosses zero renders blank.** Not a
  writer fault and not "log scales get no expansion" — expansion works whenever it
  stays positive (`body_mass VIA log` resolves `[2520, 6480]`, a real 5% pad). The
  trigger is `min - mult·span - add ≤ 0`, i.e. data spanning decades: `(1, 10, 100,
  1000) VIA log` resolves `[2.2250738585072014e-308, 1049.95]` because ggsql expands
  in linear space and then clips to the transform's allowed domain (the ggsql-core
  item below). The data then occupies the top ~1% of a 311-decade axis, and the
  breaks land at `5e-308 … 1000`. **Both writers fail, differently**: VL emits a
  2498-character `axis.labelExpr` of denormal decimal literals and crushes every
  point against the top of the panel; hephaestus renders an essentially *empty*
  figure — chrome consumes the layout and only the `y` title survives. Fixing
  expansion in ggsql fixes both; hephaestus-side expansion would only be a
  fallback.
- **No axis label thinning or rotation.** hephaestus's `Axis` is
  `rail(scale, placement)` + `title` only, with ticks coming solely from the
  scale, so long tick labels overlap in narrow facet panels (visible with binned
  range labels, and equally with long categorical labels). **Now the most visible
  gap on a temporal axis:** a *fixed*-scale facet gives every panel the full
  global break set, and an ISO date label is ~3× the width of the epoch integer
  that used to be drawn there, so six dates collide where six numbers merely
  crowded. Vega-Lite doesn't hit this because Vega-Lite's own `labelOverlap`
  hides colliding labels. The fix belongs in hephaestus's `Axis`: it needs the
  measured text metrics to decide a stride, which the writer doesn't have and
  shouldn't guess. Keeping the tick and blanking its label is the presentation to
  aim for.
### Feature gaps

- `arrow` geom — the only unsupported `GeomType` (deliberate).
- Theming: ggsql has no theme concept; the writer uses hephaestus's default and
  exposes no selection.
- A `DateTime` axis is labelled with the full ISO timestamp
  (`1973-06-25T00:00:00`), because that is what `ArrayElement::to_key_string()`
  yields and hence what ggsql's default label template produces. The Vega-Lite
  writer draws the same string from the same mapping, so the two agree — but a
  compact default (dropping a time part that is midnight on every break) would
  suit both, and belongs in ggsql's label templating rather than in either writer.
- A `linewidth` aesthetic on ggsql's Text geom would let text outline width be set
  (a core + doc change; the outline itself works).
### Architectural debt — writer doing work ggsql should own

The principle is "ggsql owns all scale domains; the writer never computes
extents". Two scoped exceptions remain, both of which would disappear if ggsql
resolved per-panel domains and spatial position scales:

- **Free facet scales**: `scales::{free_position_scale, free_binned_scale}` compute
  per-panel domains (and select the per-panel bin window). Narrowed: the *extent*
  is still the writer's, but the padding around it is ggsql's via
  `Scale::expand_range`.
- **Spatial `pos1`/`pos2`**: synthesized in `mod.rs` from `computed["bbox"]` (or
  the geometry extent) because ggsql resolves no position scales for a spatial
  layer.

### Upstream ggsql-core (each also fixes the Vega-Lite writer)

- **Range expansion runs in linear data space then clips** to the transform's valid
  domain (`resolve_common_steps` → `expand_numeric_range_selective`, then
  `clip_to_transform_domain`), so a log domain whose padded minimum crosses zero
  collapses to `[f64::MIN_POSITIVE, max]` and its breaks explode. Fix: expand in
  transform space. This is the single worst open bug for *either* writer — see the
  measured symptoms under "Correctness risks".
- `filter_breaks_to_range` only filters `ArrayElement::Number` and only when both
  range endpoints are numbers, so it cannot constrain a **temporal** break: a
  calendar-aligned major outside the resolved domain survives and both writers place
  it off-panel. The minors path sidesteps this by filtering on `to_f64()`; the majors
  path should do the same.
- **`bar` on a numeric primary axis stays continuous** (no `pos1end`), so
  band-fraction bars get no width; VL hits the same wall (`bandwidth('x')` is 0).
- **An identity `size` column has no agreed unit**, so the two writers cannot
  agree on it. `SCALE IDENTITY size` bypasses scaling by design and both writers
  receive the same raw numbers (`flipper_len`, 172–231); VL reads `size` as an
  *area in px²* (≈15 px markers), hephaestus as a *pt diameter* (≈230–310 px, so
  the panel becomes one undifferentiated field). Neither is wrong on its own
  terms — VL converts pt→area only for *literals* (`encoding.rs:558`), never for
  an identity column, so the conversion has nowhere to live but core. Settling it
  means deciding what unit ggsql promises for an identity material column, then
  having both writers honour it. `doc/syntax/scale/type/identity.qmd:15`.
- **A data-mapped `linewidth` on a boxplot/violin is rejected by ggsql**: the stat
  drops the column, so `linewidth AS w` fails validation for *both* writers
  ("Column `linewidth` … does not exist"). Grouping aesthetics (fill/stroke)
  survive; scalar ones don't.
- **`TIME` columns are broken for both writers.** ggsql's Time convention is
  nanoseconds (`casting.rs` targets `Time64(Nanosecond)`, `schema.rs` reads via the
  strict `as_time64_ns`), but `needs_cast` treats any `Time64(_)` as already the
  target, so DuckDB's `Time64(Microsecond)` is never converted. VL fails hard
  (`Internal error: Expected Time64(Nanosecond) array, got Time64(Microsecond)`);
  the hephaestus writer renders raw µs against a domain ggsql couldn't resolve.
  Fix: treat a unit mismatch as needing a cast.
- **VL pins global `axis.values` on a free facet scale**, so a free temporal panel
  shows only whichever global breaks fall inside it (often one). The hephaestus
  writer deliberately matches this; both would improve if ggsql resolved per-panel
  breaks (see the architectural-debt item above).
- VL's `build_discrete_facet_label_expr` is unreachable dead code and iterates a
  `HashMap` nondeterministically — deletion candidate.
- VL's DateTime/Time binned facet strips are broken (its midpoint-string
  comparison never matches the serialized data); the hephaestus writer computes
  these from typed values and is correct.

### Upstream hephaestus

Pinned at rev `aec4e1b`. The shape of everything that got resolved here: the
writer's job is to pass resolved values through, so wherever hephaestus had to
compute something itself, the fix was a missing *setter*, not a better algorithm.

Every item this section previously listed is now fixed upstream, verified by
re-rendering its repro (see the bump entry in the phase log for the writer-side
changes the bump required). One item remains, and it costs nothing in practice:

**No scale-level domain expansion / "nice" padding.** Not a gap anywhere the
writer can reach. ggsql owns expansion: `resolve_common_steps` applies
`SETTING expand` via `expand_numeric_range_selective` while resolving the scale,
so `numeric_domain()` is already padded before either writer sees it, and both
pass it through verbatim (`continuous_domain` → `scale::continuous(min..=max)`;
VL's `build_scale_object` → `scale.domain`). Neither writer uses its host's own
padding — VL never emits `nice` or `padding` either — so the two agree exactly on
a fixed scale.

The one place it used to cost something, **a free facet dimension**, is fixed: see
the expansion section above. VL solves the same problem by *delegating* —
`build_scale_object` skips `domain` when `is_free(...)` and the spec sets
`resolve.scale: independent`, so Vega derives each panel's domain and pads it —
whereas the hephaestus writer asks ggsql for the factors and applies them to the
extent it computed. Both end up padded; ggsql's route additionally honours an
explicit `SETTING expand` per panel, which Vega's own padding would ignore.
What is left upstream is only the *fallback* case: a host with no resolved scale
at all still gets no padding from hephaestus.

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

The visual test harness narrows this but does not close it: it renders the whole
doc corpus into one report (see the section above), so a rev bump can be
re-eyeballed in a single pass, and an error or panic is *reported* per cell. It
still compares nothing against a baseline. The remaining step is to keep a
committed set of reference PNGs and diff against them — the harness's per-cell
naming (`<source-slug>-<NN>.png`) is already stable enough to serve as one.
