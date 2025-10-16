; Tree-sitter highlighting queries for VizQL

; Keywords
[
  "VISUALISE"
  "AS"
  "PLOT"
  "WITH"
  "SCALE"
  "FACET"
  "WRAP"
  "BY"
  "COORD"
  "LABELS"
  "GUIDE"
  "THEME"
] @keyword

; Geom types
[
  "point"
  "line"
  "path"
  "bar"
  "col"
  "area"
  "tile"
  "polygon"
  "ribbon"
  "histogram"
  "density"
  "smooth"
  "boxplot"
  "violin"
  "text"
  "label"
  "segment"
  "arrow"
  "hline"
  "vline"
  "abline"
  "errorbar"
] @type.builtin

; Aesthetic names
[
  "x"
  "y"
  "xmin"
  "xmax"
  "ymin"
  "ymax"
  "xend"
  "yend"
  "color"
  "colour"
  "fill"
  "alpha"
  "size"
  "shape"
  "linetype"
  "linewidth"
  "width"
  "height"
  "label"
  "family"
  "fontface"
  "hjust"
  "vjust"
  "group"
] @attribute

; String literals
(string) @string

; Numbers
(number) @number

; Booleans
(boolean) @constant.builtin

; Comments
(comment) @comment

; Identifiers (column references)
(column_reference) @variable

; Property names
(scale_property_name) @property
(coord_property_name) @property
(guide_property_name) @property
(theme_property_name) @property
(parameter_name) @property
(label_type) @property

; Operators
"=" @operator

; Punctuation
["," "(" ")" "[" "]"] @punctuation.delimiter