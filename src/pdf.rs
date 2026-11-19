//! PDF pages rendered by Windows itself (`Windows.Data.Pdf`, no extra DLL), on a
//! worker thread, one page at a time and only the pages asked for, so a 500-page
//! manual opens as fast as a flyer.
//!
//! Adapted from Prime Files (`crates/nohrs-pages/src/explorer/pdf.rs`),
//! MIT License, Copyright (c) 2025 Nohrs contributors, (c) 2026 Prime Files contributors.

use image::RgbaImage;
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{channel, Receiver, Sender},
};

pub enum Update {
    /// Pages in the document, once it is open.
    Total(u32),
    Page(u32, RgbaImage),
    Failed(String),
}

/// A document being read; dropping it stops the worker.
pub struct Job {
    ask: Sender<u32>,
    pub rx: Receiver<Update>,
}

impl Job {
    /// Opens `path`; pages are rendered `width` pixels wide as they are asked for.
    /// `wake` runs whenever an update is ready.
    pub fn open(path: PathBuf, width: u32, wake: impl Fn() + Send + 'static) -> Self {
        let (ask, asks) = channel::<u32>();
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("pdf".into())
            .spawn(move || {
                if let Err(e) = platform::serve(&path, width, &asks, &tx, &wake) {
                    let _ = tx.send(Update::Failed(e));
                    wake();
                }
            })
            .ok();
        Self { ask, rx }
    }

    pub fn ask(&self, page: u32) {
        let _ = self.ask.send(page);
    }
}

/// The first page, for a thumbnail.
pub fn first_page(path: &Path, width: u32) -> Option<RgbaImage> {
    let (ask, asks) = channel();
    let (tx, rx) = channel();
    ask.send(0).ok()?;
    drop(ask);
    platform::serve(path, width, &asks, &tx, &|| {}).ok()?;
    rx.try_iter().find_map(|u| match u {
        Update::Page(0, img) => Some(img),
        _ => None,
    })
}

