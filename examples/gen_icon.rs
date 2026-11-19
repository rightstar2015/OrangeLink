//! Regenerates `assets/orangelink.ico` and `assets/icon.png` from `src/icon.rs`:
//! `cargo run --example gen_icon`

#[path = "../src/icon.rs"]
mod icon;

use image::codecs::ico::{IcoEncoder, IcoFrame};

fn main() {
    std::fs::create_dir_all("assets").unwrap();
    icon::render(512).save("assets/icon.png").unwrap();
    let frames: Vec<IcoFrame> = [16, 24, 32, 48, 64, 128, 256]
        .into_iter()
        .map(|s| {
            let img = icon::render(s);
            IcoFrame::as_png(img.as_raw(), s, s, image::ExtendedColorType::Rgba8).unwrap()
        })
        .collect();
    let file = std::fs::File::create("assets/orangelink.ico").unwrap();
    IcoEncoder::new(file).encode_images(&frames).unwrap();
    println!("wrote assets/orangelink.ico and assets/icon.png");
}
