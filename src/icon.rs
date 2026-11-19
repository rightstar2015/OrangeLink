//! The OrangeLink icon, drawn in code: an orange rounded square with two
//! interlocking white rings. Also used by `examples/gen_icon.rs` to write the
//! .ico embedded in the executable.

use image::{Rgba, RgbaImage};

const SS: u32 = 4; // supersamples per axis for anti-aliasing

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Color of one sample at normalized coordinates (0..1), or None if outside the tile.
fn sample(x: f32, y: f32) -> Option<[f32; 3]> {
    // Rounded-square tile.
    let r = 0.22;
    let (cx, cy) = (x.clamp(r, 1.0 - r), y.clamp(r, 1.0 - r));
    if (x - cx).powi(2) + (y - cy).powi(2) > r * r {
        return None;
    }
    let bg = [lerp(255.0, 236.0, y), lerp(168.0, 92.0, y), lerp(66.0, 24.0, y)];
    let white = [255.0, 250.0, 244.0];

    // Two rings; the right one passes over the left at the top, under it at the bottom.
    let (radius, half_w, gap) = (0.205, 0.052, 0.03);
    let ring = |cx: f32| ((x - cx).powi(2) + (y - 0.5).powi(2)).sqrt() - radius;
    let (d1, d2) = (ring(0.385).abs(), ring(0.615).abs());
    let (in1, in2) = (d1 < half_w, d2 < half_w);
    let (halo1, halo2) = (d1 < half_w + gap, d2 < half_w + gap);
    let right_on_top = y < 0.5;
    // The strand on top is drawn whole; the other stops at its halo.
    let c = if right_on_top {
        if in2 || (in1 && !halo2) {
            white
        } else {
            bg
        }
    } else if in1 || (in2 && !halo1) {
        white
    } else {
        bg
    };
    Some(c)
}

pub fn render(size: u32) -> RgbaImage {
    let mut img = RgbaImage::new(size, size);
    let n = (SS * SS) as f32;
    for py in 0..size {
        for px in 0..size {
            let (mut acc, mut cover) = ([0.0f32; 3], 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    if let Some(c) = sample(x, y) {
                        for i in 0..3 {
                            acc[i] += c[i];
                        }
                        cover += 1.0;
                    }
                }
            }
            if cover > 0.0 {
                let a = cover / n;
                img.put_pixel(
                    px,
                    py,
                    Rgba([(acc[0] / cover) as u8, (acc[1] / cover) as u8, (acc[2] / cover) as u8, (a * 255.0).round() as u8]),
                );
            }
        }
    }
    img
}
