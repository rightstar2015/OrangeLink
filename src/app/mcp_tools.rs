//! MCP tool definitions and their implementations on the UI thread.

use super::{App, Criteria, UpdateState, View};
use crate::{
    library::Library,
    mcp::ToolOutput,
    model::{Item, MediaKind, SavedFilter},
};
use base64::Engine;
use serde_json::{json, Value};



/// A drive's root, the user's own folder or a folder above it: too much to hand
/// an AI import.
fn too_broad(p: &std::path::Path, home: Option<&std::path::Path>) -> bool {
    let norm = |p: &std::path::Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    let mine = norm(p);
    p.parent().is_none() || mine.is_empty() || home.is_some_and(|h| {
        let h = norm(h);
        h == mine || h.starts_with(&format!("{mine}\\")) || h.starts_with(&format!("{mine}/"))
    })
}

/// The paths an AI asked to import, as the real folders they name: `..`, the
/// `\?\` form, other spellings and links are resolved before the size check
/// (and the user sees these resolved paths). Paths that don't exist are left out.
fn resolve_import_paths(asked: Vec<String>, home: Option<&std::path::Path>) -> Result<Vec<std::path::PathBuf>, String> {
    let paths: Vec<std::path::PathBuf> = asked.iter().filter_map(|p| crate::importer::dunce_abs(std::path::Path::new(p)).ok()).collect();
    if paths.is_empty() {
        return Err("沒有存在的路徑".into());
    }
    let home = home.map(|h| crate::importer::dunce_abs(h).unwrap_or_else(|_| h.to_path_buf()));
    if let Some(p) = paths.iter().find(|p| too_broad(p, home.as_deref())) {
        return Err(format!("{} 範圍太大，不能由 AI 匯入；請指定裡面的資料夾，或請使用者在 OrangeLink 裡匯入", p.display()));
    }
    Ok(paths)
}

fn ids_schema() -> Value {
    json!({ "type": "array", "items": { "type": "string" }, "description": "項目 id（由 search_items 取得）" })
}

/// What a tool may touch, which decides the setting it needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    /// Searches and looks; always allowed.
    Read,
    /// Changes the library's records: needs "允許 AI 修改資源庫".
    Write,
    /// Reaches files outside the records (import, frames, clips, subtitles):
    /// needs "允許 AI 存取檔案" as well.
    Files,
}

