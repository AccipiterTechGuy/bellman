//! SVG → PNG via `resvg` + `tiny-skia` (pure Rust, headless, no GPU/display).
//!
//! **Why resvg:** pure-Rust SVG stack with tiny-skia software rasterisation —
//! no display/GPU required. Calendar SVG text must be shaped against a real
//! font database: `usvg::Options::default()` ships an **empty** fontdb, which
//! silently drops every `<text>` node. We therefore load a small, ordered set
//! of known system faces (DejaVu / Liberation) plus a system-fonts fallback,
//! and pin the default family to match the SVG (`DejaVu Sans`).

use resvg::tiny_skia::{self, Pixmap};
use resvg::usvg::{Options, Tree};
use std::path::Path;
use std::sync::OnceLock;

/// Preferred faces, in load order. Absolute paths keep selection deterministic
/// on a given machine without depending on fontconfig search order for the
/// first match of each file.
const PINNED_FONT_FILES: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Oblique.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-BoldOblique.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
    "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
    "/usr/share/fonts/truetype/liberation2/LiberationSans-Bold.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
];

/// Default family name used when SVG text does not resolve a face — must match
/// the first family listed on calendar SVG `<text>` elements.
const DEFAULT_FONT_FAMILY: &str = "DejaVu Sans";

/// Rasterise an SVG document to PNG bytes.
pub fn svg_to_png(svg: &str) -> Result<Vec<u8>, String> {
    let pixmap = svg_to_pixmap(svg)?;
    pixmap
        .encode_png()
        .map_err(|e| format!("encode png: {e}"))
}

/// Rasterise to an in-memory pixmap (used by PNG export and tests).
pub fn svg_to_pixmap(svg: &str) -> Result<Pixmap, String> {
    let opt = render_options();
    let tree = Tree::from_str(svg, &opt).map_err(|e| format!("parse svg: {e}"))?;
    let size = tree.size().to_int_size();
    let width = size.width().max(1);
    let height = size.height().max(1);

    let mut pixmap = Pixmap::new(width, height)
        .ok_or_else(|| format!("pixmap {width}x{height} alloc failed"))?;
    // White background (SVG root may be transparent).
    pixmap.fill(tiny_skia::Color::from_rgba8(255, 255, 255, 255));

    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    Ok(pixmap)
}

/// Build usvg options with a populated font database.
///
/// Font loading is cached process-wide so repeated calendar renders do not
/// re-scan the filesystem. Loading is pure file I/O — no DISPLAY required.
fn render_options() -> Options<'static> {
    static FONTDB: OnceLock<std::sync::Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();

    let fontdb = FONTDB
        .get_or_init(|| std::sync::Arc::new(build_fontdb()))
        .clone();

    let mut opt = Options::default();
    opt.resources_dir = None;
    opt.font_family = DEFAULT_FONT_FAMILY.to_owned();
    // Keep default font size sensible for unstyled text.
    opt.fontdb = fontdb;
    opt
}

fn build_fontdb() -> resvg::usvg::fontdb::Database {
    let mut db = resvg::usvg::fontdb::Database::new();
    let mut loaded_any = false;
    for path in PINNED_FONT_FILES {
        if Path::new(path).is_file() {
            if db.load_font_file(path).is_ok() {
                loaded_any = true;
            }
        }
    }
    // Always also load the system set so machines without the pinned paths
    // still get readable text (e.g. Noto-only installs). Order of first load
    // of a given family is still dominated by the pinned files above.
    db.load_system_fonts();
    if !loaded_any && db.is_empty() {
        // Last-ditch: keep going; tests will fail loudly if text is missing.
        eprintln!(
            "bellman calendar: warning: no fonts loaded for SVG→PNG; text may be missing"
        );
    }
    // Prefer DejaVu / Liberation when the SVG asks for generic sans-serif.
    db.set_serif_family("DejaVu Serif");
    db.set_sans_serif_family(DEFAULT_FONT_FAMILY);
    db.set_monospace_family("DejaVu Sans Mono");
    db
}

