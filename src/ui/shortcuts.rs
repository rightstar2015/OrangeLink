//! Every keyboard shortcut, in one table. The shortcuts window (? or F1), the
//! list in the empty inspector and the README's tables all come from here; a
//! test checks that the README matches.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    General,
    Grid,
    ViewerImage,
    ViewerMedia,
}

impl Scope {
    pub const ALL: [Scope; 4] = [Scope::General, Scope::Grid, Scope::ViewerImage, Scope::ViewerMedia];

    pub fn label(self) -> &'static str {
        match self {
            Scope::General => t!("一般"),
            Scope::Grid => t!("格狀檢視"),
            Scope::ViewerImage => t!("預覽圖片與 PDF"),
            Scope::ViewerMedia => t!("預覽影片與音訊"),
        }
    }
}

/// `keys` and `what` are translation keys (`crate::i18n::tr`).
pub struct Shortcut {
    pub scope: Scope,
    pub keys: &'static str,
    pub what: &'static str,
    /// Also listed in the inspector while nothing is selected.
    pub brief: bool,
}

const fn s(scope: Scope, keys: &'static str, what: &'static str, brief: bool) -> Shortcut {
    Shortcut { scope, keys, what, brief }
}

use Scope::*;

pub const SHORTCUTS: &[Shortcut] = &[
    s(General, "? / F1", tk!("快速鍵一覽"), true),
    s(General, "Ctrl + F", tk!("搜尋"), true),
    s(General, "Ctrl + K", tk!("前往資料夾、智慧資料夾、標籤或執行指令（有選取時也能直接加入資料夾或標籤）"), false),
    s(General, "Alt + ← / Alt + →", tk!("上一個 / 下一個檢視（資料夾、標籤、搜尋、相似結果…，含捲動位置與選取；滑鼠側鍵也可以）"), false),
    s(General, "Ctrl + Z / Ctrl + Y", tk!("復原 / 重做（標籤、評分、名稱、資料夾、智慧資料夾、標籤群組、欄位等修改，含 AI 的修改）"), false),
    s(General, "Ctrl + C", tk!("複製選取的檔案（可貼到檔案總管、Premiere）"), false),
    s(General, "Ctrl + V", tk!("匯入剪貼簿裡的檔案或截圖"), false),
    s(General, "Ctrl + Alt + Shift + O", tk!("在任何程式裡叫出 OrangeLink 並聚焦搜尋框（可在設定修改）"), false),
    s(Grid, tk!("點兩下 / Enter / 空白鍵"), tk!("預覽"), true),
    s(Grid, tk!("方向鍵"), tk!("移動選取"), true),
    s(Grid, tk!("Ctrl / Shift + 點擊"), tk!("多選 / 範圍選取"), true),
    s(Grid, tk!("在空白處拖曳"), tk!("框選（按住 Ctrl / Shift 可加選；清單檢視可從名稱以外的欄位開始拖曳）"), false),
    s(Grid, "Ctrl + A", tk!("全選"), true),
    s(Grid, tk!("0～5"), tk!("為選取的項目評分（0 = 清除）"), false),
    s(Grid, "F2", tk!("重新命名：單選時直接在名稱位置編輯（Enter 確認、Esc 取消），多選時開啟批次重新命名"), false),
    s(Grid, "Delete", tk!("移到垃圾桶（在垃圾桶中為永久刪除）"), true),
    s(Grid, tk!("Ctrl + 滾輪"), tk!("調整縮圖大小（格狀、比例檢視）"), false),
    s(Grid, tk!("拖曳縮圖到資料夾 / 標籤"), tk!("分類"), true),
    s(Grid, "Esc", tk!("取消選取"), false),
    s(ViewerImage, "← / → / ↑ / ↓", tk!("上一張 / 下一張（PDF 為換頁，翻過最後一頁換到下一個素材）"), false),
    s(ViewerImage, "PageUp / PageDown", tk!("PDF 換頁"), false),
    s(ViewerImage, "Ctrl + ← / →", tk!("上一個 / 下一個素材"), false),
    s(ViewerImage, tk!("滾輪"), tk!("縮放（PDF 為換頁，Ctrl + 滾輪縮放）"), false),
    s(ViewerImage, tk!("點兩下"), tk!("在符合視窗與 100% 之間切換"), false),
    s(ViewerImage, tk!("0～5"), tk!("為目前的素材評分"), false),
    s(ViewerImage, tk!("Esc / Enter / 空白鍵"), tk!("關閉預覽"), false),
    s(ViewerMedia, tk!("空白鍵 / K"), tk!("播放或暫停 / 暫停並回到正常速度"), false),
    s(ViewerMedia, "L", tk!("播放；播放中再按加速到 2×、4×（Media Foundation）"), false),
    s(ViewerMedia, "← / →", tk!("倒退 / 前進 5 秒"), false),
    s(ViewerMedia, "Shift + ← / →", tk!("倒退 / 前進 1 秒"), false),
    s(ViewerMedia, ", / .", tk!("上一格 / 下一格（會先暫停）"), false),
    s(ViewerMedia, "J", tk!("倒播，再按加快（Media Foundation；FFmpeg 播放引擎是倒退 5 秒）"), false),
    s(ViewerMedia, "Home / End", tk!("跳到開頭 / 結尾"), false),
    s(ViewerMedia, "I / O", tk!("設定片段入點 / 出點"), false),
    s(ViewerMedia, "Enter", tk!("有入點或出點時加入片段"), false),
    s(ViewerMedia, "V", tk!("切換 9:16 裁切框（橫式影片）"), false),
    s(ViewerMedia, tk!("Ctrl + ← / →、PageUp / PageDown"), tk!("上一個 / 下一個素材"), false),
    s(ViewerMedia, tk!("↑ / ↓、[ / ]"), tk!("上一個 / 下一個片段（選取並播放）"), false),
    s(ViewerMedia, "Y / N", tk!("保留 / 刪除選取的片段，再跳到下一個（可復原）"), false),
    s(ViewerMedia, "Delete", tk!("片段分頁有選取片段時，刪除這些片段（可復原）"), false),
    s(ViewerMedia, tk!("0～5"), tk!("為目前的素材評分"), false),
    s(ViewerMedia, "Esc", tk!("關閉預覽"), false),
];

