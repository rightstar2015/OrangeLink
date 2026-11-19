//! PDF pages in the preview: one texture per page rendered, the page shown, and the
//! next one asked for ahead so paging forward is instant.

use super::*;

/// Width pages are rendered at: sharp on a 1440p screen without huge textures.
const PAGE_WIDTH: u32 = 1600;
/// Pages kept as textures around the one shown.
const KEEP: u32 = 6;

pub struct PdfView {
    pub id: String,
    pub total: Option<u32>,
    pub page: u32,
    pub pages: HashMap<u32, egui::TextureHandle>,
    pub error: Option<String>,
    /// Wheel movement not yet turned into a page.
    wheel: f32,
    asked: HashSet<u32>,
    job: crate::pdf::Job,
}

impl App {
    pub(super) fn open_pdf(&mut self, id: &str) {
        let Some(path) = self.item_path(id).filter(|_| self.item_ext(id).as_deref() == Some("pdf")) else { return };
        let ctx = self.egui_ctx.clone();
        let job = crate::pdf::Job::open(path, PAGE_WIDTH, move || ctx.request_repaint());
        self.pdf = Some(PdfView { id: id.to_string(), total: None, page: 0, pages: HashMap::new(), error: None, wheel: 0.0, asked: HashSet::new(), job });
        self.ask_pdf_pages();
    }

    fn item_ext(&self, id: &str) -> Option<String> {
        self.lib.as_ref()?.items.get(id).map(|it| it.ext.clone())
    }

    /// The page shown and the next one.
    fn ask_pdf_pages(&mut self) {
        let Some(pdf) = &mut self.pdf else { return };
        for p in [pdf.page, pdf.page + 1] {
            if pdf.total.is_none_or(|t| p < t) && pdf.asked.insert(p) {
                pdf.job.ask(p);
            }
        }
    }

    pub(super) fn poll_pdf(&mut self, ctx: &egui::Context) {
        let Some(pdf) = &mut self.pdf else { return };
        if self.viewer.as_ref().is_none_or(|v| v.id != pdf.id) {
            self.pdf = None;
            return;
        }
        let mut got_total = false;
        for update in pdf.job.rx.try_iter() {
            match update {
                crate::pdf::Update::Total(n) => {
                    pdf.total = Some(n);
                    got_total = true;
                }
                crate::pdf::Update::Page(i, img) => {
                    let size = [img.width() as usize, img.height() as usize];
                    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    pdf.pages.insert(i, ctx.load_texture(format!("pdf-{}-{i}", pdf.id), color, egui::TextureOptions::LINEAR));
                }
                crate::pdf::Update::Failed(e) => {
                    crate::log::error("pdf", &e);
                    pdf.error = Some(e);
                }
            }
        }
        let page = pdf.page;
        pdf.pages.retain(|i, _| i.abs_diff(page) <= KEEP);
        // Pages dropped from memory, or forgotten by the worker, can be asked for again.
        let kept: HashSet<u32> = pdf.pages.keys().copied().collect();
        pdf.asked.retain(|i| kept.contains(i) || i.abs_diff(page) <= 1);
        if got_total {
            self.ask_pdf_pages();
        }
    }

    /// Moves `delta` pages; false at either end (or for no PDF), so the caller can
    /// step to the next item instead.
    pub(super) fn pdf_step(&mut self, delta: i64) -> bool {
        let Some(pdf) = &mut self.pdf else { return false };
        let Some(total) = pdf.total.filter(|t| *t > 0) else { return false };
        let next = pdf.page as i64 + delta;
        if next < 0 || next >= total as i64 {
            return false;
        }
        pdf.page = next as u32;
        self.ask_pdf_pages();
        true
    }

    pub(super) fn pdf_go(&mut self, page: u32) {
        let Some(pdf) = &mut self.pdf else { return };
        pdf.page = page.min(pdf.total.unwrap_or(1).saturating_sub(1));
        self.ask_pdf_pages();
    }

    /// One wheel notch is one page; a touchpad's many small moves add up to one.
    pub(super) fn pdf_wheel(&mut self, delta: f32) {
        let Some(pdf) = &mut self.pdf else { return };
        pdf.wheel += delta;
        if pdf.wheel.abs() >= 40.0 {
            let step = if pdf.wheel > 0.0 { -1 } else { 1 };
            pdf.wheel = 0.0;
            self.pdf_step(step);
        }
    }
}
