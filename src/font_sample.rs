//! A font file's own sample text, drawn without installing it: its thumbnail
//! and the specimen the viewer shows.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::{Rgba, RgbaImage};
use std::path::Path;

/// Font files drawn here; others (.woff, .fon) keep Windows' thumbnail.
pub const FONT_EXTS: &[&str] = &["ttf", "otf", "ttc"];

pub fn is_font(path: &Path) -> bool {
    FONT_EXTS.contains(&crate::media::ext_of(path).as_str())
}

/// Sample lines, best first: each is used when the font has its characters.
const THUMB_LINES: &[&str] = &["永東國", "Aa Bb", "123"];
const SPECIMEN_LINES: &[&str] = &[
    "天地玄黃 宇宙洪荒 日月盈昃",
    "あいうえお アイウエオ",
    "The quick brown fox jumps over the lazy dog",
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
    "abcdefghijklmnopqrstuvwxyz 0123456789",
];
const SPECIMEN_SIZES: &[f32] = &[64.0, 40.0, 26.0, 18.0];

const INK: Rgba<u8> = Rgba([30, 30, 34, 255]);
const PAPER: Rgba<u8> = Rgba([250, 249, 246, 255]);

fn parse(bytes: &[u8]) -> Option<FontRef<'_>> {
    FontRef::try_from_slice_and_index(bytes, 0).ok()
}

/// Whether the font draws (nearly) every non-space character of `line`.
fn covers(font: &FontRef, line: &str) -> bool {
    let chars: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    let have = chars.iter().filter(|c| font.glyph_id(**c).0 != 0).count();
    !chars.is_empty() && have * 10 >= chars.len() * 9
}

/// Draws `line` with its baseline at `y`, from `x`; stops at `max_x`.
fn draw_line(img: &mut RgbaImage, font: &FontRef, px: f32, x: f32, y: f32, max_x: f32, line: &str) {
    let scaled = font.as_scaled(PxScale::from(px));
    let mut caret = x;
    let mut last = None;
    for c in line.chars() {
        let id = scaled.glyph_id(c);
        if let Some(prev) = last {
            caret += scaled.kern(prev, id);
        }
        let advance = scaled.h_advance(id);
        if caret + advance > max_x {
            break;
        }
        let glyph = id.with_scale_and_position(px, ab_glyph::point(caret, y));
        if let Some(outline) = font.outline_glyph(glyph) {
            let b = outline.px_bounds();
            outline.draw(|gx, gy, cover| {
                let (px_x, px_y) = (b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64);
                if px_x < 0 || px_y < 0 || px_x >= img.width() as i64 || px_y >= img.height() as i64 {
                    return;
                }
                let p = img.get_pixel_mut(px_x as u32, px_y as u32);
                let a = cover.clamp(0.0, 1.0);
                for i in 0..3 {
                    p.0[i] = (p.0[i] as f32 * (1.0 - a) + INK.0[i] as f32 * a).round() as u8;
                }
            });
        }
        caret += advance;
        last = Some(id);
    }
}

/// Lines of the font's own sample text, one per `(text, size)`, on paper.
fn render(font: &FontRef, width: u32, lines: &[(&str, f32)], pad: f32) -> Option<RgbaImage> {
    if lines.is_empty() {
        return None;
    }
    let heights: Vec<f32> = lines.iter().map(|(_, px)| {
        let s = font.as_scaled(PxScale::from(*px));
        (s.ascent() - s.descent() + s.line_gap()).max(*px)
    }).collect();
    let height = (heights.iter().sum::<f32>() + pad * 2.0).ceil().clamp(1.0, 4096.0) as u32;
    let mut img = RgbaImage::from_pixel(width, height, PAPER);
    let mut top = pad;
    for ((text, px), h) in lines.iter().zip(&heights) {
        let ascent = font.as_scaled(PxScale::from(*px)).ascent();
        draw_line(&mut img, font, *px, pad, top + ascent, width as f32 - pad, text);
        top += h;
    }
    Some(img)
}

/// The thumbnail: the first sample line the font covers, as large as fits,
/// with the next one smaller under it.
pub fn thumbnail(bytes: &[u8], side: u32) -> Option<RgbaImage> {
    let font = parse(bytes)?;
    let mut lines = THUMB_LINES.iter().filter(|l| covers(&font, l));
    let first = lines.next()?;
    let mut rows = vec![(*first, side as f32 * 0.28)];
    if let Some(second) = lines.next() {
        rows.push((*second, side as f32 * 0.16));
    }
    render(&font, side, &rows, side as f32 * 0.08)
}

/// The viewer's specimen: every sample line the font covers, at a few sizes.
pub fn specimen(bytes: &[u8], width: u32) -> Option<RgbaImage> {
    let font = parse(bytes)?;
    let covered: Vec<&str> = SPECIMEN_LINES.iter().copied().filter(|l| covers(&font, l)).collect();
    let first = *covered.first()?;
    let mut rows: Vec<(&str, f32)> = SPECIMEN_SIZES.iter().map(|px| (first, *px)).collect();
    rows.extend(covered.iter().skip(1).map(|l| (*l, 24.0)));
    render(&font, width, &rows, 32.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A system font to draw with: Windows' Arial, or a Linux one.
    fn some_font() -> Option<Vec<u8>> {
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        [format!(r"{windir}\Fonts\arial.ttf"), "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into()]
            .iter()
            .find_map(|p| std::fs::read(p).ok())
    }

    /// Fonts showed Windows' generic file icon, or nothing, in the grid.
    #[test]
    fn a_font_draws_its_own_sample_text() {
        let Some(bytes) = some_font() else {
            eprintln!("SKIPPED a_font_draws_its_own_sample_text: no system font");
            return;
        };
        let thumb = thumbnail(&bytes, 360).expect("a thumbnail");
        assert_eq!(thumb.width(), 360);
        let inked = thumb.pixels().filter(|p| p.0[0] < 128).count();
        assert!(inked > 500, "the sample text is drawn: {inked} dark pixels");
        let page = specimen(&bytes, 1200).expect("a specimen");
        assert!(page.height() > 200 && page.height() <= 4096);
    }

    #[test]
    fn a_file_that_is_not_a_font_draws_nothing() {
        assert!(thumbnail(b"not a font", 360).is_none());
        assert!(specimen(&[], 1200).is_none());
        assert!(is_font(Path::new("a/B.TTF")) && is_font(Path::new("x.otf")) && !is_font(Path::new("x.woff")));
    }
}
