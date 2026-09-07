# `ggsql-jupyter/` — Jupyter kernel

Standalone Rust binary that speaks the Jupyter messaging protocol over ZeroMQ. Embeds the `ggsql` library and renders results as Vega-Lite (visual queries) or HTML tables (pure SQL). Workspace member; published to crates.io and to PyPI (as a binary wheel via maturin).

End-user installation and usage live in [`README.md`](README.md). End-user notebook docs live in [`/doc/get_started/tooling.qmd`](../doc/get_started/tooling.qmd). This file describes the *implementation*.

## Layout

```
ggsql-jupyter/
├── Cargo.toml          Rust binary + library, depends on ggsql with duckdb + vegalite
├── pyproject.toml      maturin config (bindings = "bin") for the PyPI wheel
├── README.md           User-facing install + usage
├── src/
│   ├── main.rs         Binary entry: clap CLI (start kernel, --install)
│   ├── lib.rs          Library root (so internals can be unit-tested)
│   ├── kernel.rs       Jupyter messaging loop (ZMQ, message dispatch)
│   ├── executor.rs     Runs queries via ggsql::Reader, returns rendered output
│   ├── connection.rs   Reader lifecycle, connection-string parsing
│   ├── data_explorer.rs Positron data-explorer comm channel
│   ├── display.rs      Output formatting (Vega-Lite + vega-embed HTML, SQL → HTML table)
│   ├── message.rs      Jupyter message structs (ZMQ frames, HMAC signing)
│   └── util.rs
└── tests/
    ├── test_compliance.py   Jupyter protocol conformance
    ├── test_integration.py  End-to-end via jupyter_client
    ├── fixtures/            Sample notebooks / queries
    └── requirements.txt
```

## How it runs

