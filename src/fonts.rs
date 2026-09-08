//! Font registration.
//!
//! Every renderer-backed writer shapes text before it can lay a plot out: tick
//! labels set the margins, legend entries set their column width, a title wraps
//! against the space it has. Natively that shaping resolves against the fonts
//! the operating system enumerates and nothing here is needed.
//!
//! A browser enumerates none. `fontique` falls back to a dummy backend, so the
//! collection starts empty, `sans-serif` resolves to nothing, and a plot comes
//! out with its chrome drawn and no text at all — no error and no warning. A
//! wasm host has to hand the faces over itself, which is what this module is
//! for.
//!
//! Registration is process-global and permanent, so it is once per process, not
//! once per plot, and it must happen before the first render.

use crate::{GgsqlError, Result};

/// Register every font face in `bytes`, returning the family names they landed
/// under.
///
/// Accepts the sfnt formats — TTF, OTF, TTC and OTC — and, with the `webfonts`
/// feature, the WOFF and WOFF2 containers a font CDN serves a browser, which
/// are unwrapped to the sfnt inside before the shaper sees them. Without that
/// feature a container is refused by name rather than reaching the shaper and
/// registering nothing.
///
/// **The return value is the point.** A generic family is an indirection
/// through the font context rather than a name, so registering a face does not
/// make `sans-serif` mean it — that takes [`set_generic_family`], which takes
/// names. The only place a family's name exists is inside the file, and
/// deriving it from the filename resolves to nothing at shaping time, which
/// surfaces as a plot with no text rather than as an error.
///
/// Bytes holding no recognisable face are an error rather than an empty list,
/// since registering nothing silently is the failure this module exists to
/// prevent.
pub fn register_font(bytes: impl Into<Vec<u8>>) -> Result<Vec<String>> {
    let families = hephaestus::text::register_font_families(decode_webfont(bytes.into())?);
    if families.is_empty() {
        return Err(GgsqlError::WriterError(
            "no font faces found: the bytes are not a TTF, OTF, TTC or OTC file \
             (a WOFF or WOFF2 container has to be decoded first)"
                .to_string(),
        ));
    }
    Ok(families)
}

/// Every family available to shape with.
///
/// Empty is the answer that matters: it means the next plot rendered will have
/// no text in it.
pub fn registered_font_families() -> Vec<String> {
    hephaestus::text::registered_families()
}

/// Point a generic family at concrete families, in preference order.
///
/// `kind` is one of `serif`, `sans-serif`, `monospace`, `cursive`, `fantasy` or
/// `system-ui`, matching the CSS generics. `families` are names as
/// [`register_font`] reported them.
pub fn set_generic_family(kind: &str, families: &[String]) -> Result<()> {
    use hephaestus::text::GenericFamilyKind as K;
    let kind = match kind {
        "serif" => K::Serif,
        "sans-serif" => K::SansSerif,
        "monospace" | "mono" => K::Mono,
        "cursive" => K::Cursive,
        "fantasy" => K::Fantasy,
        "system-ui" => K::SystemUi,
        other => {
            return Err(GgsqlError::WriterError(format!(
                "unknown generic family {other:?}: expected one of serif, \
                 sans-serif, monospace, cursive, fantasy, system-ui"
            )))
        }
    };
    hephaestus::text::set_generic_family(kind, families);
    Ok(())
}

/// Unwrap a WOFF or WOFF2 container to the sfnt inside, or pass bytes through.
///
/// Taken by value so an sfnt — the common case — moves straight through to the
/// shaper rather than being copied to be handed on unchanged.
#[cfg(feature = "webfonts")]
fn decode_webfont(bytes: Vec<u8>) -> Result<Vec<u8>> {
    match bytes.get(..4) {
        Some(b"wOF2") => wuff::decompress_woff2(&bytes).map_err(|e| {
            GgsqlError::WriterError(format!("could not decode the WOFF2 font: {e:?}"))
        }),
        Some(b"wOFF") => wuff::decompress_woff1(&bytes)
            .map_err(|e| GgsqlError::WriterError(format!("could not decode the WOFF font: {e:?}"))),
        _ => Ok(bytes),
    }
}

/// Refuse a container this build cannot open, rather than letting it reach the
/// shaper: compressed bytes hold no recognisable face, so registration would
/// report nothing and the plot would come out with no text and no reason why.
#[cfg(not(feature = "webfonts"))]
fn decode_webfont(bytes: Vec<u8>) -> Result<Vec<u8>> {
    match bytes.get(..4) {
        Some(b"wOF2") | Some(b"wOFF") => Err(GgsqlError::WriterError(
            "this build cannot decode WOFF or WOFF2: use TTF, OTF, TTC or OTC, \
             or rebuild with the `webfonts` feature"
                .to_string(),
        )),
        _ => Ok(bytes),
    }
}
