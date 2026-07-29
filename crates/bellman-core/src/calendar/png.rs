//! SVG → PNG via `resvg` + `tiny-skia` (pure Rust, headless, no GPU/display).
//!
//! **Why resvg:** it is the de-facto pure-Rust SVG stack, pulls in tiny-skia
//! for software rasterisation, needs no fontconfig/display when text uses
//! system fallbacks, and keeps calendar snapshots SSH-safe and deterministic
//! for a given SVG + font availability. We always render from the same SVG
//! string the caller would write for `--format svg`.

use resvg::tiny_skia;
use resvg::usvg::{Options, Tree};

/// Rasterise an SVG document to PNG bytes.
pub fn svg_to_png(svg: &str) -> Result<Vec<u8>, String> {
    let mut opt = Options::default();
    // Deterministic-ish: do not fetch external resources.
    opt.resources_dir = None;

    let tree = Tree::from_str(svg, &opt).map_err(|e| format!("parse svg: {e}"))?;
    let size = tree.size().to_int_size();
    let width = size.width().max(1);
    let height = size.height().max(1);

    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| format!("pixmap {width}x{height} alloc failed"))?;
    // White background (SVG root may be transparent).
    pixmap.fill(tiny_skia::Color::from_rgba8(255, 255, 255, 255));

    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    pixmap
        .encode_png()
        .map_err(|e| format!("encode png: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_minimal_svg_without_display() {
        // Prove headless: unset DISPLAY for this process.
        std::env::remove_var("DISPLAY");
        let svg = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
  <rect width="40" height="20" fill="#ffffff"/>
  <text x="4" y="14" font-family="sans-serif" font-size="12" fill="#000">hi</text>
</svg>
"##;
        let png = svg_to_png(svg).expect("png");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "PNG magic");
        assert!(png.len() > 32);
        assert!(std::env::var_os("DISPLAY").is_none());
    }
}
