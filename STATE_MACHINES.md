# OrangeLink 的狀態機

OrangeLink 裡很多功能其實都是狀態機：資源庫有「沒開／開啟中／已開啟」，背景工作有「閒置／執行中／完成或失敗」。早期這些狀態分散在 `App` 的好幾個 `Option`／`bool` 欄位裡，有些欄位組合根本沒有意義，例如同時有「伺服器執行中」和「啟動失敗的錯誤訊息」，或者原圖貼圖屬於 A、失敗旗標卻屬於 B。這份文件列出程式裡所有的狀態機：哪些已經改成明確的型別，哪些暫時保留原樣、原因是什麼。

純邏輯的狀態型別放在 `src/app/state.rs`。這個檔案不依賴 egui 或 Windows，可以單獨測試：

```bash
cargo test state::
```

---

## 新增或修改狀態時的規則

1. **一組互斥的狀態用一個 `enum` 表示**，不要再用 `Option` 加 `bool` 拼湊。判斷方式：如果兩個欄位不該同時是 `Some`，它們就應該是同一個 enum 的不同變體。
2. **轉換邏輯放在狀態型別自己的方法裡**（例如 `Service::sync`、`FullImage::finish`），`App` 只負責 I/O：開執行緒、建立貼圖、顯示提示。只要用得上泛型（例如貼圖用 `T`），就把型別放進 `state.rs` 並補上測試。
3. **背景執行緒的回覆一律用 `state::poll(&rx)` 讀取**，不要用 `rx.try_recv().ok()`。後者分不出「還在做」和「執行緒已經當掉」，後一種情況會讓工作永遠顯示執行中，排在後面的佇列也會跟著卡住。`Reply::Lost` 要讓工作以失敗結束，錯誤訊息用 `state::LOST`。
4. **新的背景工作要接上四個地方**：
   - `jobs.rs` 的 `background_jobs`：狀態列會顯示這個工作，AI 也能從 `get_status` 看到。新的 `JobKind` 要在 `key()` 裡給一個固定的名稱，測試會檢查名稱不重複。
   - `cancel_job`：可以從狀態列取消。
   - `library_jobs`：切換資源庫時，提示訊息會列出這個工作。
   - `stop_library_jobs`：切換資源庫時取消這個工作，結果不會寫進下一個資源庫。
5. **AI 會用到的非同步工具**（立即回傳、在背景執行的工具）要在說明和回傳文字裡寫「用 get_status 查看進度」。

---

## 一、已改成明確型別的狀態機

### 1. 本機服務：MCP 伺服器、瀏覽器擴充功能接收、全域快捷鍵（`state::Service<T>`）

改之前，每個服務都用兩個欄位表示：`mcp` 加 `mcp_error`、`web` 加 `web_error`、`hotkey` 加 `hotkey_error`。三個 `sync_*` 函式各自重複寫了一遍「先停掉舊的，再啟動新的」。

```text
            sync(Some(設定)), 啟動成功
  Off ─────────────────────────────────▶ Running(T)
   │ ▲                                       │
   │ └────────── sync(None)（關閉）──────────┤
   │              設定改變：先釋放舊的連接埠／快捷鍵，再啟動新的
   └── sync(Some(設定)), 啟動失敗 ──▶ Failed(原因) ── sync(..) 一定重試 ──▶ …
```

- 三個服務共用 `Service::sync`，「先停掉舊的再啟動新的」這個順序寫在 `sync` 裡，並且有測試保證。
- **使用者看得到的改變**：設定視窗裡，啟動失敗的服務旁邊多了「重試」按鈕（例如關掉佔用連接埠的程式之後按一下）。關閉一個啟動失敗的服務時，錯誤訊息會一起清掉。

### 2. 預覽的原圖與 GIF 影格（`state::FullImage<T>`）

改之前分散在 5 個欄位：`full`、`full_failed`、`full_pending`、`full_frames`、`anim_start`。

```text
  Idle ──request(id)──▶ Loading(id) ──解碼完成、預覽還在這個項目──▶ Ready { id, frames }
   ▲                       │       └──解碼失敗、預覽還在這個項目──▶ Failed(id)
   │                       └──解碼完成時預覽已經換到別的項目──▶ Idle
   └── close()：Ready／Failed 釋放貼圖；Loading 保留，預覽回到同一個項目時直接沿用結果
```

