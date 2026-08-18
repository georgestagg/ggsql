# Command line interface

The `ggsql` command line interface lets you execute queries and validate syntax directly from the terminal. While it may not be the most ergonomic way to interact directly with ggsql, it is useful for scripting, automation, and building tools around ggsql.

## Installation

Install the ggsql CLI on your system using the standard [installation instructions](../installation.llms.md).

## Executing a query

You can execute a ggsql query from a string with `ggsql exec`:

``` bash
ggsql exec "VISUALISE species AS fill FROM ggsql:penguins DRAW bar"
```

Or run a `.gsql` file:

``` bash
ggsql run my_query.gsql
```

In both cases, output is written to `stdout` as a Vega-Lite JSON spec. You can redirect it to a file:

``` bash
ggsql run my_query.gsql > chart.vl.json
```

Such files can be rendered as images using tools that work with Vega-Lite specs, such as the [Online Vega Editor](https://vega.github.io/editor/) or [vl-convert](https://github.com/vega/vl-convert) command line tool. For example,

``` bash
vl-convert vl2png -i chart.vl.json -o chart.png
```

A standard SQL query can also be provided to ggsql. If the query returns a table, the resulting values will be written to `stdout`.

## Validating a query

If you only want to check that a query is syntactically valid without executing it, use `ggsql validate`:

``` bash
$ ggsql validate "VISUALISE x, y FROM table DRAW point"
✓ Query syntax is valid
```

## Database connections

Both `ggsql exec` and `ggsql run` accept a `--reader` flag (short `-r`) that can be used to specify a connection string to be used when executing the query. If not provided, ggsql will use an empty in-memory duckdb connection, equivalent to `--reader duckdb://memory`.

``` bash
$ ggsql exec --reader sqlite://sample/ggsql_test.sqlite \
  "SELECT * FROM test_table LIMIT 3"
col_a,  col_b, col_c
215.87, 75.11, delta
418.78, 71.75, delta
495.75, 12.55, delta

$ ggsql exec --reader odbc://DSN=ggsql-pg-test \
  "SELECT * FROM test_table LIMIT 3"
col_a,  col_b, col_c
319.34, 91.45, gamma
299.08, 49.36, epsilon
12.5,   29.48, gamma
```

## Output format

`ggsql exec` and `ggsql run` render with the writer named by `--writer` (short `-w`), defaulting to `--writer vegalite` (the Vega-Lite JSON above). A build that includes the optional `png` writer can also render straight to a PNG image with `--writer png`, which needs a GPU adapter available where it runs.

A writer is configured with `--writer-option key=value`, repeated once per setting:

``` bash
ggsql exec --writer png \
  --writer-option width=6 \
  --writer-option height=4 \
  --writer-option units=in \
  --writer-option dpi=150 \
  --output chart.png \
  "VISUALISE species AS fill FROM ggsql:penguins DRAW bar"
```

Several settings can also be collapsed into one flag, separated by `;`. With `-D` short for `--writer-option` (and `--writer-options` accepted as well), plus `-w` for `--writer` and `-o` for `--output`, the same call reads:

``` bash
ggsql exec -w png -D 'width=6;height=4;units=in;dpi=150' -o chart.png \
  "VISUALISE species AS fill FROM ggsql:penguins DRAW bar"
```

**Quote the collapsed form.** Most shells — bash, zsh, PowerShell — read `;` as a command separator, so unquoted it silently runs something else rather than failing. Single quotes, double quotes and `\;` all work. The two forms mix freely, and a repeated key takes its last value.

`;` is the only separator; `,` is not, because values contain commas — `background='rgb(255, 0, 0)'` has to survive intact.

The png writer understands these options:

| Option | Value | Default |
|----|----|----|
| `width` | Canvas width, in `units` | `1500` (px) |
| `height` | Canvas height, in `units` | `1000` (px) |
| `units` | `px`, `in`, `cm`, `mm`, or `pt` — how `width` and `height` are read | `px` |
| `dpi` | Pixels per inch. Sets the print resolution of a physical size, and how large text and other chrome are relative to the canvas | `300` |
| `background` | Any CSS color, e.g. `white`, `#faf3e0`, `rgb(0 0 0 / 50%)`, or `transparent` | `white` |

`units` applies to the `width` and `height` you supply — the defaults are pixel counts either way, so `--writer-option width=6 --writer-option units=in` gives a canvas 6 inches wide and 1000 pixels tall.

The Vega-Lite writer takes no options: its output is resolution-independent, so size, resolution and background belong to whatever renders the spec. Passing an option a writer doesn’t understand is an error rather than a setting quietly ignored.

## Documentation

The ggsql CLI has built-in documentation for ggsql syntax and usage. Run `ggsql docs` for an overview of available documentation topics, and `ggsql docs [topic]` to read about a specific topic.

``` bash
$ ggsql docs draw
DRAW is perhaps the most important clause in ggsql as it defines a layer in your 
visualisation. A layer is a single instance of a visual representation of a dataset.
[...]
```

A ggsql [skill](../../syntax/skill.llms.md), a usage guide intended for AI assistants and humans, can also be output using the `ggsql skill` command (also available as `ggsql agent-info`).

``` bash
$ ggsql skill
                          ggsql Query Writer
ggsql is a SQL extension for declarative data visualization based on
Grammar of Graphics principles.
[...]
```
