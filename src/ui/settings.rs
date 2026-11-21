//! The settings window: categories on the left, their settings on the right, a
//! search box over both. Server settings (MCP, the browser extension, the global
//! shortcut) apply half a second after the last change, not on closing.

use super::*;
use crate::config::SettingsPage;
use std::cell::Cell;

/// Server settings apply this long after the last change.
const APPLY_AFTER: f64 = 0.5;

/// The search box: a setting shows when its words contain what was typed. The
/// first hit in a category prints the category's name above it.
struct Filter {
    q: String,
    heading: Cell<Option<&'static str>>,
    hits: Cell<usize>,
}

impl Filter {
    fn searching(&self) -> bool {
        !self.q.is_empty()
    }

    fn show(&self, ui: &mut Ui, words: &str) -> bool {
        if self.q.is_empty() {
            return true;
        }
        let hit = words.to_lowercase().contains(&self.q);
        self.hits.set(self.hits.get() + hit as usize);
        if let Some(h) = hit.then(|| self.heading.take()).flatten() {
            ui.add_space(10.0);
            ui.label(RichText::new(h).strong().color(accent()));
        }
        hit
    }
}

/// What a page asks for that needs more than the config.
#[derive(Default)]
struct PageOut {
    ffmpeg_change: Option<Option<std::path::PathBuf>>,
    open_extension: bool,
    reset: Option<SettingsPage>,
    then: Option<Action>,
    /// Run the tools check (first look at the page, or 重新檢查).
    check_tools: bool,
}

fn group(ui: &mut Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong());
}

/// A server's state beside its port: running, the error it hit, or waiting to apply.
/// Returns true when 重試 was clicked (shown after a failed start, for example once
/// the program holding the port is closed).
fn server_state<T>(ui: &mut Ui, service: &crate::app::Service<T>, at: impl Fn(&T) -> String, pending: bool) -> bool {
    use crate::app::Service;
    let text = match service {
        _ if pending => RichText::new(t!("套用中…")).small().weak(),
        Service::Running(t) => RichText::new(t!("● 執行中 {t}", t = at(t))).small().color(good()),
        Service::Failed(e) => RichText::new(format!("⚠ {e}")).small().color(danger()),
        Service::Off => RichText::new(t!("未啟動")).small().weak(),
    };
    ui.label(text);
    !pending && service.error().is_some() && ui.small_button(t!("重試")).on_hover_text(t!("用目前的設定再啟動一次")).clicked()
}

