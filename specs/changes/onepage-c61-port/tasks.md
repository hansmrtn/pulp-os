# onepage-c61-port — Tasks

每項可由新 session 接手；先讀 proposal／spec。依賴不等於可跳過驗證；若改共用 API，需維持已完成項目的建置。第一版預設 Wi-Fi disabled／full refresh。

- [x] T1: 固定 compiler、rust-src、targets 與相容 Espressif dependency set，記錄可重現基線和 smol revision；排除本機 E0514 — satisfies R1, R22, R23；無依賴。
- [x] T2: 建立 OnePage／X4 board selection、target／runner 與錯誤檢查，完成 C61 最小 boot ELF — satisfies R1, R2, R3；依賴 T1。
- [x] T3: 建立 Wi-Fi optional boundary，涵蓋依賴、main／manager／menu，確認離線 artifact 不連入 radio — satisfies R4, R5；依賴 T2。
- [x] T4: 移植 board pin ownership 與 GPIO27 啟動／runtime reset 契約，加入操作順序測試 — satisfies R6, R7；依賴 T3。
- [x] T5: 遷移 SPI／DMA 與 SD wiring、card detect／錯誤處理，移除 C61 對 C3 raw GPIO registers 的依賴 — satisfies R8, R9；依賴 T4。
- [x] T6: 移植 SSD1677 full-refresh／rotation 路徑與有界 BUSY handling，驗證 command trace／strip 資料 — satisfies R10, R11；依賴 T5。
- [x] T7: 移植單 ADC ladder 與 GPIO keys 解碼／debounce／long-press／repeat／startup grace，加入邊界測試 — satisfies R12, R13；依賴 T4。
- [x] T8: 建立 PSRAM 與 internal-memory 分工、失敗處理和配置預算，保存 ELF sections／stack headroom 證據 — satisfies R14, R15, R22, R23；依賴 T2。
- [x] T9: 移植電池取樣／充電控制與 USB detect，記錄 polarity 來源及待實機確認項 — satisfies R16, R17；依賴 T7。
- [x] T10: 建立 C61 session 的 SD 持久化與有效／損壞狀態回歸，避免依賴未配置的 RTC section — satisfies R18, R19, R20；依賴 T5。T10 交付 R18／R20 的持久化與恢復元件（雙槽 SD session、CRC、SD-active 約束）；**R19 的 shutdown／wake 順序由 T11 完成**，app 層接線由 T12 完成。
- [x] T11: 移植 deep-sleep 外設 shutdown／GPIO2 wake，加入 power-order contract 測試 — satisfies R18, R19, R20；依賴 T6, T9, T10。
- [x] T12: 整合完整離線 main／tasks／app lifecycle，完成 C61 與 X4 離線 release link — satisfies R1, R2, R3, R21；依賴 T6, T7, T8, T11。
- [ ] T13: 驗收英文 TXT／EPUB 的分頁／導航／設定／書籤回歸與完整 build／memory matrix — satisfies R21, R22, R23；依賴 T12；使用正式邏輯的最小 host seam 完成軟體回歸；不依賴後續 host-validation change。
- [ ] T14: 交付實機 bring-up 指令與驗收記錄模板，列 boot／PSRAM／SD／ADC／EPD／USB／sleep／電流的未驗狀態 — satisfies R22, R23；依賴 T13；交付文件即可完成此 task，實測是後續 gate。
