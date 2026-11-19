use crate::model::PaletteColor;
use eframe::egui::ColorImage;
use image::{imageops::FilterType, DynamicImage};
use std::{collections::HashMap, path::Path};

pub const SUPPORTED_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jfif", "gif", "webp", "bmp", "tif", "tiff", "ico",
];
pub const THUMB_SIDE: u32 = 360;
/// Full-size previews are downscaled to this so they fit any GPU texture limit.
pub const MAX_VIEW_SIDE: u32 = 4096;

/// FNV-1a; stable across builds, good enough for duplicate detection.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Content fingerprint for duplicate detection. Small files are hashed whole;
/// big ones (videos) only by size + first and last MiB, so imports stay fast.
pub fn quick_hash(path: &Path) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 1 << 20;
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    if size <= 4 * CHUNK {
        let mut buf = Vec::with_capacity(size as usize);
        f.read_to_end(&mut buf)?;
        return Ok(format!("{:016x}", fnv1a(&buf)));
    }
    let mut buf = vec![0u8; (2 * CHUNK) as usize];
    f.read_exact(&mut buf[..CHUNK as usize])?;
    f.seek(SeekFrom::End(-(CHUNK as i64)))?;
    f.read_exact(&mut buf[CHUNK as usize..])?;
    buf.extend_from_slice(&size.to_le_bytes());
    Ok(format!("p{:016x}", fnv1a(&buf)))
}

/// A `quick_hash` taken from only the start and end of a large file: two files
/// with the same one may still differ in the middle.
pub fn is_partial_hash(hash: &str) -> bool {
    hash.starts_with('p')
}

/// Whether two files hold exactly the same bytes, compared 1 MiB at a time.
pub fn same_contents(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    fn fill(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match f.read(&mut buf[n..])? {
                0 => break,
                k => n += k,
            }
        }
        Ok(n)
    }
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    if fa.metadata()?.len() != fb.metadata()?.len() {
        return Ok(false);
    }
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let (n, m) = (fill(&mut fa, &mut ba)?, fill(&mut fb, &mut bb)?);
        if n != m || ba[..n] != bb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Decodes by sniffing content, so files with a wrong extension still load.
pub fn decode_file(path: &Path) -> anyhow::Result<DynamicImage> {
    let bytes = std::fs::read(path)?;
    Ok(image::load_from_memory(&bytes)?)
}

/// For animated GIFs, the frame with the most detail (luminance spread), since
/// many start on a blank or white frame. Other formats: None.
pub fn representative_gif_frame(path: &Path) -> Option<DynamicImage> {
    use image::AnimationDecoder;
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif")) {
        return None;
    }
    let file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let decoder = image::codecs::gif::GifDecoder::new(file).ok()?;
    let mut best: Option<(f32, DynamicImage)> = None;
    for frame in decoder.into_frames().take(120) {
        let Ok(frame) = frame else { break };
        let img = DynamicImage::ImageRgba8(frame.into_buffer());
        let small = img.thumbnail(48, 48).to_luma_alpha8();
        let lum: Vec<f32> = small.pixels().map(|p| if p.0[1] < 128 { 255.0 } else { p.0[0] as f32 }).collect();
        let mean = lum.iter().sum::<f32>() / lum.len().max(1) as f32;
        let spread = lum.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / lum.len().max(1) as f32;
        if best.as_ref().is_none_or(|(s, _)| spread > *s) {
            best = Some((spread, img));
        }
    }
    best.map(|(_, img)| img)
}

/// 64-bit difference hash of an image file: similar pictures differ in few bits.
pub fn dhash(path: &Path) -> Option<u64> {
    let small = decode_file(path).ok()?.resize_exact(9, 8, FilterType::Triangle).to_luma8();
    let mut h = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            h = (h << 1) | (small.get_pixel(x, y).0[0] < small.get_pixel(x + 1, y).0[0]) as u64;
        }
    }
    Some(h)
}

pub fn make_thumbnail(img: &DynamicImage) -> DynamicImage {
    if img.width() <= THUMB_SIDE && img.height() <= THUMB_SIDE {
        img.clone()
    } else {
        img.thumbnail(THUMB_SIDE, THUMB_SIDE)
    }
}