impl App {
    /// Server settings changed a moment ago: start, stop or move the servers now.
    fn apply_pending_settings(&mut self, ctx: &egui::Context) {
        let Some(at) = self.settings_apply_at else { return };
        if self.now < at {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(at - self.now + 0.02));
            return;
        }
        self.settings_apply_at = None;
        // Saved first: `--mcp-stdio` reads the token from the config file.
        self.config.save();
        self.sync_mcp();
        self.sync_webclip();
        self.sync_hotkey();
    }

    pub fn settings_window(&mut self, ctx: &egui::Context) {
        self.apply_pending_settings(ctx);
        if !self.show_settings {
            self.chat_keywords_draft = None;
            return;
        }
        let mut open = true;
        let before = self.config.clone();
        let mut out = PageOut::default();
        let center = ctx.screen_rect().center();
        egui::Window::new(t!("設定"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([720.0, 560.0])
            .default_pos([center.x - 360.0, (center.y - 280.0).max(24.0)])
            .min_width(560.0)
            .max_width(860.0)
            .show(ctx, |ui| {
                ui.add(TextEdit::singleline(&mut self.settings_search).hint_text(t!("🔍 搜尋設定…")).desired_width(f32::INFINITY));
                ui.separator();
                let f = Filter { q: self.settings_search.trim().to_lowercase(), heading: Cell::new(None), hits: Cell::new(0) };
                let height = (ctx.screen_rect().height() - 200.0).clamp(240.0, 640.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(130.0);
                        for page in SettingsPage::ALL {
                            let r = ui.add_sized([130.0, 24.0], egui::SelectableLabel::new(!f.searching() && self.config.settings_page == page, page.label()));
                            if r.clicked() {
                                self.config.settings_page = page;
                                self.settings_search.clear();
                            }
                        }
                    });
                    ui.separator();
                    ScrollArea::vertical().id_salt("settings-page").max_height(height).auto_shrink([false, false]).show(ui, |ui| ui.vertical(|ui| {
                        ui.set_width(ui.available_width());
                        if f.searching() {
                            for page in SettingsPage::ALL {
                                f.heading.set(Some(page.label()));
                                self.settings_page(ui, page, &f, &mut out);
                            }
                            if f.hits.get() == 0 {
                                ui.label(RichText::new(t!("沒有符合的設定")).weak());
                            }
                        } else {
                            let page = self.config.settings_page;
                            ui.label(RichText::new(page.label()).heading());
                            self.settings_page(ui, page, &f, &mut out);
                            ui.add_space(16.0);
                            if ui.button(t!("還原此分類的預設值")).clicked() {
                                out.reset = Some(page);
                            }
                        }
                    }));
                });
            });

        if let Some(page) = out.reset {
            self.config.reset_page(page);
            self.ui_scale_draft = self.config.ui_scale;
            if page == SettingsPage::Highlights {
                self.chat_keywords_draft = None;
            }
            if page == SettingsPage::Tools {
                out.ffmpeg_change = Some(None);
            }
            self.toast(t!("已還原「{label}」的預設值", label = page.label()));
        }
        if let Some(a) = out.then.take() {
            self.act(a);
        }
        if out.check_tools {
            self.check_tools();
        }
        if out.open_extension {
            match crate::webclip::write_extension() {
                Ok(dir) => {
                    let _ = open::that_detached(dir);
                }
                Err(e) => self.toast(t!("無法建立擴充功能資料夾：{e}", e)),
            }
        }
        if let Some(dir) = out.ffmpeg_change {
            let wanted = dir.is_some();
            self.set_ffmpeg_dir(dir);
            if wanted && self.ffmpeg_dir.is_none() {
                self.toast(t!("該資料夾裡沒有 ffmpeg.exe"));
            }
        }
        let c = &self.config;
        if c.volume != before.volume {
            self.act(Action::VolumeChanged);
        }
        if c.theme != before.theme || c.ui_scale != before.ui_scale || c.language != before.language {
            self.apply_settings(ctx);
        }
        let servers = |c: &crate::config::Config| {
            (c.mcp_enabled, c.mcp_port, c.mcp_token.clone(), c.webclip_enabled, c.webclip_port, c.webclip_token.clone(), c.hotkey_enabled, c.hotkey.clone())
        };
        if servers(&self.config) != servers(&before) {
            self.settings_apply_at = Some(self.now + APPLY_AFTER);
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(APPLY_AFTER + 0.02));
        }
        if !open {
            self.show_settings = false;
            self.chat_keywords_draft = None;
            self.config.save();
            if self.settings_apply_at.is_some() {
                self.settings_apply_at = Some(self.now);
                self.apply_pending_settings(ctx);
            }
        }
    }

    /// The settings of one category.
    fn settings_page(&mut self, ui: &mut Ui, page: SettingsPage, f: &Filter, out: &mut PageOut) {
        let pending = self.settings_apply_at.is_some();
        let c = &mut self.config;
        match page {
            SettingsPage::General => {
                // "Language" in English too, so someone who can't read the current one finds it.
                if f.show(ui, t!("語言 language 語系 介面")) {
                    ui.horizontal(|ui| {
                        ui.label(format!("🌐 {} Language", t!("語言")));
                        let name = |l: Option<crate::i18n::Lang>| match l {
                            Some(l) => l.native_name().to_string(),
                            None => t!("跟隨 Windows（{name}）", name = crate::i18n::Lang::system().native_name()),
                        };
                        egui::ComboBox::from_id_salt("language").selected_text(name(c.language)).show_ui(ui, |ui| {
                            for l in std::iter::once(None).chain(crate::i18n::Lang::ALL.map(Some)) {
                                ui.selectable_value(&mut c.language, l, name(l));
                            }
                        });
                    });
                }
                if f.show(ui, t!("主題 深色 淺色 跟隨系統")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("主題"));
                        ui.selectable_value(&mut c.theme, ThemeSetting::Dark, t!("深色"));
                        ui.selectable_value(&mut c.theme, ThemeSetting::Light, t!("淺色"));
                        ui.selectable_value(&mut c.theme, ThemeSetting::System, t!("跟隨系統"));
                    });
                }
                if f.show(ui, t!("介面縮放 字體大小")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("介面縮放"));
                        // Applying the zoom mid-drag rescales this very window under the cursor,
                        // so the slider edits a draft that is applied on release.
                        let draft = &mut self.ui_scale_draft;
                        let r = ui.add(egui::Slider::new(draft, 0.75..=1.75).step_by(0.05).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
                        if r.drag_stopped() || (r.changed() && !r.dragged()) {
                            c.ui_scale = *draft;
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_space(60.0);
                        for preset in [0.9_f32, 1.0, 1.1, 1.25, 1.5] {
                            let selected = (c.ui_scale - preset).abs() < 0.01;
                            if ui.selectable_label(selected, format!("{:.0}%", preset * 100.0)).clicked() {
                                self.ui_scale_draft = preset;
                                c.ui_scale = preset;
                            }
                        }
                    });
                }
                if f.show(ui, t!("平滑捲動 滾輪")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("平滑捲動"));
                        ui.add(egui::Slider::new(&mut c.scroll_smoothness, 0..=100).custom_formatter(|v, _| if v == 0.0 { t!("關閉").into() } else { format!("{v:.0}") }));
                    });
                    ui.label(RichText::new(t!("滾輪捲動縮圖時滑過去的時間；0 為一格一格捲動")).small().weak());
                }
                if f.show(ui, t!("拖出視窗 premiere 檔案總管 拖曳")) {
                    ui.checkbox(&mut c.drag_out, t!("允許把縮圖拖出視窗，放到 Premiere、檔案總管等其他程式"));
                }
            }
            SettingsPage::ImportDelete => {
                if f.show(ui, t!("匯入方式 複製到資源庫 保留在原位置 每次詢問")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("匯入方式"));
                        ui.selectable_value(&mut c.import_mode, ImportMode::Ask, t!("每次詢問"));
                        ui.selectable_value(&mut c.import_mode, ImportMode::Copy, t!("複製到資源庫"));
                        ui.selectable_value(&mut c.import_mode, ImportMode::Link, t!("保留在原位置"));
                    });
                }
                if f.show(ui, t!("匯入前確認 大量 檔案 數量 大小 重複 標籤")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("一次匯入超過"));
                        ui.add(egui::DragValue::new(&mut c.import_preview_min).range(0..=10_000));
                        ui.label(t!("個檔案時，先顯示數量、大小、重複數，並可選資料夾和標籤"));
                    });
                    ui.label(RichText::new(t!("0 表示一律直接匯入。拖進資料夾時會先數裡面的檔案。")).small().weak());
                }
                if f.show(ui, t!("略過重複的檔案")) {
                    ui.checkbox(&mut c.skip_duplicates, t!("略過重複的檔案（內容相同或已連結過的路徑）"));
                }
                if f.show(ui, t!("在資料夾中匯入時 自動加入該資料夾")) {
                    ui.checkbox(&mut c.import_into_current_folder, t!("在資料夾中匯入時，自動加入該資料夾"));
                }
                if f.show(ui, t!("監看資料夾檢查間隔")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("監看資料夾檢查間隔"));
                        ui.add(egui::Slider::new(&mut c.watch_interval_min, 0.5..=60.0).logarithmic(true).suffix(t!(" 分鐘")));
                    });
                }
                if f.show(ui, t!("刪除 永久刪除 原始檔案 資源回收筒")) {
                    group(ui, t!("刪除"));
                    ui.checkbox(&mut c.delete_linked_originals, t!("永久刪除「保留在原位置」的項目時，一併刪除硬碟上的原始檔案"));
                    if c.delete_linked_originals {
                        ui.indent("recycle", |ui| {
                            ui.checkbox(&mut c.originals_to_recycle_bin, t!("移到 Windows 資源回收筒（可從回收筒還原）"));
                            if !c.originals_to_recycle_bin {
                                ui.label(RichText::new(t!("⚠ 原始檔案會直接刪除，無法還原")).small().color(danger()));
                            }
                        });
                    } else {
                        ui.label(RichText::new(t!("預設：只從資源庫移除，硬碟上的原始檔案保留")).small().weak());
                    }
                }
            }
            SettingsPage::Preview => {
                if f.show(ui, t!("音量")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("音量"));
                        ui.add(egui::Slider::new(&mut c.volume, 0.0..=1.0).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)));
                    });
                    ui.label(RichText::new(t!("預覽視窗的控制列也能調整；懸停預覽用同一個音量")).small().weak());
                }
                if cfg!(windows) && f.show(ui, t!("播放引擎 硬體解碼 Media Foundation FFmpeg")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("播放引擎"));
                        egui::ComboBox::from_id_salt("play-engine").selected_text(c.play_engine.label()).show_ui(ui, |ui| {
                            for e in crate::config::PlayEngine::ALL {
                                ui.selectable_value(&mut c.play_engine, e, e.label());
                            }
                        });
                    });
                    let note = match c.play_engine {
                        crate::config::PlayEngine::Auto => t!("用 Windows 的 Media Foundation 播放（GPU 硬體解碼，4K 和長直播檔較省 CPU、跳轉較快）；Windows 開不了的格式改用 FFmpeg"),
                        crate::config::PlayEngine::MediaFoundation => t!("只用 Media Foundation；Windows 沒有解碼器的格式會顯示錯誤"),
                        crate::config::PlayEngine::Ffmpeg => t!("只用 FFmpeg 播放（需要 FFmpeg）"),
                    };
                    ui.label(RichText::new(note).small().weak());
                    ui.label(RichText::new(t!("下一次開始播放時套用")).small().weak());
                }
                if f.show(ui, t!("雙擊 預設程式開啟")) {
                    ui.checkbox(&mut c.media_dblclick_external, t!("雙擊影片 / 音訊 / 其他檔案時，以預設程式開啟"));
                }
                if f.show(ui, t!("放大小圖 填滿視窗")) {
                    ui.checkbox(&mut c.viewer_upscale, t!("預覽時放大小圖以填滿視窗"));
                }
                if f.show(ui, t!("懸停預覽 自動播放 gif 聲音")) {
                    ui.checkbox(&mut c.hover_preview, t!("游標停在素材上時自動播放預覽（GIF 動畫、影片、音訊）"));
                    if c.hover_preview {
                        ui.indent("hover-sound", |ui| {
                            ui.checkbox(&mut c.hover_preview_sound, t!("預覽時播放聲音（音量依上方的設定）"));
                        });
                    }
                }
                if f.show(ui, t!("幻燈片間隔")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("幻燈片間隔"));
                        ui.add(egui::Slider::new(&mut c.slideshow_secs, 1.0..=30.0).suffix(t!(" 秒")));
                    });
                }
            }
            SettingsPage::Highlights => {
                if f.show(ui, t!("加權關鍵字 聊天室")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("加權關鍵字"));
                        let draft = self.chat_keywords_draft.get_or_insert_with(|| c.chat_keywords.join(t!("、")));
                        let r = ui
                            .add(TextEdit::singleline(draft).desired_width(280.0))
                            .on_hover_text(t!("聊天室訊息含這些字會加重計分，用頓號或逗號分隔"));
                        if r.changed() {
                            c.chat_keywords = split_tags(draft);
                        }
                        if r.lost_focus() {
                            *draft = c.chat_keywords.join(t!("、"));
                        }
                    });
                }
                if f.show(ui, t!("候選片段 高峰前 後 秒數 最多 數量 合併 間隔")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("候選片段保留高峰前"));
                        ui.add(egui::DragValue::new(&mut c.highlight_before).range(0.0..=120.0).suffix(t!(" 秒")));
                        ui.label(t!("後"));
                        ui.add(egui::DragValue::new(&mut c.highlight_after).range(0.0..=120.0).suffix(t!(" 秒")));
                    });
                    ui.horizontal(|ui| {
                        ui.label(t!("最多"));
                        ui.add(egui::DragValue::new(&mut c.highlight_max).range(1..=200).suffix(t!(" 個高峰")));
                        ui.label(t!("，相隔不到"));
                        ui.add(egui::DragValue::new(&mut c.highlight_merge).range(0.0..=120.0).suffix(t!(" 秒")));
                        ui.label(t!("的合併成一段")).on_hover_text(t!("前一段的結尾到下一段的開頭不到這麼久，就併成一個候選（最長 {signals} 秒）；0 不合併", signals = crate::signals::MAX_MERGED));
                    });
                    ui.label(RichText::new(t!("在預覽的「片段」分頁按「自動找精華候選」。需要和影片同名的 .live_chat.json，或先分析音量。")).small().weak());
                }
                if f.show(ui, t!("片段輸出 編碼器 nvenc x264 gpu 顯卡")) {
                    use crate::config::VideoEncoder;
                    ui.horizontal(|ui| {
                        ui.label(t!("輸出編碼器"));
                        egui::ComboBox::from_id_salt("video-encoder").selected_text(c.video_encoder.label()).show_ui(ui, |ui| {
                            for e in VideoEncoder::ALL {
                                ui.selectable_value(&mut c.video_encoder, e, e.label());
                            }
                        });
                    });
                    let note = match c.video_encoder {
                        VideoEncoder::Auto => t!("有 NVIDIA 顯卡時用 NVENC（快數倍），失敗就改用 x264"),
                        VideoEncoder::H264Nvenc | VideoEncoder::HevcNvenc => t!("需要 NVIDIA 顯卡；失敗時不會改用其他編碼器"),
                        VideoEncoder::X264 => t!("用 CPU 編碼，最相容但最慢"),
                    };
                    ui.label(RichText::new(note).small().weak());
                }
                if f.show(ui, t!("燒入字幕 字型 字級 大小 描邊 位置 字幕")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("燒入字幕的字型"));
                        ui.add(TextEdit::singleline(&mut c.burn_font).desired_width(180.0)).on_hover_text(t!("Windows 已安裝的字型名稱，例如 Microsoft JhengHei、Noto Sans TC"));
                    });
                    ui.horizontal(|ui| {
                        ui.label(t!("字級"));
                        ui.add(egui::DragValue::new(&mut c.burn_size).range(1.0..=15.0).speed(0.1).suffix(" %"));
                        ui.label(t!("離底部"));
                        ui.add(egui::DragValue::new(&mut c.burn_margin).range(0.0..=60.0).speed(0.2).suffix(" %"));
                        ui.label(t!("描邊"));
                        ui.add(egui::DragValue::new(&mut c.burn_outline).range(0.0..=12.0).speed(0.1).suffix(" px"));
                    });
                    ui.label(RichText::new(t!("字級和位置是畫面高度的百分比；直式 1920 高時 4% 約 77 px。Shorts 底部有標題和按鈕，離底部 15% 左右比較不會被擋住。在輸出預設組勾「燒入字幕」才會用到。")).small().weak());
                }
                if f.show(ui, t!("輸出預設組 片段輸出 shorts 直式 9:16 1080 響度 lufs 資料夾 音訊 字幕")) {
                    export_presets_ui(ui, &mut c.export_presets);
                }
            }
            SettingsPage::ExternalApps => {
                if f.show(ui, t!("以其他程式開啟 右鍵選單 premiere photoshop 外部程式")) {
                    ui.label(RichText::new(t!("加入後會出現在素材右鍵選單的「開啟方式」")).small().weak());
                    let mut remove_app = None;
                    for (i, app) in c.external_apps.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.small_button("✖").clicked() {
                                remove_app = Some(i);
                            }
                            ui.add(TextEdit::singleline(&mut app.name).desired_width(120.0));
                            ui.label(RichText::new(app.path.display().to_string()).small().weak());
                        });
                    }
                    if let Some(i) = remove_app {
                        c.external_apps.remove(i);
                    }
                    if ui.button(t!("＋ 新增程式（例如 Premiere、Photoshop）…")).clicked() {
                        if let Some(exe) = rfd::FileDialog::new().set_title(t!("選擇程式")).add_filter(t!("程式"), &["exe"]).pick_file() {
                            let name = exe.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                            c.external_apps.push(crate::config::ExternalApp { name, path: exe });
                        }
                    }
                }
            }
            SettingsPage::Extensions => {
                if f.show(ui, t!("擴充套件 外掛 plugin extension 精華")) {
                    self.extensions_page(ui, out);
                }
            }
            SettingsPage::Integration => {
                if f.show(ui, t!("ai 整理 mcp claude 連接埠 token")) {
                    group(ui, t!("AI 整理（MCP）"));
                    ui.checkbox(&mut c.mcp_enabled, t!("啟用本機 MCP 伺服器，讓 Claude 等 AI 助理搜尋與整理素材"));
                    if c.mcp_enabled {
                        ui.horizontal(|ui| {
                            ui.label(t!("連接埠"));
                            ui.add(egui::DragValue::new(&mut c.mcp_port).range(1024..=65535));
                            if server_state(ui, &self.mcp, |m| m.url(), pending) {
                                self.settings_apply_at = Some(self.now);
                            }
                        });
                        ui.checkbox(&mut c.mcp_allow_write, t!("允許 AI 修改資源庫（加標籤、評分、資料夾、片段；不會刪除素材或檔案）"));
                        ui.add_enabled_ui(c.mcp_allow_write, |ui| {
                            ui.checkbox(&mut c.mcp_allow_files, t!("允許 AI 存取檔案（匯入、擷取畫面、匯出片段、產生字幕）"))
                                .on_hover_text(t!("匯入的文字檔內容可以被 AI 讀回，所以每次 AI 要求匯入都會先在這裡請你確認。磁碟根目錄和使用者資料夾本身不能由 AI 匯入。"));
                        });
                        if self.mcp_grant.write {
                            ui.horizontal(|ui| {
                                let what = if self.mcp_grant.files { t!("修改資源庫與存取檔案") } else { t!("修改資源庫") };
                                ui.label(RichText::new(t!("本次開啟期間另外允許 AI {what}（關閉 OrangeLink 後失效）", what)).small());
                                if ui.small_button(t!("撤銷")).clicked() {
                                    self.mcp_grant = Default::default();
                                }
                            });
                        }
                        if ui.small_button(t!("AI 操作紀錄…")).on_hover_text(t!("AI 在這次開啟期間做的修改，可以逐筆復原")).clicked() {
                            self.show_ai_log = true;
                        }
                        ui.checkbox(&mut c.ai_import_hint, t!("匯入後在狀態列提醒，並可複製整理指令給 AI 助理"));
                        ui.label(RichText::new(t!("只接受本機連線（127.0.0.1），且每個請求都要帶 token。")).small().weak());
                        if c.mcp_token.is_empty() {
                            c.mcp_token = crate::webclip::new_token();
                        }
                        ui.horizontal(|ui| {
                            ui.label("Token");
                            ui.label(RichText::new(&c.mcp_token).small().monospace());
                            if ui.small_button(t!("重新產生")).on_hover_text(t!("舊的 token 會失效，已連接的 AI 助理需要用新的指令重新加入")).clicked() {
                                c.mcp_token = crate::webclip::new_token();
                            }
                        });
                        let url = format!("http://127.0.0.1:{}/mcp", c.mcp_port);
                        let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
                        let claude_id = egui::Id::new("orangelink-claude-path");
                        let claude = ui.ctx().data_mut(|d| {
                            d.get_temp_mut_or_insert_with(claude_id, crate::mcp::find_claude).clone()
                        });
                        let claude_code = crate::mcp::claude_add_command(claude.as_deref(), &url, &c.mcp_token);
                        let stdio_json = serde_json::to_string_pretty(&serde_json::json!({
                            "mcpServers": { "orangelink": { "command": exe, "args": ["--mcp-stdio"] } }
                        }))
                        .unwrap_or_default();
                        ui.add_space(4.0);
                        ui.label(RichText::new(t!("Claude Code（在 PowerShell 執行，所有專案都能使用）：")).small());
                        ui.add(egui::Label::new(RichText::new(&claude_code).small().monospace()).wrap());
                        if ui.small_button(t!("複製")).clicked() {
                            ui.ctx().copy_text(claude_code.clone());
                        }
                        ui.label(
                            RichText::new(t!("這行指令含有 token，會留在 PowerShell 的歷史記錄裡；請不要分享。外流時按上面的「重新產生」，再用新指令重新加入。"))
                                .small()
                                .weak(),
                        );
                        ui.label(RichText::new(t!("Claude Desktop 等只支援 stdio 的程式（加入設定檔的 mcpServers，token 會自動帶上）：")).small());
                        ui.add(egui::Label::new(RichText::new(&stdio_json).small().monospace()).wrap());
                        if ui.small_button(t!("複製")).clicked() {
                            ui.ctx().copy_text(stdio_json.clone());
                        }
                        ui.label(RichText::new(t!("使用時 OrangeLink 需要開著（可以縮到背景，但不要最小化）。")).small().weak());
                    }
                }
                if f.show(ui, t!("全域快捷鍵 叫出 搜尋框 hotkey")) {
                    group(ui, t!("全域快捷鍵"));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut c.hotkey_enabled, t!("在任何程式裡按"));
                        ui.add_enabled(c.hotkey_enabled, TextEdit::singleline(&mut c.hotkey).desired_width(140.0)).on_hover_text(t!("例如 Ctrl+Alt+Shift+O、Ctrl+Shift+F、Win+Alt+O"));
                        ui.label(t!("叫出 OrangeLink 並聚焦搜尋框"));
                    });
                    if c.hotkey_enabled {
                        let parsed = crate::hotkey::parse(&c.hotkey);
                        let failed = parsed.is_ok() && !pending && self.hotkey.error().is_some();
                        let text = match (self.hotkey.error(), parsed) {
                            (_, Err(e)) => RichText::new(format!("⚠ {e}")).small().color(danger()),
                            _ if pending => RichText::new(t!("套用中…")).small().weak(),
                            (Some(e), _) => RichText::new(format!("⚠ {e}")).small().color(danger()),
                            _ => RichText::new(t!("● 已啟用 {hotkey}", hotkey = c.hotkey)).small().color(good()),
                        };
                        ui.horizontal(|ui| {
                            ui.label(text);
                            if failed && ui.small_button(t!("重試")).on_hover_text(t!("用目前的設定再註冊一次（例如關掉佔用這組按鍵的程式之後）")).clicked() {
                                self.settings_apply_at = Some(self.now);
                            }
                        });
                    }
                    ui.label(
                        RichText::new(t!("叫出後輸入關鍵字，按 Enter 把第一個結果（或選取的素材）複製到剪貼簿並回到原本的程式，可在 Premiere 的專案面板貼上；也可以直接把縮圖拖進時間軸。按 Esc 返回。"))
                            .small()
                            .weak(),
                    );
                }
                if f.show(ui, t!("瀏覽器擴充功能 brave chrome edge 配對碼 連接埠")) {
                    group(ui, t!("瀏覽器擴充功能（Brave / Chrome / Edge）"));
                    ui.checkbox(&mut c.webclip_enabled, t!("接收瀏覽器傳來的素材（在網頁上按右鍵「加入 OrangeLink」）"));
                    if c.webclip_enabled {
                        // Made now, not when the settings are applied: the code shown
                        // (and maybe copied) right after ticking the box must work.
                        if c.webclip_token.is_empty() {
                            c.webclip_token = crate::webclip::new_token();
                        }
                        ui.horizontal(|ui| {
                            ui.label(t!("連接埠"));
                            ui.add(egui::DragValue::new(&mut c.webclip_port).range(1024..=65535));
                            if server_state(ui, &self.web, |w| format!("127.0.0.1:{}", w.port), pending) {
                                self.settings_apply_at = Some(self.now);
                            }
                        });
                        let code = crate::webclip::pairing_code(c.webclip_port, &c.webclip_token);
                        ui.horizontal(|ui| {
                            ui.label(t!("配對碼"));
                            ui.label(RichText::new(&code).small().monospace());
                            if ui.small_button(t!("複製")).clicked() {
                                ui.ctx().copy_text(code.clone());
                            }
                            if ui.small_button(t!("重新產生")).on_hover_text(t!("舊的配對碼會失效，擴充功能需要重新貼上")).clicked() {
                                c.webclip_token = crate::webclip::new_token();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(t!("單檔上限"));
                            ui.add(egui::DragValue::new(&mut c.webclip_max_gb).range(1..=256).suffix(" GB"));
                            ui.label(RichText::new(t!("超過的檔案會被拒收，不會留下暫存檔")).small().weak());
                        });
                        if let Some(web) = self.web.running() {
                            let clients = web.clients();
                            ui.label(RichText::new(t!("這次開啟後連線過的擴充功能")).small());
                            if clients.is_empty() {
                                ui.label(RichText::new(t!("還沒有（擴充功能每分鐘會連線一次更新資料夾選單）")).small().weak());
                            }
                            for cl in &clients {
                                let id = cl.origin.strip_prefix("chrome-extension://").unwrap_or(t!("沒有 Origin 的程式"));
                                let ago = cl.last_seen.elapsed().map_or(0, |d| d.as_secs());
                                let ago = if ago < 60 { t!("剛剛").to_string() } else { t!("{ago} 分鐘前", ago = ago / 60) };
                                let only = c.webclip_only_origin.as_deref() == Some(cl.origin.as_str());
                                ui.horizontal(|ui| {
                                    let text = t!("{id}　{ago}，{requests} 次", id, ago, requests = cl.requests);
                                    let label = if cl.blocked { RichText::new(t!("⛔ {text}（已拒絕）", text)).small().color(danger()) } else { RichText::new(text).small().monospace() };
                                    ui.label(label);
                                    if only {
                                        ui.label(RichText::new(t!("✔ 只允許這個")).small().color(good()));
                                    } else if !cl.origin.is_empty()
                                        && ui.small_button(t!("只允許這個")).on_hover_text(t!("其他擴充功能和程式即使有配對碼也會被拒絕")).clicked()
                                    {
                                        c.webclip_only_origin = Some(cl.origin.clone());
                                    }
                                });
                            }
                        }
                        if let Some(only) = c.webclip_only_origin.clone() {
                            ui.horizontal(|ui| {
                                let id = only.strip_prefix("chrome-extension://").unwrap_or(&only);
                                ui.label(RichText::new(t!("只接受擴充功能 {id}", id)).small());
                                if ui.small_button(t!("取消限制")).on_hover_text(t!("改回只要有配對碼就能連線（重新安裝擴充功能後 ID 會改變，需要取消）")).clicked() {
                                    c.webclip_only_origin = None;
                                }
                            });
                        }
                        if ui.button(t!("開啟擴充功能資料夾")).clicked() {
                            out.open_extension = true;
                        }
                        ui.label(
                            RichText::new(
                                t!("安裝：在瀏覽器開啟 brave://extensions（Chrome 為 chrome://extensions），打開「開發人員模式」，\
                                 按「載入未封裝項目」並選擇上面的資料夾，再點工具列的 OrangeLink 圖示貼上配對碼。"),
                            )
                            .small()
                            .weak(),
                        );
                        ui.label(RichText::new(t!("只接受本機連線，並需要配對碼。使用時 OrangeLink 需要開著。")).small().weak());
                    }
                }
            }
            SettingsPage::Tools => {
                if f.show(ui, t!("工具檢查 ffmpeg whisper yt-dlp deno")) {
                    group(ui, t!("工具檢查"));
                    if self.tools_check.result.is_none() && !self.tools_check.checking() {
                        out.check_tools = true;
                    }
                    if let Some(s) = &self.tools_check.result {
                        for (ok, line) in s.lines() {
                            let text = RichText::new(format!("{} {line}", if ok { "✔" } else { "✖" })).small().color(if ok { good() } else { danger() });
                            ui.add(egui::Label::new(text).wrap());
                        }
                    }
                    ui.horizontal(|ui| {
                        if self.tools_check.checking() {
                            ui.spinner();
                            ui.label(RichText::new(t!("檢查中…")).small().weak());
                        } else if ui.button(t!("重新檢查")).clicked() {
                            out.check_tools = true;
                        }
                    });
                }
                if f.show(ui, t!("ffmpeg 影片縮圖 長度 播放")) {
                    group(ui, t!("FFmpeg（影片 / 音訊縮圖、長度與播放）"));
                    let status = match &self.ffmpeg_dir {
                        Some(d) => RichText::new(format!("✔ {}", d.display())).small().color(good()),
                        None => RichText::new(t!("✖ 找不到 ffmpeg，影片將沒有縮圖")).small().color(danger()),
                    };
                    ui.add(egui::Label::new(status).wrap());
                    ui.horizontal(|ui| {
                        if ui.button(t!("指定 ffmpeg 資料夾…")).clicked() {
                            if let Some(dir) = rfd::FileDialog::new().set_title(t!("選擇含有 ffmpeg.exe 的資料夾")).pick_folder() {
                                out.ffmpeg_change = Some(Some(dir));
                            }
                        }
                        if c.ffmpeg_dir.is_some() && ui.button(t!("改回自動偵測")).clicked() {
                            out.ffmpeg_change = Some(None);
                        }
                        if ui.button(t!("安裝或更新…")).on_hover_text(t!("在「從網址下載」視窗用 winget 安裝")).clicked() {
                            out.then = Some(Action::OpenYtdlp);
                        }
                    });
                }
                if f.show(ui, t!("yt-dlp 網址下載 deno")) {
                    group(ui, t!("yt-dlp（從網址下載）"));
                    ui.label(RichText::new(t!("工具檢查、更新與下載選項都在下載視窗裡")).small().weak());
                    if ui.button(t!("開啟下載視窗")).clicked() {
                        out.then = Some(Action::OpenYtdlp);
                    }
                }
                if f.show(ui, t!("whisper 語音轉文字 字幕 模型 語言")) {
                    group(ui, t!("Whisper（語音轉文字）"));
                    ui.label(RichText::new(t!("模型 {whisper_model}；語言與模型下載在語音轉文字視窗裡", whisper_model = c.whisper_model)).small().weak());
                    if ui.button(t!("開啟語音轉文字視窗")).clicked() {
                        out.then = Some(Action::ShowJob(crate::app::jobs::JobKind::Whisper));
                    }
                }
            }
            SettingsPage::Advanced => {
                if f.show(ui, t!("自動更新 新版 檢查 GitHub 下載")) {
                    ui.checkbox(&mut c.auto_update, t!("啟動時檢查 GitHub 新版本並跳出更新詢問"));
                    ui.label(
                        RichText::new(t!("偵測到新版本時，會詢問是否前往 GitHub Releases 手動下載；取消勾選則僅在左下角狀態列顯示提示。"))
                            .small()
                            .weak(),
                    );
                }
                if f.show(ui, t!("收合狀態 側邊欄 右側面板 重設")) {
                    ui.horizontal(|ui| {
                        ui.label(t!("收合的區塊：{n} 個", n = c.collapsed_sections.len()));
                        if ui.add_enabled(!c.collapsed_sections.is_empty(), egui::Button::new(t!("全部展開"))).clicked() {
                            c.collapsed_sections.clear();
                        }
                    });
                }
                if f.show(ui, t!("設定檔 記錄資料夾 log")) {
                    ui.horizontal(|ui| {
                        if ui.button(t!("開啟設定與記錄資料夾")).on_hover_text(t!("config.json 和 logs 都在這裡")).clicked() {
                            if let Some(d) = crate::log::dir().and_then(|d| d.parent().map(|p| p.to_path_buf())) {
                                let _ = open::that_detached(d);
                            }
                        }
                    });
                }
                if f.show(ui, t!("版本 關於 GitHub")) {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("OrangeLink {}", env!("CARGO_PKG_VERSION"))).small().weak());
                        if ui.small_button(t!("前往 GitHub Releases")).clicked() {
                            let _ = open::that_detached(crate::update::releases_url());
                        }
                    });
                }
            }
        }
    }
}

