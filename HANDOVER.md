# 交接文件（給接手開發的 AI 環境或人）

寫於 2026-11-25，對應版本 **v0.2.3**。這份文件只講「接手要知道的事」，細節都連到 repo 內既有文件，不重複內容。

## 1. 專案是什麼

OrangeLink 是 Rust + [egui](https://github.com/emilk/egui) 寫的 **Windows 素材管理程式**（類似 Eagle）：匯入圖片、影片、音訊、字型、PDF，用標籤、資料夾、智慧資料夾、搜尋語法整理；影片有剪輯工作台（時間軸、字幕校對、精華候選、片段輸出）；另有 MCP 伺服器（給 AI 整理資源庫）、瀏覽器擴充功能、yt-dlp 下載、Whisper 字幕、外部擴充套件（找直播精華）。介面有繁中、簡中、英、日、韓。

- 功能清單：[docs/FEATURES.md](docs/FEATURES.md)
- 擴充套件格式：[docs/EXTENSIONS.md](docs/EXTENSIONS.md)
- 狀態機：[STATE_MACHINES.md](STATE_MACHINES.md)
- 對使用者資料的保證與對應測試：[docs/persistence-invariants.md](docs/persistence-invariants.md)

### 程式碼地圖

| 位置 | 內容 |
|------|------|
| `src/library.rs`、`config.rs`、`model.rs` | 資源庫與設定的持久化、資料模型（改動前務必讀 persistence-invariants） |
| `src/importer.rs`、`watch.rs`、`export.rs` | 匯入、監看資料夾、輸出 |
| `src/media.rs`、`player.rs`、`mf_player.rs` | FFmpeg 縮圖與預覽、Media Foundation 播放器（Windows） |
| `src/mcp.rs`、`src/app/mcp_tools.rs` | MCP 伺服器與工具登錄表 |
| `src/extension.rs`、`src/app/extensions.rs` | 外部擴充套件機制 |
| `src/process.rs` | 子程序統一入口 `hidden()` / `visible()` |
| `src/app/` | App 狀態與各功能（復原 `undo.rs`、字幕 `subtitle_edit.rs`、精華 `highlights.rs`、背景工作 `jobs.rs` 等） |
| `src/ui/`、`locales/` | 畫面與多語翻譯 |
| `extension/` | 瀏覽器擴充功能 |

## 2. 兩個 repo 的關係

- **`rightstar2015/OrangeLink`（公開，MIT）**：現行開發與 release 都在這裡。**接手後只在這個 repo 做事。**
- **`rightstar2015/OrangeLink-private`**：停在 0.1.5 的舊歷史，只留舊規劃文件（TODO、CODE_REVIEW、IMPROVEMENTS）。當參考，不再開發。

## 3. 建置、測試、CI

先讀 [CLAUDE.md](CLAUDE.md)（每條規則都對應一個真的發生過的 bug）與 [CONTRIBUTING.md](CONTRIBUTING.md)。重點：

- 檢查：`cargo clippy --locked --all-targets -- -D warnings`（任何警告都算失敗）
- 測試：`cargo test --locked`。需要 FFmpeg 的測試沒有 FFmpeg 時印 SKIPPED，設 `ORANGELINK_REQUIRE_FFMPEG=1` 則改為失敗（CI 這樣跑）
- 執行程式只用 `.\scripts\dev-sandbox.ps1`（`-Release` 跑 release 版），它把 `APPDATA`、`LOCALAPPDATA` 指到 `_test_sandbox\dev`。**不要直接執行 `orangelink.exe`**，會動到使用者真實的資源庫
- 不用 rustfmt；`.rs/.toml/.yml` 固定 LF，其他檔案跟 autocrlf
- **Linux 雲端環境無法完整建置**（`rfd` 需要 gtk 開發套件，許多功能只在 Windows 編譯）。在 Linux 只能讀程式碼與跑部分測試；真正的驗證靠 GitHub Actions 的 `windows-latest`（`.github/workflows/ci.yml`：build、clippy、測試、安裝 FFmpeg）
- 子程序一律 `crate::process::hidden/visible`、鎖一律 `crate::lock`，`clippy.toml` 會擋直接用法

## 4. 發布流程（重要，順序不能亂）

完整步驟在 [docs/RELEASING.md](docs/RELEASING.md)。摘要：

1. 先合併一個 **「Release X.Y.Z」PR**：改 `Cargo.toml`、`Cargo.lock`、`extension/manifest.json` 的版本，補 `CHANGELOG.md` 該版區段；commit 訊息含 `[no-test] version bump and changelog only`。**版本與標籤不一致時 Release 工作流程會直接失敗**（v0.2.2 就發生過）。
2. **由使用者自己在本機推標籤**：`git fetch origin && git tag vX.Y.Z origin/main && git push origin vX.Y.Z`。雲端 AI session 推標籤會被代理拒絕（403），不要嘗試。
3. **不要先在 GitHub 網頁建 release**：`release.yml` 發現該標籤已有 release 就會失敗。
4. 工作流程會建置、跑測試，並建立**草稿** release，附 zip、`SHA256SUMS.txt`、`latest.json`。確認 zip 與 `latest.json` 內容正確後，才由使用者手動發布。
5. **沒有簽章**（PR #11 移除）。更新機制只讀 `latest.json` 的版本號，**只提示、不自動下載安裝**，使用者手動到 Releases 頁下載；提示開啟的網址由程式依版本號組出來，不採用 `latest.json` 內的網址。0.1.6–0.1.9 的使用者收不到通知，要手動下載一次。

## 5. 目前狀態

- 最新版本 **v0.2.3**（main 最新 commit `03bbc35 Release 0.2.3`）。
- **v0.2.2 QA 的 A–D 批已全部完成並併入 0.2.3**：
  - A 復原／還原的結構一致性（BUG-01、02、13、14）
  - B 預覽 seek 狀態（BUG-05、09、10、20 的 FFmpeg 部分）
  - C 字幕復原與預覽快捷鍵（BUG-04、06、07；08 只修點外面關閉）
  - D 小修一包（BUG-12、22、17、18、15 的 `capture`）
- 完整 QA 覆核與每個 bug 的判斷：`/mnt/project-files/orangelink/qa-0.2.2-verified.md`（本機專案資料夾；內容不在 repo 內）。

## 6. 尚未完成的事項

**E 批：需要使用者在 Windows 實機測試才能做**
- BUG-03：`mf_player.rs` 收到指令就 `continue`，指令間隔小於 8 ms（144 Hz 螢幕拖曳時間軸）時 seek 與取影格會被餓死。修法是一次 `try_recv` 清空佇列再往下走。**先請使用者確認真的有感再改。**
- BUG-20 的 MF 半邊：`Play` 在 `seek_to` 套用之前就執行。
- BUG-11：`scrub::grab`／`show_still` 的子程序沒納管，預覽關閉時要停止。實際鎖檔窗口很小，優先度低。
- BUG-08 的 IME：Modal 在微軟注音組字時 Enter 是否誤觸，只能實測。

**UI 小改（QA 判定值得做，尚未做）**
- 搜尋框寬度寫死的 `620.0`（`src/ui/toolbar.rs:241`）改用量測寬度
- 篩選視窗內與條件列兩個「全部清除」行為不一致
- 多選時的 `multi_action_bar` 在窄視窗超出（`grid.rs`），改 wrap 或「更多」選單
- 預設 20 個智慧資料夾全展開，可改預設摺疊（只影響新庫）

**不用做**：BUG-19、21，以及 16 的路徑部分。

**功能規劃（使用者還沒選要做哪些）**
- 整合規劃：`/mnt/project-files/orangelink/feature-uiux-plan.md`，階段 0 快速修正 → 1 長直播剪輯工作台 → 2 好辨識的素材 → 3 導覽一致性 → 4 創作者加值。0.2.0 已完成大部分階段 1。
- 原始提案：同資料夾的 `claude-proposal-features-uiux.md`、`gemini-proposal.md`。
- 外部擴充套件：0.2.1 已有（子程序 CLI + `orangelink-extension.json` + `@@ {json}` 進度行 + result.json，結果成為可復原的精華候選）。**尚未做**：MCP 的 `run_extension`、輸出 MP4/SRT 的 render 類動作；也還沒在使用者真實 Windows 上完整驗證。機制是通用的，公開文件不要點名使用者自用的 `stream-highlights-pipeline`（它的轉接器在它自己的 repo）。
- 舊 TODO：字幕校對與編輯（0.2.0 已做）、長影片 Whisper 測試與進度、排程 AI 自動標籤。

## 7. 工作慣例

- **一個討論串／一個 PR 只做一件事**，做完就收；一個 commit 只做一件事，commit 訊息用英文，第一行寫使用者看得到的行為。
- 修 bug 一定附「沒修會失敗」的回歸測試，名稱用描述行為的句子；真的不能測才在訊息寫 `[no-test]` 與原因（`git config core.hooksPath scripts/hooks` 會警告）。
- 使用者已授權的流程：**修一批 → CI 通過 → 自我審查 → 合併 → 繼續下一批**。PR 用 rebase 合併，維持一個 commit 一件事。每批合併後，另開獨立覆核再做下一批。
- 對外部輸入（MCP 參數、HTTP 請求、檔案內容）型別或範圍不對就**回傳錯誤**，不要夾進範圍；新增 MCP 工具照 CLAUDE.md 的檢查清單。
- 功能以邊界素材驗證：直式影片、GBK 字幕、超過片尾的時間點、空字串、很大的資源庫。
- README 與 `docs/FEATURES.md` 宣稱的能力要有測試或手動測試項目；README 的快速鍵表要與 `src/ui/shortcuts.rs` 一致。
- **翻譯類工作用小模型**（例如 haiku 等級）即可，不必動用大模型。
- 回覆使用者用繁體中文（台灣），簡短，不要整份貼檔案。
- 使用者是「橘貓」（GitHub `rightstar2015`），他負責在 Windows 實機測試、推標籤與發布 release。