/// Opaque thumbnails are stored as JPEG (several times smaller); ones with
/// transparency stay PNG. The file name stays the same: decoding sniffs content.
pub fn save_thumbnail(img: &DynamicImage, path: &Path) -> anyhow::Result<()> {
    let thumb = make_thumbnail(img);
    let rgba = thumb.to_rgba8();
    if rgba.pixels().all(|p| p.0[3] == 255) {
        let file = std::io::BufWriter::new(std::fs::File::create(path)?);
        let rgb = DynamicImage::ImageRgba8(rgba).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(file, 85).encode_image(&rgb)?;
    } else {
        rgba.save_with_format(path, image::ImageFormat::Png)?;
    }
    Ok(())
}

pub fn fit_for_view(img: DynamicImage) -> DynamicImage {
    if img.width().max(img.height()) > MAX_VIEW_SIDE {
        img.resize(MAX_VIEW_SIDE, MAX_VIEW_SIDE, FilterType::Triangle)
    } else {
        img
    }
}

pub fn to_color_image(img: &DynamicImage) -> ColorImage {
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

/// Perceptual-ish RGB distance ("redmean"), roughly 0..765.
pub fn color_distance(a: [u8; 3], b: [u8; 3]) -> f32 {
    let rm = (a[0] as f32 + b[0] as f32) / 2.0;
    let dr = a[0] as f32 - b[0] as f32;
    let dg = a[1] as f32 - b[1] as f32;
    let db = a[2] as f32 - b[2] as f32;
    (((512.0 + rm) * dr * dr) / 256.0 + 4.0 * dg * dg + ((767.0 - rm) * db * db) / 256.0).sqrt()
}

/// Dominant colors: bucket pixels of a small copy into a 16x16x16 grid, then merge
/// buckets that are visually close.
pub fn extract_palette(img: &DynamicImage) -> Vec<PaletteColor> {
    let small = img.thumbnail(64, 64).to_rgba8();
    let mut buckets: HashMap<u16, [u64; 4]> = HashMap::new();
    let mut total = 0u64;
    for p in small.pixels() {
        let [r, g, b, a] = p.0;
        if a < 128 {
            continue;
        }
        let key = (((r >> 4) as u16) << 8) | (((g >> 4) as u16) << 4) | ((b >> 4) as u16);
        let e = buckets.entry(key).or_insert([0; 4]);
        e[0] += r as u64;
        e[1] += g as u64;
        e[2] += b as u64;
        e[3] += 1;
        total += 1;
    }
    if total == 0 {
        return Vec::new();
    }

    let mut sorted: Vec<[u64; 4]> = buckets.into_values().collect();
    sorted.sort_by(|a, b| b[3].cmp(&a[3]));

    let mut merged: Vec<([u8; 3], u64)> = Vec::new();
    for s in sorted {
        let c = [
            (s[0] / s[3]) as u8,
            (s[1] / s[3]) as u8,
            (s[2] / s[3]) as u8,
        ];
        if let Some(near) = merged.iter_mut().find(|(m, _)| color_distance(*m, c) < 80.0) {
            near.1 += s[3];
        } else if merged.len() < 6 {
            merged.push((c, s[3]));
        }
    }
    merged.sort_by_key(|m| std::cmp::Reverse(m.1));
    merged
        .into_iter()
        .map(|(rgb, n)| PaletteColor { rgb, ratio: n as f32 / total as f32 })
        .filter(|p| p.ratio >= 0.01)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{codecs::gif::GifEncoder, Delay, Frame, Rgba, RgbaImage};

    #[test]
    fn gif_thumbnail_skips_blank_first_frame() {
        let path = std::env::temp_dir().join(format!("orangelink-{}.gif", crate::model::new_id()));
        {
            let mut enc = GifEncoder::new(std::fs::File::create(&path).unwrap());
            let blank = RgbaImage::from_pixel(32, 32, Rgba([255, 255, 255, 255]));
            let mut busy = RgbaImage::from_pixel(32, 32, Rgba([255, 255, 255, 255]));
            for (x, y, p) in busy.enumerate_pixels_mut() {
                if (x / 4 + y / 4) % 2 == 0 {
                    *p = Rgba([20, 20, 200, 255]);
                }
            }
            let d = Delay::from_numer_denom_ms(100, 1);
            enc.encode_frames([Frame::from_parts(blank, 0, 0, d), Frame::from_parts(busy, 0, 0, d)]).unwrap();
        }
        let frame = representative_gif_frame(&path).unwrap().to_rgba8();
        assert!(frame.pixels().any(|p| p.0[2] > 150 && p.0[0] < 100), "picked the frame with content");
        let _ = std::fs::remove_file(&path);
    }
}