impl App {
    /// Settings → 擴充套件: the added folders, whether each is trusted, its options.
    fn extensions_page(&mut self, ui: &mut Ui, out: &mut PageOut) {
        ui.label(
            RichText::new(t!("擴充套件是這台電腦上的程式資料夾（裡面有 orangelink-extension.json）。OrangeLink 只在你按下時執行它，結果會變成精華候選，不會直接改資源庫。"))
                .small()
                .weak(),
        );
        let t = tokens();
        for entry in &mut self.config.extensions {
            let Some(found) = self.ext.list.iter().find(|i| i.dir == entry.dir) else { continue };
            ui.add_space(8.0);
            egui::Frame::none().fill(t.bg_elevated).stroke(Stroke::new(1.0_f32, t.separator)).rounding(ROUNDING).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let folder = entry.dir.file_name().map_or_else(|| entry.dir.display().to_string(), |n| n.to_string_lossy().to_string());
                match &found.manifest {
                    Err(e) => {
                        ui.label(RichText::new(folder).strong());
                        ui.add(egui::Label::new(RichText::new(format!("⚠ {e}")).small().color(danger())).wrap());
                    }
                    Ok((m, hash)) => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&m.name).strong());
                            ui.label(RichText::new(&m.version).small().weak());
                        });
                        let trusted = entry.trusted.as_deref() == Some(hash.as_str());
                        ui.horizontal_wrapped(|ui| {
                            if trusted {
                                ui.checkbox(&mut entry.enabled, t!("啟用"));
                            } else {
                                let why = if entry.trusted.is_some() { t!("⚠ 設定檔改過了，要重新確認才能使用") } else { t!("還沒有信任，不會出現在選單") };
                                ui.label(RichText::new(why).small().color(danger()));
                                if ui.small_button(t!("檢視並信任…")).clicked() {
                                    out.then = Some(Action::TrustExtensionAsk(entry.dir.clone()));
                                }
                            }
                        });
                        for a in &m.actions {
                            ui.label(RichText::new(format!("🧩 {}", a.label)).small());
                            for o in &a.options {
                                let key = format!("{}.{}", a.id, o.id);
                                let mut on = entry.options.get(&key).copied().unwrap_or(o.default);
                                if ui.checkbox(&mut on, RichText::new(&o.label).small()).changed() {
                                    entry.options.insert(key, on);
                                }
                            }
                        }
                        ui.horizontal_wrapped(|ui| {
                            let checking = self.ext.check.as_ref().is_some_and(|c| c.dir == entry.dir);
                            if m.check.is_some() && trusted && entry.enabled && !checking && ui.small_button(t!("檢查環境")).clicked() {
                                out.then = Some(Action::CheckExtension(entry.dir.clone()));
                            }
                            if let Some((label, _)) = m.ui.as_ref().filter(|_| trusted && entry.enabled) {
                                if ui.small_button(label).clicked() {
                                    out.then = Some(Action::OpenExtensionUi(entry.dir.clone()));
                                }
                            }
                            if checking {
                                ui.spinner();
                            }
                        });
                        if let Some((_, ok, text)) = self.ext.check_result.as_ref().filter(|(d, ..)| *d == entry.dir) {
                            let head = if *ok { RichText::new(t!("✔ 檢查通過")).small().color(good()) } else { RichText::new(t!("✖ 檢查沒有通過")).small().color(danger()) };
                            ui.label(head);
                            ScrollArea::vertical().id_salt(("ext-check", &entry.dir)).max_height(120.0).show(ui, |ui| {
                                ui.add(egui::Label::new(RichText::new(text).small().monospace()).wrap());
                            });
                        }
                    }
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(entry.dir.display().to_string()).small().weak());
                    if ui.small_button(t!("移除")).on_hover_text(t!("從清單移除；資料夾裡的檔案不會刪除")).clicked() {
                        out.then = Some(Action::RemoveExtension(entry.dir.clone()));
                    }
                });
            });
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(t!("＋ 加入擴充資料夾…")).clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_title(t!("選擇含有 orangelink-extension.json 的資料夾")).pick_folder() {
                    out.then = Some(Action::AddExtension(dir));
                }
            }
            if !self.config.extensions.is_empty() && ui.button(t!("重新讀取")).on_hover_text(t!("擴充的檔案改過（例如更新了）之後按這裡")).clicked() {
                out.then = Some(Action::ReloadExtensions);
            }
        });
    }
}