/// Count pixels whose RGB sum is below `max_sum` (used to prove text ink exists).
pub fn count_dark_pixels(pixmap: &Pixmap, max_sum: u32) -> usize {
    let data = pixmap.data();
    let mut n = 0usize;
    for px in data.chunks_exact(4) {
        // tiny-skia stores premultiplied RGBA; for near-black text on white
        // backgrounds alpha is high and rgb is low.
        let r = px[0] as u32;
        let g = px[1] as u32;
        let b = px[2] as u32;
        if r + g + b < max_sum {
            n += 1;
        }
    }
    n
}

/// Count pixels near a target sRGB colour within `tol` per channel (absolute).
pub fn count_pixels_near(pixmap: &Pixmap, target: (u8, u8, u8), tol: u8) -> usize {
    let (tr, tg, tb) = target;
    let data = pixmap.data();
    let mut n = 0usize;
    for px in data.chunks_exact(4) {
        let a = px[3];
        if a < 200 {
            continue; // ignore transparent / anti-aliased fringe
        }
        let r = px[0];
        let g = px[1];
        let b = px[2];
        if r.abs_diff(tr) <= tol && g.abs_diff(tg) <= tol && b.abs_diff(tb) <= tol {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_text_without_display() {
        // Prove headless: unset DISPLAY for this process.
        std::env::remove_var("DISPLAY");
        let svg = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="40">
  <rect width="200" height="40" fill="#ffffff"/>
  <text x="8" y="28" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="20" font-weight="600" fill="#111827">August 2026</text>
</svg>
"##;
        let pixmap = svg_to_pixmap(svg).expect("pixmap");
        let png = pixmap.encode_png().expect("encode");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "PNG magic");
        assert!(std::env::var_os("DISPLAY").is_none());

        // Text ink must exist — blank white image has zero dark pixels.
        let dark = count_dark_pixels(&pixmap, 200);
        assert!(
            dark > 50,
            "expected dark text pixels, got {dark} (fontdb empty?)"
        );
        // Near the calendar title colour #111827.
        let near_title = count_pixels_near(&pixmap, (0x11, 0x18, 0x27), 40);
        assert!(
            near_title > 20,
            "expected pixels near #111827, got {near_title}"
        );
    }

    #[test]
    fn calendar_svg_png_has_header_and_day_ink() {
        std::env::remove_var("DISPLAY");
        // Minimal calendar-shaped SVG matching production colours.
        let svg = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="400" height="200">
  <rect width="400" height="200" fill="#ffffff"/>
  <text x="16" y="36" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="22" font-weight="600" fill="#111827">August 2026 · UTC</text>
  <text x="40" y="70" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="12" font-weight="600" fill="#6b7280">Mon</text>
  <text x="20" y="100" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="13" font-weight="600" fill="#111827">1</text>
  <text x="26" y="120" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="11" fill="#374151">09:00 backup</text>
  <text x="20" y="140" font-family="DejaVu Sans, Liberation Sans, Arial, sans-serif" font-size="11" fill="#6b7280">+15 more</text>
</svg>
"##;
        let pixmap = svg_to_pixmap(svg).expect("pixmap");
        assert!(
            count_dark_pixels(&pixmap, 200) > 100,
            "calendar text missing from PNG"
        );
        assert!(
            count_pixels_near(&pixmap, (0x11, 0x18, 0x27), 48) > 30,
            "title/day colour missing"
        );
        assert!(
            count_pixels_near(&pixmap, (0x37, 0x41, 0x51), 48) > 10,
            "entry label colour missing"
        );
        assert!(
            count_pixels_near(&pixmap, (0x6b, 0x72, 0x80), 48) > 10,
            "muted label colour missing"
        );
        assert!(std::env::var_os("DISPLAY").is_none());
    }
}
