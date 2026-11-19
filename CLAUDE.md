# OrangeLink 專案規則

Rust + egui 的 Windows 素材管理程式。下面每條規則都對應一個真的發生過的 bug;給參與開發的人與 AI 助理看。

## 指令

- 檢查:`cargo clippy --locked --all-targets -- -D warnings`(CI 同一條,任何警告都算失敗)
- 測試:`cargo test --locked`。需要 FFmpeg 的測試在沒有 FFmpeg 時印 SKIPPED;設 `ORANGELINK_REQUIRE_FFMPEG=1` 會讓它們失敗而不是跳過(CI 就是這樣跑)
- 執行程式:**只用** `.\scripts\dev-sandbox.ps1`(`-Release` 跑 release 版)。它把 `APPDATA`、`LOCALAPPDATA` 指到 `_test_sandbox\dev`
- 不用 rustfmt(程式碼不是 rustfmt 排的,不要整檔格式化)

## 執行與測試程式

- 不要直接執行 `orangelink.exe`,也不要用 computer use 的 `open_application` 開 OrangeLink:那會讀寫使用者真實的 config 和資源庫。
- 測試資料放 `_test_sandbox\`(已在 .gitignore)或系統暫存資料夾,不要碰 `%APPDATA%\OrangeLink`。

## 子程序

- 一律用 `crate::process::hidden(program)`(FFmpeg、yt-dlp、curl、tar 這類背景工具)或 `crate::process::visible(program)`(使用者要看到的程式)。`clippy.toml` 禁止直接用 `Command::new`。
  - 原因:視窗版沒有主控台,工具要 stdin null、不開視窗,否則 FFmpeg 會卡住等輸入。這段設定以前複製了 8 份,而且彼此不一致。
- 取消工作時要確認子程序真的結束了。在 Windows 上,沒結束的程序會一直握著檔案,檔案就不能刪或改名。

## 路徑(Windows)

- 做安全或範圍判斷前,先用 `importer::dunce_abs` 正規化(`canonicalize` 再去掉 `\\?\` 前綴),再比較。不要拿路徑字串直接比較(`..` 和 `\\?\` 曾經繞過「範圍太大」的檢查)。
- 測試至少涵蓋:`..`、`\\?\` 前綴、8.3 短檔名、結尾的點或空白、`CON` 這類裝置名。

## 外部輸入

外部輸入包括:MCP 工具參數、擴充功能的 HTTP 請求、搜尋語法、命令列、讀進來的檔案(字幕、聊天 JSON)、更新檢查讀到的 `latest.json`。

- 型別或範圍不對時**回傳錯誤**:不要夾到範圍內,不要清空原值(`set_rating("5")` 曾經清除了評分)。
- 只看得到的文字(名稱、標籤)要去掉控制字元和方向控制字元,並限制長度。
- 能用白名單就不要用黑名單。危險副檔名的黑名單已經擴充過兩次。
- 新增 MCP 工具的檢查清單:
  1. 加進 `app/mcp_tools.rs` 的 `registry()`,並設定權限等級。
  2. 每個參數都要驗證。
  3. `fuzz_tests::every_tool_survives_bad_arguments` 會從登錄表自動測新工具:不能 panic、回傳錯誤時不能改到資源庫、名稱和標籤要在上限內。工具有自己特殊的規則時,再加一個專門的測試。
  4. 會跑很久的工具要在 `get_status` 裡回報進度。
  5. 更新 `docs/FEATURES.md` 的 MCP 工具列表。
- 對外部文字切片之前先確認是 char boundary(`#aébbb` 曾經讓 UI 執行緒 panic)。

## 持久化(library.rs、config.rs、備份、刪除)

- 動手前先讀 `docs/persistence-invariants.md`:裡面列出每一條對使用者資料的保證和檢查它的測試,也有一次跑完這些測試的指令。改動不能讓任何一條失效;新增保證時,同時寫測試、加一列。
- 先想清楚失敗的情況:寫到一半失敗、檔案損壞、從備份還原。
- 寫檔要寫暫存檔、`sync_all`、再 rename。內容沒變就不要寫。
- 讀不懂的檔案要保留成 `.broken`,不能被預設值蓋掉。
- 會刪除使用者原始檔的行為,預設一律不刪。

## 背景工作與鎖

- 鎖一律用 `crate::lock(&mutex)`。`clippy.toml` 禁止直接 `Mutex::lock`,因為一個執行緒 panic 不能拖垮其他執行緒。
- 新的背景工作必須:
  1. 出現在狀態列的工作清單裡。
  2. 可以取消。
  3. 切換資源庫時停止,而且結果不能寫進新的資源庫。
  4. 執行緒 panic 時把工作標成失敗,不能永遠停在「進行中」。
- 回覆用 `app/state.rs` 的 `Reply<T>`,有狀態的服務用 `Service<T>`。
- 非同步後端(例如 Media Foundation)回報的狀態可能是舊的。要累積的值(seek 目標之類)自己保存,不要每次重讀。

## UI 執行緒(egui 每一格都會跑 `update()`)

- `update()` 和繪製路徑裡不要做檔案 I/O、不要啟動子程序、不要對所有項目做 O(n) 的計算。
- 需要的話先算好快取起來,用資源庫的 revision 判斷什麼時候要重算。
- 使用者連續的輸入(按住方向鍵、拖曳、打字)要 debounce。

## 更新與發布

- 更新只從本 repo 的 GitHub Releases 取得;zip 的網址由程式自己組出來,`latest.json` 只能指定檔名。
- 簽章驗證(`self_update.rs`,公鑰清單在 repo 根目錄的 `release_keys.txt`)不能拿掉或放寬。公鑰只寫在 `release_keys.txt`,app 和 `tools/sign_release` 都讀這個檔案。私鑰不能進 repo,也不能放進 GitHub secret。

## 修 bug 與 commit

- 修 bug 要附一個「沒有修正就會失敗」的回歸測試。測試名稱用描述行為的句子,沿用現有風格,例如 `tells_gbk_from_big5`。
  - 真的沒辦法寫測試的話,在 commit 訊息寫 `[no-test]` 和原因。`scripts/hooks/commit-msg` 會對沒有測試的修正發出警告;啟用方式:`git config core.hooksPath scripts/hooks`。
- 一個 commit 只做一件事,不要一次塞很多個項目。
- Commit 訊息用英文,第一行描述使用者看得到的行為。
- 功能要用邊界素材驗證,不只是手邊的典型素材:直式影片、GBK 字幕、超過片尾的時間點、空字串、很大的資源庫。
- README 和 `docs/FEATURES.md` 宣稱的能力要有對應的測試或手動測試項目。README 裡的快速鍵表要和 `src/ui/shortcuts.rs` 一致（有測試檢查）。

## 編輯檔案

- `core.autocrlf=true`,工作目錄裡同時有 LF 和 CRLF 的檔案。`.gitattributes` 把 `*.rs`、`*.toml`、`*.yml` 固定成 LF;其他檔案(README 等)跟著 autocrlf 走,讀檔的程式和測試要同時接受 LF 和 CRLF。用腳本改檔時保留原本的換行。在 Windows 上 Python 的 `write_text` 會把 LF 改成 CRLF,要用 `write_bytes`。
