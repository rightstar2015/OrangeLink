//! Editing a smart folder's conditions, including those the toolbar has no
//! control for: several kinds, length, clips, subtitles, missing files, use.

use super::*;

impl App {
    pub(super) fn smart_edit_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.smart_edit.as_ref().map(|e| e.0.clone()) else { return };
        let Some(name) = self.lib.as_ref().and_then(|l| l.smart_folder(&id)).map(|s| s.name.clone()) else {
            self.smart_edit = None;
            return;
        };
        let Some((_, f)) = &mut self.smart_edit else { return };
        let (mut open, mut save, mut cancel) = (true, false, false);
        egui::Window::new(t!("智慧資料夾條件：{name}", name))
            .id(egui::Id::new("smart-edit"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(460.0)
            .show(ctx, |ui| {
                egui::Grid::new("smart-edit-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(t!("搜尋"));
                    ui.add(egui::TextEdit::singleline(&mut f.search).hint_text(t!("可用搜尋語法，例如 #直播 -草稿")).desired_width(280.0));
                    ui.end_row();

                    ui.label(t!("類型"));
                    ui.horizontal(|ui| {
                        for k in MediaKind::ALL {
                            let mut on = f.kinds.contains(&k) || f.kind == Some(k);
                            if ui.checkbox(&mut on, k.label()).changed() {
                                // The single kind folds into the list once one is changed here.
                                if let Some(old) = f.kind.take() {
                                    if !f.kinds.contains(&old) {
                                        f.kinds.push(old);
                                    }
                                }
                                if on {
                                    f.kinds.push(k);
                                } else {
                                    f.kinds.retain(|x| *x != k);
                                }
                            }
                        }
                    });
                    ui.end_row();

                    ui.label(t!("長度"));
                    ui.horizontal(|ui| {
                        minutes(ui, t!("至少"), &mut f.min_duration);
                        minutes(ui, t!("最多"), &mut f.max_duration);
                    });
                    ui.end_row();

                    ui.label(t!("片段"));
                    ui.horizontal(|ui| {
                        either(ui, "smart-clips", &mut f.has_clips, [t!("不限"), t!("有片段"), t!("沒有片段")]);
                        ui.checkbox(&mut f.has_candidates, t!("有還沒處理的精華候選"));
                    });
                    ui.end_row();

                    ui.label(t!("字幕"));
                    either(ui, "smart-sub", &mut f.has_subtitle, [t!("不限"), t!("有字幕"), t!("沒有字幕")]).on_hover_text(t!("只看影片和音訊：Whisper 產生的、手動指定的，或資源庫裡同名的 .srt／.vtt"));
                    ui.end_row();

                    ui.label(t!("使用"));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut f.never_used, t!("從未使用"));
                        days(ui, t!("最近"), &mut f.used_days, t!("天內用過"));
                    });
                    ui.end_row();

                    ui.label(t!("加入時間"));
                    days(ui, t!("最近"), &mut f.added_days, t!("天內加入"));
                    ui.end_row();

                    ui.label(t!("評分"));
                    ui.add(egui::Slider::new(&mut f.min_rating, 0..=5).text(t!("星以上")));
                    ui.end_row();

                    ui.label(t!("其他"));
                    ui.horizontal_wrapped(|ui| {
                        ui.checkbox(&mut f.missing, t!("找不到原始檔"));
                        ui.checkbox(&mut f.untagged, t!("未標籤"));
                        ui.checkbox(&mut f.uncategorized, t!("未分類"));
                        ui.checkbox(&mut f.unrated, t!("未評分"));
                    });
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.label(egui::RichText::new(t!("標籤、顏色、副檔名、尺寸等條件保持原樣；要改這些，先在工具列篩選，再右鍵選「以目前的篩選條件更新」。")).small().weak());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    save = ui.button(t!("儲存")).clicked();
                    cancel = ui.button(t!("取消")).clicked();
                });
            });
        if save {
            match self.smart_edit.take() {
                Some((_, f)) if f.is_empty() => {
                    self.smart_edit = Some((id, f));
                    self.toast(t!("至少要有一個條件"));
                }
                Some((id, f)) => {
                    if let Some(lib) = &mut self.lib {
                        lib.update_smart_filter(&id, f);
                    }
                    self.toast(t!("已更新智慧資料夾的條件"));
                }
                None => {}
            }
        } else if !open || cancel {
            self.smart_edit = None;
        }
    }
}

/// A checkbox and, when on, a length in minutes.
fn minutes(ui: &mut egui::Ui, label: &str, v: &mut Option<f64>) {
    let mut on = v.is_some();
    ui.checkbox(&mut on, label);
    if on {
        let mut m = v.unwrap_or(30.0 * 60.0) / 60.0;
        ui.add(egui::DragValue::new(&mut m).range(0.0..=24.0 * 60.0).speed(1.0).suffix(t!(" 分")));
        *v = Some(m * 60.0);
    } else {
        *v = None;
    }
}

/// A checkbox and, when on, a number of days.
fn days(ui: &mut egui::Ui, label: &str, v: &mut Option<u32>, after: &str) {
    let mut on = v.is_some();
    ui.checkbox(&mut on, label);
    if on {
        let mut d = v.unwrap_or(7);
        ui.add(egui::DragValue::new(&mut d).range(1..=3650));
        *v = Some(d);
    }
    if !on {
        *v = None;
    }
    ui.label(after);
}

/// 不限 / yes / no.
fn either(ui: &mut egui::Ui, id: &str, v: &mut Option<bool>, labels: [&str; 3]) -> egui::Response {
    let shown = match v {
        None => labels[0],
        Some(true) => labels[1],
        Some(false) => labels[2],
    };
    egui::ComboBox::from_id_salt(id)
        .selected_text(shown)
        .show_ui(ui, |ui| {
            ui.selectable_value(v, None, labels[0]);
            ui.selectable_value(v, Some(true), labels[1]);
            ui.selectable_value(v, Some(false), labels[2]);
        })
        .response
}