/// Every MCP tool with what it may touch: the one list that `tools/list`, the
/// permission check and the tests all read, so they can't disagree.
fn registry() -> Vec<(Access, Value)> {
    use Access::*;
    let kind = json!({ "type": "string", "enum": ["image", "video", "audio", "other"] });
    vec![
        (Read, json!({ "name": "get_status",
              "description": "OrangeLink 目前的狀態：資源庫（open／loading／none）、正在執行的背景工作（匯入、輸出片段、語音轉字幕、整場處理、下載…，含進度與剩餘秒數）、預覽中與選取的項目、是否允許修改。排入背景工作的工具會立即回傳，用這個查看是否完成：jobs 裡沒有該類工作就是做完了。沒有開啟資源庫時也能使用。",
              "inputSchema": { "type": "object", "properties": {} } })),
        (Read, json!({ "name": "library_info", "description": "資源庫概況：名稱、項目數、各類型數量、資料夾、標籤、智慧資料夾。",
              "inputSchema": { "type": "object", "properties": {} } })),
        (Read, json!({ "name": "search_items",
              "description": "搜尋素材。query 會比對名稱、標籤、註解、網址、副檔名與字幕文字（多個關鍵字需全部符合）。結果依加入時間新到舊。",
              "inputSchema": { "type": "object", "properties": {
                  "query": { "type": "string" },
                  "tags": { "type": "array", "items": { "type": "string" }, "description": "必須全部包含的標籤" },
                  "kind": kind,
                  "ext": { "type": "string", "description": "副檔名，例如 png、mp4" },
                  "min_rating": { "type": "integer", "minimum": 0, "maximum": 5 },
                  "color": { "type": "string", "description": "主要顏色，#RRGGBB" },
                  "folder_id": { "type": "string" },
                  "smart_folder_id": { "type": "string" },
                  "untagged": { "type": "boolean", "description": "只找沒有標籤的項目" },
                  "include_trash": { "type": "boolean" },
                  "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 },
                  "offset": { "type": "integer", "minimum": 0, "default": 0 } } } })),
        (Read, json!({ "name": "get_item", "description": "單一項目的完整資訊（含檔案路徑、色票、字幕/文字內容）。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } })),
        (Read, json!({ "name": "get_thumbnail", "description": "取得項目的縮圖（圖片），用來查看內容、判斷該加什麼標籤。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] } })),
        (Read, json!({ "name": "list_folders", "description": "資料夾樹（id、名稱、項目數）。",
              "inputSchema": { "type": "object", "properties": {} } })),
        (Read, json!({ "name": "list_tags", "description": "所有標籤與使用次數、標籤群組。",
              "inputSchema": { "type": "object", "properties": {} } })),
        (Read, json!({ "name": "select_items", "description": "在 OrangeLink 視窗中選取並顯示這些項目，讓使用者看到找到的素材。",
              "inputSchema": { "type": "object", "properties": { "ids": ids_schema() }, "required": ["ids"] } })),
        (Write, json!({ "name": "add_tags", "description": "為項目加上標籤。",
              "inputSchema": { "type": "object", "properties": { "ids": ids_schema(),
                  "tags": { "type": "array", "items": { "type": "string" } } }, "required": ["ids", "tags"] } })),
        (Write, json!({ "name": "remove_tags", "description": "從項目移除標籤。",
              "inputSchema": { "type": "object", "properties": { "ids": ids_schema(),
                  "tags": { "type": "array", "items": { "type": "string" } } }, "required": ["ids", "tags"] } })),
        (Write, json!({ "name": "set_rating", "description": "設定評分（0 = 清除）。",
              "inputSchema": { "type": "object", "properties": { "ids": ids_schema(),
                  "rating": { "type": "integer", "minimum": 0, "maximum": 5 } }, "required": ["ids", "rating"] } })),
        (Write, json!({ "name": "set_annotation", "description": "設定或附加項目的註解。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "text": { "type": "string" },
                  "append": { "type": "boolean", "default": false } }, "required": ["id", "text"] } })),
        (Write, json!({ "name": "rename_item", "description": "修改項目的顯示名稱（不改硬碟上的檔名）。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "name": { "type": "string" } },
                  "required": ["id", "name"] } })),
        (Write, json!({ "name": "create_folder", "description": "建立資料夾，回傳新資料夾 id。",
              "inputSchema": { "type": "object", "properties": { "name": { "type": "string" },
                  "parent_id": { "type": "string" } }, "required": ["name"] } })),
        (Write, json!({ "name": "rename_folder", "description": "重新命名資料夾。",
              "inputSchema": { "type": "object", "properties": { "folder_id": { "type": "string" }, "name": { "type": "string" } },
                  "required": ["folder_id", "name"] } })),
        (Write, json!({ "name": "delete_empty_folder", "description": "刪除空的資料夾（裡面沒有項目也沒有子資料夾才會刪除；不影響任何素材）。",
              "inputSchema": { "type": "object", "properties": { "folder_id": { "type": "string" } }, "required": ["folder_id"] } })),
        (Write, json!({ "name": "add_to_folder", "description": "把項目加入資料夾（項目可同時在多個資料夾）。",
              "inputSchema": { "type": "object", "properties": { "ids": ids_schema(), "folder_id": { "type": "string" } },
                  "required": ["ids", "folder_id"] } })),
        (Write, json!({ "name": "remove_from_folder", "description": "把項目移出資料夾（不會刪除檔案）。",
              "inputSchema": { "type": "object", "properties": { "ids": ids_schema(), "folder_id": { "type": "string" } },
                  "required": ["ids", "folder_id"] } })),
        (Write, json!({ "name": "create_smart_folder", "description": "以條件建立智慧資料夾。",
              "inputSchema": { "type": "object", "properties": { "name": { "type": "string" }, "query": { "type": "string" },
                  "tags": { "type": "array", "items": { "type": "string" } }, "kind": kind,
                  "min_rating": { "type": "integer" }, "parent_id": { "type": "string", "description": "智慧資料夾群組 id" } },
                  "required": ["name"] } })),
        (Files, json!({ "name": "import_files", "description": "匯入檔案或資料夾（背景執行，立即回傳；同時只能有一個匯入，用 get_status 查看是否完成）。link=true 時保留在原位置不複製。",
              "inputSchema": { "type": "object", "properties": {
                  "paths": { "type": "array", "items": { "type": "string" } },
                  "link": { "type": "boolean", "default": true } }, "required": ["paths"] } })),
        (Read, json!({ "name": "get_subtitles",
              "description": "影片/音訊的字幕（自動找同名 .srt/.vtt），每句含開始與結束秒數。可用來找出某句台詞出現的時間，再用 add_clip 標記片段。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" },
                  "query": { "type": "string", "description": "只回傳包含這段文字的句子" } }, "required": ["id"] } })),
        (Write, json!({ "name": "add_clip", "description": "在影片/音訊上標記一個片段（入點到出點，單位秒），回傳片段 id。匯出 Premiere XML 時會一起輸出。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "start": { "type": "number" },
                  "end": { "type": "number" }, "name": { "type": "string" } }, "required": ["id", "start", "end"] } })),
        (Files, json!({ "name": "capture_frame", "description": "把影片某個時間點（秒）的畫面存成圖片並匯入資源庫（背景執行，立即回傳；用 get_status 查看是否完成）。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "time": { "type": "number" } },
                  "required": ["id", "time"] } })),
        (Write, json!({ "name": "delete_clip", "description": "刪除項目上的片段。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "clip_id": { "type": "string" } },
                  "required": ["id", "clip_id"] } })),
        (Read, json!({ "name": "list_smart_folders",
              "description": "智慧資料夾樹：id、名稱、是否為群組、上層群組、目前符合的項目數與條件。用 search_items 的 smart_folder_id 取得裡面的項目。",
              "inputSchema": { "type": "object", "properties": {} } })),
        (Read, json!({ "name": "get_signals",
              "description": "直播影片的精華訊號：聊天室熱度（需要同名 .live_chat.json）與音量（需先分析，find_highlight_candidates 會做）。回傳最突出的時間點（秒）、分數、原因，以及每 30 秒一格的聊天量與最大音量，讓你自己判斷精華並用 add_clip 標記、命名。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" },
                  "top": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20, "description": "回傳幾個高峰" } }, "required": ["id"] } })),
        (Write, json!({ "name": "find_highlight_candidates",
              "description": "排入背景的整場處理：分析音量（已分析過會沿用）、在聊天和音量高峰建立「候選 01」等片段（取代還沒改名的舊候選），沒有字幕時排入語音轉字幕。耗時工作，立即回傳；用 get_status 查看進度，完成後用 get_item 看 clips（note 是被選上的原因）。",
              "inputSchema": { "type": "object", "properties": { "ids": ids_schema() }, "required": ["ids"] } })),
        (Files, json!({ "name": "export_clip",
              "description": "用輸出預設組把片段寫成檔案（加入背景輸出佇列，立即回傳）。preset 是預設組名稱（部分即可），省略時用第一個；vertical=true 時用第一個直式 9:16 預設組。預設組可能會燒入字幕、標準化響度，並在完成後匯入資源庫。用 get_status 查看輸出佇列。",
              "inputSchema": { "type": "object", "properties": { "id": { "type": "string" }, "clip_id": { "type": "string" },
                  "preset": { "type": "string" }, "vertical": { "type": "boolean", "default": false } }, "required": ["id", "clip_id"] } })),
        (Files, json!({ "name": "generate_subtitles",
              "description": "把影片／音訊排入 Whisper 語音轉字幕（背景執行，立即回傳）。需要使用者已下載 Whisper 模型。用 get_status 查看進度，完成後可用 get_subtitles 讀取。",
              "inputSchema": { "type": "object", "properties": { "ids": ids_schema() }, "required": ["ids"] } })),
        (Write, json!({ "name": "set_fields",
              "description": "設定自訂欄位（例如 授權、拍攝地點、客戶）。fields 是「欄位名稱 → 值」，值為空字串表示清除。新欄位名稱會自動加入資源庫。",
              "inputSchema": { "type": "object", "properties": { "ids": ids_schema(),
                  "fields": { "type": "object", "additionalProperties": { "type": "string" } } }, "required": ["ids", "fields"] } })),
    ]
}

/// Tool list for `tools/list`, each marked read-only or not for the client.
pub fn tool_defs() -> Value {
    registry()
        .into_iter()
        .map(|(access, mut def)| {
            def["annotations"] = json!({ "readOnlyHint": access == Access::Read });
            def
        })
        .collect()
}

/// What `name` may touch; None for a tool that doesn't exist.
fn access_of(name: &str) -> Option<Access> {
    registry().into_iter().find(|(_, d)| d["name"] == name).map(|(a, _)| a)
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

/// Longest tag the AI may set, in characters.
const TAG_MAX: usize = 128;
/// Longest item, folder or clip name the AI may set, in characters.
const NAME_MAX: usize = 255;

/// The invisible characters that change the direction text runs in: U+202E
/// makes "evil\u{202E}gpj.exe" read "evilexe.jpg".
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `s` as a tag or name: tabs and line breaks become spaces, other control
/// characters and direction overrides go, the edges are trimmed and it is cut
/// to `max` characters. None when nothing is left.
fn clean_text(s: &str, max: usize) -> Option<String> {
    let s: String = s
        .chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' => Some(' '),
            c if c.is_control() || is_bidi_control(c) => None,
            c => Some(c),
        })
        .collect();
    let s: String = s.trim().chars().take(max).collect();
    let s = s.trim_end();
    (!s.is_empty()).then(|| s.to_string())
}

/// `rating`: a whole number from 0 to 5 (3.0 counts). A string, a fraction or
/// anything out of range is refused rather than read as 0 or 5.
fn rating_arg(v: Option<&Value>) -> Result<u8, String> {
    let n = v.and_then(|v| v.as_u64().or_else(|| v.as_f64().filter(|f| f.fract() == 0.0 && *f >= 0.0).map(|f| f as u64)));
    match n {
        Some(n) if n <= 5 => Ok(n as u8),
        _ => Err(format!("rating 要是 0 到 5 的整數（0 = 清除），收到的是 {}", v.map_or_else(|| "（沒有）".to_string(), Value::to_string))),
    }
}

