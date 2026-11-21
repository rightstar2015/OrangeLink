//! What AI assistants changed while OrangeLink has been open, newest first,
//! each with 復原; and the permissions granted for this run only ("本次允許").

use super::*;

/// Kept while OrangeLink is open; the oldest go first.
const AI_LOG_MAX: usize = 100;

pub struct AiEntry {
    /// Local time, HH:MM:SS.
    pub at: String,
    pub tool: String,
    pub summary: String,
    /// The undo steps it made (first and last `seq`), when it can be undone.
    pub steps: Option<(u64, u64)>,
}

/// What the user allowed AI assistants until OrangeLink closes, on top of the settings.
#[derive(Default, Clone, Copy)]
pub struct McpGrant {
    pub write: bool,
    pub files: bool,
    /// When each was last asked for (`App::now`), so a retrying assistant doesn't
    /// pile up prompts.
    pub asked_write: Option<f64>,
    pub asked_files: Option<f64>,
}

/// Where the change made of undo steps `first..=last` stands in an undo stack
/// (its steps' `seq`, oldest first).
fn undo_state(stack: &[u64], steps: Option<(u64, u64)>) -> AiUndo {
    let Some((first, last)) = steps else { return AiUndo::Unavailable };
    let own = |s: &u64| (first..=last).contains(s);
    let Some(pos) = stack.iter().position(own) else { return AiUndo::Unavailable };
    match stack[pos..].iter().filter(|s| !own(s)).count() {
        0 => AiUndo::Now,
        n => AiUndo::WithLater(n),
    }
}

/// How long one prompt for a permission stands before asking again.
const ASK_EVERY: f64 = 20.0;

/// Where an entry's change stands.
pub enum AiUndo {
    /// Its steps are the newest: undone alone.
    Now,
    /// `n` later changes would be undone with it.
    WithLater(usize),
    /// Already undone, too old for the undo history, or never undoable (files).
    Unavailable,
}

impl App {
    pub(super) fn can_write(&self) -> bool {
        self.config.mcp_allow_write || self.mcp_grant.write
    }

    pub(super) fn can_use_files(&self) -> bool {
        (self.config.mcp_allow_write && self.config.mcp_allow_files) || self.mcp_grant.files
    }

    /// Offers "本次允許" for what `tool` needs, at most once per `ASK_EVERY`.
    pub(super) fn ask_grant(&mut self, files: bool, tool: &str) {
        let asked = if files { &mut self.mcp_grant.asked_files } else { &mut self.mcp_grant.asked_write };
        if asked.is_some_and(|t| self.now - t < ASK_EVERY) {
            return;
        }
        *asked = Some(self.now);
        let what = if files { "存取檔案（匯入、擷取畫面、匯出、產生字幕）" } else { "修改資源庫（標籤、評分、資料夾、片段）" };
        self.toast_with(format!("AI 想要{what}，呼叫了 {tool}"), "本次允許", Action::GrantMcp { files });
    }

    pub(super) fn grant_mcp(&mut self, files: bool) {
        self.mcp_grant.write = true;
        self.mcp_grant.files |= files;
        let what = if files { "修改資源庫與存取檔案" } else { "修改資源庫" };
        self.toast(format!("在 OrangeLink 關閉前允許 AI {what}，可在設定 → 整合撤銷；請 AI 再試一次"));
    }

    pub(crate) fn revoke_mcp_grant(&mut self) {
        self.mcp_grant = McpGrant::default();
    }

    /// Records a change an AI tool made; `first_seq` is `undo_seq` before it ran.
    pub(super) fn log_ai(&mut self, tool: &str, summary: &str, first_seq: u64) {
        // Folder and field changes become undo steps once a frame; take them now.
        self.track_structure();
        let steps = (self.edits.seq > first_seq).then_some((first_seq + 1, self.edits.seq));
        self.ai_log.push(AiEntry {
            at: chrono::Local::now().format("%H:%M:%S").to_string(),
            tool: tool.to_string(),
            summary: summary.lines().next().unwrap_or_default().chars().take(200).collect(),
            steps,
        });
        if self.ai_log.len() > AI_LOG_MAX {
            self.ai_log.remove(0);
        }
        let msg = format!("AI：{}", summary.lines().next().unwrap_or_default());
        match steps {
            Some((first, last)) => self.toast_with(msg, "復原", Action::UndoAi { first, last }),
            None => self.toast(msg),
        }
    }

    pub(crate) fn ai_undo_state(&self, steps: Option<(u64, u64)>) -> AiUndo {
        let seqs: Vec<u64> = self.edits.undo.iter().map(|s| s.seq).collect();
        undo_state(&seqs, steps)
    }

