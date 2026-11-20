# 更新紀錄

格式參考 [Keep a Changelog](https://keepachangelog.com/zh-TW/1.1.0/),版本號依 [語意化版本](https://semver.org/lang/zh-TW/)。

## [0.1.7]

### 變更
- 沒有使用者看得到的功能變更;這一版把版本號帶到公開 repo 的第一個正式版。
- `Cargo.toml` 標明 MIT 授權,與 `LICENSE` 一致。
- 簽章與發布流程的維護者說明不再放在公開 repo。

## [0.1.6]

### 變更
- 新版本改從 GitHub Releases 檢查與下載;`latest.json` 以檔名指定 zip,下載只會來自本專案同一版本的 release。
- 以 MIT 授權公開原始碼,發布的 zip 附上 `LICENSE`、`THIRD_PARTY_NOTICES.md` 和所有 Rust 套件的授權全文(`THIRD_PARTY_LICENSES.txt`)。
- 發布版建置時去掉建置機器上的路徑。

### 安全性(自動更新)
- **從 0.1.6 開始,自動更新必須通過 Ed25519 簽章驗證。** 公鑰列在 `release_keys.txt`:一把主金鑰負責簽章,一把備援金鑰離線保存。0.1.5 及更早的版本沒有這個機制;從 0.1.4、0.1.5 到 0.1.6 的這一次,由原本的 Google 雲端硬碟更新管道以 SHA-256 比對完成(自動更新開著時會自動安裝)。
- 0.1.3 及更早的版本(沒有自動下載),以及自行從 0.1.5 之後的原始碼建置的版本,需要手動更新一次到 0.1.6;之後的版本都會從 GitHub 自動更新。
- `latest.json` 沒有簽章、或簽章驗不過時,不顯示 ⬆ 徽章與下載頁連結,只寫進記錄檔。下載後驗證失敗的提示是「請勿安裝,並請回報」,不會引導到下載頁手動更新。
- 下載不讀取使用者的 `_curlrc`,只用 System32 的 `curl`/`tar`(找不到就報錯),並限制重新導向次數與檔案大小;取消下載時等 curl 結束才刪暫存檔。
- 用 UTF-8 BOM 存的 `latest.json` 也能讀;UTF-16 會記錄錯誤。

### 發布流程
- 簽章工具是獨立的 `tools/sign_release`:`keygen` 直接產生以密碼加密的金鑰檔,`sign` 先確認 zip 裡 exe 的版本、金鑰在 `release_keys.txt` 裡,`verify` 用和程式相同的規則驗證 `latest.json` 與 zip,不需要編譯主程式。
- 發布工作流程拆成唯讀的建置工作和只負責建立草稿的工作,不保留權杖、檢查標籤格式、不覆蓋已有的 release;草稿只能剛好有 zip、`SHA256SUMS.txt`、`latest.json`,而且三者互相一致。action 固定到 commit SHA,Rust 版本固定在 `rust-toolchain.toml`,CI 加上 zizmor 檢查。
- `.rs`、`.toml`、`.yml`、`.txt` 固定以 LF 簽出。

### 已知限制
- 自動更新還沒有處理:重放舊版本的有效簽章、套用更新途中中斷、`--apply-update` 參數的限制、解壓後取檔的位置、pre-release 版本號。詳見 SECURITY.md 的「已知限制(自動更新)」。

## 0.1.5 以前

0.1.5 以前的版本是私下發布的,沒有公開的更新紀錄。