fn strings(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

fn parse_kind(args: &Value) -> Option<MediaKind> {
    args.get("kind").and_then(|k| serde_json::from_value(k.clone()).ok())
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    // Checked as ASCII first: slicing by bytes inside a multi-byte character panics.
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([p(0)?, p(2)?, p(4)?])
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn summary(lib: &Library, it: &Item) -> Value {
    let folders: Vec<&str> = it.folders.iter().filter_map(|f| lib.folder_name(f)).collect();
    let mut v = json!({
        "id": it.id, "name": it.name, "kind": it.kind, "ext": it.ext, "size_bytes": it.size,
        "tags": it.tags, "folders": folders, "rating": it.rating,
    });
    if it.width > 0 {
        v["width"] = json!(it.width);
        v["height"] = json!(it.height);
    }
    if let Some(d) = it.duration {
        v["duration_sec"] = json!((d * 10.0).round() / 10.0);
    }
    if !it.annotation.is_empty() {
        v["annotation"] = json!(it.annotation.chars().take(200).collect::<String>());
    }
    if it.deleted {
        v["in_trash"] = json!(true);
    }
    v
}

impl App {
    /// Runs one MCP tool call against the open library.
    pub(crate) fn mcp_call(&mut self, name: &str, args: &Value) -> ToolOutput {
        // Read-only and useful before a library is open: answered first.
        if name == "get_status" {
            return ToolOutput::json(self.status_json());
        }
        if self.lib.is_none() {
            return ToolOutput::error(match &self.lib_loading {
                Some((path, _)) => format!("OrangeLink 正在開啟資源庫 {}，請稍候再試（可用 get_status 查看）", path.display()),
                None => "OrangeLink 目前沒有開啟資源庫".to_string(),
            });
        }
        let access = access_of(name).unwrap_or(Access::Read);
        // Not allowed: the user is offered 「本次允許」 in OrangeLink, and the
        // assistant is told to try again once they have.
        if access == Access::Write && !self.can_write() {
            self.ask_grant(false, name);
            return ToolOutput::error("使用者沒有允許 AI 修改資源庫。OrangeLink 已詢問使用者；使用者按「本次允許」後再試一次，或請使用者在設定中開啟「允許 AI 修改資源庫」");
        }
        if access == Access::Files && !self.can_use_files() {
            self.ask_grant(true, name);
            return ToolOutput::error(format!("使用者沒有允許 AI 存取檔案，{name} 無法使用。OrangeLink 已詢問使用者；使用者按「本次允許」後再試一次"));
        }
        let seq = self.edits.seq;
        let result = self.mcp_dispatch(name, args);
        if result.is_error {
            let text = result.content.first().and_then(|c| c.get("text")).and_then(Value::as_str).unwrap_or_default();
            crate::log::error("mcp", format!("{name} 失敗：{text}"));
        }
        if access != Access::Read && !result.is_error {
            let text = match result.content.first().and_then(|c| c.get("text")) {
                Some(Value::String(text)) => text.clone(),
                _ => name.to_string(),
            };
            self.log_ai(name, &text, seq);
        }
        result
    }

    fn mcp_dispatch(&mut self, name: &str, args: &Value) -> ToolOutput {
        let lib = self.lib.as_ref().expect("checked by caller");
        let existing = |ids: &[String]| -> Vec<String> { ids.iter().filter(|id| lib.items.contains_key(*id)).cloned().collect() };
        match name {
            "library_info" => {
                let live: Vec<&Item> = lib.items.values().filter(|i| !i.deleted).collect();
                let mut kinds = serde_json::Map::new();
                for k in MediaKind::ALL {
                    kinds.insert(format!("{k:?}").to_lowercase(), json!(live.iter().filter(|i| i.kind == k).count()));
                }
                let smart: Vec<Value> = lib
                    .meta
                    .smart_folders
                    .iter()
                    .filter(|s| !s.group)
                    .map(|s| json!({ "id": s.id, "name": s.name }))
                    .collect();
                ToolOutput::json(json!({
                    "name": lib.name(), "path": lib.root, "items": live.len(),
                    "in_trash": lib.items.values().filter(|i| i.deleted).count(),
                    "by_kind": kinds, "folders": lib.meta.folders.len(),
                    "tags": lib.tag_counts().len(), "smart_folders": smart,
                    "untagged": live.iter().filter(|i| i.tags.is_empty()).count(),
                    "missing_files": lib.missing.len(), "custom_fields": lib.meta.fields,
                }))
            }
            "search_items" => {
                let filter = SavedFilter {
                    search: str_arg(args, "query").unwrap_or_default().to_string(),
                    tags: strings(args, "tags"),
                    ext: str_arg(args, "ext").map(|e| e.trim_start_matches('.').to_lowercase()),
                    min_rating: args.get("min_rating").and_then(|v| v.as_u64()).unwrap_or(0).min(5) as u8,
                    color: str_arg(args, "color").and_then(parse_hex),
                    kind: parse_kind(args),
                    untagged: args.get("untagged").and_then(|v| v.as_bool()).unwrap_or(false),
                    ..Default::default()
                };
                let crit = Criteria::new(&filter);
                let smart = str_arg(args, "smart_folder_id").and_then(|id| lib.smart_folder(id)).map(|s| Criteria::for_library(&s.filter, lib));
                let folder = str_arg(args, "folder_id");
                let trash = args.get("include_trash").and_then(|v| v.as_bool()).unwrap_or(false);
                let mut hits: Vec<&Item> = lib
                    .items
                    .values()
                    .filter(|it| (trash || !it.deleted) && crit.matches(it))
                    .filter(|it| smart.as_ref().is_none_or(|s| s.matches(it)))
                    .filter(|it| folder.is_none_or(|f| it.folders.iter().any(|x| x == f)))
                    .collect();
                hits.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(a.id.cmp(&b.id)));
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50).clamp(1, 200) as usize;
                let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let items: Vec<Value> = hits.iter().skip(offset).take(limit).map(|it| summary(lib, it)).collect();
                ToolOutput::json(json!({ "total": hits.len(), "offset": offset, "items": items }))
            }
            "get_item" => {
                let Some(it) = str_arg(args, "id").and_then(|id| lib.items.get(id)) else {
                    return ToolOutput::error("找不到這個 id");
                };
                let mut v = summary(lib, it);
                v["path"] = json!(lib.file_path(it));
                v["linked_in_place"] = json!(it.source.is_some());
                v["annotation"] = json!(it.annotation);
                v["url"] = json!(it.url);
                v["palette"] = json!(it.palette.iter().map(|p| json!({ "color": hex(p.rgb), "ratio": p.ratio })).collect::<Vec<_>>());
                if let Some(fps) = it.fps {
                    v["fps"] = json!(fps);
                }
                if let Some(c) = &it.codec {
                    v["codec"] = json!(c);
                }
                if let Some(t) = &it.text {
                    v["text"] = json!(t.chars().take(4000).collect::<String>());
                }
                if !it.clips.is_empty() {
                    v["clips"] = json!(it.clips);
                }
                let related: Vec<Value> = lib.linked(&it.id).iter().map(|r| json!({ "id": r.id, "name": r.name, "kind": r.kind })).collect();
                if !related.is_empty() {
                    v["related"] = json!(related);
                }
                if !it.fields.is_empty() {
                    v["fields"] = json!(it.fields);
                }
                if matches!(it.kind, MediaKind::Video | MediaKind::Audio) {
                    v["subtitle_file"] = json!(lib.find_subtitle(&it.id));
                }
                v["missing_file"] = json!(lib.missing.contains(&it.id));
                ToolOutput::json(v)
            }
            "get_thumbnail" => {
                let Some(it) = str_arg(args, "id").and_then(|id| lib.items.get(id)) else {
                    return ToolOutput::error("找不到這個 id");
                };
                match std::fs::read(lib.thumb_path(&it.id)) {
                    Ok(bytes) => {
                        let mime = if bytes.starts_with(&[0xFF, 0xD8]) { "image/jpeg" } else { "image/png" };
                        let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        ToolOutput {
                            content: vec![
                                json!({ "type": "image", "data": data, "mimeType": mime }),
                                json!({ "type": "text", "text": format!("{}（{}）", it.name, it.kind.label()) }),
                            ],
                            is_error: false,
                        }
                    }
                    Err(_) => ToolOutput::error(format!("「{}」沒有縮圖（{}檔案）", it.name, it.kind.label())),
                }
            }
            "list_folders" => {
                let counts = lib.folder_counts();
                let folders: Vec<Value> = lib
                    .folder_flat()
                    .into_iter()
                    .map(|(id, name, depth)| {
                        let parent = lib.meta.folders.iter().find(|f| f.id == id).and_then(|f| f.parent.clone());
                        json!({ "id": id, "name": name, "depth": depth, "parent_id": parent, "items": counts.get(&id).copied().unwrap_or(0) })
                    })
                    .collect();
                ToolOutput::json(json!({ "folders": folders }))
            }
            "list_tags" => {
                let tags: Vec<Value> = lib.tag_counts().into_iter().map(|(t, n)| json!({ "tag": t, "count": n })).collect();
                let groups: Vec<Value> = lib.meta.tag_groups.iter().map(|g| json!({ "name": g.name, "tags": g.tags })).collect();
                ToolOutput::json(json!({ "tags": tags, "groups": groups }))
            }
            "select_items" => {
                let ids = existing(&strings(args, "ids"));
                if ids.is_empty() {
                    return ToolOutput::error("沒有有效的 id");
                }
                // Show everything so the selection is visible, then select.
                self.view = View::All;
                self.search.clear();
                self.tag_filter.clear();
                self.ext_filter.clear();
                self.min_rating = 0;
                self.color_filter = None;
                self.viewer = None;
                self.selected = ids.iter().cloned().collect();
                self.anchor = ids.first().cloned();
                self.scroll_to = ids.first().cloned();
                ToolOutput::text(format!("已在 OrangeLink 中選取 {} 個項目", ids.len()))
            }
            "add_tags" | "remove_tags" => {
                let ids = existing(&strings(args, "ids"));
                let asked = strings(args, "tags");
                let mut tags: Vec<String> = Vec::new();
                for t in asked.iter().filter_map(|t| clean_text(t, TAG_MAX)) {
                    if !tags.contains(&t) {
                        tags.push(t);
                    }
                }
                if ids.is_empty() || tags.is_empty() {
                    return ToolOutput::error("需要 ids 與 tags（標籤不能只有空白或控制字元）");
                }
                let add = name == "add_tags";
                self.edit_items(&ids, |it| {
                    for t in &tags {
                        if add && !it.tags.contains(t) {
                            it.tags.push(t.clone());
                        } else if !add {
                            it.tags.retain(|x| x != t);
                        }
                    }
                    // A tag stored before cleaning came in, asked for as it is.
                    if !add {
                        it.tags.retain(|x| !asked.contains(x));
                    }
                });
                let verb = if add { "加上" } else { "移除" };
                ToolOutput::text(format!("已為 {} 個項目{verb}標籤：{}", ids.len(), tags.join("、")))
            }
            "set_rating" => {
                let ids = existing(&strings(args, "ids"));
                let rating = match rating_arg(args.get("rating")) {
                    Ok(r) => r,
                    Err(e) => return ToolOutput::error(e),
                };
                if ids.is_empty() {
                    return ToolOutput::error("沒有有效的 id");
                }
                self.edit_items(&ids, |it| it.rating = rating);
                ToolOutput::text(format!("已將 {} 個項目評為 {rating} 星", ids.len()))
            }
            "list_smart_folders" => {
                let live: Vec<&Item> = lib.items.values().filter(|i| !i.deleted).collect();
                let folders: Vec<Value> = lib
                    .meta
                    .smart_folders
                    .iter()
                    .map(|s| {
                        let mut v = json!({ "id": s.id, "name": s.name, "group": s.group, "parent_id": s.parent });
                        if !s.group {
                            let c = Criteria::for_library(&s.filter, lib);
                            v["count"] = json!(live.iter().filter(|it| c.matches(it)).count());
                            // Only the conditions in use.
                            let full = serde_json::to_value(&s.filter).unwrap_or_default();
                            let blank = serde_json::to_value(SavedFilter::default()).unwrap_or_default();
                            let used: serde_json::Map<String, Value> = full
                                .as_object()
                                .into_iter()
                                .flatten()
                                .filter(|(k, val)| blank.get(k.as_str()) != Some(val))
                                .map(|(k, val)| (k.clone(), val.clone()))
                                .collect();
                            v["conditions"] = Value::Object(used);
                        }
                        v
                    })
                    .collect();
                ToolOutput::json(json!({ "smart_folders": folders }))
            }
            "get_signals" => {
                let Some(it) = str_arg(args, "id").and_then(|id| lib.items.get(id)) else {
                    return ToolOutput::error("找不到這個 id");
                };
                if !matches!(it.kind, MediaKind::Video | MediaKind::Audio) {
                    return ToolOutput::error("只有影片和音訊有聊天和音量訊號");
                }
                let duration = it.duration.unwrap_or(0.0);
                let chat = it
                    .imported_from()
                    .and_then(|p| crate::signals::chat_file_for(p))
                    .and_then(|f| crate::signals::read_chat(&f, &self.config.chat_keywords, duration));
                let loud: Option<Vec<f32>> = std::fs::read(crate::library::item_dir(&lib.root, &it.id).join("loudness.json")).ok().and_then(|b| serde_json::from_slice(&b).ok());
                if chat.is_none() && loud.is_none() {
                    return ToolOutput::error("沒有聊天室紀錄（同名 .live_chat.json），音量也還沒分析；可以先呼叫 find_highlight_candidates 分析音量");
                }
                let (curve, hits) = chat.as_ref().map_or((None, None), |(c, h)| (Some(c.as_slice()), Some(h)));
                let top = args.get("top").and_then(|v| v.as_u64()).unwrap_or(20).clamp(1, 100) as usize;
                let c = &self.config;
                let (before, after) = (c.highlight_before.max(0.0) as f64, c.highlight_after.max(0.0) as f64);
                let gap = (before + after).max(10.0) as usize;
                let peaks: Vec<Value> = crate::signals::peaks(curve, loud.as_deref(), gap, top)
                    .into_iter()
                    .map(|p| {
                        let (s, e) = ((p.at as f64 - before).max(0.0), p.at as f64 + after);
                        json!({ "time": p.at, "score": (p.score * 10.0).round() / 10.0, "suggested_start": s, "suggested_end": e,
                                "why": crate::signals::describe(curve, hits, loud.as_deref(), s, e) })
                    })
                    .collect();
                const BUCKET: usize = 30;
                let len = curve.map_or(0, |c| c.len()).max(loud.as_ref().map_or(0, |l| l.len()));
                let buckets: Vec<Value> = (0..len.div_ceil(BUCKET))
                    .map(|b| {
                        let r = b * BUCKET..((b + 1) * BUCKET).min(len);
                        let chat_sum = curve.map(|c| c[r.start.min(c.len())..r.end.min(c.len())].iter().sum::<f32>().round());
                        let loud_max = loud.as_ref().and_then(|l| l[r.start.min(l.len())..r.end.min(l.len())].iter().copied().reduce(f32::max));
                        json!({ "start": r.start, "chat": chat_sum, "loudness_lufs": loud_max })
                    })
                    .collect();
                ToolOutput::json(json!({
                    "id": it.id, "duration_sec": it.duration, "has_chat": chat.is_some(), "has_loudness": loud.is_some(),
                    "chat_keywords": self.config.chat_keywords, "peaks": peaks, "every_30s": buckets,
                }))
            }
            "find_highlight_candidates" => {
                let ids: Vec<String> = existing(&strings(args, "ids"))
                    .into_iter()
                    .filter(|id| lib.items.get(id).is_some_and(|i| matches!(i.kind, MediaKind::Video | MediaKind::Audio)))
                    .collect();
                if ids.is_empty() {
                    return ToolOutput::error("沒有可處理的影片或音訊");
                }
                let n = ids.len();
                self.process_streams(ids);
                ToolOutput::text(format!("已把 {n} 個項目排入整場處理（背景工作，用 get_status 查看進度）。完成後用 get_item 查看 clips，名稱是「候選 01」等，note 是原因"))
            }
            "export_clip" => {
                let (Some(id), Some(cid)) = (str_arg(args, "id").map(str::to_string), str_arg(args, "clip_id").map(str::to_string)) else {
                    return ToolOutput::error("需要 id 與 clip_id");
                };
                let Some(it) = lib.items.get(&id) else { return ToolOutput::error("找不到這個 id") };
                let Some(clip) = it.clips.iter().find(|c| c.id == cid) else { return ToolOutput::error("找不到這個片段") };
                if self.ffmpeg_dir.is_none() {
                    return ToolOutput::error("需要 FFmpeg 才能輸出片段");
                }
                let presets = &self.config.export_presets;
                let wanted = str_arg(args, "preset").map(str::to_lowercase);
                let vertical = args.get("vertical").and_then(|v| v.as_bool()).unwrap_or(false);
                let picked = match (&wanted, vertical) {
                    (Some(w), _) => presets.iter().find(|p| p.name.to_lowercase() == *w).or_else(|| presets.iter().find(|p| p.name.to_lowercase().contains(w.as_str()))),
                    (None, true) => presets.iter().find(|p| p.shape == crate::config::ExportShape::Vertical),
                    (None, false) => presets.first(),
                };
                let Some(preset) = picked.cloned() else {
                    let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
                    return ToolOutput::error(format!("找不到這個預設組；可用的有：{}", names.join("、")));
                };
                let (pname, cname) = (preset.name.clone(), clip.name.clone());
                self.export_clip(&id, &cid, preset);
                ToolOutput::text(format!("已把片段「{cname}」以「{pname}」加入輸出佇列（背景工作，用 get_status 查看進度）"))
            }
            "generate_subtitles" => {
                let ids: Vec<String> = existing(&strings(args, "ids"))
                    .into_iter()
                    .filter(|id| lib.items.get(id).is_some_and(|i| matches!(i.kind, MediaKind::Video | MediaKind::Audio)))
                    .collect();
                if ids.is_empty() {
                    return ToolOutput::error("沒有可轉字幕的影片或音訊");
                }
                let model = self.config.whisper_model.clone();
                if !crate::whisper::has_model(&model) {
                    return ToolOutput::error(format!("還沒有 Whisper 模型（{model}）；請使用者在 OrangeLink 的「產生字幕」視窗下載"));
                }
                if !self.ffmpeg_dir.as_deref().is_some_and(crate::whisper::has_filter) {
                    return ToolOutput::error("這個 FFmpeg 沒有 whisper 濾鏡，無法轉字幕");
                }
                let n = ids.len();
                for id in ids {
                    self.queue_whisper(&id);
                }
                ToolOutput::text(format!("已把 {n} 個項目排入語音轉字幕（背景工作，用 get_status 查看進度）"))
            }
            "get_subtitles" => {
                let Some(id) = str_arg(args, "id").filter(|id| lib.items.contains_key(*id)) else {
                    return ToolOutput::error("找不到這個 id");
                };
                let Some(path) = lib.find_subtitle(id) else {
                    return ToolOutput::error("這個項目沒有找到同名的 .srt / .vtt 字幕檔");
                };
                let q = str_arg(args, "query").map(str::to_lowercase);
                let cues: Vec<Value> = crate::media::read_text(&path, 16 << 20)
                    .map(|t| crate::media::parse_cues(&t))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| q.as_ref().is_none_or(|q| c.text.to_lowercase().contains(q)))
                    .take(2000)
                    .map(|c| json!({ "start": (c.start * 1000.0).round() / 1000.0, "end": (c.end * 1000.0).round() / 1000.0, "text": c.text }))
                    .collect();
                ToolOutput::json(json!({ "file": path, "cues": cues }))
            }
            "add_clip" => {
                let Some(it) = str_arg(args, "id").and_then(|id| lib.items.get(id)) else {
                    return ToolOutput::error("找不到這個 id");
                };
                if !matches!(it.kind, MediaKind::Video | MediaKind::Audio) {
                    return ToolOutput::error("只有影片和音訊可以標記片段");
                }
                let num = |k: &str| args.get(k).and_then(|v| v.as_f64());
                let (Some(a), Some(b)) = (num("start"), num("end")) else {
                    return ToolOutput::error("需要 start 與 end（秒）");
                };
                let max = it.duration.unwrap_or(f64::MAX);
                let (start, end) = (a.min(b).clamp(0.0, max), a.max(b).clamp(0.0, max));
                if end - start < 0.05 {
                    return ToolOutput::error("片段太短");
                }
                let id = it.id.clone();
                let clip = crate::model::Clip {
                    id: crate::model::new_id(),
                    name: str_arg(args, "name").and_then(|n| clean_text(n, NAME_MAX)).unwrap_or_else(|| format!("片段 {}", it.clips.len() + 1)),
                    start,
                    end,
                    ..Default::default()
                };
                let (cid, name) = (clip.id.clone(), clip.name.clone());
                self.edit_items(&[id], |it| it.clips.push(clip.clone()));
                ToolOutput::text(format!("已加入片段「{name}」（clip_id: {cid}）"))
            }
            "capture_frame" => {
                let Some(it) = str_arg(args, "id").and_then(|id| lib.items.get(id)) else {
                    return ToolOutput::error("找不到這個 id");
                };
                if it.kind != MediaKind::Video {
                    return ToolOutput::error("只有影片可以擷取畫面");
                }
                let max = it.duration.unwrap_or(f64::MAX);
                let time = args.get("time").and_then(|v| v.as_f64()).unwrap_or(0.0).clamp(0.0, max);
                let id = it.id.clone();
                match self.capture_frame(&id, time) {
                    Ok(path) => ToolOutput::text(format!(
                        "開始擷取 {} 的畫面（背景執行），完成後會匯入：{}。用 get_status 查看是否完成。",
                        crate::media::fmt_precise(time),
                        path.display()
                    )),
                    Err(e) => ToolOutput::error(format!("擷取失敗：{e}")),
                }
            }
            "delete_clip" => {
                let (Some(id), Some(cid)) = (str_arg(args, "id").map(str::to_string), str_arg(args, "clip_id").map(str::to_string)) else {
                    return ToolOutput::error("需要 id 與 clip_id");
                };
                if !lib.items.get(&id).is_some_and(|it| it.clips.iter().any(|c| c.id == cid)) {
                    return ToolOutput::error("找不到這個片段");
                }
                self.edit_items(&[id], |it| it.clips.retain(|c| c.id != cid));
                ToolOutput::text("已刪除片段")
            }
            "set_fields" => {
                let ids = existing(&strings(args, "ids"));
                let Some(fields) = args.get("fields").and_then(|f| f.as_object()) else {
                    return ToolOutput::error("需要 fields 物件");
                };
                let pairs: Vec<(String, String)> = fields
                    .iter()
                    .filter(|(k, _)| !k.trim().is_empty())
                    .map(|(k, v)| (k.trim().to_string(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()).trim().to_string()))
                    .collect();
                if ids.is_empty() || pairs.is_empty() {
                    return ToolOutput::error("沒有可更新的項目或欄位");
                }
                if let Some(lib) = &mut self.lib {
                    for (k, _) in &pairs {
                        lib.add_field(k);
                    }
                }
                self.edit_items(&ids, |it| {
                    for (k, v) in &pairs {
                        if v.is_empty() {
                            it.fields.remove(k);
                        } else {
                            it.fields.insert(k.clone(), v.clone());
                        }
                    }
                });
                ToolOutput::text(format!("已更新 {} 個項目的自訂欄位", ids.len()))
            }
            "set_annotation" => {
                let Some(id) = str_arg(args, "id").filter(|id| lib.items.contains_key(*id)).map(str::to_string) else {
                    return ToolOutput::error("找不到這個 id");
                };
                let text = str_arg(args, "text").unwrap_or_default().to_string();
                let append = args.get("append").and_then(|v| v.as_bool()).unwrap_or(false);
                self.edit_items(&[id], |it| {
                    if append && !it.annotation.is_empty() {
                        it.annotation.push('\n');
                        it.annotation.push_str(&text);
                    } else {
                        it.annotation = text.clone();
                    }
                });
                ToolOutput::text("已更新註解")
            }
            "rename_item" => {
                let (Some(id), Some(new)) = (str_arg(args, "id").map(str::to_string), str_arg(args, "name").and_then(|n| clean_text(n, NAME_MAX))) else {
                    return ToolOutput::error("需要 id 與 name（名稱不能只有空白或控制字元）");
                };
                if !lib.items.contains_key(&id) {
                    return ToolOutput::error("找不到這個 id");
                }
                self.edit_items(&[id], |it| it.name = new.clone());
                ToolOutput::text(format!("已重新命名為「{new}」"))
            }
            "create_folder" => {
                let Some(fname) = str_arg(args, "name").and_then(|n| clean_text(n, NAME_MAX)) else {
                    return ToolOutput::error("需要 name（名稱不能只有空白或控制字元）");
                };
                let parent = str_arg(args, "parent_id").filter(|p| lib.folder_name(p).is_some()).map(str::to_string);
                let lib = self.lib.as_mut().expect("open");
                let id = lib.add_folder(&fname, parent);
                ToolOutput::text(format!("已建立資料夾「{fname}」\nfolder_id: {id}"))
            }
            "rename_folder" | "delete_empty_folder" => {
                let Some(folder) = str_arg(args, "folder_id").filter(|f| lib.folder_name(f).is_some()).map(str::to_string) else {
                    return ToolOutput::error("找不到這個 folder_id（用 list_folders 查詢）");
                };
                let old = lib.folder_name(&folder).unwrap_or_default().to_string();
                if name == "rename_folder" {
                    let Some(new) = str_arg(args, "name").and_then(|n| clean_text(n, NAME_MAX)) else {
                        return ToolOutput::error("需要 name（名稱不能只有空白或控制字元）");
                    };
                    self.lib.as_mut().expect("open").rename_folder(&folder, &new);
                    return ToolOutput::text(format!("已將資料夾「{old}」重新命名為「{new}」"));
                }
                let used = lib.items.values().any(|i| i.folders.contains(&folder));
                let has_children = !lib.child_folders(Some(&folder)).is_empty();
                if used || has_children {
                    return ToolOutput::error(format!("資料夾「{old}」不是空的（有項目或子資料夾），不會刪除"));
                }
                self.lib.as_mut().expect("open").delete_folder(&folder);
                if self.view == View::Folder(folder) {
                    self.view = View::All;
                }
                ToolOutput::text(format!("已刪除空資料夾「{old}」"))
            }
            "add_to_folder" | "remove_from_folder" => {
                let ids = existing(&strings(args, "ids"));
                let Some(folder) = str_arg(args, "folder_id").filter(|f| lib.folder_name(f).is_some()).map(str::to_string) else {
                    return ToolOutput::error("找不到這個 folder_id（用 list_folders 查詢）");
                };
                let fname = lib.folder_name(&folder).unwrap_or_default().to_string();
                let add = name == "add_to_folder";
                self.edit_items(&ids, |it| {
                    if add && !it.folders.contains(&folder) {
                        it.folders.push(folder.clone());
                    } else if !add {
                        it.folders.retain(|f| *f != folder);
                    }
                });
                let verb = if add { "加入" } else { "移出" };
                ToolOutput::text(format!("已將 {} 個項目{verb}「{fname}」", ids.len()))
            }
            "create_smart_folder" => {
                let Some(sname) = str_arg(args, "name").map(str::to_string) else { return ToolOutput::error("需要 name") };
                let filter = SavedFilter {
                    search: str_arg(args, "query").unwrap_or_default().to_string(),
                    tags: strings(args, "tags"),
                    kind: parse_kind(args),
                    min_rating: args.get("min_rating").and_then(|v| v.as_u64()).unwrap_or(0).min(5) as u8,
                    ..Default::default()
                };
                if filter.is_empty() {
                    return ToolOutput::error("至少需要一個條件（query、tags、kind 或 min_rating）");
                }
                let parent = str_arg(args, "parent_id").filter(|p| lib.smart_folder(p).is_some_and(|s| s.group)).map(str::to_string);
                let lib = self.lib.as_mut().expect("open");
                let id = lib.add_smart_folder(&sname, filter, parent, false);
                ToolOutput::text(format!("已建立智慧資料夾「{sname}」\nsmart_folder_id: {id}"))
            }
            "import_files" => {
                let home = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from);
                let paths = match resolve_import_paths(strings(args, "paths"), home.as_deref()) {
                    Ok(p) => p,
                    Err(e) => return ToolOutput::error(e),
                };
                if self.import_running() {
                    return ToolOutput::error("目前有匯入正在進行，請稍後再試（用 get_status 查看是否完成）");
                }
                if self.dialog.is_some() {
                    return ToolOutput::error("OrangeLink 有對話框開著，請使用者關閉後再試");
                }
                let link = args.get("link").and_then(|v| v.as_bool()).unwrap_or(true);
                let n = paths.len();
                // Text files are read into the library and can be read back: the
                // user says yes to each AI import.
                const SHOWN: usize = 8;
                let mut list: Vec<String> = paths.iter().take(SHOWN).map(|p| format!("• {}", p.display())).collect();
                if n > SHOWN {
                    list.push(format!("…還有 {} 個", n - SHOWN));
                }
                let how = if link { "保留在原位置" } else { "複製到資源庫" };
                self.dialog = Some(super::Dialog::Confirm {
                    message: format!("AI 助理要求匯入 {n} 個路徑（{how}）：\n{}\n\n只在你確實請 AI 匯入這些檔案時才按「確定」。", list.join("\n")),
                    on_yes: super::Confirmed::AiImport { paths, link },
                });
                self.egui_ctx.request_repaint();
                ToolOutput::text(format!("已請使用者在 OrangeLink 視窗確認匯入 {n} 個路徑（{how}）。確認後才會開始；用 get_status 查看是否完成，之後可用 search_items 查詢。"))
            }
            _ => ToolOutput::error(format!("未知的工具：{name}")),
        }
    }
}

