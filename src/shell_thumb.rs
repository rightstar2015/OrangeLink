//! Thumbnails and icons from the Windows Shell (`IShellItemImageFactory`), the source
//! Explorer uses: PSD, PDF, HEIC, RAW, fonts and the like whenever a thumbnail handler
//! is installed, read from the system thumbnail cache when it has them.
//!
//! Adapted from Prime Files (`crates/nohrs-pages/src/explorer/thumbnails.rs`),
//! MIT License, Copyright (c) 2025 Nohrs contributors, (c) 2026 Prime Files contributors.

use image::RgbaImage;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Only what the system cache already holds; fails at once otherwise.
    Cached,
    /// Has the handler read the file and make one: slow.
    Generate,
    /// The file type's icon.
    Icon,
}

/// A picture of at most `size` pixels on its longer side. Icons come with
/// premultiplied alpha; thumbnails are opaque.
#[cfg(windows)]
pub fn thumbnail(path: &Path, size: u32, mode: Mode) -> Option<RgbaImage> {
    platform::init_thread();
    platform::extract(path, size, mode)
}

#[cfg(not(windows))]
pub fn thumbnail(_path: &Path, _size: u32, _mode: Mode) -> Option<RgbaImage> {
    None
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod platform {
    //! `unsafe` stays in here: COM and GDI have no safe bindings.

    use super::Mode;
    use image::RgbaImage;
    use std::{cell::Cell, path::Path};
    use windows::{
        core::HSTRING,
        Win32::{
            Foundation::SIZE,
            Graphics::Gdi::{
                CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
                HGDIOBJ,
            },
            System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED},
            UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY, SIIGBF_INCACHEONLY, SIIGBF_RESIZETOFIT, SIIGBF_THUMBNAILONLY},
        },
    };

    thread_local! {
        static COM_READY: Cell<bool> = const { Cell::new(false) };
    }

    /// Some thumbnail handlers are apartment-threaded, so each thread is its own STA, as
    /// Explorer's are. Worker threads live for the whole run and are never uninitialized.
    pub fn init_thread() {
        COM_READY.with(|ready| {
            if !ready.get() {
                // SAFETY: no pointers; once per thread, which then stays in its apartment.
                let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
                ready.set(true);
            }
        });
    }

    pub fn extract(path: &Path, size: u32, mode: Mode) -> Option<RgbaImage> {
        let edge = i32::try_from(size).ok()?;
        // Without THUMBNAILONLY the Shell answers with the type's icon, as a success,
        // whenever it has no picture, and INCACHEONLY has none for files Explorer never
        // showed. The icon is asked for on its own instead.
        let flags = match mode {
            Mode::Icon => SIIGBF_ICONONLY,
            Mode::Cached => SIIGBF_RESIZETOFIT | SIIGBF_INCACHEONLY | SIIGBF_THUMBNAILONLY,
            Mode::Generate => SIIGBF_RESIZETOFIT | SIIGBF_THUMBNAILONLY,
        };
        // SAFETY: COM is set up on this thread (`init_thread`); the path is an owned
        // HSTRING that outlives the call, and the factory is released when dropped.
        let bitmap = unsafe {
            // The Shell's parser takes `\` only; paths joined in code can mix in `/`.
            let path = path.to_string_lossy().replace('/', "\\");
            let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
            factory.GetImage(SIZE { cx: edge, cy: edge }, flags).ok()?
        };
        let image = read_bitmap(bitmap);
        // SAFETY: the bitmap is ours (GetImage hands it over) and nothing uses it after.
        let _ = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
        image
    }

    /// The pixels of a 32-bit bitmap as RGBA, top row first. The bitmap stays the
    /// caller's to free.
    fn read_bitmap(bitmap: HBITMAP) -> Option<RgbaImage> {
        // SAFETY: GetObjectW writes at most size_of::<BITMAP>() into `info`; GetDIBits
        // writes `height` rows of `width` 32-bit pixels, exactly the buffer's size,
        // as the header asks. The memory DC is deleted on every path that made it.
        let (width, height, mut px) = unsafe {
            let mut info = BITMAP::default();
            let filled = GetObjectW(HGDIOBJ(bitmap.0), std::mem::size_of::<BITMAP>() as i32, Some((&mut info as *mut BITMAP).cast()));
            if filled == 0 || info.bmWidth <= 0 || info.bmHeight <= 0 {
                return None;
            }
            let (width, height) = (info.bmWidth, info.bmHeight);
            let mut header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    // Negative height asks for the rows top-down.
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buffer = vec![0u8; width as usize * height as usize * 4];
            let device = CreateCompatibleDC(None);
            let lines = GetDIBits(device, bitmap, 0, height as u32, Some(buffer.as_mut_ptr().cast()), &mut header, DIB_RGB_COLORS);
            let _ = DeleteDC(device);
            if lines != height {
                return None;
            }
            (width as u32, height as u32, buffer)
        };
        // Photo and video thumbnails come back with their alpha byte never written;
        // read as written they would be invisible, so a bitmap with no alpha is opaque.
        let no_alpha = px.as_chunks::<4>().0.iter().all(|p| p[3] == 0);
        for p in px.as_chunks_mut::<4>().0 {
            p.swap(0, 2); // BGRA -> RGBA
            if no_alpha {
                p[3] = 0xFF;
            }
        }
        RgbaImage::from_raw(width, height, px)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn shell_gives_thumbnails_and_icons() {
        let dir = std::env::temp_dir().join(format!("orangelink-shell-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("red.png");
        image::RgbImage::from_pixel(400, 200, image::Rgb([255, 0, 0])).save(&png).unwrap();
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, "hello").unwrap();

        let thumb = thumbnail(&png, 128, Mode::Generate).expect("Windows makes PNG thumbnails");
        assert!(thumb.width().max(thumb.height()) <= 128 && thumb.width() > thumb.height(), "{:?}", thumb.dimensions());
        let mid = thumb.get_pixel(thumb.width() / 2, thumb.height() / 2).0;
        assert!(mid[0] > 200 && mid[1] < 50 && mid[3] == 255, "red, opaque, in RGBA order: {mid:?}");

        assert!(thumbnail(&txt, 64, Mode::Generate).is_none(), "plain text has no picture, not its icon");
        assert!(thumbnail(&txt, 64, Mode::Icon).is_some(), "but it has an icon");
        assert!(thumbnail(&dir.join("missing.psd"), 64, Mode::Icon).is_none());
        let forward = std::path::PathBuf::from(txt.to_string_lossy().replace('\\', "/"));
        assert!(thumbnail(&forward, 64, Mode::Icon).is_some(), "forward slashes work too");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Prints what the Shell gives for the files in OL_SHELL (a folder), to see which
/// thumbnail handlers this machine has.
#[cfg(all(test, windows))]
mod probe {
    #[test]
    #[ignore]
    fn shell_answers() {
        for e in std::fs::read_dir(std::env::var("OL_SHELL").unwrap()).unwrap().flatten() {
            let p = e.path();
            let dims = |m| super::thumbnail(&p, 256, m).map(|i| i.dimensions());
            println!("{}: cached {:?}, generate {:?}, icon {:?}", p.display(), dims(super::Mode::Cached), dims(super::Mode::Generate), dims(super::Mode::Icon));
        }
    }
}
