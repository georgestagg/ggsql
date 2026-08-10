# `writer/hephaestus/` — raster (PNG) writer internals

`HephaestusWriter` renders a resolved ggsql `Spec` to **PNG bytes** via
[hephaestus](https://github.com/posit-dev/hephaestus), a 2D scene renderer with a
grammar-of-graphics plot API. Behind the non-default `hephaestus` cargo feature.

Two companion documents, with different jobs:

- **[`PLAN.md`](PLAN.md)** — the design of record and the **phase log**: why this
  writer exists, what each work item changed, what was verified, and §9's
  inventory of everything deliberately deferred (bugs, upstream items, accepted
  constraints). Read it when you need *history* or *known gaps*.
- **This file** — the **architecture**: the abstractions, the invariants, and how
  to extend them. Read it when you need to *change the code*.

For ggsql language semantics see [`/doc/syntax/`](../../../doc/syntax/); for the
sibling writer's internals, [`../vegalite/CLAUDE.md`](../vegalite/CLAUDE.md).

## The governing principle

**ggsql owns every scale domain; the writer never computes its own extents.**
The `Spec` arrives with each `Scale` fully resolved — type, domain (already
expanded, transform-aware, trained globally over all layers and the whole
position family), transform, breaks, formatted labels, and a concrete output
range for material aesthetics. The writer's job is to *pass those through* to
hephaestus, which performs the value→pixel mapping at draw time. This mirrors
the Vega-Lite writer, which passes `input_range` into `scale.domain`.

The practical consequence: when something looks wrong, the fix is usually a
missing *pass-through*, not a better computation here. The same shape held
upstream — every hephaestus gap this writer hit was a missing setter, not a
missing algorithm.

There are exactly **two scoped exceptions**, both flagged in the code and in
PLAN.md §9 as debt that would disappear if ggsql resolved more:

| Exception | Where | Why |
| --- | --- | --- |
| Free facet dimensions | `scales::{free_position_scale, free_binned_scale}` | ggsql resolves one global domain; a `free` panel needs its own. Only the *extent* is computed — the padding around it is still ggsql's, via `Scale::expand_range`. |
| Spatial `pos1`/`pos2` | `mod.rs::map_bbox` | A spatial layer positions by geometry, so ggsql resolves no position scales. The bbox still comes from ggsql (`Projection.computed["bbox"]`), falling back to the geometry extent only for a bare `spatial` geom. |

## Configuration

Raster output needs concrete dimensions, so unlike the Vega-Lite writer this one
carries state: `width`, `height` (both pixels), `dpi`, and `background`.
`HephaestusWriter::new` + `.background()` set them directly;
`Writer::from_options` builds the same thing from the frontend-agnostic
key–value [`WriterOptions`](../options.rs) (`-D width=1600` on the CLI). The user-facing table of keys lives in the struct's rustdoc and in
[`/doc/get_started/tooling/cli.qmd`](../../../doc/get_started/tooling/cli.qmd);
what matters here:

- **`units` interprets supplied dimensions only.** `to_pixels` converts a
  physical unit through inches at `dpi`, so a figure given in inches grows with
  resolution. The defaults are pixel counts and so are unit-independent.
- **DPI is not just print resolution.** hephaestus converts the theme's physical
  sizes (text, strokes, spacing — all points) at render DPI, so `dpi` also sets
  how large the chrome is relative to a pixel canvas.
- **Every option is validated, none is ignored.** `reject_unknown` first, then a
  per-key error naming the option; `whole_pixels` rejects a dimension outside
  `1..=MAX_DIMENSION` so a slipped unit conversion fails with a message rather
  than by exhausting GPU memory.

## Render flow

Unlike the Vega-Lite writer, which emits a declarative document and lets the VL
runtime do layout and scale application, hephaestus **is** the runtime. So
`write` builds a live object graph and renders it.

```
HephaestusWriter::write(&Plot, &HashMap<String, DataFrame>)
 │
 ├─ facet::build_panels(spec, data)      → (Composition, Vec<Panel>)
 │      1×1 grid + one Panel when unfaceted; else grid(nrow, ncol, cells)
 ├─ PlotComposition::new(&composition).shape_registry(..)
 ├─ wiring::plot_label → composition title / subtitle / caption
 ├─ for scale in spec.scales:            scales::build_scale(scale, RangeKind)
 │      → view.insert_scale(scale.aesthetic, hs)      ← the fixed/shared scales
 ├─ map_bbox → insert continuous "pos1"/"pos2" for a map / spatial plot
 │
 ├─ for panel in panels:                              ← one hephaestus Plot each
 │    ├─ facet::panel_dataframe(layer_df, panel)      per-layer row slice
 │    ├─ facet::PanelScales::new(spec, panel)         free dims → "pos1__p{idx}"
 │    ├─ free dims: scales::free_position_scale → view.insert_scale
 │    ├─ for (layer, df) in slices:  geom::build_into_plot(&mut plot, &Ctx{..})
 │    │      geoms set channels, plot.set_binding(channel, scale), push legends
 │    ├─ projection::apply_projection(plot, spec, panel, &ps)   ← axes live here
 │    ├─ map: plot.aspect_ratio(h/w).aspect_mode(Range)
 │    ├─ panel.strip_top / strip_right → plot.strip(AxisSide::…)
 │    └─ view.attach_plot(plot)
 │
 ├─ legend_sink (captured from the *first* panel) → view.add_legend(..)
 ├─ view.validate()
 └─ render_png: VelloRenderer → RGBA8 buffer → hephaestus::png::encode_png
```

Layers draw in `spec.layers` order, which is DRAW order, which is z-order.

## Module map

| File | Role |
| --- | --- |
| [`mod.rs`](mod.rs) | `HephaestusWriter` (size / dpi / background), `Writer` impl including `from_options`, the orchestration above, `map_bbox`, `render_png`, and the writer's test suite. |
| [`wiring.rs`](wiring.rs) | The shared, geom-generic machinery: `Ctx`, `GeomSpec` + its parts, `build_and_add`, `wire_positions`, `wire_material`, `MaterialSource`/`resolve_material`, `BandAxes`, `side`/band helpers, `material_legend`, label resolution. |
| [`scales.rs`](scales.rs) | ggsql `Scale` → hephaestus `Scale`. `RangeKind`, transform + palette + break mapping, temporal scales, free-panel scales, `binned_bins`/`bin_at_centre`. |
| [`channels.rs`](channels.rs) | DataFrame column → typed channel data (`ChannelData`, `column_to_*`), group keys, WKB/WKT geometry decoding. |
| [`facet.rs`](facet.rs) | `FACET` → `Composition` + `Vec<Panel>`; level ordering, strip labelling, per-panel row slicing, `PanelScales`. |
| [`projection.rs`](projection.rs) | `PROJECT` → hephaestus `Projection`, **and the axes** (they depend on the coord). |
| [`geom/`](geom/) | One module per geom family, each declaring a `GeomSpec` or supplying a custom builder. `geom/mod.rs` is the dispatch + `is_supported`. |

## Core abstractions

### `Ctx` — what a geom is given

Read-only per (layer, panel): the `Plot`, the `Layer`, that panel's sliced
`DataFrame`, `transposed`, the scale names to bind positions to
(`pos1_scale`/`pos2_scale` — panel-aware, so free facets work), and the legend
sink. Geoms write bindings directly onto the `HPlot` and push legends through
`Ctx::push_legend`; there is no accumulator to thread.

### `GeomSpec` — the declarative path

Most geoms are just data. A module returns a `GeomSpec` and
`wiring::build_and_add::<G>` does the rest (see [`geom/point.rs`](geom/point.rs)
for the minimal case):

| Field | Meaning |
| --- | --- |
| `positions: Vec<PositionSpec>` | hephaestus `channel` ← ggsql `aesthetic`, plus which `PanelAxis` it drives (so the right `pos` scale is bound and the right dodge/jitter offsets picked up). |
| `material: Vec<MaterialSpec>` | ggsql aesthetic → hephaestus channel, a `RangeKind`, and a `MatDefault` fallback matching ggsql's own geom default. Several aesthetics may target one channel (`fill`/`color`/`colour` → `fill`); the first that resolves wins. |
| `raw_strings` | Unscaled string channels from a mapped aesthetic (text labels). |
| `raw_numbers` | Constant panel-space values that bypass scales (a rule's 0..1 span), materialised one per row. |
| `data_channels` | Per-row values the geom computes itself (bar/tile band edges). Channels listed here are *claimed*: `wire_positions` won't overwrite them with the raw offsets, because the geom already folded those in. |
| `legend_key: LegendKind` | Point / Line / Rect swatch, so a line legend shows a line. |
| `grouped: bool` | Derive hephaestus `keys` from `layer.partition_by`, for multi-vertex marks (line, area, polygon). |

### The three ways ggsql delivers an aesthetic

This is the single most important thing to get right, and it mirrors the
Vega-Lite writer's `build_encoding_channel` exactly. `wire_material` (whole
column) and `resolve_material` (row-subsettable, for composites) both dispatch
the same three ways:

| `AestheticValue` | Meaning | Handling |
| --- | --- | --- |
| `Literal(..)` | A fixed value — **every geom default and every `SETTING` constant** arrives this way, not as a materialized column | `Raw` constant channel value (`set_literal_channel` / `constant_material`), converted by `RangeKind` |
| `Column` with a non-identity scale | Data-mapped | Set the column, `plot.set_binding(channel, aesthetic)`, record one legend |
| `Column` with an identity scale, or `AnnotationColumn` | Visual-space values already | Per-row `Raw` |

`MatDefault` is a true last-resort fallback, only for an aesthetic ggsql didn't
map at all. Keying off columns alone silently drops every literal — which is how
`SETTING color => 'red'` once rendered black.

**Only a ggsql-mapped column goes through a scale; everything the writer
resolves itself is `Raw`.** A hephaestus binding belongs to the *plot channel*,
not to the geom that set it, so one layer mapping `colour` binds `stroke` to a
categorical scale for **every** layer in the panel. A plain (non-`Raw`) constant
on that channel — a literal, a `MatDefault`, a composite's
`MaterialSource::Constant` — is then looked up in that scale's domain, resolves
to `Null`, and the mark silently disappears. `Raw` bypasses the binding, which
is what a value already in visual space wants anyway. The same holds for a
position given as a constant: `wire_positions` materialises it per row through
`constant_position` so it still travels through its position scale, because a
hephaestus geom whose geometry varies per row rejects a constant position
channel outright (`"x" must be data, not constant`).

### `MaterialSource` — composites

A composite geom (boxplot, violin) decomposes one ggsql layer into several
hephaestus geoms that must all be styled *identically*, each from its own row
subset. `wiring::resolve_material` resolves an aesthetic once — registering the
binding and one legend if data-mapped — and returns a `MaterialSource` that
components `.apply(&mut builder, channel, &row_indices)`. `resolve_color` /
`resolve_optional_color` are the color-typed wrappers (the latter for aesthetics
whose ggsql default is `Null`, e.g. a text geom's `stroke`, where "unmapped"
must leave the channel unset). This is the raster analog of Vega-Lite's *shared
encoding* on a composite mark.

### `BandAxes` — banded geoms and orientation

Boxplot, violin and range measure *across* the axis they sit on. `BandAxes`
names channels by **role** rather than by axis — `band()`, `value()`, `dodge()`,
`band_channels()`, `band_fraction_channels()`, `band_offset_channels()` — and
flips them when ggsql transposed the layer. Bindings never swap: a hephaestus
channel always drives the same panel axis; only the column feeding it moves.

`side_sign` + `band_edges(half, side)` halve a mark onto one side of the band
(`'both'` → `±half`, else centreline → `±half`). hephaestus band offsets are
positive-right on x and positive-up on y, so `'top'`/`'right'` are positive in
*either* orientation — one predicate, where the Vega-Lite writer flips the sign
because VL's y offsets point down.

## Channel naming

hephaestus channels are named per **panel axis**; scales are registered under the
**ggsql aesthetic**. `plot.set_binding(channel, scale_name)` ties them together
and is idempotent, so repeated bindings across layers and components are
harmless.

| Concept | hephaestus channel |
| --- | --- |
| Positions | `x`, `x2`, `y`, `y2` |
| Band fraction offsets (dodge/jitter, width) | `x_band`, `x2_band`, `y_band`, `y2_band` |
| Absolute (pt) offsets — hinge caps | `x_offset`, `x2_offset`, `y_offset`, `y2_offset` |
| Color | `fill`, `stroke` (`stroke2` = a ribbon's far edge; `text_stroke` = a glyph outline) |
| Scalars | `size`, `linewidth`, `linetype`, `shape`, `fill_opacity` / `stroke_opacity` / `alpha` |
| Geometry / text | `geometry`; `text`, `anchor_x`, `anchor_y`, `angle`, `weight`, `italic`, `family` |

| Scale registry key | Source |
| --- | --- |
| `pos1`, `pos2`, `fill`, `stroke`, `size`, `shape`, `linetype`, … | The ggsql aesthetic name, from `spec.scales` |
| `pos1__p{index}`, `pos2__p{index}` | A **free** facet dimension's per-panel scale |

Under Polar, ggsql assigns pos1→radius and pos2→theta (as the Vega-Lite writer
does); `projection.rs` tells hephaestus so via `PolarProjection`'s
`angle_channel`/`radius_channel` rather than renaming anything.

## Scales

`scales::build_scale(Option<&GScale>, RangeKind) -> Option<HScale>` is the whole
translation. It returns `None` when ggsql resolved no scale type — the writer
registers nothing rather than fabricating a scale.

- `ScaleTypeKind` maps 1:1 (Continuous / Discrete / Ordinal / Binned /
  Identity), as do the transforms; cast and temporal transforms map to identity
  because values arrive already projected to `f64`.
- A **temporal** continuous scale becomes `scale::temporal` in the unit its
  transform names (days / µs since epoch, ns since midnight — exactly
  `ArrayElement`'s units), so hephaestus's own ticks read as dates.
- `RangeKind` selects how a resolved `OutputRange::Array` becomes a hephaestus
  range: `range_colors` / `range_numbers` / `range_strings` / `range_linetypes`,
  and nothing for `Position`. Palettes are already concrete by the time the
  writer runs.
- **Breaks are ggsql's, majors and minors alike.** `apply_breaks` feeds
  `break_labels()` in as `with_breaks_labeled`, so ticks (and `RENAMING`
  overrides) match ggsql — and therefore the Vega-Lite writer — exactly.
  `apply_minor_breaks` does the same for `numeric_minor_breaks()`, where
  `Some(vec![])` ("resolved to none") must stay distinct from `None` ("not
  resolved, fall back to hephaestus's automatic minors").

## Faceting and panels

ggsql resolves faceting fully (layout, `free` flags, Wrap's `ncol`, per-row
`__ggsql_aes_facet1__`/`facet2__`), so `facet.rs` only lays it out: a
`Composition` of named patches plus a `Vec<Panel>` the write loop iterates.
Unfaceted is the same path with one panel, so there is no branch.

- **Ordering** mirrors the Vega-Lite writer's `resolve_facet_ordering`: binned
  facets by bin centre, else the facet scale's `input_range` then numeric-aware
  ascending, then `reverse`.
- **Slicing** is `DataFrame::take` on matching row indices. A layer with no facet
  column is used whole in every panel; a grid cell with no data in any layer is
  skipped, leaving an empty framed panel.
- **Strip labels** come from `Level { key, value, is_null, label }` — `key`
  selects rows, `label` is the text. Discrete levels honour `RENAMING`
  (suppressed → `Some("")`, *not* `None`, so hephaestus still reserves the strip
  slot and panels stay aligned); binned levels join the column's bin **centre**
  back to its bin via `scales::bin_at_centre` and label it with the bin's range.
- **Edge-only axes** (the ggplot2 look): `Panel::{first_col, last_row}` are
  honoured in `projection::apply_proj_cartesian`. A free dimension forces its
  axis onto every panel.

## Projections and axes

`projection::apply_projection` dispatches on `CoordKind` and **creates the axes**,
because the axis kind depends on the coord: Cartesian gets bottom/left rails,
Polar an angular ring + a radial rail, Map neither (the clip boundary and
graticules are the chrome). `has_real_axis` suppresses an axis whose position
scale is a synthetic `__ggsql_stat_dummy` (a pie's radius, a bar with no x),
mirroring the VL writer's `AxisInfo::suppress`.

Map coordinates arrive **pre-projected from SQL**, so hephaestus reprojects
nothing: a `CustomProjection` takes `computed["panel_boundary"]` as its clip
surface and `graticule_lon`/`graticule_lat` as its grid, all decoded from WKT by
`channels::wkt_to_*`. Custom's coordinate math equals Cartesian, which is exactly
what pre-projected data wants.

## Legends

Legends live on the **composition**, never on a per-panel plot, so a faceted plot
gets one shared legend. `Ctx` carries a `RefCell<Vec<Legend>>` sink that is
`Some` only while building the **first** panel — every panel produces the same
legends, since all are built from the globally resolved scales — and `write`
registers the captured set once. This covers the single-panel case with no
special-casing.

Beyond that, deduplication is hephaestus's: `collapse_legends` merges legends
whose scales are equivalent, which is what makes `color AS <var>` (mapped onto
*both* `fill` and `stroke`, hence two scales) render as one swatch. Do not build
a writer-side dedup map.

One subtlety in `material_legend`: a legend key paints only what it is told to
paint, so a non-color scaled channel (`size`, `shape`, `linetype`) needs a fixed
body color or the swatch comes out empty. `key_color` supplies the layer's
constant `fill`/`stroke`, skipping a *data-mapped* color aesthetic — its column
holds domain values, not colors.

## Adding a geom

1. Add a module under [`geom/`](geom/) returning a `GeomSpec`, and dispatch it in
   `geom/mod.rs::build_into_plot` via `build_and_add::<TheHephaestusGeom>`. Add
   the `GeomType` to `is_supported` — `validate` rejects anything not listed.
2. Get the ggsql defaults right in the `material` table: check what the geom's
   ggsql definition actually sets, since those arrive as `Literal`s and the
   `MatDefault` only fires when nothing is mapped.
3. Reach for a **custom builder** (as [`geom/text.rs`](geom/text.rs),
   [`geom/spatial.rs`](geom/spatial.rs) and the composites do) only when the geom
   has no plain x/y columns, needs unit conversion, or computes its positions.
   Even then, route materials through `wire_material` / `resolve_material` rather
   than hand-rolling them — that is what keeps data-mapped aesthetics working.
4. Check the **densified** path: under a map `PROJECT`, ggsql expands segment /
   rule / ribbon / tile into per-vertex rows and remaps the extent aesthetics
   onto plain `pos1`/`pos2`. [`geom/densified.rs`](geom/densified.rs) runs
   *before* the `GeomType` match and reuses `line::spec` / `polygon::spec` whole.
   A geom that densifies but isn't handled there fails *silently*, not loudly.
5. When in doubt about behaviour, read what the Vega-Lite writer does for the
   same geom and match it — the two writers are meant to agree.

## Testing

Tests live at the bottom of [`mod.rs`](mod.rs):

```sh
cargo test --features hephaestus --lib writer::hephaestus
```

Two kinds, plus a third that doesn't exist yet:

- **`renders_*` smoke tests** — render succeeds and the output carries the PNG
  signature. `assert_png_or_skip` tolerates a headless machine with no GPU
  adapter (it skips rather than fails), so a green run does not prove a render
  happened locally.
- **Exact-text assertions** — `facet_strips_*` and the `binned_bins` /
  `bin_at_centre` / temporal-scale unit tests need no GPU and are the real
  regression net.
- **Snapshot PNG tests do not exist** (PLAN.md §9). Visual correctness is
  verified by eyeballing, usually against the Vega-Lite render of the same
  query. With a moving pinned hephaestus rev, assume a bump needs re-eyeballing:

```sh
cargo run -p ggsql-cli --features hephaestus -- exec "<query>" \
    --reader "duckdb://memory" --writer hephaestus --output /tmp/out.png
```

For eyeballing *at scale* — after a hephaestus bump, or when hunting the kind of
small omission that only shows up across the whole feature surface — use the
visual-test harness instead of one-off queries. It renders every executable
```` ```{ggsql} ```` cell in [`/doc/`](../../../doc/) (≈190 in `doc/syntax/`
alone) and writes one HTML report pairing each query with its render, optionally
beside the Vega-Lite render of the same `Spec`:

```sh
cargo run -p ggsql-cli --features hephaestus --example visual_test -- --compare
open target/visual-test/index.html
```

It never stops on a failure — an error or a panic is captured against its cell —
so one run inventories every gap at once. Implementation notes:
[`/ggsql-cli/CLAUDE.md`](../../../ggsql-cli/CLAUDE.md).

## Operational constraints

- **A GPU adapter is required at render time.** Vello/wgpu is hephaestus's only
  working backend. CI installs Mesa's lavapipe; headless containers need
  something equivalent.
- **Raster only.** No SVG/PDF — hephaestus's other backends are declared
  placeholders.
- **MSRV split.** hephaestus needs rustc ≥1.88; ggsql's MSRV is CRAN-locked at
  1.86. The feature is therefore non-default and excluded from the MSRV job (CI
  runs the hephaestus steps with `cargo +stable`), which also means this writer
  is not viable for the R/CRAN target and is not the wasm default. Always check a
  change still builds under `cargo +1.86 build` *without* the feature.
- **The dependency is a pinned git rev** on an unpublished `0.0.1` crate
  (`src/Cargo.toml`). crates.io rejects git dependencies even when optional, so
  this blocks publishing ggsql — the one item in PLAN.md §9 that is a release
  blocker rather than polish.

## See also

- [`PLAN.md`](PLAN.md) — design of record, phase log, and §9's deferred work.
- [`../vegalite/CLAUDE.md`](../vegalite/CLAUDE.md) — the sibling writer to mirror.
- [`../../CLAUDE.md`](../../CLAUDE.md) — the core crate: feature flags, pipeline.
- [`../../plot/CLAUDE.md`](../../plot/CLAUDE.md) — the AST and scale types this
  writer consumes.
- [`/doc/syntax/`](../../../doc/syntax/) — authoritative ggsql syntax reference.
