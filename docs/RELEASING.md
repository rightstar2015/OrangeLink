# 發布新版本（維護者用）

這份文件給維護者：怎麼建置不洩漏路徑的版本，以及發布流程。以下指令都在 repo 根目錄執行。開發環境和 commit 規則見 [CONTRIBUTING.md](../CONTRIBUTING.md)，安全性政策見 [SECURITY.md](../SECURITY.md)。

## 不洩漏建置路徑的建置

Rust 會把原始碼的路徑（包括 `%USERPROFILE%\.cargo` 底下相依套件的路徑）寫進執行檔的錯誤訊息。發布用的 zip 由 GitHub Actions 建置，並用 `--remap-path-prefix` 換掉這些路徑。自己建置要給別人用的執行檔時，在 PowerShell 這樣做（路徑含空白時改用 `CARGO_ENCODED_RUSTFLAGS`）：

```powershell
$env:RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=~ --remap-path-prefix=$PWD=."
cargo build --release --locked
```

檢查成品裡沒有使用者名稱或本機路徑（沒有輸出就是乾淨的）：

```powershell
$text = [Text.Encoding]::GetEncoding(28591).GetString([IO.File]::ReadAllBytes("target\release\orangelink.exe"))
[regex]::Matches($text, '[\x20-\x7e]{6,}').Value | Select-String -SimpleMatch $env:USERNAME, $env:USERPROFILE, $PWD.Path
```

## 發布流程

1. 把 `Cargo.toml` 和 `extension/manifest.json` 的 `version` 改成新版本並 commit（測試會檢查兩者一致），再推送同名標籤：

   ```bash
   git tag v0.1.6
   git push origin v0.1.6
   ```

2. GitHub Actions 建置、執行測試，並建立該版本的**草稿** release，附上 zip、`SHA256SUMS.txt` 和還沒簽章的 `latest.json`。標籤必須是 `vX.Y.Z` 而且和 `Cargo.toml` 的版本一致。建置用唯讀的權杖，只有最後建立草稿的步驟能寫入。同一個標籤已經有 release（草稿或已發布）時工作流程會失敗，不會覆蓋檔案；要重建草稿，先刪掉草稿再重跑。
3. 維護者在非公開的環境為草稿裡的 zip 簽署 `latest.json`，換掉草稿裡那份沒有簽章的（簽章流程不在這個 repo 公開）。
4. 確認後發布草稿。發布前 OrangeLink 看不到這個版本；`latest.json` 沒有 `sig`、或簽章驗不過時，OrangeLink 完全不提示這個版本（不顯示 ⬆ 徽章，也不提供下載頁連結），只寫進記錄檔。

### 0.1.6:雲端硬碟上的過渡版（只做這一次）

0.1.5 及更早的版本還是從原本的 Google 雲端硬碟資料夾讀取 `latest.json`，而且沒有簽章驗證：0.1.4、0.1.5 在 `latest.json` 有 `zip` 和 `sha256` 時，只比對 SHA-256 就自動安裝。0.1.6 發布到 GitHub Releases 之後，照下面的順序把同一個 zip 放上雲端硬碟，讓這些版本更新到 0.1.6；之後的版本只走 GitHub，不必再動雲端硬碟。

1. **上傳 zip**：把 GitHub Releases 上的 `OrangeLink-v0.1.6-windows-x64.zip`（同一個檔案，不要重新打包或改名）上傳到雲端硬碟資料夾，記下它的檔案 ID。
2. **下載回來重算 SHA-256**：從雲端硬碟用「知道連結的使用者」的下載網址把 zip 下載回來，重算 SHA-256，必須和本機那份、以及 GitHub 上 `SHA256SUMS.txt` 的數值完全一致（不一致就刪掉重傳，不要繼續）：

   ```powershell
   (Get-FileHash -Algorithm SHA256 .\OrangeLink-v0.1.6-windows-x64.zip).Hash.ToLower()
   ```

3. **最後才換掉 `latest.json`**：在雲端硬碟對原本的 `latest.json` 用「管理版本」上傳新版本（不要另外上傳新檔案，舊版程式只認得原本那個檔案 ID）。內容是舊格式、不含 `sig`：

   ```json
   {"version": "0.1.6", "zip": "<zip 在雲端硬碟的檔案 ID>", "sha256": "<第 2 步算出的 64 個十六進位字元>", "notes": "更新到 0.1.6"}
   ```

   用沒有 BOM 的 UTF-8 寫出，例如在 PowerShell：`[IO.File]::WriteAllText("$PWD/latest.json", $json)`。不要用 Windows PowerShell 5.1 的 `>` 轉向或會加 BOM 的編輯器：舊版程式讀不懂 BOM，會退回只看檔名，只顯示 ⬆ 徽章，不會自動安裝。
4. **分享權限**：zip 和 `latest.json` 對外都只能是「知道連結的使用者：檢視者」，不能給編輯權限。

順序不能反過來：`latest.json` 一換掉，0.1.4、0.1.5 就會開始下載它指定的 zip。

0.1.3 及更早的版本（沒有自動下載），以及自行從 0.1.5 之後的原始碼建置的版本，要手動更新一次：在同一個資料夾放一份簡短的手動更新說明（關閉 OrangeLink → 下載 zip → 比對 SHA-256 → 解壓後用新的 `OrangeLink.exe` 取代舊的；資源庫和設定在 `%APPDATA%\OrangeLink`，不受影響）。