/// The README's tables in `lang`, one per scope.
#[cfg(test)]
pub fn markdown(lang: crate::i18n::Lang) -> String {
    use crate::i18n::tr_in;
    let mut out = String::new();
    for scope in Scope::ALL {
        let label = match scope {
            General => tr_in(lang, tk!("一般")),
            Grid => tr_in(lang, tk!("格狀檢視")),
            ViewerImage => tr_in(lang, tk!("預覽圖片與 PDF")),
            ViewerMedia => tr_in(lang, tk!("預覽影片與音訊")),
        };
        let colon = tr_in(lang, tk!("："));
        out += &format!("{label}{colon}\n\n| {} | {} |\n| --- | --- |\n", tr_in(lang, tk!("按鍵")), tr_in(lang, tk!("動作")));
        for k in SHORTCUTS.iter().filter(|k| k.scope == scope) {
            out += &format!("| {} | {} |\n", tr_in(lang, k.keys), tr_in(lang, k.what));
        }
        out += "\n";
    }
    out
}

/// Two columns: the keys, and what they do.
fn table(ui: &mut Ui, id: &str, rows: impl Iterator<Item = &'static Shortcut>) {
    let t = tokens();
    egui::Grid::new(id).num_columns(2).spacing([16.0, 5.0]).show(ui, |ui| {
        for k in rows {
            egui::Frame::none()
                .fill(t.kbd_bg)
                .stroke(Stroke::new(1.0_f32, t.separator))
                .rounding(3.0)
                .inner_margin(egui::Margin::symmetric(5.0, 1.0))
                .show(ui, |ui| {
                    ui.label(RichText::new(crate::i18n::tr(k.keys)).small().monospace());
                });
            ui.add(egui::Label::new(RichText::new(crate::i18n::tr(k.what)).small().weak()).wrap());
            ui.end_row();
        }
    });
}