1. `ggsql-jupyter --install` writes a kernelspec into the active Python environment (Jupyter, conda, uv, virtualenv — auto-detected).
2. `ggsql-jupyter <connection-file>` is the entry point Jupyter invokes; it reads the connection JSON, opens the five ZMQ sockets (shell, control, iopub, stdin, heartbeat), and runs `kernel.rs`'s message loop.
3. Each `execute_request` is dispatched through `executor.rs` → `ggsql::reader::DuckDBReader::execute(...)`. The kernel keeps a single persistent in-memory DuckDB session so cells share state.
4. The result is wrapped by `display.rs` into a Jupyter message. A plot is **rendered here, in the kernel** (see [Rendering](#rendering)); pure SQL goes out as an HTML table.

## Where output goes: `SessionKind`

`display.rs`'s `SessionKind` decides which of three output slots a result is aimed at, and it is the thing every rendering decision keys off:

| `SessionKind` | Slot |
| --- | --- |
| `PositronConsole` | Positron's Plots pane |
| `PositronNotebook` | The notebook cell |
| `Standalone` | A static document — Jupyter, Quarto, nbconvert |

**The console/notebook split is not cosmetic.** Positron routes a plot comm to the Plots pane whatever kind of session opened it, so a notebook session that used one would put its picture in the pane and leave its cell empty. The two need different output paths, and this is what tells them apart.

`SessionKind::resolve(session, mode)` prefers what the frontend *declared* over what its session id looks like:

- **`--session-mode console|notebook|background`** is authoritative. Only a frontend that creates the session can say, which in practice means the ggsql extension — `manager.ts`'s `createKernelSpec` appends it from `sessionMetadata.sessionMode`. The enum's values are already the flag's spelling, so nothing is translated. `background` maps to `Standalone`: it is Positron's session, but attached to no UI, so there is no Positron slot to render into.
- **The session-id heuristic** is the fallback: a `ggsql-` prefix means Positron (its supervisor tags every session it manages), and `notebook` in the id means a notebook. It exists for external Jupyter and Quarto, which pass no flag, and for extension versions predating it.

Two places deliberately **do not** pass the flag. `writeKernelJson` and `ggsql-jupyter --install` write kernelspecs for *external* frontends, which are exactly the ones that should classify as `Standalone`. And `restoreSession` doesn't rebuild the spec at all — the supervisor replays the argv the session was created with, and a session's mode never changes.

## Rendering

Plots are rendered in the kernel and travel as images. **Nothing fetches a renderer from a CDN**, so a plot works offline, in CI, and behind a firewall — which the previous vega-embed payload could not do.

`plot/` holds the whole of it:

| File | Role |
| --- | --- |
| `plot/mod.rs` | `Format`, `RenderRequest`, and `choose` — the one function that decides what a plot becomes |
| `plot/backend.rs` | `PlotBackend`: the render thread, and the GPU probe |
| `plot/sizing.rs` | `Canvas`: logical size × device pixel ratio → device pixels + dpi |
| `plot/quarto.rs` | `QUARTO_FIG_*` → a format and a canvas |

### `choose`

Three things have to agree — where the output is going, what this build and machine can produce, and what the frontend asked for — so they are reconciled in one readable function rather than spread through the formatting code:

| `SessionKind` | Result |
| --- | --- |
| `PositronConsole` | A `positron.plot` comm, with **no `execute_result`** — or a static SVG bundle when there is no GPU adapter |
| `PositronNotebook` | A static image bundle in the cell |
| `Standalone` | What `QUARTO_FIG_FORMAT` asked for, else a static image |

**SVG is the fallback wherever raster output is unavailable** — no GPU adapter, or a build without `raster-plots`. That is why `ggsql/svg` and `ggsql/pdf` are *non-optional* dependencies while the raster formats are behind a feature: the path that always works must always be compiled in, and never the one you have to opt into. It costs nothing to hold that line, since the vector writers pull in no wgpu.

### The render thread

`kernel.rs` awaits `handle_shell_message` **inline** in its `select!`, so anything blocking there stalls the heartbeat, the control channel and the SIGINT handler alike. So `PlotBackend` owns a thread, probes for an adapter once at startup (10 s ceiling), and keeps **one** renderer for the session, which handles a changing frame size internally.

The probe is eager rather than lazy because a lazy one would leave the *first* plot unable to choose a path.

### What the cold start actually costs

Measured on a release build, Apple GPU, vello-hybrid:

| | warm | cold (first process after a build) |
| --- | --- | --- |
| `RasterRenderer::new()` | 14 ms | ~185 ms |
| **first render** | **~85 ms** | **~1.35 s** |
| later renders, 3-point plot at 1200×800 | 5 ms | 5 ms |
| later renders, 50k points | ~200 ms | ~200 ms |
| a render at a size not seen before | 9–22 ms | — |

**Constructing the renderer is not the expensive part — the first render is**, and most of that is text: parley/fontique enumerating and loading system faces. Rendering an SVG first (no GPU, same text work) drops the first raster render from ~85 ms to ~20 ms, which is what identifies the cost. It is per *process*, not per renderer, so the SVG fallback pays it too.

That is why the thread renders a **throwaway 64×64 SVG frame at startup**, before anything is waiting on it: with the warm-up, the first plot of a session renders in ~14 ms rather than ~85 ms, and far better than that on a genuinely cold start. `backend::warm_up` builds its own in-memory database to do it — never the session's reader, since executing a query through that would materialise ggsql's internal views in the user's session.

### The plot comm

A console session gets one `positron.plot` comm per plot, modelled on `positron.dataExplorer` rather than the singleton connection comm — the pane shows plots as a history. `plot/comm.rs` holds the protocol (all pure, so it is tested without a Positron host); `kernel.rs` holds the transport.

Five things about it are load-bearing:

- **The comm alone creates the pane entry, so the kernel emits no `execute_result` alongside it.** An output message as well — whether an `image/png` bundle or `output_location: "plot"` — puts a second copy of the plot in the pane.
- **`comm_open` must follow `execute_input`** and be parented to the `execute_request`. Positron populates `_recentExecutions` from `execute_input`, and that is where the plot's `code` metadata comes from.
- **`render` is answered asynchronously; `get_metadata` is not.** A render goes to the thread and its reply comes back through the `select!` outcome arm, so the message loop stays free. `get_metadata` is answered from `plot_comms` because it gets Positron's default 5 s timeout where `render` gets 30 s — it must never queue behind a render.
- **`get_intrinsic_size` returns `null`.** ggsql has no figure-size syntax, so there is no intrinsic size, and `null` makes Positron use its fill policy rather than offer an "Intrinsic" option that would be a lie.
- **`show`/`update` are never sent, and are `MethodNotFound` inbound.** They mean "the backend mutated this figure, re-fetch it"; a ggsql `Spec` is immutable per execution, so re-running a cell opens a *new* comm, as the R and matplotlib backends do. An unknown method is an error rather than `result: null`, so a future Positron method fails visibly instead of being silently satisfied with garbage.

Plots are retained on the render thread and capped by `--max-plots` (default 32), evicted oldest-first with `comm_close` on iopub — which cleanly removes the plot from the pane, the right semantic for "the kernel no longer keeps that plot". Positron imposes no cap of its own, so this is where a long console session's memory is bounded.

`GGSQL_PLOT_VEGALITE=1` puts a console session back on the old Vega-Lite payload. That is a temporary escape hatch for confirming a comm problem against the previous behaviour without a rebuild, and it goes away with the Vega-Lite plot path.

### Why renders are asynchronous

Verified rather than assumed: with a large dense render in flight, a `kernel_info_request` issued immediately after it was answered in **1 ms**, while the render's own reply arrived much later. The message loop — and with it the heartbeat, the control channel and the interrupt handler — is genuinely free during a render.

That matters most for the interactive path, where dragging the Plots pane asks for a frame per event. Positron's `PositronPlotRenderQueue` already serialises renders per session and cancels superseded ones, so the kernel never sees overlapping renders for the same comm and needs no cancellation of its own.

### Pre-rendering, and the one notification we subscribe to

`manager.ts` sets `uiSubscriptions: ['did_change_plots_render_settings']`. Without it the frontend never tells the kernel how large the Plots pane is, and the kernel needs that for exactly one thing: rendering a **new** plot at the right size so `comm_open` can carry it as `pre_render`, and the pane shows it immediately instead of blank until its own render request lands. Every other render already carries its own size on the request.

Three details are easy to get wrong:

- **It is a notification, not a request.** It has no JSON-RPC `id`, and replying to one is a protocol error — so the ui branch checks `rpc_id.is_null()` before building a reply. It previously answered *everything* on that comm with `result: null`.
- **The first plot of a session carries no pre-render.** The pane has not reported yet, and rendering at a guessed size would show the wrong-sized picture and have it replaced the moment the pane asks properly. The flash is worse than the wait; the reference Python backend skips it for the same reason.
- **A pre-render without `settings` is silently discarded** (`languageRuntimePlotClient.ts` gates on `pre_render?.settings`), so it always goes through `RenderParams::to_result`, which includes them.

`plot_render_settings` has the same shape as a `render` request's params, so the same parser handles both — which is what keeps a pre-render identical to what a render would have produced.

### Base64, twice over, differently

The two transports disagree, and both are right:

| Transport | `image/svg+xml` | Binary formats |
| --- | --- | --- |
| Static display bundle | as text | base64 |
| Plot comm reply, and `pre_render` | **base64** | base64 |

The comm's convention is forced: Positron builds `data:{mime_type};base64,{data}` from the reply, so text sent as itself yields an invalid URI. The reference Python backend encodes unconditionally for the same reason. `RenderParams::encode` is the comm's one entry point so this cannot drift.

### The 4096 px raster ceiling

`vello_hybrid` builds its intermediate target with a default `max_texture_size` of **4096**, which the renderer does not override, and exceeding it fails the whole render. A pane on a large display at 2x reaches that easily, so `sizing::MAX_PX` is 4096 — a clamped request gives a slightly softer plot, where an unclamped one gives no plot at all. `ggsql::writer::MAX_RASTER_DIMENSION` is the same number, checked up front by the raster writers so the error names the limit and points at `svg`/`pdf`, which have none.

### Sizing

`Canvas::from_logical` scales pixels **and** dpi by the device pixel ratio together. Scaling the pixels alone renders the same chrome into more pixels — a blurry plot at the right size; scaling dpi alone grows the chrome instead of the resolution. This matches matplotlib's Positron backend. `metadata[mime].width/height` then carries the CSS size to display at, so a 2× render appears sharp rather than twice as big.

**Two different channels report size, and they report different things:**

| Channel | Carries | Sizes |
| --- | --- | --- |
| `execute_request`'s `positron` dict | `output_width_px`, `output_pixel_ratio` | A **cell output** slot — as wide as the cell, as tall as whatever it is given |
| plot comm `render` params, and the ui comm's `did_change_plots_render_settings` | a required `{width, height}` plus `pixel_ratio` and `format` | The **Plots pane**, which is why a plot in the pane fits it exactly |

So `RenderHints::canvas` picks a height (golden ratio, close to ggplot2's default figure) because a cell genuinely reports none — while the pane never comes through that function at all, since its size arrives per render rather than per execution.

**A static bundle carries no `output_location`.** That key routes an output to Positron's plot widget, which would show the picture in the Plots pane *as well as* in the cell — one plot arriving twice.

## Positron-specific bits

- The Vega-Lite console payload carries `"output_location": "plot"` so it routes to Positron's Plots pane. A static image bundle deliberately does not — see above.
- `data_explorer.rs` implements Positron's data-explorer comm channel (registered query results become explorable tables).
- The companion VS Code extension (`ggsql-vscode/`) discovers this binary via the `ggsql.kernelPath` setting, the active Jupyter kernelspec, or `PATH`.

## Build & install

```sh
# Dev: build the binary and register with the active env
cargo build --release --package ggsql-jupyter
./target/release/ggsql-jupyter --install

# Run a one-off install from crates.io
cargo install ggsql-jupyter
ggsql-jupyter --install

# PyPI distribution (built via maturin in CI; pyproject.toml is a wheel-builder shim)
pip install ggsql-jupyter && ggsql-jupyter --install
```

`pyproject.toml` declares `bindings = "bin"` — there is no Python module, the wheel just delivers the binary cross-platform.

## Features

```toml
default = ["all-readers"]
all-readers = ["sqlite", "odbc", "duckdb"]
```

Each feature passes through to `ggsql/<feature>`. The default install therefore supports DuckDB, SQLite, and ODBC connection strings.

## Testing

The Rust side has unit tests inline (`cargo test -p ggsql-jupyter`). The Jupyter protocol tests are Python:

```sh
cd ggsql-jupyter/tests
python -m venv .venv && source .venv/bin/activate
pip install -r requirements.txt
pytest
```

`test_compliance.py` verifies handler coverage (`execute_request`, `kernel_info_request`, `is_complete_request`, `shutdown_request`); `test_integration.py` drives a real kernel via `jupyter_client`.

## See also

- [`/CLAUDE.md`](../CLAUDE.md) — workspace overview.
- [`/ggsql-vscode/CLAUDE.md`](../ggsql-vscode/CLAUDE.md) — the VS Code / Positron extension that drives this kernel.
- [`/src/CLAUDE.md`](../src/CLAUDE.md) — the underlying `ggsql` library.
- [`/doc/get_started/tooling.qmd`](../doc/get_started/tooling.qmd) — user-facing notebook docs.
