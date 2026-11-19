# 參與開發

OrangeLink 是 Rust + [egui](https://github.com/emilk/egui) 寫的 Windows 程式。這份文件說明怎麼建置、測試和送出修改。每條規則背後的原因和更細的規定（子程序、路徑、外部輸入、持久化、背景工作與鎖、UI 執行緒）寫在 [CLAUDE.md](CLAUDE.md)，動手前請先看過；對使用者資料的保證列在 [docs/persistence-invariants.md](docs/persistence-invariants.md)，狀態機的說明在 [STATE_MACHINES.md](STATE_MACHINES.md)。發布和簽章是維護者的工作，見 [docs/RELEASING.md](docs/RELEASING.md)。

## 建置與執行

需要 Rust，版本固定在 `rust-toolchain.toml`（rustup 會自動安裝那一版）。

**執行程式請只用沙盒腳本**：它把 `APPDATA` 和 `LOCALAPPDATA` 指到 `_test_sandbox\dev`，開發和測試時才不會讀寫你真實的設定和資源庫。

```powershell
.\scripts\dev-sandbox.ps1            # debug 版
.\scripts\dev-sandbox.ps1 -Release   # release 版
```

不要直接執行 `orangelink.exe`，也不要用電腦操作工具開 OrangeLink。測試資料放 `_test_sandbox\` 或系統暫存資料夾，不要碰 `%APPDATA%\OrangeLink`。

也可以從命令列指定資源庫與要匯入的檔案。第一個路徑是資源庫就開啟它；是一般檔案或有內容的資料夾，就全部匯入上次開啟的資源庫（不會在素材資料夾裡建立資源庫）；路徑不存在或是空資料夾時，會先詢問是否在那裡建立新的資源庫：

```powershell
.\scripts\dev-sandbox.ps1 -- "D:\MyLibrary" "D:\Footage\refs"
```

## 檢查與測試

```powershell
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

- clippy 是 CI 的同一條指令，任何警告都算失敗。專案不用 rustfmt，請不要整檔格式化。
- 需要 FFmpeg 的測試在找不到 FFmpeg 時會印出 `SKIPPED` 並略過；設定環境變數 `ORANGELINK_REQUIRE_FFMPEG=1` 時改為失敗（CI 就是這樣跑）。
- 處理外部輸入的函式（擴充功能送來的檔名與網址參數、字幕檔、AI 傳來的顏色、匯出的檔名）另有隨機輸入測試（`src/fuzz.rs`）：每次用固定的種子產生兩萬組含特殊字元、多位元組字元與控制字元的字串，確認不會當掉、產生的檔名 Windows 都能用。
- 每次推送到 main 和每個 pull request 都會由 GitHub Actions（`.github/workflows/ci.yml`）安裝 FFmpeg 後執行 clippy 和全部測試，並用 `cargo audit` 檢查依賴套件的已知漏洞。
- 呼叫 Windows API 的 `unsafe` 區塊都要附 `SAFETY:` 註解說明為什麼安全；執行緒共用的鎖一律經過 `crate::lock`，某個執行緒當掉時其他執行緒仍能繼續使用。
- 快速鍵表的內容在 `src/ui/shortcuts.rs`，README 裡的三張表必須和它一致（有測試檢查）。改了快速鍵之後，用 `cargo test print_shortcuts -- --ignored --nocapture` 產生新的表格貼回 README。

## 送出修改

- 修 bug 要附一個「沒有修正就會失敗」的回歸測試，測試名稱用描述行為的句子，例如 `tells_gbk_from_big5`。真的沒辦法寫測試時，在 commit 訊息寫 `[no-test]` 和原因。`scripts/hooks/commit-msg` 會對沒有測試的修正發出警告，啟用方式：

  ```powershell
  git config core.hooksPath scripts/hooks
  ```

- 一個 commit 只做一件事。Commit 訊息用英文，第一行描述使用者看得到的行為。
- 功能要用邊界素材驗證，不只是手邊的典型素材：直式影片、GBK 字幕、超過片尾的時間點、空字串、很大的資源庫。
- README 和 [docs/FEATURES.md](docs/FEATURES.md) 宣稱的能力要有對應的測試或手動測試項目。
- 新增 MCP 工具的檢查清單在 [CLAUDE.md](CLAUDE.md) 的「外部輸入」一節；工具列表更新在 [docs/FEATURES.md](docs/FEATURES.md)。
- 安全性問題請不要開公開的 issue，回報方式見 [SECURITY.md](SECURITY.md)。

## README 的截圖

`docs/images/` 的截圖和 GIF 都用虛構的素材拍成，可以重現：

1. `python scripts/make_demo_media.py C:\demo\media` 畫出圖片、用 ffmpeg 產生測試影片與音訊，並寫出虛構的字幕。需要 Pillow 和 ffmpeg。
2. 用 `.\scripts\dev-sandbox.ps1 -Release -- C:\demo\DemoLibrary` 開啟沙盒資源庫，再匯入 `C:\demo\media`。
3. `.\scripts\capture_window.ps1 -Out shot.png` 只擷取 OrangeLink 視窗；視窗標題和狀態列不會出現使用者名稱時才能放進 repo，所以示範資源庫請放在 `C:\demo` 這類中性路徑。