/// What the search box understands, for the shortcuts window and its tooltip.
/// Translation keys: the examples are ordinary words in each language, and the
/// syntax itself (`ext:`, `type:影片`…) works in all of them.
pub const SEARCH_SYNTAX: [(&str, &str); 9] = [
    (tk!("貓 狗"), tk!("兩個字都要有（名稱、標籤、註解、網址、字幕、欄位、片段名稱）")),
    (tk!("-草稿"), tk!("排除含「草稿」的")),
    (tk!("\"開場 動畫\""), tk!("完整片語（也可用「」）")),
    (tk!("貓 OR 狗"), tk!("任一個符合")),
    (tk!("#直播"), tk!("標籤完全相同")),
    (tk!("ext:mp4　type:影片"), tk!("格式、類型（圖片／影片／音訊／其他）")),
    (tk!("★>=3　rating:3"), tk!("評分三星以上；rating=3 只要三星")),
    (tk!("dur>10m　dur<30s"), tk!("長度（h、m、s，或 1:30）")),
    (tk!("w>=1920　h<1080　授權:內部"), tk!("寬、高（像素）、自訂欄位")),
];

/// The search syntax as a two-column table.
pub fn search_syntax_table(ui: &mut Ui) {
    egui::Grid::new("search-syntax").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        for (example, what) in SEARCH_SYNTAX {
            ui.label(RichText::new(crate::i18n::tr(example)).small().monospace());
            ui.add(egui::Label::new(RichText::new(crate::i18n::tr(what)).small().weak()).wrap());
            ui.end_row();
        }
    });
}

impl App {
    /// The floating list of shortcuts, by where they apply.
    pub fn shortcuts_window(&mut self, ctx: &egui::Context) {
        if !self.show_shortcuts {
            return;
        }
        let mut open = true;
        let center = ctx.screen_rect().center();
        egui::Window::new(t!("快速鍵"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([620.0, 520.0])
            .default_pos([center.x - 310.0, (center.y - 260.0).max(24.0)])
            .show(ctx, |ui| {
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for scope in Scope::ALL {
                        ui.add_space(6.0);
                        ui.label(RichText::new(scope.label()).strong().color(accent()));
                        table(ui, scope.label(), SHORTCUTS.iter().filter(|k| k.scope == scope));
                    }
                    ui.add_space(6.0);
                    ui.label(RichText::new(t!("搜尋語法")).strong().color(accent()));
                    search_syntax_table(ui);
                    ui.add_space(8.0);
                    ui.label(RichText::new(t!("按 ? 或 F1 開關這個視窗，Esc 關閉")).small().weak());
                });
            });
        if !open {
            self.show_shortcuts = false;
        }
    }

    /// The few shortcuts worth knowing first, for the empty inspector.
    pub(super) fn brief_shortcuts(&self, ui: &mut Ui) {
        table(ui, "brief-keys", SHORTCUTS.iter().filter(|k| k.brief));
        if ui.link(RichText::new(t!("全部快速鍵（? 或 F1）")).small()).clicked() {
            self.act(Action::ShowShortcuts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_key_is_listed_twice_in_one_place() {
        for scope in Scope::ALL {
            let keys: Vec<_> = SHORTCUTS.iter().filter(|k| k.scope == scope).map(|k| k.keys).collect();
            let mut unique = keys.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(keys.len(), unique.len(), "a key repeats under {}", scope.label());
        }
    }

    #[test]
    fn the_readme_lists_the_same_shortcuts() {
        use crate::i18n::Lang;
        for (lang, readme) in [
            (Lang::ZhTw, include_str!("../../README.md")),
            (Lang::ZhCn, include_str!("../../docs/README.zh-CN.md")),
            (Lang::En, include_str!("../../docs/README.en.md")),
            (Lang::Ja, include_str!("../../docs/README.ja.md")),
            (Lang::Ko, include_str!("../../docs/README.ko.md")),
        ] {
            let readme = readme.replace("\r\n", "\n");
            assert!(readme.contains(&markdown(lang)), "{lang:?} 的 README 快速鍵表和 src/ui/shortcuts.rs 不一致；用 `cargo test print_shortcuts -- --ignored --nocapture` 產生新的表格");
        }
    }

    #[test]
    #[ignore]
    fn print_shortcuts() {
        for lang in crate::i18n::Lang::ALL {
            print!("== {lang:?}\n{}", markdown(lang));
        }
    }
}
