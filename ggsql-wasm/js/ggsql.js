// The ggsql wasm client.
//
// The Rust side draws a plot to SVG markup and stops there. Everything
// browser-shaped — measuring a container, observing resizes, fetching fonts,
// putting markup in the page — lives here, because wasm bytes are expensive
// and JavaScript is not.

import init, {
  convert_csv,
  convert_parquet,
  GgsqlContext,
  hasFonts,
  initExtensionLoader,
  installExtension,
  registerFont,
  setGenericFamily,
} from './ggsql_wasm.js';

// One entry point for the package: everything the glue exposes, plus the
// browser-shaped helpers below.
export {
  init as default,
  convert_csv,
  convert_parquet,
  GgsqlContext,
  hasFonts,
  initExtensionLoader,
  installExtension,
  registerFont,
  setGenericFamily,
};

// The faces shipped with the package, one file per (weight, style).
//
// One file per weight and style is a rule, not an accident: the shaper selects
// within a family by weight, width and style and has no notion of CSS
// `unicode-range`, so registering several subset files that share a family name
// lets one without basic Latin win the attribute match — every tick label
// becomes tofu while the bold title still renders.
const FACES = [
  { file: 'roboto-regular.ttf', weight: 400, style: 'normal' },
  { file: 'roboto-bold.ttf', weight: 700, style: 'normal' },
  { file: 'roboto-italic.ttf', weight: 400, style: 'italic' },
  { file: 'roboto-bolditalic.ttf', weight: 700, style: 'italic' },
];

const DEFAULT_FAMILY = 'Roboto';

let fontsPromise = null;

// What each generic has been pointed at, so a drawn plot can be told which
// concrete family its theme's generic resolves to. Mirrors the font context's
// own state, which is not readable back out of it.
const genericFamilies = new Map();

// The generics a plot's theme can name. Only these get redirected at the faces
// this module registered; a theme naming a family outright asked for it.
const GENERICS = new Set([
  'sans-serif', 'serif', 'monospace', 'cursive', 'fantasy', 'system-ui',
]);

/**
 * Register the bundled faces, and tell the browser about them too.
 *
 * Both halves are needed, and for different reasons. The shaper needs the faces
 * because it measures every string to lay the plot out — without them a plot
 * has no text and the wrong margins. The *browser* needs them because the SVG
 * positions each run with one anchor plus `textLength`: if it resolves some
 * other face than the one the advances were measured from, it squeezes that
 * face into the measured box, which looks plausible and is wrong. Pointing both
 * at the same files is what keeps them agreeing.
 *
 * Registration is process-global and permanent, so this is once per page. Safe
 * to call repeatedly; the work happens once.
 */
export function registerDefaultFonts(baseUrl) {
  if (fontsPromise) return fontsPromise;
  // Resolved against the document first: `new URL(file, './fonts/')` throws,
  // since a URL base has to be absolute, and a relative path is the natural
  // thing for a caller to pass.
  const base = baseUrl
    ? new URL(baseUrl, typeof document !== 'undefined' ? document.baseURI : import.meta.url).href
    : new URL('./fonts/', import.meta.url).href;

  fontsPromise = (async () => {
    const families = new Set();
    for (const face of FACES) {
      const url = new URL(face.file, base).href;
      const response = await fetch(url);
      if (!response.ok) {
        throw new Error(`could not fetch ${url}: ${response.status}`);
      }
      const bytes = new Uint8Array(await response.arrayBuffer());
      for (const family of registerFont(bytes)) families.add(family);
      injectFontFace(face, url);
    }
    // A generic is an indirection through the font context rather than a name,
    // so registering Roboto does not on its own make `sans-serif` mean Roboto.
    const names = [...families];
    if (names.length) pointGenericAt('sans-serif', names);
    return names;
  })();

  return fontsPromise;
}

/** Point a generic at concrete families, remembering it for the SVG fixup. */
function pointGenericAt(kind, families) {
  setGenericFamily(kind, families);
  genericFamilies.set(kind, families);
}

/**
 * Register a font from a URL, and optionally make a generic mean it.
 *
 * The two steps belong together: a generic is an indirection through the font
 * context rather than a name, so fetching a face is not enough on its own —
 * a theme asking for `sans-serif` resolves to nothing until something says
 * what `sans-serif` means here, and only the file knows its own family name.
 *
 * WOFF and WOFF2 are accepted, which is what makes a font CDN's URL usable
 * directly: those are what it serves a browser.
 *
 * Registration is process-global and permanent, and must precede the first
 * draw — a plot shaped without a font has no text and the wrong layout.
 */
export async function registerFontFromUrl(url, opts = {}) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`could not fetch the font at ${url}: ${response.status}`);
  }
  const families = registerFont(new Uint8Array(await response.arrayBuffer()));
  if (opts.genericFor) pointGenericAt(opts.genericFor, families);
  return families;
}

/** Give the browser the same face the shaper just measured from. */
function injectFontFace(face, url) {
  if (typeof document === 'undefined') return;
  const id = `ggsql-font-${face.file}`;
  if (document.getElementById(id)) return;
  const style = document.createElement('style');
  style.id = id;
  style.textContent =
    `@font-face{font-family:'${DEFAULT_FAMILY}';` +
    `src:url('${url}') format('truetype');` +
    `font-weight:${face.weight};font-style:${face.style};font-display:block}`;
  document.head.appendChild(style);
}

