# 第三方聲明

OrangeLink 本身以 MIT 授權釋出(見 [LICENSE](LICENSE))。下面列出來自其他專案的程式碼與內容,以及它們的授權。

## 改寫自其他專案的程式碼

### Prime Files

來源:<https://github.com/CakeDev4k/prime-files>(MIT 授權)。下列檔案改寫自 Prime Files,每個檔案開頭也註明了來源與版權:

| OrangeLink | Prime Files |
| --- | --- |
| `src/shell_thumb.rs`(Windows Shell 縮圖) | `crates/nohrs-pages/src/explorer/thumbnails.rs` |
| `src/shell_menu.rs`(Windows 右鍵選單) | `crates/nohrs-pages/src/shell_menu.rs` |
| `src/pdf.rs`(PDF 預覽) | `crates/nohrs-pages/src/explorer/pdf.rs` |
| `src/mf_player.rs`(Media Foundation 播放) | `crates/nohrs-pages/src/explorer/media.rs` |
| `src/self_update.rs`(自動更新的流程) | `crates/nohrs-pages/src/updater.rs` |
| `src/grouping.rs`(分組顯示) | `crates/nohrs-pages/src/explorer/grouping.rs` |
| `src/ui/smooth_scroll.rs`(平滑捲動) | `crates/nohrs-pages/src/explorer/smooth_scroll.rs` |
| `src/hotkey.rs`(全域快捷鍵的註冊方式) | `crates/nohrs-launcher/src/hotkey.rs` |

```
MIT License

Copyright (c) 2025 Nohrs contributors
Copyright (c) 2026 Prime Files contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## 編譯進執行檔的 Rust 套件

執行檔包含 `Cargo.lock` 列出的 Rust 套件(crate),授權以 MIT、Apache-2.0 為主,另有 BSD、Zlib、ISC、Unicode-3.0、BSL-1.0、CC0 等寬鬆授權,沒有 GPL 類授權。各套件的授權全文在發布 zip 的 `THIRD_PARTY_LICENSES.txt`,由 `examples/third_party_licenses.rs` 在建置發布版時產生:

```bash
cargo metadata --format-version 1 --locked --filter-platform x86_64-pc-windows-msvc | cargo run --example third_party_licenses > THIRD_PARTY_LICENSES.txt
```

其中值得一提的:

- **egui 預設字型**(`epaint_default_fonts`,內嵌在執行檔中):Hack(MIT 與 Bitstream Vera License)、Noto Emoji(SIL Open Font License 1.1)、emoji-icon-font(MIT,Copyright (c) 2014 John Slegers)、Ubuntu Light(Ubuntu Font Licence 1.0)。授權全文同樣在 `THIRD_PARTY_LICENSES.txt`。
- **配色**:以 [Catppuccin](https://github.com/catppuccin/egui)(`catppuccin-egui`,MIT)為基礎。

## 不隨附、由使用者自行安裝的外部程式

OrangeLink 不打包下列程式,只在使用者要求時從官方來源下載或呼叫已安裝的版本,它們各自依自己的授權散布:

| 程式 | 取得方式 | 授權 |
| --- | --- | --- |
| FFmpeg | winget(`Gyan.FFmpeg`)或使用者自行安裝 | LGPL-2.1+ / GPL-2.0+(依建置而定) |
| yt-dlp | 從 yt-dlp 的 GitHub Releases 下載並比對官方 SHA-256 | Unlicense |
| deno | winget | MIT |
| Whisper 模型(`ggml-*.bin`) | 從 Hugging Face `ggerganov/whisper.cpp` 下載並比對 SHA-256 | MIT |

## 商標

Adobe 與 Premiere Pro 是 Adobe 的商標;YouTube 是 Google 的商標;Windows 是 Microsoft 的商標。OrangeLink 與這些公司沒有隸屬關係。