#[cfg(windows)]
mod platform {
    use super::Update;
    use std::{
        path::Path,
        sync::mpsc::{Receiver, Sender},
    };
    use windows::{
        core::HSTRING,
        Data::Pdf::{PdfDocument, PdfPageRenderOptions},
        Storage::{
            StorageFile,
            Streams::{DataReader, InMemoryRandomAccessStream},
        },
        Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED},
    };

    /// Renders the pages asked for until nobody asks any more.
    pub fn serve(path: &Path, width: u32, asks: &Receiver<u32>, tx: &Sender<Update>, wake: &dyn Fn()) -> Result<(), String> {
        // WinRT needs an apartment. Threads calling this are this module's own or the
        // thumbnail workers, which are STA already: then this keeps theirs.
        // SAFETY: no pointers; a thread already in an apartment just gets an error code.
        #[allow(unsafe_code)]
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let e = |e: windows::core::Error| e.message().to_string();
        let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(full.to_string_lossy().replace('/', "\\"))).and_then(|a| a.join()).map_err(e)?;
        let doc = PdfDocument::LoadFromFileAsync(&file).and_then(|a| a.join()).map_err(|err| {
            if doc_locked(&err) {
                "PDF 有密碼保護".to_string()
            } else {
                e(err)
            }
        })?;
        let total = doc.PageCount().map_err(e)?;
        if tx.send(Update::Total(total)).is_err() {
            return Ok(());
        }
        wake();
        // Newest wish first, so paging fast shows where it stops; older wishes wait,
        // and the oldest are forgotten past a few (the app asks again for those it needs).
        let mut wishes: Vec<u32> = Vec::new();
        loop {
            if wishes.is_empty() {
                match asks.recv() {
                    Ok(i) => wishes.push(i),
                    Err(_) => break,
                }
            }
            wishes.extend(asks.try_iter());
            if wishes.len() > 8 {
                wishes.drain(..wishes.len() - 8);
            }
            let Some(index) = wishes.pop() else { continue };
            wishes.retain(|w| *w != index);
            if index >= total {
                continue;
            }
            let img = render(&doc, index, width).map_err(e)?;
            if tx.send(Update::Page(index, img)).is_err() {
                return Ok(());
            }
            wake();
        }
        Ok(())
    }

    fn doc_locked(err: &windows::core::Error) -> bool {
        // HRESULT_FROM_WIN32(ERROR_WRONG_PASSWORD)
        err.code().0 as u32 == 0x8007_052B
    }

    fn render(doc: &PdfDocument, index: u32, width: u32) -> windows::core::Result<image::RgbaImage> {
        let page = doc.GetPage(index)?;
        let options = PdfPageRenderOptions::new()?;
        options.SetDestinationWidth(width)?;
        let stream = InMemoryRandomAccessStream::new()?;
        page.RenderWithOptionsToStreamAsync(&stream, &options)?.join()?;
        let length = u32::try_from(stream.Size()?).unwrap_or(u32::MAX);
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
        reader.LoadAsync(length)?.join()?;
        let mut bytes = vec![0u8; length as usize];
        reader.ReadBytes(&mut bytes)?;
        image::load_from_memory(&bytes).map(|i| i.into_rgba8()).map_err(|_| windows::core::Error::empty())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Update;
    use std::{
        path::Path,
        sync::mpsc::{Receiver, Sender},
    };
    pub fn serve(_: &Path, _: u32, _: &Receiver<u32>, _: &Sender<Update>, _: &dyn Fn()) -> Result<(), String> {
        Err("只有 Windows 能預覽 PDF".into())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// A two-page PDF written by hand: page 1 red, page 2 blue, 200 x 100 points.
    pub fn sample_pdf(path: &Path) {
        let mut objs = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_string(),
        ];
        for (page, content, color) in [(3, 4, "1 0 0"), (5, 6, "0 0 1")] {
            objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents {content} 0 R >>"));
            let _ = page;
            let s = format!("{color} rg 0 0 200 100 re f");
            objs.push(format!("<< /Length {} >>\nstream\n{s}\nendstream", s.len()));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
        for o in offsets {
            out.extend(format!("{o:010} 00000 n \n").bytes());
        }
        out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn pages_come_when_asked_and_the_first_makes_a_thumbnail() {
        let dir = std::env::temp_dir().join(format!("orangelink-pdf-{}", crate::model::new_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pdf = dir.join("two pages.pdf");
        sample_pdf(&pdf);

        let thumb = first_page(&pdf, 256).expect("page 1");
        assert_eq!(thumb.width(), 256);
        let mid = thumb.get_pixel(128, thumb.height() / 2).0;
        assert!(mid[0] > 200 && mid[2] < 60, "red: {mid:?}");

        // Asked back to back, as the preview does for the page shown and the next:
        // both come, the newest first.
        let job = Job::open(pdf.clone(), 400, || {});
        job.ask(0);
        job.ask(1);
        let mut total = None;
        let mut pages = Vec::new();
        while pages.len() < 2 {
            match job.rx.recv_timeout(std::time::Duration::from_secs(20)).expect("an update") {
                Update::Total(n) => total = Some(n),
                Update::Page(i, img) => pages.push((i, img)),
                Update::Failed(e) => panic!("{e}"),
            }
        }
        assert_eq!(total, Some(2));
        let mut got: Vec<u32> = pages.iter().map(|p| p.0).collect();
        got.sort();
        assert_eq!(got, [0, 1]);
        let page = pages.into_iter().find(|p| p.0 == 1).unwrap();
        assert_eq!(page.1.dimensions(), (400, 200));
        let mid = page.1.get_pixel(200, 100).0;
        assert!(mid[2] > 200 && mid[0] < 60, "blue: {mid:?}");

        std::fs::write(dir.join("broken.pdf"), "not a pdf").unwrap();
        assert!(first_page(&dir.join("broken.pdf"), 64).is_none());
        drop(job);
        std::fs::remove_dir_all(&dir).ok();
    }
}