impl App {
    /// What `get_status` reports: the explicit states of the library, the
    /// background jobs, the preview and the update, for an AI assistant to wait on.
    pub(crate) fn status_json(&mut self) -> Value {
        let library = match (&self.lib, &self.lib_loading) {
            (Some(lib), _) => json!({ "state": "open", "name": lib.name(), "path": lib.root }),
            (None, Some((path, _))) => json!({ "state": "loading", "path": path }),
            (None, None) => json!({ "state": "none" }),
        };
        let jobs: Vec<Value> = self
            .background_jobs()
            .into_iter()
            .map(|j| {
                json!({
                    "kind": j.kind.key(), "name": j.name, "detail": j.detail,
                    "progress": j.progress.map(|p| (p * 1000.0).round() / 1000.0),
                    "eta_seconds": j.eta.map(f64::round), "can_cancel": j.can_cancel,
                })
            })
            .collect();
        let update = match &self.update {
            UpdateState::Checking(_) => json!({ "state": "checking" }),
            UpdateState::Latest => json!({ "state": "latest" }),
            UpdateState::Available { release } => json!({ "state": "available", "version": release.tag }),
        };
        const SHOWN: usize = 50;
        let selected = self.selected_ids();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "library": library,
            "busy": !jobs.is_empty(),
            "jobs": jobs,
            "preview": self.viewer.as_ref().map(|v| v.id.clone()),
            "selected_count": selected.len(),
            "selected": selected.into_iter().take(SHOWN).collect::<Vec<_>>(),
            "allow_write": self.can_write(),
            "allow_files": self.can_use_files(),
            // Allowed by the user for this run only (「本次允許」), not in the settings.
            "allowed_for_session": self.mcp_grant.write,
            "update": update,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratings_are_whole_numbers_from_0_to_5() {
        for (v, want) in [(json!(0), Some(0)), (json!(5), Some(5)), (json!(3.0), Some(3))] {
            assert_eq!(rating_arg(Some(&v)).ok(), want, "{v}");
        }
        for v in [json!("5"), json!(-1), json!(9), json!(2.5), json!(null), json!(true)] {
            assert!(rating_arg(Some(&v)).is_err(), "{v}");
        }
        assert!(rating_arg(None).is_err());
    }

    #[test]
    fn tags_and_names_are_cleaned() {
        assert_eq!(clean_text("a\u{0}b", TAG_MAX).as_deref(), Some("ab"));
        assert_eq!(clean_text("\u{202E}evil", TAG_MAX).as_deref(), Some("evil"));
        assert_eq!(clean_text("\u{2067}x\u{2069}\u{200F}", TAG_MAX).as_deref(), Some("x"));
        assert_eq!(clean_text("直播\t精華\n", TAG_MAX).as_deref(), Some("直播 精華"));
        assert_eq!(clean_text(&"x".repeat(5000), TAG_MAX).map(|t| t.chars().count()), Some(128));
        assert_eq!(clean_text(&"貓".repeat(300), NAME_MAX).map(|t| t.chars().count()), Some(255));
        // Cut where a space was: no space left at the end.
        assert_eq!(clean_text(&format!("{} b", "a".repeat(127)), TAG_MAX), Some("a".repeat(127)));
        for blank in ["   ", "\t\n", "\u{202E}", "\u{0}\u{1}", ""] {
            assert_eq!(clean_text(blank, NAME_MAX), None, "{blank:?}");
        }
        // Ordinary text, emoji and joiners stay.
        assert_eq!(clean_text("Vtuber 精華 🎉 👨‍👩‍👧", TAG_MAX).as_deref(), Some("Vtuber 精華 🎉 👨‍👩‍👧"));
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex("#FF8800"), Some([255, 136, 0]));
        assert_eq!(parse_hex(" ff8800 "), Some([255, 136, 0]));
        assert_eq!(parse_hex("#aébbb"), None, "six bytes, but not six hex digits: no panic");
        assert_eq!(parse_hex("#紅色"), None);
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#GG0000"), None);
    }