/**
 * Point the drawn SVG at the face its advances were measured from.
 *
 * Every run is placed with one anchor plus `textLength`, so the browser fits
 * whatever it resolves into the width the shaper measured. The shaper measured
 * with the registered face; the root names only the generic the theme asked
 * for, which the browser resolves to its own default — a different face, at
 * which point `textLength` scales it horizontally to fit. That reads as
 * plausible and is wrong, and it is wrong differently on every platform.
 *
 * Naming the family on the root is enough: `font-family` inherits, and a span
 * that named its own — `code`, which has to stay monospace — keeps it. The
 * generic stays on as the fallback for the glyphs the face lacks.
 */
function nameRegisteredFamily(root) {
  if (!root || root.tagName?.toLowerCase() !== 'svg') return;
  const current = root.getAttribute('font-family');
  // Only a generic is ambiguous. A theme that named a family outright asked
  // for it, and the browser can resolve that name as well as we can.
  if (!current || !GENERICS.has(current)) return;
  const families = genericFamilies.get(current);
  if (!families?.length) return;
  const named = families.map((f) => `'${f}'`).join(', ');
  root.setAttribute('font-family', `${named}, ${current}`);
}

/**
 * The container's content box, for the first draw — before the observer has
 * reported one. `clientWidth` / `clientHeight` include padding, so it is
 * subtracted back off rather than drawn over.
 */
function contentBox(el) {
  const style = getComputedStyle(el);
  const x = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
  const y = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
  return [el.clientWidth - (x || 0), el.clientHeight - (y || 0)];
}

let nextViewId = 0;

/**
 * One plot bound to one container element.
 *
 * Redraws on resize rather than scaling: the layout is re-solved at the new
 * size, so a wider box gets more tick labels instead of stretched ones. That is
 * the whole reason a resize costs a render at all.
 */
export class PlotView {
  /**
   * @param {HTMLElement} container element to draw into; its box sets the size
   * @param {object} [opts]
   * @param {string} [opts.idPrefix] namespace for generated element ids
   * @param {number} [opts.aspect] width/height; height follows width when set
   */
  constructor(container, opts = {}) {
    this.container = container;
    // Without this the height comes from the container, which is fine when CSS
    // gives it one. Where the container is instead sized *by* its content —
    // a docs cell wrapping whatever output it holds — measuring it after
    // filling it feeds back on itself and collapses to nothing. Deriving the
    // height from the width breaks that loop and keeps the page from shifting.
    this.aspect = opts.aspect && opts.aspect > 0 ? opts.aspect : null;
    // Inline SVGs share the page's id space, so two plots on one page collide
    // on gradient and clip-path ids without this. A docs page carries several.
    this.idPrefix = opts.idPrefix || `ggsql-${nextViewId++}-`;
    this.plot = null;
    this.warnings = [];
    this._frame = null;
    this._lastSize = null;
    this._freed = false;
    this._box = null;

    // `contentRect` is the *content* box. Measuring with `clientHeight`
    // instead would include padding, so each draw would be taller than the
    // space it was given — and where the container is free to grow rather
    // than clip, that feeds straight back in and the plot climbs by the
    // padding every frame.
    this._observer = new ResizeObserver((entries) => {
      const rect = entries[entries.length - 1]?.contentRect;
      if (rect) this._box = [rect.width, rect.height];
      this._schedule();
    });
    this._observer.observe(this.container);
  }

  /**
   * Show a plot, or clear the view when given `null`.
   *
   * Takes ownership of `plot`: the previous one is freed, since a wasm object
   * is not reclaimed by the garbage collector.
   */
  setPlot(plot) {
    if (this._freed) return;
    if (this.plot && this.plot !== plot) this.plot.free();
    this.plot = plot;
    this._lastSize = null;
    // Cleared before drawing, not after: a draw that cannot happen yet —
    // a container with no size, because it is in a hidden tab — would
    // otherwise leave the last plot's warnings standing against this one.
    this.warnings = [];
    if (!plot) {
      this.container.replaceChildren();
      return;
    }
    this._renderNow();
  }

  /** Redraw at the container's current size. */
  redraw() {
    this._lastSize = null;
    this._renderNow();
  }

  _schedule() {
    if (this._freed || !this.plot) return;
    // Coalesce to one draw per frame. Unlike a canvas — where assigning
    // `width` clears the drawing buffer and a deferred draw shows the cleared
    // one — the markup already in the page stays visible until it is replaced,
    // so there is nothing to flicker and no reason to draw synchronously.
    if (this._frame !== null) return;
    this._frame = requestAnimationFrame(() => {
      this._frame = null;
      this._renderNow();
    });
  }

  _renderNow() {
    if (this._freed || !this.plot) return;
    const [boxWidth, boxHeight] = this._box || contentBox(this.container);
    const width = Math.round(boxWidth);
    const height = this.aspect
      ? Math.round(width / this.aspect)
      : Math.round(boxHeight);
    if (width < 1 || height < 1) return;
    // A hidden element reports zero and a scrollbar can settle a pixel either
    // way, so skip a size we already drew.
    if (this._lastSize && this._lastSize[0] === width && this._lastSize[1] === height) return;

    const render = this.plot.toSvg(width, height, this.idPrefix);
    try {
      this.warnings = render.warnings;
      this.container.innerHTML = render.svg;
      const root = this.container.firstElementChild;
      // An SVG is inline by default, which reserves descender space under it.
      // That is a few more pixels of content than the box being measured —
      // the same feedback the padding caused, in miniature.
      if (root) root.style.display = 'block';
      nameRegisteredFamily(root);
      this._lastSize = [width, height];
    } finally {
      render.free();
    }
  }

  /** Stop observing and release the plot. */
  free() {
    if (this._freed) return;
    this._freed = true;
    if (this._frame !== null) cancelAnimationFrame(this._frame);
    this._observer.disconnect();
    if (this.plot) this.plot.free();
    this.plot = null;
  }
}