- 畫面只會顯示目前項目的原圖。GIF 依時間選影格的邏輯集中在 `state::animation_frame`，預覽和懸停預覽共用。
- **使用者看得到的改變**：預覽切換到無法解碼的檔案（例如影片）時，會立刻釋放上一張原圖的貼圖。懸停預覽 GIF 在一輪播完的瞬間，不會再閃回縮圖。

### 3. 新版提示（`app::UpdateState`）

改之前分散在 4 個欄位：`update_rx`、`new_version`、`update_download`、`update_ready`。0.1.8 起不再自己下載和安裝，只剩檢查和提示。

```text
  Checking ──沒有新版，或檢查沒有回應──▶ Latest
     └──有通過簽章的新版──▶ Available（⬆ 徽章；開啟「檢查新版本並詢問」時跳出「發現新版本」）
```

- **使用者看得到的改變**：有新版時，⬆ 徽章和「發現新版本」視窗都可以開啟該版本的 GitHub release 頁面，手動下載。

### 4. 匯入（`app::ImportRun`，存在 `App::import: Option<ImportRun>`）

改之前分散在 7 個欄位：`import_rx`、`import_progress`、`import_stats`、`import_errors`、`import_new`、`import_cancel`、`import_silent`。

```text
  None ──start_import──▶ Some(ImportRun) ──Imported／Duplicate／Failed／Progress──▶（累計數量）
    ▲                        │ 取消：匯入器提早停止，仍然會送出 Finished
    └── Finished：顯示提示（「查看」剛匯入的項目）；切換資源庫時直接丟棄 ──┘
        匯入器沒送 Finished 就結束（當掉）：以目前收到的結果結束這次匯入，並記一筆失敗
```

- `import_running()` 等於 `import.is_some()`，不可能再出現「欄位說閒置、其他欄位卻還留著上一次的資料」這種組合。
- **使用者看得到的改變**：匯入器當掉時，不會再讓之後的每一次匯入都顯示「匯入進行中，請稍候」。

### 5. 就地改名：素材名稱與片段名稱（`state::InlineEdit`）

改之前用 tuple 表示：`(String, String, bool, bool)` 和 `(String, String, bool)`，從程式碼看不出每個欄位的意思。

```text
  None ──F2／點名稱──▶ Some { focused: false } ──第一幀：取得焦點並全選──▶ Some { focused: true }
                                               ──失去焦點──▶ 確認改名（按 Esc 則取消）──▶ None
```

### 6. 背景執行緒的回覆（`state::Reply<T>` 與 `state::poll`）

不是單一功能，而是所有背景工作共用的一層。之前用 `try_recv().ok()` 讀取回覆，執行緒當掉（panic）時，工作會永遠停在執行中。現在改用 `Reply::Lost` 判斷，工作會以失敗結束，佇列繼續處理下一個：

| 工作 | 之前當掉的後果 | 現在 |
| --- | --- | --- |
| 開啟資源庫 | 永遠停在「正在開啟資源庫…」 | 顯示「無法開啟資源庫」 |
| 輸出片段 | 輸出佇列卡住 | 這個片段失敗（紅框提示），繼續下一個 |
| 整場處理 | 佇列卡住 | 這個項目失敗，繼續下一個 |
| 音量分析 | 一直顯示分析中 | 顯示錯誤 |
| 語音轉字幕 | 佇列卡住 | 這個項目失敗，繼續下一個 |
| yt-dlp 下載 | 一直顯示下載中 | 這個下載失敗 |
| 找出重複／相似 | 工作永遠存在 | 紅框提示失敗 |
| 匯入 | 之後無法再匯入 | 見第 4 項 |
| 檢查與下載更新 | 一直顯示檢查中或下載中 | 回到可重試的狀態 |

### 給 AI 助理用的狀態：MCP 工具 `get_status`

上面這些狀態都整理成一個唯讀的 MCP 工具 `get_status`。沒有開啟資源庫時也能使用。回傳內容包括：