/// Editing the clip export presets: name, shape, size, loudness, folder, import.
fn export_presets_ui(ui: &mut Ui, presets: &mut Vec<crate::config::ExportPreset>) {
    use crate::config::{ExportPreset, ExportShape};
    ui.label(RichText::new(t!("輸出預設組")).strong());
    ui.label(RichText::new(t!("出現在預覽「片段」分頁的「輸出」選單")).small().weak());
    let (mut remove, mut up) = (None, None);
    let n = presets.len();
    for (i, p) in presets.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut p.name).desired_width(220.0));
                if ui.add_enabled(i > 0, egui::Button::new("⏶").small()).on_hover_text(t!("往上移")).clicked() {
                    up = Some(i);
                }
                if ui.add_enabled(n > 1, egui::Button::new("✖").small()).on_hover_text(t!("刪除這個預設組")).clicked() {
                    remove = Some(i);
                }
            });
            ui.horizontal(|ui| {
                ui.label(t!("輸出"));
                egui::ComboBox::from_id_salt(("preset-shape", i)).selected_text(p.shape.label()).show_ui(ui, |ui| {
                    for s in ExportShape::ALL {
                        ui.selectable_value(&mut p.shape, s, s.label());
                    }
                });
            });
            match p.shape {
                ExportShape::Vertical => {
                    ui.checkbox(&mut p.full_hd, t!("縮放成 1080×1920")).on_hover_text(t!("不勾：保持裁切後的解析度（1080p 來源約 608×1080）"));
                }
                ExportShape::Copy => {
                    ui.label(RichText::new(t!("直接複製影音資料，幾乎瞬間完成也不損失畫質；開頭會落在最近的關鍵格，可能比入點早幾秒")).small().weak());
                }
                _ => {}
            }
            if p.shape != ExportShape::Copy {
                ui.checkbox(&mut p.loudnorm, t!("響度標準化（-14 LUFS，YouTube 參考值）")).on_hover_text(t!("讓各片段音量一致；輸出會慢一點"));
            }
            if matches!(p.shape, ExportShape::Original | ExportShape::Vertical) {
                ui.checkbox(&mut p.subtitles, t!("燒入字幕")).on_hover_text(t!("把影片的字幕（Whisper 產生的或同名 .srt）中落在片段裡的句子畫進畫面；樣式在上面「燒入字幕」調整"));
            }
            ui.horizontal(|ui| {
                ui.label(t!("存到"));
                let shown = p.folder.as_ref().map_or(t!("資源庫的 captures 資料夾").to_string(), |f| f.display().to_string());
                ui.label(RichText::new(shown).small());
                if ui.small_button(t!("選擇…")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title(t!("輸出到哪個資料夾")).pick_folder() {
                        p.folder = Some(dir);
                    }
                }
                if p.folder.is_some() && ui.small_button(t!("改回 captures")).clicked() {
                    p.folder = None;
                }
            });
            ui.checkbox(&mut p.import, t!("輸出後加入資源庫"));
        });
    }
    if let Some(i) = remove {
        presets.remove(i);
    }
    if let Some(i) = up {
        presets.swap(i - 1, i);
    }
    if ui.button(t!("＋ 新增預設組")).clicked() {
        presets.push(ExportPreset::default());
    }
}
