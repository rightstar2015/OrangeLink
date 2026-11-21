//! File lists on the Windows clipboard (CF_HDROP), so Ctrl+C / Ctrl+V work with
//! Explorer, Premiere and other apps. Images go through `arboard`.

use std::path::{Path, PathBuf};

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    pub const CF_HDROP: u32 = 15;
    pub const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "user32")]
    extern "system" {
        pub fn OpenClipboard(owner: *mut c_void) -> i32;
        pub fn CloseClipboard() -> i32;
        pub fn EmptyClipboard() -> i32;
        pub fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
        pub fn GetClipboardData(format: u32) -> *mut c_void;
        pub fn IsClipboardFormatAvailable(format: u32) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        pub fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        pub fn GlobalLock(mem: *mut c_void) -> *mut c_void;
        pub fn GlobalUnlock(mem: *mut c_void) -> i32;
        pub fn GlobalFree(mem: *mut c_void) -> *mut c_void;
    }
    #[link(name = "shell32")]
    extern "system" {
        pub fn DragQueryFileW(drop: *mut c_void, index: u32, buf: *mut u16, len: u32) -> u32;
    }
}

/// Puts the files on the clipboard as if copied in Explorer.
#[cfg(windows)]
pub fn copy_files(paths: &[PathBuf]) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use win::*;
    if paths.is_empty() {
        return false;
    }
    // DROPFILES header (20 bytes) followed by a double-NUL-terminated UTF-16 list.
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.as_os_str().encode_wide());
        list.push(0);
    }
    list.push(0);
    let header: [u32; 5] = [20, 0, 0, 0, 1]; // pFiles, pt.x, pt.y, fNC, fWide
    let bytes = 20 + list.len() * 2;
    // SAFETY: the block is allocated `bytes` long and both copies stay inside it
    // (20 header bytes, then the list); it is written only while locked, and
    // freed here unless the clipboard took it. The clipboard is closed on every
    // path that opened it.
    unsafe {
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if mem.is_null() {
            return false;
        }
        let ptr = GlobalLock(mem) as *mut u8;
        if ptr.is_null() {
            GlobalFree(mem);
            return false;
        }
        std::ptr::copy_nonoverlapping(header.as_ptr() as *const u8, ptr, 20);
        std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, ptr.add(20), list.len() * 2);
        GlobalUnlock(mem);
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            GlobalFree(mem);
            return false;
        }
        EmptyClipboard();
        let ok = !SetClipboardData(CF_HDROP, mem).is_null();
        CloseClipboard();
        if !ok {
            GlobalFree(mem);
        }
        ok
    }
}

/// Files currently on the clipboard (copied in Explorer etc.).
#[cfg(windows)]
pub fn read_files() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use win::*;
    let mut out = Vec::new();
    // SAFETY: the drop handle is the clipboard's and is only read while the
    // clipboard is open; each name is read into a buffer one longer than the
    // length Windows reported for it, which is the size passed.
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP) == 0 || OpenClipboard(std::ptr::null_mut()) == 0 {
            return out;
        }
        let drop = GetClipboardData(CF_HDROP);
        if !drop.is_null() {
            let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
            for i in 0..count {
                let len = DragQueryFileW(drop, i, std::ptr::null_mut(), 0);
                let mut buf = vec![0u16; len as usize + 1];
                DragQueryFileW(drop, i, buf.as_mut_ptr(), len + 1);
                buf.truncate(len as usize);
                out.push(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
            }
        }
        CloseClipboard();
    }
    out
}

#[cfg(not(windows))]
pub fn copy_files(_paths: &[PathBuf]) -> bool {
    false
}

#[cfg(not(windows))]
pub fn read_files() -> Vec<PathBuf> {
    Vec::new()
}

/// Saves an image on the clipboard (e.g. a screenshot) as a PNG in `dir`.
pub fn save_clipboard_image(dir: &Path) -> Option<PathBuf> {
    let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
    let buf = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())?;
    std::fs::create_dir_all(dir).ok()?;
    save_png_new(dir, &t!("貼上_{time}", time = chrono::Local::now().format("%Y%m%d_%H%M%S")), &buf)
}

/// Writes `img` as `<stem>.png` in `dir`, or `<stem> (2).png` and so on when that
/// is taken (two pastes in one second): a file there is never replaced.
fn save_png_new(dir: &Path, stem: &str, img: &image::RgbaImage) -> Option<PathBuf> {
    for n in 1..1000 {
        let path = dir.join(if n == 1 { format!("{stem}.png") } else { format!("{stem} ({n}).png") });
        let file = match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        };
        let written = img.write_to(&mut std::io::BufWriter::new(file), image::ImageFormat::Png);
        if written.is_err() {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        return Some(path);
    }
    None
}

#[cfg(test)]
mod paste_tests {
    use super::*;

    #[test]
    fn two_pastes_in_one_second_keep_both_pictures() {
        let dir = std::env::temp_dir().join(format!("orangelink-paste-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let red = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let blue = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 0, 255, 255]));
        let a = save_png_new(&dir, "貼上_20261121_120000", &red).unwrap();
        let b = save_png_new(&dir, "貼上_20261121_120000", &blue).unwrap();
        assert_ne!(a, b);
        assert_eq!(image::open(&a).unwrap().to_rgba8(), red, "the first paste was replaced");
        assert_eq!(image::open(&b).unwrap().to_rgba8(), blue);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
