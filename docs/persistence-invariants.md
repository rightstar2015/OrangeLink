# 持久化不變式

OrangeLink 對使用者資料做出的保證,每一條都對應會檢查它的測試。改 `library.rs`、`config.rs`、`importer.rs` 的存檔、備份、刪除、還原程式碼之前,先讀這份清單;改完之後,下面列出的測試都要通過。

新增保證時,同時寫測試、在這裡加一列。沒有測試的保證放在最後的「缺口」,不要寫成已經做到。

執行全部持久化測試:

```bash
cargo test --locked -- library:: importer:: config:: app::integrity:: app::tests::deleting app::tests::the_command_line app::tests::split_by_contents app::mcp_tools::
```

## 寫入

| # | 保證 | 測試 |
| --- | --- | --- |
| W1 | 一個項目寫入失敗,不會丟掉其他項目的變更;失敗的項目在下一次 flush 時重寫 | `library::tests::a_failed_write_keeps_the_rest_and_retries` |
| W2 | 設定檔整個取代,不會留下暫存檔;內容沒變就不寫檔(修改時間也不變) | `config::tests::save_replaces_the_file_whole` |
| W3 | 關閉、重新開啟資源庫之後,變更和垃圾桶狀態都還在 | `importer::tests::changes_survive_reopening_and_trash_round_trips`、`import_dedupes_and_survives_reopen` |
| W4 | 測試不會讀寫使用者真實的 config 和 log(`config_path`、`log::dir` 在測試中指向別處) | `app::mcp_tools::fuzz_tests::every_tool_survives_bad_arguments` 會 drop 一個 `App`,drop 時會存 config |

## 損壞與復原

| # | 保證 | 測試 |
| --- | --- | --- |
| R1 | 讀不懂的 config.json 會先保留成 `config.broken.json`,才改用預設值;不會被靜默蓋掉 | `config::tests::unreadable_config_is_kept_aside` |
| R1b | 讀不到的 config.json(被防毒軟體鎖住、權限不足)在能讀取並另存成 `config.broken.json` 之前不會被覆蓋 | `config::tests::a_config_that_could_not_be_read_is_not_replaced_by_the_defaults` |
| R2 | library.json 壞掉時,資料夾、智慧資料夾、標籤群組、欄位從最新的備份取回,壞檔保留成 `library.json.broken` | `importer::tests::damaged_library_json_comes_back_from_a_backup` |
| R3 | 壞掉的 metadata.json 會被回報,可以從備份還原 | `importer::tests::damaged_metadata_is_reported_and_restored_from_backup` |
| R4 | 資料夾樹有環或父資料夾不見時,仍然打得開,每個資料夾都看得到 | `library::tests::folder_loops_and_lost_parents_are_repaired` |
| R5 | 從備份還原後,不會有項目指向已經不存在的資料夾 | `library::tests::a_restore_drops_folders_that_no_longer_exist` |
| R6 | 匯入時某個檔案損壞、路徑太深或讓解碼器 panic,只有那個檔案失敗,整批匯入繼續 | `importer::tests::a_panic_becomes_a_failed_file`、`damaged_and_deeply_nested_files_do_not_stop_an_import` |

## 備份

| # | 保證 | 測試 |
| --- | --- | --- |
| B1 | 當天的工作在備份裡:有存檔、而且最新的備份超過 `BACKUP_EVERY`(30 分鐘)就補一份,關閉或切換資源庫時也寫一份。新資源庫的備份已經包含預設的智慧資料夾 | `library::tests::todays_work_is_in_a_backup` |
| B2 | 每一天保留第一份和最新一份備份,保留 `BACKUP_DAYS`(10)天;「還原前」的備份另外保留最近 `BEFORE_RESTORE_KEEP`(5)份 | `library::tests::backups_rotate_by_day` |
| B3 | 今天的「還原前」備份不能代替今天的每日備份 | `library::tests::a_before_restore_backup_is_not_the_days_backup` |
| B4 | 用縮圖雜湊從備份還原時,標籤也一起回來 | `importer::tests::dhash_finds_resized_copy_and_backup_restores_tags` |

## 刪除與原始檔

| # | 保證 | 測試 |
| --- | --- | --- |
| D1 | 永久刪除「保留原位置」的項目時,預設保留原始檔;只有使用者打開設定才刪,而且預設送到資源回收筒 | `app::tests::deleting_for_good_says_what_happens_to_originals`、`importer::tests::deleting_forever_handles_originals_as_chosen` |
| D2 | 「保留原位置」匯入不會搬動或複製原始檔 | `importer::tests::linked_import_leaves_files_in_place` |
| D3 | 重新連結時,不會連到同名但大小不同的檔案;同大小的多個候選用雜湊分辨 | `importer::tests::relinking_a_moved_original` |
| D4 | 只有內容真的相同才算重複;大小、開頭、結尾都一樣但中間不同的大檔案不算 | `importer::tests::large_files_differing_in_the_middle_are_not_duplicates`、`app::tests::split_by_contents_keeps_only_true_copies` |
| D5 | 「檢查資源庫」只把真正的殘留資料夾當殘留,項目和讀不懂的項目資料夾都不算 | `library::tests::the_check_finds_leftovers_and_changed_files`、`app::integrity::tests::leftovers_that_became_items_stay` |

## 存取

| # | 保證 | 測試 |
| --- | --- | --- |
| A1 | 同一個資源庫同時只能在一個視窗開啟;關閉後可以再開 | `importer::tests::one_window_per_library` |
| A2 | 命令列的第一個參數不是資源庫時,不會在那裡建立資源庫:現有的檔案或資料夾會匯入,不存在的路徑或空資料夾會先詢問 | `app::tests::the_command_line_never_makes_a_library_unasked` |
| A3 | AI(MCP)呼叫參數不合法而回傳錯誤時,資源庫完全不變;任何呼叫之後,名稱、標籤都不含控制字元或方向控制字元,長度也在上限內,評分在 0–5 | `app::mcp_tools::fuzz_tests::every_tool_survives_bad_arguments` |
| A4 | AI 匯入不能指定整個磁碟、使用者資料夾或它們的上層,任何寫法都一樣(`..`、`\\?\`) | `app::mcp_tools::tests::ai_imports_of_whole_drives_or_home_are_refused`、`other_spellings_of_home_are_too_broad` |

## 缺口(只有程式碼保證,沒有測試)

- **library.json 和 config 在 rename 前會 `sync_all`**(`library.rs` 的 `write_json_durable`):斷電的情況沒辦法在測試裡重現,只能靠審查程式碼。
- **程式當掉之後資源庫鎖會自動釋放**:是 Windows 在程序結束時釋放的。手動用 `Stop-Process -Force` 驗證過一次,沒有自動化測試。
- **切換資源庫時,舊資源庫的背景工作結果不會寫進新的資源庫**:沒有測試。下次修改 `app.rs` 的切庫流程時,應該補上。