    #[test]
    fn every_tool_is_listed_once_with_its_access() {
        let defs = tool_defs();
        let names: Vec<&str> = defs.as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
        let unique: std::collections::HashSet<&&str> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "a tool is listed twice");
        for n in ["get_status", "list_smart_folders", "get_signals", "find_highlight_candidates", "export_clip", "generate_subtitles"] {
            assert!(names.contains(&n), "{n}");
        }
        for d in defs.as_array().unwrap() {
            assert_eq!(d["inputSchema"]["type"], "object", "{}", d["name"]);
            let read_only = access_of(d["name"].as_str().unwrap()) == Some(Access::Read);
            assert_eq!(d["annotations"]["readOnlyHint"], read_only, "{}", d["name"]);
        }
        assert_eq!(access_of("get_status"), Some(Access::Read), "get_status only reads");
        assert_eq!(access_of("search_items"), Some(Access::Read));
        assert_eq!(access_of("add_tags"), Some(Access::Write));
        assert_eq!(access_of("delete_clip"), Some(Access::Write));
        let mut files: Vec<&str> = names.iter().copied().filter(|n| access_of(n) == Some(Access::Files)).collect();
        files.sort_unstable();
        assert_eq!(files, ["capture_frame", "export_clip", "generate_subtitles", "import_files"]);
        assert_eq!(access_of("no_such_tool"), None);
    }

    #[test]
    fn ai_imports_of_whole_drives_or_home_are_refused() {
        use std::path::Path;
        let home = Some(Path::new(r"C:\Users\me"));
        for p in [r"C:\", r"D:\", r"C:\Users", r"C:\Users\", r"c:\users\ME"] {
            assert!(too_broad(Path::new(p), home), "{p}");
        }
        for p in [r"C:\Users\me\Pictures", r"D:\Footage", r"C:\Users\meg"] {
            assert!(!too_broad(Path::new(p), home), "{p}");
        }
    }

    /// Other spellings of the user's folder don't get past the check,
    /// and the paths shown and imported are the resolved ones.
    #[test]
    fn other_spellings_of_home_are_too_broad() {
        let home = std::env::temp_dir().join(format!("orangelink-home-{}", crate::model::new_id()));
        let docs = home.join("Documents");
        std::fs::create_dir_all(docs.join("clips")).unwrap();
        let h = home.to_string_lossy().to_string();
        for p in [format!(r"{h}\Documents\.."), format!(r"\\?\{h}"), format!(r"{h}\Documents\clips\..\.."), format!(r"{h}\.\")] {
            let e = resolve_import_paths(vec![p.clone()], Some(&home)).unwrap_err();
            assert!(e.contains("範圍太大"), "{p}: {e}");
        }
        let ok = resolve_import_paths(vec![format!(r"{h}\Documents\clips\..")], Some(&home)).unwrap();
        assert_eq!(ok, [crate::importer::dunce_abs(&docs).unwrap()]);
        assert!(!ok[0].to_string_lossy().contains(".."));
        assert_eq!(resolve_import_paths(vec![format!(r"{h}\nope")], Some(&home)).unwrap_err(), "沒有存在的路徑");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn every_job_kind_has_its_own_key_for_get_status() {
        use super::super::jobs::JobKind as K;
        let kinds = [K::Import, K::Ytdlp, K::Whisper, K::SubtitleIndex, K::Loudness, K::Thumbnails, K::ClipExport, K::Stream, K::FileCopy, K::Capture, K::Integrity];
        let keys: std::collections::HashSet<&str> = kinds.iter().map(|k| k.key()).collect();
        assert_eq!(keys.len(), kinds.len());
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;

    /// Colors from AI tool calls: never a panic, and what parses writes back the same.
    #[test]
    fn random_colors() {
        for s in crate::fuzz::strings_with(3, 20_000, &["#", "ff", "0a", "FF00", "字字"]) {
            if let Some(c) = parse_hex(&s) {
                assert_eq!(parse_hex(&hex(c)), Some(c), "{s:?}");
            }
        }
        assert_eq!(parse_hex(" #ff8000 "), Some([255, 128, 0]));
    }

    /// What any tool call may leave in the library: names and tags an AI could
    /// set stay within the limits `clean_text` keeps, ratings within 0 to 5.
    fn check_limits(lib: &Library, tool: &str, args: &Value) {
        let clean = |s: &str, max: usize| !s.trim().is_empty() && s.chars().count() <= max && !s.chars().any(|c| c.is_control() || is_bidi_control(c));
        for it in lib.items.values() {
            assert!(clean(&it.name, NAME_MAX), "{tool} {args}: name {:?}", it.name);
            assert!(it.rating <= 5, "{tool} {args}: rating {}", it.rating);
            for t in &it.tags {
                assert!(clean(t, TAG_MAX), "{tool} {args}: tag {t:?}");
            }
            for c in &it.clips {
                assert!(clean(&c.name, NAME_MAX), "{tool} {args}: clip name {:?}", c.name);
            }
        }
    }

    /// Every tool in the registry gets wrong types, odd numbers, hostile text and
    /// random strings in each argument, alone and on top of usable arguments.
    /// None may panic; a call that answers with an error leaves the library as
    /// it was; and the library stays within its limits after every call. A new
    /// tool is covered by being in the registry.
    #[test]
    fn every_tool_survives_bad_arguments() {
        let root = std::env::temp_dir().join(format!("orangelink-mcp-fuzz-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        let mut ids = Vec::new();
        for (kind, ext) in [("image", "png"), ("video", "mp4")] {
            let it: Item = serde_json::from_value(json!({
                "id": crate::model::new_id(), "name": format!("{kind} 素材"), "file_name": format!("a.{ext}"), "ext": ext,
                "kind": kind, "size": 1000, "width": 1920, "height": 1080, "duration": 60.0,
                "added_at": 0, "modified_at": 0, "tags": ["直播"],
                "clips": [{ "id": "c1", "name": "片段", "start": 1.0, "end": 5.0 }]
            }))
            .unwrap();
            std::fs::create_dir_all(root.join("images").join(format!("{}.info", it.id))).unwrap();
            ids.push(it.id.clone());
            lib.insert_item(it);
        }
        let config = crate::config::Config { mcp_allow_write: true, mcp_allow_files: true, ..Default::default() };
        let mut app = App::with_config(&eframe::egui::Context::default(), config);
        app.lib = Some(lib);

        // Usable values, so a bad one in another argument gets past the first checks.
        let (image, video) = (ids[0].clone(), ids[1].clone());
        let usable = |name: &str, ty: &str| -> Value {
            match (name, ty) {
                ("id", _) => json!(video),
                ("ids", _) => json!([image, video]),
                ("clip_id", _) => json!("c1"),
                (_, "string") => json!("精華"),
                (_, "integer" | "number") => json!(1),
                (_, "boolean") => json!(true),
                (_, "array") => json!(["精華"]),
                (_, "object") => json!({ "授權": "內部" }),
                _ => Value::Null,
            }
        };
        let mut bad: Vec<Value> = vec![
            json!(null), json!(true), json!(-1), json!(9), json!(2.5), json!(1e308), json!(-1e308), json!(u64::MAX),
            json!(""), json!("   "), json!("\u{202e}x\u{0}"), json!("x".repeat(5000)), json!(image),
            json!([]), json!([null, 1, {}]), json!({}), json!({ "a": [1] }), json!({ "": "\u{0}" }),
            json!(["", "  ", "\u{0}"]), json!(["..", r"\\?\C:\", r"C:\"]),
        ];
        bad.extend(crate::fuzz::strings_with(11, 40, &["#", "..", "ext:", "dur>", "c1", "★"]).map(Value::String));
        bad.push(Value::Array(crate::fuzz::strings(12, 20).map(Value::String).collect()));

        let snapshot = |app: &App| {
            let lib = app.lib.as_ref().unwrap();
            (serde_json::to_value(&lib.items).unwrap(), serde_json::to_value(&lib.meta).unwrap())
        };
        let start = app.lib.as_ref().map(|l| (l.items.clone(), l.meta.clone())).unwrap();
        let mut calls = 0;
        for def in tool_defs().as_array().unwrap() {
            let name = def["name"].as_str().unwrap();
            // Each tool starts from the same library (hundreds of smart folders
            // left by create_smart_folder would slow every later call).
            let lib = app.lib.as_mut().unwrap();
            (lib.items, lib.meta) = start.clone();
            let props = def["inputSchema"]["properties"].as_object().cloned().unwrap_or_default();
            let full: serde_json::Map<String, Value> = props.iter().map(|(k, s)| (k.clone(), usable(k, s["type"].as_str().unwrap_or("")))).collect();
            let mut cases = vec![json!({}), json!(null), json!([1]), Value::Object(full.clone())];
            for key in props.keys() {
                for v in &bad {
                    cases.push(json!({ key: v }));
                    let mut over = full.clone();
                    over.insert(key.clone(), v.clone());
                    cases.push(Value::Object(over));
                }
            }
            for args in cases {
                // An import's confirmation would make the next import answer "a dialog is open".
                app.dialog = None;
                let before = snapshot(&app);
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.mcp_call(name, &args)))
                    .unwrap_or_else(|_| panic!("{name} panicked on {args}"));
                calls += 1;
                if out.is_error {
                    assert!(snapshot(&app) == before, "{name} answered with an error on {args} but changed the library");
                }
                check_limits(app.lib.as_ref().unwrap(), name, &args);
            }
        }
        assert!(calls > 1000, "only {calls} calls");
        drop(app);
        let _ = std::fs::remove_dir_all(&root);
    }
}
