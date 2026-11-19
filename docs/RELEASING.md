# 發布新版本（維護者用）

這份文件給維護者：怎麼建置不洩漏路徑的版本、發布、簽章，以及簽章金鑰的保管。以下指令都在 repo 根目錄執行。開發環境和 commit 規則見 [CONTRIBUTING.md](../CONTRIBUTING.md)，安全性政策見 [SECURITY.md](../SECURITY.md)。

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
3. 在發版電腦下載草稿裡的 zip（不要改檔名），用 `tools/sign_release` 產生簽好的 `latest.json`。它會先確認 zip 裡 `OrangeLink/OrangeLink.exe` 的版本就是要簽的版本，再詢問私鑰的密碼。`-o` 直接寫檔，寫出的是不含 BOM 的 UTF-8；不要用 PowerShell 的 `>` 轉向，它會改成 UTF-16 或加上 BOM。需要的話填上 `notes`，再用 `verify` 驗證一次（它和程式用同樣的規則讀 `latest.json`，用 `release_keys.txt` 裡的公鑰驗簽章，並比對 zip 的檔名、SHA-256 和 exe 版本；不需要私鑰，也不會編譯主程式），通過後換掉草稿裡的那份：

   ```bash
   cargo run --release --locked --manifest-path tools/sign_release/Cargo.toml -- sign OrangeLink-v0.1.6-windows-x64.zip 0.1.6 -o latest.json
   ```

   ```bash
   cargo run --release --locked --manifest-path tools/sign_release/Cargo.toml -- verify latest.json OrangeLink-v0.1.6-windows-x64.zip
   ```

   ```bash
   gh release upload v0.1.6 latest.json --clobber
   ```

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

## 簽章金鑰

發版用兩把金鑰，公鑰都列在 repo 根目錄的 `release_keys.txt`（每行一把，64 個十六進位字元），程式和簽章工具都讀這個檔案：

- **主金鑰**：每次發版都用它簽章，加密的私鑰檔放在簽章帳號的 `%USERPROFILE%\.orangelink\release-signing.key.enc`。
- **備援金鑰**：平常不用，**離線保存**（例如加密隨身碟，或不連網的電腦），**不放在簽章帳號裡**。只有主金鑰遺失或外洩時才拿出來簽章，並依 [SECURITY.md](../SECURITY.md) 的流程換掉主金鑰。

私鑰只以密碼加密的形式存在，不在專案裡，也不放進 GitHub。密碼只在簽章時輸入，不會存下來。簽章工具 `tools/sign_release` 是獨立的小 crate，直接相依只有 `ed25519-compact` 和 `sha2`；加上它們的相依套件，在 Windows 上共編譯 12 個小套件，其中 `generic-array` 和 `getrandom` 有 build script，沒有 proc-macro。主程式那幾百個套件不會在這裡建置。

### 在獨立的簽章帳號簽章

**簽章請在一個不用來開發主程式的獨立 Windows 帳號進行，並使用乾淨的 `CARGO_HOME`。** 在平常開發的帳號裡，每次建置或測試主程式都會執行幾百個套件的 build script 和 proc-macro；它們可以改寫 `~/.cargo` 底下已解壓的套件原始碼（cargo 只在下載時檢查 checksum）或簽章工具編出的 exe，等下次簽章時連同密碼和私鑰一起帶走。私鑰檔加密只能擋住「直接讀檔」。做法：建立一個只用來簽章的 Windows 帳號，主金鑰的私鑰檔只放在那個帳號；在那裡用新的 `CARGO_HOME` 從乾淨的 clone 建置 `tools/sign_release`，不要在那個帳號建置或測試主程式：

```powershell
$env:CARGO_HOME = "$env:LOCALAPPDATA\orangelink-sign-cargo"
git clone https://github.com/rightstar2015/OrangeLink.git; cd OrangeLink
cargo run --release --locked --manifest-path tools/sign_release/Cargo.toml -- verify latest.json OrangeLink-v0.1.6-windows-x64.zip
```

### 產生金鑰

`keygen` 直接把新金鑰以密碼加密寫進檔案，過程中不會有明文的私鑰檔；最後印出一行公鑰，貼進 `release_keys.txt`：

```powershell
# 主金鑰，在簽章帳號
cargo run --release --locked --manifest-path tools/sign_release/Cargo.toml -- keygen
# 備援金鑰，直接寫到離線的媒體（例如 E:）
cargo run --release --locked --manifest-path tools/sign_release/Cargo.toml -- keygen -o E:\orangelink-release-backup.key.enc
```

用備援金鑰簽章時加上 `--key <檔案>`。`check <檔案>` 會要求輸入那個檔案的密碼、實際解密並試簽，印出公鑰和它是否在 `release_keys.txt` 裡；`seed` 那一行損毀時會報錯。`pubkey [<檔案>]` 只讀檔案裡的公鑰那一行、不解密，不能用來確認檔案可用。

### 金鑰的保管

- **加密的私鑰檔至少備份兩處**（例如兩個不同的離線媒體），主金鑰和備援金鑰都一樣。
- **密碼放在密碼管理器**，用密碼管理器隨機產生（20 個字元以上），不要用自己想的句子。私鑰檔和密碼任何一個遺失，那把金鑰就不能再用。
- **刪除任何金鑰檔之前，先確認至少兩處備份都在，而且能用**：每一份備份都用 `check <備份檔>` 檢查，要顯示 `OK` 而且「in release_keys.txt: yes」。
- 換掉金鑰時，依 [SECURITY.md](../SECURITY.md) 的流程更新 `release_keys.txt`。兩把金鑰都遺失時，之後的版本就無法自動更新，要換新金鑰並請使用者手動下載一次新版。