- 資源庫：`open`、`loading` 或 `none`
- `jobs`：執行中的背景工作，每項有 `kind`、名稱、進度、剩餘秒數、能不能取消
- 預覽中的項目、選取的項目
- 是否允許 AI 修改資源庫
- 更新狀態

`import_files`、`export_clip`、`generate_subtitles`、`find_highlight_candidates` 都會立即回傳，回傳文字裡會提示用 `get_status` 查看進度。之前 AI 沒辦法知道這些工作什麼時候完成。資源庫開啟中時呼叫其他工具，錯誤訊息會說明「正在開啟，請稍候」，而不是「沒有開啟資源庫」。

---

## 二、保留原樣的狀態機

這些狀態機已經是明確的 enum，或者規模很小、轉換集中在一兩個函式裡。改寫的風險比好處大，所以先保留原樣。之後要改時，請依照前面的規則。

| 狀態機 | 欄位 | 狀態 | 為什麼保留 |
| --- | --- | --- | --- |
| 資源庫生命週期 | `lib`、`lib_loading`、`pending_import` | 沒開 → 開啟中 → 已開啟 | `lib` 有數百處使用。轉換只發生在 `switch_library` 和 `poll_library_loading` 兩個函式裡，`get_status` 也已經回報這個狀態。之後可以改成 `LibraryState` enum |
| 預覽開啟去抖動 | `viewer`、`viewer_loading`、`pending_media` | 已開啟 → 停留 35 ms 後才載入 → 已載入 | 轉換集中在 `open_viewer_at` 和 `finish_viewer_open` |
| 播放引擎 | `player::Backend` | Media Foundation 失敗 → 從同一個位置改用 FFmpeg | 已經是 enum |
| 工作佇列 | 輸出片段（`clip_rx`、`clip_queue`）、語音轉字幕（`whisper.queue`、`running`）、整場處理（`stream_queue`、`stream_run`）、yt-dlp（`queue`、`running`、`run_queue`） | 閒置 → 執行一個並有 N 個等待 → 閒置 | 四個佇列的形狀相同，執行緒當掉時的處理已經補上。之後可以合併成一個共用的 `WorkQueue` |
| 懸停預覽 | `hover_since`、`preview` | 游標停留 → 預覽（GIF 或播放器） | 只在 `update_hover_preview` 裡轉換 |
| 時間軸縮圖 | `scrub::Scrub` | 一次擷取一格，`wanted` 記住下一格 | 已經封裝成獨立模組 |
| 關閉流程 | `guard_close`、`force_close`、`update_relaunch` | 要求關閉 → 儲存 →（失敗時）確認對話框 → 關閉並安裝更新 | 必須配合 egui 的 viewport 指令，轉換集中在 `guard_close` 和 `Drop` |
| 設定套用去抖動 | `settings_apply_at` | 變更 → 0.5 秒後套用 | 只有一個欄位 |
| 智慧資料夾計數 | `smart_counts`、`smart_counts_due` | 修改 → 停止修改一陣子後重算 | 以版本號判斷，只有一個函式 |
| 全域快捷鍵叫出 | `summoned`、`focus_search` | 叫出 → Enter／Esc 送回原本的程式 | 只有兩個 bool，轉換集中 |
| 拖曳 | `marquee`、`clip_drag`、`dragged_out`、`release_pointer` | 框選、拖曳片段邊緣、拖出視窗 | 都是在一次拖曳期間有效的 `Option` |
| 預覽中的暫時狀態 | `mark`、`crop_frame`、`seek_flash`、`slideshow` | 入點／出點、9:16 框、跳轉提示、幻燈片 | 彼此獨立，都是單一 `Option` |
| 監看資料夾 | `watcher`、`watch_ready`、`watch_scan_queue`、`last_watch_scan` | 事件 → 檔案穩定後匯入；定期掃描作為備援 | 要等匯入閒置才處理，已經依賴 `ImportRun` |
| 字幕索引 | `subtitle_index` | 背景補建 → 完成 | 已經封裝 |
| yt-dlp／Whisper 視窗的工具檢查 | `check_rx`、`install_rx`、`probe_rx`、`filter_rx`、`download_rx` | 檢查中 → 有結果 | 只影響視窗內顯示的文字 |