    /// Undoes an AI change. When later changes were made on top of it, asks
    /// first: they are undone too (and can be redone with Ctrl+Y).
    pub(super) fn undo_ai(&mut self, first: u64, last: u64, confirmed: bool) {
        match self.ai_undo_state(Some((first, last))) {
            AiUndo::Unavailable => self.toast("這個修改已經復原過，或已超過可以復原的範圍"),
            AiUndo::WithLater(n) if !confirmed => {
                if self.dialog.is_some() {
                    return self.toast("請先關閉目前的對話框");
                }
                self.dialog = Some(Dialog::Confirm {
                    message: format!("這個 AI 修改之後還有 {n} 個修改。要一起復原嗎？\n之後可以用 Ctrl+Y 重做。"),
                    on_yes: Confirmed::UndoAi { first, last },
                });
            }
            _ => {
                let Some(pos) = self.edits.undo.iter().position(|s| (first..=last).contains(&s.seq)) else { return };
                for _ in pos..self.edits.undo.len() {
                    self.restore_items(false);
                }
            }
        }
    }

    /// The 「AI 操作紀錄」 window.
    pub fn ai_log_window(&mut self, ctx: &egui::Context) {
        if !self.show_ai_log {
            return;
        }
        let mut open = true;
        let mut undo = None;
        egui::Window::new("AI 操作紀錄").open(&mut open).default_width(460.0).show(ctx, |ui| {
            ui.label(
                egui::RichText::new("AI 助理在這次開啟期間做的修改，最新的在最上面。標籤、評分、資料夾、片段等修改可以復原；匯入、匯出等檔案操作請直接處理檔案。")
                    .small()
                    .weak(),
            );
            if self.mcp_grant.write {
                ui.horizontal(|ui| {
                    let what = if self.mcp_grant.files { "修改資源庫與存取檔案" } else { "修改資源庫" };
                    ui.label(egui::RichText::new(format!("本次開啟期間已允許 AI {what}")).small());
                    if ui.small_button("撤銷").clicked() {
                        undo = Some(None);
                    }
                });
            }
            ui.separator();
            if self.ai_log.is_empty() {
                ui.label(egui::RichText::new("還沒有 AI 修改").weak());
            }
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for e in self.ai_log.iter().rev() {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&e.at).small().monospace().weak());
                        ui.add(egui::Label::new(egui::RichText::new(&e.summary).small()).truncate()).on_hover_text(format!("{}\n{}", e.tool, e.summary));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match self.ai_undo_state(e.steps) {
                            AiUndo::Now => {
                                if ui.small_button("復原").clicked() {
                                    undo = Some(e.steps);
                                }
                            }
                            AiUndo::WithLater(n) => {
                                if ui.small_button("復原").on_hover_text(format!("之後還有 {n} 個修改，會先詢問")).clicked() {
                                    undo = Some(e.steps);
                                }
                            }
                            AiUndo::Unavailable => {
                                let why = if e.steps.is_some() { "已復原" } else { "—" };
                                ui.label(egui::RichText::new(why).small().weak());
                            }
                        });
                    });
                }
            });
        });
        self.show_ai_log = open;
        match undo {
            Some(Some((first, last))) => self.undo_ai(first, last, false),
            Some(None) => self.revoke_mcp_grant(),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ai_change_is_undone_alone_only_when_it_is_the_newest() {
        let state = |stack: &[u64], steps| match undo_state(stack, steps) {
            AiUndo::Now => "now".to_string(),
            AiUndo::WithLater(n) => format!("with {n}"),
            AiUndo::Unavailable => "gone".to_string(),
        };
        assert_eq!(state(&[1, 2, 3], Some((3, 3))), "now");
        assert_eq!(state(&[1, 2, 3], Some((2, 3))), "now", "a tool call that made two steps");
        assert_eq!(state(&[1, 2, 3, 4, 5], Some((2, 3))), "with 2", "changes made after it");
        assert_eq!(state(&[1, 4], Some((2, 3))), "gone", "already undone");
        assert_eq!(state(&[1, 2, 3], None), "gone", "never undoable (files)");
    }

    #[test]
    fn an_ai_change_in_the_last_library_cannot_undo_a_change_in_the_next() {
        let mut app = App::with_config(&egui::Context::default(), Config::default());
        let step = || super::super::undo::UndoStep { seq: 0, time: 0.0, items: Vec::new(), meta: None };
        app.push_undo(step());
        app.log_ai("set_rating", "評分", 0);
        let steps = app.ai_log.last().and_then(|e| e.steps);
        assert!(matches!(app.ai_undo_state(steps), AiUndo::Now));

        let root = std::env::temp_dir().join(format!("orangelink-ailog-{}", crate::model::new_id()));
        Library::create(&root).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok((Library::open(&root).unwrap(), None))).unwrap();
        app.lib_loading = Some((root.clone(), rx));
        app.poll_library_loading();
        assert!(app.lib.is_some(), "switched");
        assert!(app.ai_log.is_empty(), "the log was about the other library");

        app.push_undo(step());
        assert!(matches!(app.ai_undo_state(steps), AiUndo::Unavailable), "a 復原 left on a toast from the last library");
        drop(app);
        let _ = std::fs::remove_dir_all(&root);
    }
}
