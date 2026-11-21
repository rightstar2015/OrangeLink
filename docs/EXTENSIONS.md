# 擴充套件

擴充套件是你電腦上的一個程式資料夾。OrangeLink 在你按下時，把一個影片或音訊交給它，它找出精華片段後交回來，OrangeLink 再把結果變成剪輯工作台裡的**精華候選**。之後照平常審核：↑↓ 跳、Y 保留、N 刪除、9:16 框、輸出預設組、Premiere XML 都一樣。

- 只在本機執行：沒有線上商店，OrangeLink 不會下載或更新擴充。
- 擴充不能直接改資源庫。它只能回傳候選片段，OrangeLink 檢查過才套用，整批可以復原。
- 信任一個擴充，就是信任它的程式碼：它用你的帳號執行，讀寫得到這台電腦上的檔案，和你自己執行任何程式一樣。只加入你自己寫的或信任的擴充。

## 使用

1. 設定 →「擴充套件」→「＋ 加入擴充資料夾…」，選含有 `orangelink-extension.json` 的資料夾。
2. 確認視窗會列出它會執行的每一條指令。按「確定」才算信任；沒信任的擴充不會出現在選單。
3. 在影片上按右鍵 →「影音」→「🧩 …」，或在剪輯工作台的「⚡ 精華」分頁、Ctrl+K 執行。可多選，會依序在背景執行，進度在狀態列的背景工作，可以取消。
4. 完成後的提示按「查看」打開片段分頁。新的候選會取代這個項目還沒保留的候選；已保留或改過名的片段不受影響。擴充有建議片段名稱時，按 Y 保留就用那個名稱。

擴充資料夾裡的 `orangelink-extension.json` 改過（例如 `git pull` 更新之後），要回設定頁按「檢視並信任…」再確認一次才能執行；改了其他檔案則不用。設定頁的「重新讀取」會重新讀所有清單。

取消或切換資源庫時，擴充和它啟動的所有程式（例如它呼叫的 FFmpeg、Whisper）都會結束，結果不會寫進資源庫。關閉 OrangeLink 時也一樣。

## 寫一個擴充

資料夾裡放 `orangelink-extension.json`：

```json
{
  "protocol": 1,
  "id": "demo-highlights",
  "name": "示範精華",
  "version": "0.1.0",
  "check": ["python", "check.py"],
  "actions": [
    {
      "id": "find",
      "label": "找精華候選",
      "applies_to": { "kinds": ["video"], "min_duration_sec": 600 },
      "run": ["python", "adapter.py", "{request}", "{result}"],
      "returns": "clips",
      "options": [
        { "id": "slow", "label": "精確模式（較慢）", "type": "bool", "default": false }
      ]
    }
  ],
  "ui": { "label": "開啟設定介面", "run": ["python", "app.py"] }
}
```

| 欄位 | 說明 |
| --- | --- |
| `protocol` | 目前是 `1`。比 OrangeLink 支援的新時，會顯示「需要較新版的 OrangeLink」。 |
| `id` | `a-z`、`0-9`、`-`，最多 40 字元。同一個 id 只能加入一次。 |
| `name`、`label` | 顯示用，最多 60 字元；控制字元和方向控制字元會被去掉。 |
| `actions` | 1 到 16 個動作。`returns` 目前只有 `clips`。 |
| `applies_to` | `kinds` 是 `video`、`audio`；`min_duration_sec` 以下的項目不顯示這個動作。 |
| `run` | 參數陣列，不經過 shell。第一個是 `python`、`python3`、`py`、`node`（從 PATH 找），或擴充資料夾裡的 `.exe`（相對路徑，不能有 `..`）。`{request}` 和 `{result}` 各出現一次，而且要是整個參數；其他參數不能有 `{` `}`。 |
| `options` | 最多 16 個勾選項（`type` 只有 `bool`），在設定頁調整，執行時放進 request 的 `options`。 |
| `check` | 選用。設定頁的「檢查環境」會執行它並顯示輸出，結束代碼 0 表示通過。 |
| `ui` | 選用。擴充自己的介面，以有主控台視窗的方式啟動，關掉視窗就停止。 |

不認得的欄位、型別不對、數值超出範圍都會讓清單載入失敗，設定頁會顯示原因。

### 執行時

工作目錄是擴充資料夾，環境變數多了 `ORANGELINK_PROTOCOL=1`、`PYTHONIOENCODING=utf-8`、`PYTHONUTF8=1`。`{request}` 換成 OrangeLink 寫好的 `request.json` 路徑：

```json
{
  "protocol": 1,
  "action": "find",
  "options": { "slow": false },
  "item": {
    "id": "…", "name": "直播", "kind": "video",
    "path": "D:/直播/20260101/直播.mp4",
    "original": "D:/直播/20260101/直播.mp4",
    "duration": 22140.5, "width": 1920, "height": 1080,
    "chat_file": "D:/直播/20260101/直播.live_chat.json",
    "subtitles": null
  },
  "workdir": "D:/直播/20260101/直播_demo-highlights"
}
```

- `path` 是要讀的檔案；`original` 是匯入前的原始檔（還在的話），複製進資源庫的項目兩者不同。
- `chat_file`、`subtitles` 是 OrangeLink 已知的聊天室紀錄和字幕，沒有就是 `null`。
- `workdir` 是建議放分析資料的資料夾（不一定已經存在）：原始檔旁邊的 `<檔名>_<擴充 id>`，沒有原始檔時在資源庫的 `extensions/<擴充 id>/<項目 id>`。擴充也可以用自己的位置。

執行中可以在 stdout 印進度行，會顯示在狀態列：

```
@@ {"progress": 0.4, "message": "轉錄中"}
@@ {"step": "transcribe", "status": "done"}
```

`progress` 是 0 到 1。其他輸出只在失敗時寫進 OrangeLink 的記錄（最後 40 行）。

結束時把結果寫到 `{result}` 的路徑，結束代碼 0：

```json
{
  "protocol": 1,
  "message": "找到 3 段",
  "clips": [
    { "start": 3723.5, "end": 3741.0, "name": "開場暴走", "note": "聊天 ×3.2", "score": 8.4, "crop_x": 0.42 }
  ]
}
```

| 欄位 | 說明 |
| --- | --- |
| `start`、`end` | 秒數，`0 ≤ start < end`，不超過影片長度（超過 0.5 秒以內視為進位誤差）。 |
| `name` | 選用，最多 80 字元。保留候選時用它當片段名稱。 |
| `note` | 選用，最多 2000 字元。顯示在片段的 ⓘ，也會成為 Premiere 標記的註解。 |
| `score` | 選用，用來「依精華分數」排序。 |
| `crop_x` | 選用，0 到 1，9:16 直式框的中心位置。 |

最多 200 個片段。只要有一個片段不合格，整份結果都不套用，提示會說是第幾個、哪裡不對。結束代碼不是 0 時，提示顯示最後一行輸出。

一個候選只能是一段連續的範圍。擴充內部有跳剪（多段）時，請回傳整段範圍，把各段寫在 `note`。
