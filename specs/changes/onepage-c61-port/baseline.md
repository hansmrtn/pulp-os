# onepage-c61-port 可重現基線（T1）

日期：2026-10-04。Pulp 基底 commit `3bb911af`（branch `onepage`）。以下全部是在本機實際執行的結果；沒有任何硬體驗收。

## 1. 工具鏈（已固定於 `rust-toolchain.toml`）

```toml
[toolchain]
channel    = "nightly-2026-09-22"
profile    = "minimal"
components = ["rust-src", "rustfmt", "clippy"]
targets    = ["riscv32imc-unknown-none-elf", "riscv32imac-unknown-none-elf"]
```

| 項目 | 值 |
|---|---|
| rustc | `1.100.0-nightly (1303417c4 2026-09-21)`，LLVM 23.1.1 |
| cargo | `1.100.0-nightly (495c385d0 2026-09-16)` |
| 編譯方式 | `.cargo/config.toml` 的 `[unstable] build-std = ["alloc","core"]`（需 nightly）；core／alloc 由 pinned toolchain 的 `rust-src` 重新編譯，不依賴預編譯 std |
| X4 target | `riscv32imc-unknown-none-elf`（`.cargo/config.toml` 預設 target，未改） |
| C61 target | `riscv32imac-unknown-none-elf`（T2 起由 board selection 選用） |

選 nightly 的原因：`build-std` 是 `-Z` 功能，`channel = "stable"` 會隨時間漂移（本機即因此與 rust-std 版本不一致），且 stable cargo 本不接受 `[unstable]`。釘住日期後 compiler、rust-src、cargo 同一份。升級 nightly 日期時必須同時重跑第 5 節全部指令。

### E0514 根因與排除

- 重現：`cd ../smol-epub && cargo +stable check --offline --target riscv32imac-unknown-none-elf`
  → `error[E0514]: found crate core compiled by an incompatible version of rustc`，
  `crate core compiled by rustc 1.98.1 (48a229cea 2026-09-01): ~/.rustup/toolchains/stable-.../lib/rustlib/riscv32imac-unknown-none-elf/lib/libcore-*.rmeta`，而 `stable` 的 rustc 是 1.99.0。
- 根因：本機 `stable` toolchain 目錄已被汙染——rust-std 的 riscv 目標檔是舊版 1.98.1，compiler 卻升到 1.99.0；`rustup` 要補裝 `rust-std-riscv32imc-unknown-none-elf` 時又因舊 manifest 衝突而 rollback（`detected conflict: lib/rustlib/manifest-rust-std-riscv32imc-unknown-none-elf`）。
- 排除：repo 改為固定的 `nightly-2026-09-22`，使用該 toolchain 自己乾淨的 rust-src 以 build-std 重編 core／alloc，完全不碰 `stable` toolchain、不使用 `/tmp` 產物、不設 `RUSTC_BOOTSTRAP`。本機 `stable` 毀損狀態未被修改（若要清理：`rustup toolchain uninstall stable && rustup toolchain install stable`，非本 repo 的需求）。
- 網路需求：首次進 repo 時 rustup 會依 toolchain 檔補裝 `clippy` 與 `rust-std-riscv32imc`（約數十 MB；本次已實際下載完成）。

## 2. smol-epub（path dependency，Cargo.lock 不固定）

- 路徑：`../smol-epub`（`Cargo.toml` 的 `smol-epub = { path = "../smol-epub", features = ["async"] }`）
- revision：`832609d967d4d06452dd49d35b2c9aeb578612de`（2026-03-05，`perf improvements`），工作樹乾淨（`git status --short` 無輸出）。
- 複現：`git -C ../smol-epub checkout 832609d967d4d06452dd49d35b2c9aeb578612de`。與 `specs/changes/README.md` 的固定研究起點一致。
- 其他 git 依賴由 Cargo.lock 固定：`embedded-sdmmc 0.9.0` = `git+https://github.com/hansmrtn/embedded-sdmmc-rs?branch=async#0bf12548d1144b0e2b06b290acde3e4bb46cd91b`。

## 3. Dependency 基線

### 3a. 目前 X4（esp32c3）鎖定版本——T1 **未更動** Cargo.toml／Cargo.lock

| crate | locked |
|---|---|
| esp-hal | 1.0.0 |
| esp-rtos | 0.2.0 |
| esp-alloc | 0.9.0 |
| esp-radio | 0.17.0 |
| esp-radio-rtos-driver | 0.2.0 |
| esp-bootloader-esp-idf | 0.4.0 |
| esp-backtrace | 0.18.1 |
| esp-println | 0.16.1 |
| embassy-executor | 0.9.1 |
| embassy-time | 0.5.0 |
| embassy-net | 0.8.0 |
| embassy-sync | 0.7.2（另有 0.6.2 傳遞依賴） |

### 3b. C61 目標依賴組（與 esp-hal 1.2 一致、可解析）

來源為 `specs/references/onepage-wifi-support.md`；本 task 另外實際驗證「這組版本在此 pinned toolchain 下能解析並對 `riscv32imac-unknown-none-elf` 完成 `cargo check`」。這是版本相容性的驗證，不是 C61 移植；C61 feature 與 Cargo 結構由 T2／T3 加入。

| crate | 解析版本 | features |
|---|---|---|
| esp-hal | `=1.2.0` | esp32c61, unstable |
| esp-rtos | `=0.4.0` | esp32c61, embassy, esp-radio, esp-alloc |
| esp-alloc | `=0.11.0` | esp32c61 |
| esp-radio | `=1.0.0-beta.1` | default-features=false; esp32c61, wifi, esp-alloc |
| esp-bootloader-esp-idf | `=0.6.0` | esp32c61 |
| embassy-executor | `=0.10.0` | |
| embassy-net | `=0.8.0` | dhcpv4, medium-ethernet, tcp, udp |
| embassy-futures | 0.1 → 0.1.2 | |
| 傳遞 | esp-radio-rtos-driver 0.4.2、esp-phy 0.3.0、esp-wifi-sys-esp32c61 0.3.0、esp-sync 0.3.0、esp-config 0.8.0、esp-metadata-generated 0.5.3、esp-riscv-rt 0.15.0、embassy-time 0.5.1、embassy-sync 0.8.0（也見 0.6.2／0.7.2） | |

重要事實：

- X4 現行組（HAL 1.0／rtos 0.2／radio 0.17／alloc 0.9／bootloader 0.4）**沒有** esp32c61 feature；C61 必須升到上表組（HAL 1.2 一族）。同一 Cargo.lock 不能並存兩個 semver 相容的 esp-hal，所以 T2 必須把 X4 與 C61 一併遷移到新組或用獨立 lock／workspace，並重新驗證 X4 build；T1 刻意不做，以免破壞 X4。
- esp-radio 為 beta、esp-rtos 0.x，升級要重驗；beta API 未穩定。本組版本是 Wi-Fi 候選，離線版（T3）不連入 radio。
- 驗證用 probe 位於 `target/t1-c61-deps-probe/`（被 `.gitignore` 的 `target/` 忽略，不入庫）。其 `Cargo.toml` 即上表 `=` 版本固定，`rust-toolchain.toml` 複製自本 repo，`.cargo/config.toml` 只設 `[unstable] build-std = ["alloc","core"]`；`src/main.rs` 僅 `use` 各 crate。重建請依上表重寫即可。

## 4. 目前記憶體基線（X4，未優化對比用）

`target/t1-x4/riscv32imc-unknown-none-elf/release/pulp-os`（13,538,816 bytes 含 debuginfo），以 `size`：

```
   text    data     bss     dec     hex
1740414   32124  313972 2086510  1fd66e
```

這是 X4 的現況，不是 C61 預算（C61 預算由 T8 建立）。

## 5. 實際執行的指令與結果

環境：`cwd=/home/raiven/dev/pulp-os`；為避免汙染既有 `target/`，以 `CARGO_TARGET_DIR` 指向 `target/` 下新目錄（從零編譯）。

| # | 指令 | 結果 |
|---|---|---|
| 1 | `rustc -vV`（toolchain 檔生效，補裝 clippy／rust-std-imc） | 成功：`1.100.0-nightly (1303417c4 2026-09-21)` |
| 2 | `rustup show active-toolchain` | `nightly-2026-09-22-x86_64-unknown-linux-gnu (overridden by '.../rust-toolchain.toml')` |
| 3 | 舊狀態 `cargo build --release --offline`（`channel="stable"`） | **失敗**：`rustup` 補裝 `rust-std-riscv32imc-unknown-none-elf` 衝突並 rollback（見 E0514 小節） |
| 4 | `CARGO_TARGET_DIR=target/t1-x4 cargo build --release --locked` | **成功**：`Finished release profile [optimized + debuginfo] in 21.29s`；產物 ELF 32-bit RISC-V RVC soft-float；第二次 `Finished ... in 0.08s`（無重新編譯） |
| 5 | `CARGO_TARGET_DIR=target/t1-imac cargo check --locked --release --target riscv32imac-unknown-none-elf -p smol-epub` | **成功**：build-std 編譯 `compiler_builtins`、`core`、`alloc` 後 `Checking smol-epub`，`Finished ... in 8.23s` |
| 6 | 3b probe：`cargo check --release --target riscv32imac-unknown-none-elf`（C61 目標依賴組，build-std） | **成功**：`Finished release profile [optimized] in 23.75s`（僅「unused dependency embassy-net」警告，預期） |
| 7 | `cd ../smol-epub && cargo +stable check --offline --target riscv32imac-unknown-none-elf` | **失敗（重現 E0514）**：core 由 rustc 1.98.1 編譯，stable 是 1.99.0 |

說明：

- 指令 5 的 `-p smol-epub` 只驗證 pinned toolchain 在 `riscv32imac` 上能 build-std 並編譯真實 no_std crate；整個 Pulp 對 `riscv32imac` 的連結要等 T2／T12。
- 指令 4 為 X4 現況的完整 release build（有 link）；沒有 `riscv32imac` 的 release ELF，這是 T2 之後的目標，**尚未達成**。
- 本機沒有 `llvm-size`／`riscv32-unknown-elf-size`；以系統 `size` 讀取 section 大小。

## 6. 尚未驗證項目

- **硬體驗收：全部未驗**——boot、PSRAM、SD、ADC 按鍵、EPD、USB／電池、deep sleep／wake、電流；沒有 OnePage 實機。
- 完整 Pulp 對 `riscv32imac` 的 release ELF 與 memory 預算（T2、T8、T12、T13）。
- esp-hal 1.2 一族對 X4 build 的相容性（T2 遷移時驗證）。
- Wi-Fi radio 的 runtime 行為（link pass 不代表可用；屬後續 change）。
- host test 尚未執行（T13／host-validation）。
- Cargo.lock 未更動；smol-epub 的 revision 目前僅記錄在本文件，不被任何機制強制。
- 其他機器首次建置需要網路（crates.io、GitHub 的 embedded-sdmmc 分支）與 rustup 下載 pinned nightly；本次未驗證乾淨機器／CI。

---

# T2：board selection、target／runner、C61 最小 boot ELF

日期：2026-10-04。以下均為本機實際結果；**沒有任何硬體驗收，不宣稱 C61 能 boot，只主張「ELF 可連結」**。

## 7. 決策與理由

### 7a. HAL 遷移：X4 與 C61 一起遷到 HAL 1.2 族，單一 Cargo.lock

放棄「獨立 lock／workspace」：kernel 在 T4–T11 要同時服務兩塊板，兩個 HAL 版本意味著 kernel 必須維持兩份 API 版本，不可行。X4 與 C61 共用下列版本（Cargo.lock 已固定）：

| crate | 版本 | 備註 |
|---|---|---|
| esp-hal | `=1.2.0` | features：log-04、unstable；chip 由 target 決定（見 7b） |
| esp-rtos | `=0.4.0` | embassy、log-04；`esp-radio`／`esp-alloc` 只在 `wifi` feature 開 |
| esp-alloc | `=0.11.0` | internal-heap-stats |
| esp-bootloader-esp-idf | `=0.6.0` | |
| esp-backtrace | 0.20.0（lock） | 原 0.18.1 |
| esp-println | 0.18.0（lock） | 原 0.16.1 |
| esp-radio | `=1.0.0-beta.1`，optional | 只在 `wifi` feature；T2 由 `board-x4` 隱含 |
| embassy-executor | 0.10 | 原 0.9；`#[task]` 改為回傳 `Result<SpawnToken,_>` |
| embassy-net | 0.8（不變） | optional，隨 `wifi` |

`rust-version` 由 1.88 提高為 1.95（esp-backtrace 0.20／radio beta.1 的 MSRV）。

X4 為了編過 HAL 1.2 被迫做的「最小必要」修正（全部是 API 遷移，非功能變更；**X4 實機行為未重驗**）：

- `kernel/src/board/mod.rs`：`SpiDmaBus` 已併入 `SpiDma`（型別別名改為 `spi::master::SpiDma<'static, Blocking>`）；`dma_buffers!`+`DmaRxBuf::new` 改為 `dma_rx_buffer!(4096)`／`dma_tx_buffer!(4096)`（同樣 4096 B 靜態 internal 記憶體）；電源鍵 GPIO3 加 `apply_wakeup_config(WakeupConfig::default().with_low_power_path(true))`（HAL 1.2 的 deep-sleep GPIO 喚醒必須先宣告 low-power path）。
- `kernel/src/kernel/scheduler.rs::enter_sleep`：舊 `Rtc::sleep(&cfg, &[&RtcioWakeupSource])` 在 HAL 1.2 消失，改為 `LowPower::new(LPWR).sleep_deep(cfg)`；喚醒源是 GPIO3 `listen(FallingEdge)`（= Low level）加上 low-power path；RTC FAST 保持供電的 `set_rtc_fastmem_pd_en(false)` 仍保留。**行為等價性只靠讀 HAL 原始碼推得，未在 X4 上測；T11 已重審，結論見 §37e。**
- `src/bin/main.rs`：`SoftwareInterruptControl` 移除，`esp_rtos::start(timer, FROM_CPU_INTR0)`；task spawn 改 `spawner.spawn(task(..).expect(..))`。
- `src/apps/upload.rs`（Wi-Fi，被 esp-radio 0.17 → 1.0.0-beta.1 強迫遷移）：移除 `esp_radio::init`／`wifi::new`／`ClientConfig`／`set_config`／`start_async`；改為 `WifiController::new(WIFI, ControllerConfig{ initial_config: Station{ ssid, Wpa2Personal(password) } })` → `connect_async` → `Interface::station()`。SSID／密碼超長會顯示 "WiFi config error!"。認證固定為 WPA2-Personal（舊 `ClientConfig` 預設行為，未確認等價）。**未在 AP 上驗證**；radio 最佳化 level、生命週期等屬 T3／wifi-upload change。

### 7b. board selection：feature 選板，target 選晶片

- 互斥 Cargo features：`board-x4`、`board-onepage-c61`（root 與 `pulp-kernel` 同名；root 的 board feature 轉發給 kernel）。**無 default feature**。
- `esp-*` 的 chip feature（esp32c3／esp32c61）不由 board feature 驅動，而是用 `[target.riscv32imc-unknown-none-elf.dependencies]`／`[target.riscv32imac-unknown-none-elf.dependencies]` 兩張表。原因（實測）：若 board feature 去開 chip feature，未選／多選時 Cargo 會建出衝突的 esp-* chip crate，先在上游失敗：
  `esp-sync v0.3.0` build script：`Expected exactly one of the following features to be enabled: esp32, esp32c2, ...`，或 `esp-metadata-generated`：`error[E0428]: the name define_lp_functions is defined multiple times`，我們的訊息根本不會出現。chip＝f(target) 之後所有 esp-* 都能建，錯誤由 `kernel/src/lib.rs` 的 `compile_error!` 先報。
- 檢查（`kernel/src/lib.rs`）：未選、多選、`board-x4` 配 A-extension target、`board-onepage-c61` 配無 A target（以 `target_feature = "a"` 區分 imc／imac）。
- Wi-Fi：`wifi` feature（optional `esp-radio`／`embassy-net`／`embedded-io-async`，`esp-rtos/esp-radio`、`esp-rtos/esp-alloc`）。T2 僅 `board-x4 ⇒ wifi` 使 X4 韌體內容不變；C61 不開。正式的 optional boundary（main／manager／menu 切分）屬 T3。
- C61 最小 boot：`[[bin]] pulp-os-c61-boot`（`src/bin/c61_boot.rs`，`required-features = ["board-onepage-c61"]`）；X4 的 `pulp-os` bin `required-features = ["board-x4"]`。
- cfg 隔離：C61 build 下 `pulp_kernel` 的 `board`／`drivers`／`kernel`／`ui`／`util` 與 root 的 `apps`／`fonts`／`ui` 不編譯（它們寫死 C3 的 ADC 腳位、SPI2/DMA、C3 raw GPIO register、RTC sleep）。T4–T11 逐步放寬這些 `#[cfg(feature = "board-x4")]`。
- `c61_boot.rs` 內容：`esp_bootloader_esp_idf::esp_app_desc!()`、`esp_hal::init(CpuClock::max())`、`esp_alloc::heap_allocator!(64 KiB)`、`esp_rtos::start(TIMG0.timer0, FROM_CPU_INTR0)`、embassy `#[esp_rtos::main]`、`esp_println` log 迴圈、`esp_backtrace` panic handler。**沒有** SD／EPD／按鍵／sleep／PSRAM／radio。

### 7c. target／runner

- `.cargo/config.toml` aliases（皆明確帶 `--target`＋`--features`）：`build-x4`／`run-x4`／`check-x4`（imc）、`build-c61`／`run-c61`／`check-c61`（imac，`--bin pulp-os-c61-boot`）。預設 `[build] target` 仍為 imc（相容舊習慣）；因無 default board，裸 `cargo build` 會報「no board selected」，`--features board-onepage-c61` 但 target 為 imc 會報「requires --target riscv32imac…」。
- runner：`[target.riscv32imac-unknown-none-elf] runner = "espflash flash --monitor --chip esp32c61 --flash-mode dio --flash-freq 40mhz --flash-size 16mb"`（對應 BSP README：16 MB、40 MHz；80 MHz 會造成 image hash boot loop）。X4 runner 不變。
- **阻塞性發現：本機 espflash 4.3.0 不認得 esp32c61**：`error: invalid value 'esp32c61' for '--chip <CHIP>' [possible values: esp32, esp32c2, esp32c3, esp32c5, esp32c6, esp32h2, esp32p4, esp32s2, esp32s3]`。espflash 4.6.0（crates.io）原始碼含 `Chip::Esp32c61` 與 `esp32c61-bootloader.bin`；已 `cargo install espflash --version 4.6.0 --locked --root target/t2-tools`（不入庫），對 ELF 做 `save-image --chip esp32c61 --flash-mode dio --flash-freq 40mhz --flash-size 16mb` 成功（見 8），但**沒有實機，未驗證實際燒錄與 boot**。使用 `cargo run-c61` 需升級 espflash ≥ 4.6.0。
- flash40／PSRAM40：esp-hal 1.2 沒有 flash 頻率／PSRAM 頻率的 Cargo／esp_config 選項（flash 頻率由 2nd-stage bootloader 的 image header 決定，所以放在 runner 的 `--flash-freq 40mhz`）；PSRAM 的 init 與 40 MHz 屬 T8，本 task 不做。

## 8. T2 實際指令與結果（`CARGO_TARGET_DIR=target/t2-*`，皆從零編譯）

| # | 指令 | 結果 |
|---|---|---|
| 1 | `CARGO_TARGET_DIR=target/t2-x4 cargo build-x4 --locked` | **成功**：`Finished release profile [optimized + debuginfo] in 22.10s`，無 warning。`file`：`ELF 32-bit LSB executable, UCB RISC-V, RVC, soft-float ABI`；`readelf -A`：`rv32i2p1_m2p0_c2p0_zicsr…` |
| 2 | `size`（X4） | `text 1787928 / data 33104 / bss 309476`（T1 基線 1740414 / 32124 / 313972；text +47,514 B 來自 HAL 1.2／radio beta.1／embassy 0.10 升級，bss −4,496 B） |
| 3 | `CARGO_TARGET_DIR=target/t2-c61 cargo build-c61 --locked` | **成功**：`Finished release profile [optimized + debuginfo] in 17.24s` |
| 4 | `file`（C61） | `pulp-os-c61-boot: ELF 32-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1 (GNU/Linux), statically linked, with debug_info, not stripped` |
| 5 | `readelf -h`（C61） | Class ELF32、Little endian、Machine RISC-V、Flags `0x1, RVC, soft-float ABI`、Entry `0x42003cb6`；`readelf -A`：`rv32i2p1_m2p0_a2p1_c2p0_zmmul1p0_zaamo1p0_zalrsc1p0_zca1p0`（含 A 與 C，對應 imac） |
| 6 | `size`（C61） | `text 237604 / data 1760 / bss 66224`（ELF 檔 2,890,196 B 含 debuginfo）。這是最小程式，不是 C61 預算（T8） |
| 7 | app descriptor | `esp_app_desc` symbol 位於 `0x42000020`、256 B、section `.flash.appdesc`，magic bytes `32 54 cd ab`（0xABCD5432），version 字串 `0.1.0` |
| 8 | C61 不連入 radio | `cargo tree --locked -e normal -i esp-radio --target riscv32imac-unknown-none-elf --features board-onepage-c61` → `error: package ID specification 'esp-radio' did not match any packages`；`-i esp-radio-rtos-driver` 亦無。ELF 中含 `wifi_*`／`esp_wifi_*` 字樣的 22 個 symbol 為 esp-hal 的 ROM linker script／PAC 暫存器名（如 `WIFI_MAC`、`wifi_rf_phy_enable`），不是 radio driver 程式碼 |
| 9 | `target/t2-tools/bin/espflash save-image --chip esp32c61 --flash-mode dio --flash-freq 40mhz --flash-size 16mb <elf> /tmp/c61.bin`（espflash 4.6.0） | 成功，`App/part. size: 58,160/16,384,000 bytes`，image header `e9 04 02 40`（dio，16 MB）；`--merge` 輸出 16,777,216 B，內含 espflash 附的 C61 bootloader。僅證明可打包，未燒錄 |
| 10 | `cargo test --lib --features board-x4`（host 不可行） | `error[E0463]: can't find crate for 'test'`；`--target x86_64-unknown-linux-gnu` 亦失敗（esp-hal 無法在 host 建置，且 chip 依 target 選擇）。host 測試需 T13 的最小 host seam，此處以 11 取代 |
| 11 | `scripts/check-board-selection.sh`（R2 自動化，4 個負向 build） | 全部 `ok`：no board／both boards／x4 配 imac／c61 配 imc |

### R2 訊息原文（實跑）

未選 board（`cargo build --release`）：

```
error: no board selected. Enable exactly one board feature: `board-x4` (Xteink X4, esp32c3, target riscv32imc-unknown-none-elf) or `board-onepage-c61` (OnePage C61, esp32c61, target riscv32imac-unknown-none-elf). Use the cargo aliases `cargo build-x4` / `cargo build-c61` (see README.txt).
  --> kernel/src/lib.rs:15:1
error: could not compile `pulp-kernel` (lib) due to 1 previous error
```

同時選兩個（`cargo build --release --features board-x4,board-onepage-c61`）：

```
error: multiple boards selected. Features `board-x4` and `board-onepage-c61` are mutually exclusive; enable exactly one of them (check `--features` and `--all-features`).
  --> kernel/src/lib.rs:20:1
error: could not compile `pulp-kernel` (lib) due to 1 previous error
```

board 與 target 不符（`cargo build --release --features board-onepage-c61`，預設 imc target）：

```
error: feature `board-onepage-c61` requires --target riscv32imac-unknown-none-elf (esp32c61), but this target has no A extension. Use `cargo build-c61`.
```

反向（`--target riscv32imac-unknown-none-elf --features board-x4`）會先印 `feature 'board-x4' requires --target riscv32imc-unknown-none-elf (esp32c3), but this target has the A extension. Use 'cargo build-x4'.`，之後因 C3 的程式碼被拿去對 C61 HAL 型別檢查，還會接著出現多筆 HAL 型別錯誤（雜訊，但我們的訊息在最前面）。

## 9. T2 尚未驗證項目

- **全部硬體驗收未驗**：C61 boot、log 輸出、embassy／rtos 實際啟動、flash40 啟動、PSRAM、SD、ADC、EPD、USB、sleep、電流。ELF 只證明「可連結」。
- **X4 實機行為未重驗**：HAL 1.0 → 1.2 影響 SPI/DMA、deep sleep／GPIO3 喚醒（改用 `LowPower::sleep_deep`）、esp-rtos 啟動、embassy task spawn、Wi-Fi upload（esp-radio 0.17 → 1.0.0-beta.1，WPA2 固定）。目前只驗證 X4 release 連結成功。
- `cargo run-c61`／實際燒錄：本機 espflash 4.3.0 不支援 esp32c61；4.6.0 僅驗證 `save-image`，未接裝置燒錄。flash header 的頻率欄位編碼未對照 ESP-IDF 實機確認。
- host `cargo test` 不可行（見 8-10）；host seam 屬 T13。
- C61 build 目前會有 internal-only 64 KiB heap（placeholder，非預算）；PSRAM／heap 分工屬 T8；radio optional boundary 屬 T3；pin／GPIO27 屬 T4。
- Cargo.lock 因 target-specific dependency 同時鎖了 esp32c3 與 esp32c61 兩組 chip PAC／wifi-sys（含舊版本殘留）；`--locked` build 兩邊都成功，lock 內是否有可清除的殘留項未整理。
- 本 task 沒有 CI／乾淨機器重驗。升級 nightly／HAL 時須重跑第 8 節。

---

# T3：Wi-Fi optional boundary（R4, R5）

日期：2026-10-04。以下均為本機實際結果；**無硬體驗收，X4 的 Wi-Fi／選單行為未在實機重驗**。

## 10. 決策與理由

- **X4 預設改為離線（wifi disabled）**：`board-x4` 不再隱含 `wifi`（T2 暫時性的 `board-x4 = [..., "wifi"]` 已移除）。理由：proposal 第一版預設 Wi-Fi disabled，離線為預設才使「預設建置即滿足 R4／R5」，C61 也不需特例；上傳功能以 `--features wifi` 明確選用。`default = []` 不變。
- 含 upload 的 X4：`cargo build-x4 --features wifi`，或新增 alias `cargo build-x4-wifi`／`cargo run-x4-wifi`（`.cargo/config.toml`）。README.txt 已更新。
- `wifi` feature 控制（Cargo.toml）：optional `esp-radio`、`embassy-net`、`embedded-io-async`，以及 `esp-rtos/esp-radio`、`esp-rtos/esp-alloc`（T2 已建立，T3 僅移除隱含關係）。
- **wifi + board-onepage-c61 組合**：`src/lib.rs` 加 `compile_error!`（"feature `wifi` is not ported to board-onepage-c61 yet ..."），避免 C61 悄悄建出未驗證的 radio 路徑（Wi-Fi 移植為 out of scope）。實跑 `cargo build-c61 --features wifi --locked` 會在 `src/lib.rs:21` 報此訊息。
- 程式碼隔離（皆 `#[cfg(feature = "wifi")]`，沒有刪除任何 upload 實作）：
  - `src/apps/mod.rs`：`pub mod upload` 與 `AppId::Upload` variant。
  - `src/apps/manager.rs`：`with_app!`／`with_app_ref!` 的 `Upload` arm；新增 `is_upload(AppId)` helper（無 wifi 時恆為 false），取代 `apply_transition`／`needs_special_mode` 中對 `AppId::Upload` 的直接比較；`run_special_mode` 在無 wifi 時為空實作（`needs_special_mode()` 恆 false，永遠不會被呼叫），有 wifi 時才 `WIFI::steal()` 並呼叫 `run_upload_mode`。
  - `src/apps/home.rs`：選單項數 `UPLOAD_ITEMS = cfg!(feature = "wifi") as usize`，離線為 3 項（有 recent 時 4 項）；`item_label`／`item_action` 重寫為先處理 Continue 再以統一索引映射，離線無 `Upload` 項目。導航迴圈用 `item_count`，因此 rem_euclid／selected 重置邏輯自動適用。
  - `src/bin/main.rs`：不需改動（不直接參照 upload／radio；task 與 heap 為離線也需要的共用部分）。`pulp-kernel` 本身不依賴 radio crate，無需改。
- **刻意保留**：`WifiConfig`／`wifi_ssid`／`wifi_pass` 在 kernel 的 settings.txt 解析與寫回、`AppLayer::wifi_config()` trait 方法。離線韌體若不保留這兩個 key，寫回 SETTINGS.TXT 會抹掉使用者在 X4 上存的 WiFi 密碼。它們不依賴 radio driver，離線 ELF 內因此仍有 `wifi_ssid`／`wifi_pass` 字串與 `# wifi credentials for upload mode` 註解，以及 `kernel/src/error.rs` 的 "read error during upload" 等錯誤字串映射；驗證用「upload 模式 UI／伺服器字串與 `Upload` 選單標籤」而非單純 grep "wifi"。
- `embedded-io-async` 注意：`cargo tree -i embedded-io-async` 離線仍有 0.6.1 與 0.7.0，來自 `embassy-sync`（經 esp-hal）與 embedded-sdmmc 等**既有**依賴，不是 radio 路徑；Pulp 對它的**直接**依賴（root optional dep）離線不存在（`cargo tree --depth 1` 無輸出），所以 R4 的證明以 esp-radio／esp-radio-rtos-driver／embassy-net／smoltcp／esp-wifi-sys 為準。

## 11. T3 實際指令與結果

| # | 指令 | 結果 |
|---|---|---|
| 1 | `CARGO_TARGET_DIR=target/t3-x4 cargo build-x4 --locked` | **成功**，`Finished release profile ... in 21.11s`，無 warning |
| 2 | `size`（X4 離線） | `text 1222160 / data 27608 / bss 348828`（T2 含 wifi：1787928 / 33104 / 309476；離線 text −565,768 B、data −5,496 B；bss +39,352 B；**T8 已查明：不是靜態配置，是 `.stack` 吃掉剩餘 RAM，見 §28**） |
| 3 | `cargo tree --locked -e normal -i esp-radio --target riscv32imc-unknown-none-elf --features board-x4` | `error: package ID specification 'esp-radio' did not match any packages`（esp-radio-rtos-driver、embassy-net、smoltcp、esp-wifi-sys 同樣不在圖中） |
| 4 | 離線 X4 ELF：`nm -C`（排除 absolute 型別）grep `esp_radio\|esp_wifi\|embassy_net\|smoltcp` | 0 筆 |
| 5 | 離線 X4 ELF：`strings -a` 找 `pulp.local`／`WiFi config error!`／`No WiFi credentials!`／`Connection failed!`／獨立一行 `Upload` | 全部沒有（仍有設定 key／error 對映字串，見 10） |
| 6 | `CARGO_TARGET_DIR=target/t3-x4-wifi cargo build-x4-wifi --locked`，及 `cargo build-x4 --features wifi --locked`（同 target dir） | 皆**成功**；`cargo tree ... --features board-x4,wifi` 含 `esp-radio v1.0.0-beta.1`、`embassy-net v0.8.0`、`embedded-io-async v0.7.0` 為 pulp-os 直接依賴 |
| 7 | wifi X4 ELF 探針 | `size`：`1787912 / 33080 / 309500`（與 T2 的 1787928 / 33104 / 309476 基本一致，證明含 wifi 路徑未回歸）；`nm` 有 347 個 radio／net 符號；`strings` 有 `pulp.local`、`WiFi config error!`、`No WiFi credentials!`、獨立一行 `Upload` |
| 8 | `CARGO_TARGET_DIR=target/t3-c61 cargo build-c61 --locked` | **成功**，`Finished ... in 18.03s`；`size` 237604 / 1760 / 66224（與 T2 完全一致） |
| 9 | `bash scripts/check-board-selection.sh` | 4 項皆 `ok` |
| 10 | `bash scripts/check-offline-boundary.sh`（`OFFLINE_CHECK_TARGET_ROOT=target/t3-check`，從零建置三種配置） | exit 0，20 項皆 `ok`：X4 離線 8 項、X4+wifi 正向對照 4 項、C61 離線 8 項 |
| 11 | `cargo build-c61 --features wifi --locked` | 預期失敗，訊息見 10 |

`scripts/check-offline-boundary.sh`：可重複執行，建 X4 離線／X4+wifi／C61 三個 ELF，檢查 (a) `cargo tree -i` 無 radio crate，(b) `nm` 無 radio／net 符號，(c) `strings` 無 upload 模式字串與 `Upload` 選單標籤；X4+wifi 作為對照確保探針有效。注意：C61 ELF 含 7 個 `esp_wifi_cert_tx_*`／`esp_wifi_internal_tx` 型別 `A`（absolute）符號，是 esp-hal ROM linker script 的固定位址，不是連結進來的程式碼，探針已排除 `A` 型別（T2 第 8 項已說明同類）。

## 12. T3 尚未驗證項目

- **硬體驗收全部未驗**：X4 實機的離線選單導航（3／4 項、Select 對應）、含 wifi 的 upload 流程、C61 任何行為。
- 離線 home 選單未做畫面截圖或 host 單元測試（host `cargo test` 仍不可行，host seam 屬 T13）；只以編譯、索引邏輯檢視和 ELF 字串確認。
- 離線 X4 bss +39 KB 的來源：**T8 已查明（見 §28）**，是 `.stack` 較大、不是額外靜態配置。
- 含 wifi 的 X4 只驗證「可連結且符號／字串在」，與 T2 一樣未在 AP 上驗證。
- C61 的 wifi 為 out of scope；`compile_error!` 僅為防誤用，未來移植時需移除。

---

# T4：C61 pin ownership、GPIO27 啟動／runtime 契約、host 測試 seam

日期：2026-10-04。**沒有任何硬體驗收；GPIO27 實際時序、SD 供電穩定性均未驗。**

## 13. 設計

### 13a. host 測試 seam（T5–T13 沿用）

- 新 workspace crate `board-logic/`（package `pulp-board-logic`）：`#![no_std]`、**零依賴**、不含 esp-hal、不含 board feature cfg。硬體只透過 trait（`RailPin`、`DelayMs`）接觸。
- 韌體使用**同一份**程式碼：`pulp-kernel`（`board-onepage-c61` feature 下的 `kernel/src/board_c61/`）依賴它，`src/bin/c61_boot.rs` 呼叫 `PeripheralPower::power_cycle`；ELF 內可見 `pulp_board_logic::power::*` 符號（`nm -C`）。沒有測試專用副本。
- host 測試指令（alias 在 `.cargo/config.toml`）：`cargo test-board-logic`，展開為 `cargo test -p pulp-board-logic --target x86_64-unknown-linux-gnu --config unstable.build-std=["std","test"]`。為何要 `--config`：根 `.cargo/config.toml` 預設 target 是 riscv 並設 `build-std = [alloc, core]`；`--config` 的陣列是**合併**而非取代，只有 core／alloc 的 build-std 會與測試 harness 的 sysroot core 衝突（`error[E0152]: duplicate lang item in crate core ... sized`，實測），所以補 `std`、`test` 讓整套 build-std 針對 host 一致（首次冷建置約 15 s）。`CARGO_UNSTABLE_BUILD_STD=""` 無效（同樣 E0152）。
- 之後要加可 host 測的邏輯：放進 `board-logic`（或同型態的 HAL-free crate），hardware 以 trait 注入，kernel 內寫薄 adapter。

### 13b. pin ownership

- `board-logic/src/pins.rs`：pin 常數（每個附 BSP 行號）、`PIN_MAP`（18 項）、host 測試（每個 GPIO、每個 role 唯一；GPIO27 只出現一次；pin 號對 BSP；皆 ≤ GPIO29，C61 metadata 只有 GPIO0–29）。
- `kernel/src/board_c61/pins.rs`：`Pins` 結構，每支 pin 一個 esp-hal GPIO singleton（SPI SCK22/MOSI23/MISO24、EPD CS25/DC8/BUSY29、SD CS26/CD28、前面板 ADC4、wake2/prev6/next9、battery ADC5/charge10/USB11、預留 PDM CLK7／DIN3）。**GPIO27 不以 raw pin 提供**，只存在於 `Pins.power: PeripheralPower<Gpio27Rail>` 內。`take_c61_pins!(peripherals)` 為 macro（需 partial move，讓 TIMG0 等仍可用）。T5–T11 以欄位 by-value 取走並建 driver；T4 不實作這些 driver。
- 未納入：EPD RST 沒有獨立欄位（就是 GPIO27）；GPIO1 等未使用 pin 不處理。
- BSP 註解自相矛盾（只記錄，未決）：`board_c61.c:63` 寫 battery「GPIO5 = ADC1_CH3」，同檔 322 行 diag 讀 `ADC_CHANNEL_2` 並稱 GPIO5 = ADC1_CH2，`board_keys.c` 稱前面板 GPIO4 = ADC1_CH2。T7／T9 需對 esp-hal 的 C61 ADC 通道對照表確認。

### 13c. GPIO27 狀態機（`board-logic/src/power.rs`）

狀態：`Unpowered → PowerCycled → SdInitializing → SdActive → ShuttingDown → PoweredOff`。

- `power_cycle(&mut delay)`：僅 `Unpowered` 可呼叫；序列 **低 → delay 20 ms → 高 → delay 20 ms**（`board_c61.c:80-84`，註解「low 20ms -> high 20ms」；BSP 用 `vTaskDelay`，這裡是 blocking `esp_hal::delay::Delay`，boot 時 2 x 20 ms）。其他狀態回 `InvalidState`，**完全不碰 pin**（避免多餘呼叫讓 SD 掉電）。
- `begin_sd_init()` 回傳 `SdInitPermit`（不可 Clone、欄位私有、只由此函式產生）；非 `PowerCycled` 回 `InvalidState`。T5 的 SD init 必須要求 `&SdInitPermit`（型別層強制）；`finish_sd_init(permit, ok)`：成功→`SdActive`，失敗→回 `PowerCycled`（rail 維持高，可重試）。同一時間只有一個 permit。
- `display_reset()`：回 `DisplayReset::Software`（唯一 variant；呼叫端須下 SSD1677 軟體 reset），在 `PowerCycled／SdInitializing／SdActive` 允許，其餘回 `InvalidState`。永不動 GPIO27。`hardware_reset_display()`：任何狀態皆回 `HardwareResetForbidden`（tripwire）。Boot 期間唯一的硬體 reset 就是 power-cycle 本身。
- `begin_shutdown()`（rail 仍高）→ `cut_peripheral_power()`（才拉低 GPIO27）；對應 BSP 先 park EPD、靜音 SCK/MOSI/CS/SD_CS/PDM_CLK、最後才 `board_peripherals_power(false)`（`board_c61.c:337-346`）。SD init 進行中不可 begin_shutdown；`PoweredOff` 為終態（晶片 deep sleep 後重開機）。靜音共享線與 EPD park 的實際動作屬 T11；T4 只提供「cut 前必須先 begin_shutdown」的順序保證。
- 不重複 BSP 的 `board_sd_mount` 再次 `gpio_set_level(PIN_RST,1)` + 20 ms（`board_c61.c:386-387`）：power-cycle 已含 20 ms 高電位 settle，且 R7 要求 SD init 起不再碰 GPIO27。若實機顯示 SD 需更長 settle，T5 在 init 內加 delay，不要在這裡重驅動。
- kernel adapter `kernel/src/board_c61/power.rs`：`Gpio27Rail(Output)` 實作 `RailPin`（`take_rail` 以 `Level::Low` 建立，對應 BSP 先 config output 再拉低），`HalDelay` 實作 `DelayMs`。`Gpio27Rail` 只被 `PeripheralPower` 持有，沒有其他路徑能 toggle GPIO27。

## 14. T4 實際指令與結果

| # | 指令 | 結果 |
|---|---|---|
| 1 | `cargo test-board-logic`（冷 target dir） | **20 passed; 0 failed**（`pins::tests` 4 項、`power::tests` 16 項；名稱含 `r6_*`／`r7_*`，涵蓋：power-cycle 事件序列與 20/20 ms、SD init 事件在所有 power-cycle 事件之後、未 power-cycle 即 SD init 被拒、第二次 power-cycle 被拒且無 GPIO 事件、重複 SD init 被拒、SD init 失敗後 rail 不變可重試、SD init 後 runtime reset 不碰 GPIO27、hardware reset 於各狀態皆被拒、shutdown 前不能拉低、shutdown 期間禁止 SD init／reset／power-cycle、SD init 進行中禁止 shutdown、PoweredOff 終態） |
| 2 | `CARGO_TARGET_DIR=target/t4-c61 cargo build-c61 --locked` | **成功**；`size`：text 238742 / data 1800 / bss 66224（T3：237604 / 1760 / 66224）；`nm -C` 含 `pulp_board_logic::power::RailState as Debug>::fmt`、`pulp_kernel::board_c61::power::HalDelay as DelayMs>::delay_ms`；`strings` 含 `gpio27 power-cycle` log 字串。`power_cycle` 本體被 inline，無獨立符號 |
| 3 | `CARGO_TARGET_DIR=target/t4-x4 cargo build-x4 --locked` | **成功**；`size`：1222160 / 27608 / 348828（與 T3 離線 X4 完全相同） |
| 4 | `bash scripts/check-board-selection.sh` | 4 項 `ok` |
| 5 | `OFFLINE_CHECK_TARGET_ROOT=target/t4-check bash scripts/check-offline-boundary.sh` | 20 項皆 `ok`（X4 離線 8、X4+wifi 對照 4、C61 離線 8） |

Cargo.lock：加入 `pulp-board-logic`（新 workspace member，無外部依賴）與 `pulp-kernel` 對它的依賴；其餘不變。首次需要 `--offline`（無 `--locked`）更新 lock，之後 `--locked` 通過。

## 15. T4 尚未驗證項目

- **硬體全未驗**：GPIO27 實際低／高電位與 20 ms 時序是否足以重置 EPD 與讓 SD 上電穩定、power-cycle 後 SD 是否在 20 ms 內可 probe、GPIO27 在 SD 運作期間是否真的維持高、deep sleep 時 GPIO27 低電位是否被 SoC 保持（hold）等。
- `esp-hal` 的 `Output` 在 `Pins` 被 drop 或進 deep sleep 後的 pad 行為（hold／floating）未驗；c61_boot 讓 `pins` 活到程式結束。
- `take_c61_pins!` 對所有 C61 GPIO 型別已通過編譯，但各 pin 的實際功能（ADC 通道對應、LP wake 能力）未驗，見 13b 的 BSP 註解矛盾。
- 狀態機只在 host 以 recording fake 驗證順序；沒有在韌體上跑，也沒有 SD init 實作可呼叫 `begin_sd_init`（T5）。
- blocking 40 ms delay 位於 async main；boot 階段可接受，T5 若移到執行期需改為非阻塞。
- X4 未改動行為；X4 不使用 `board-logic`（僅被 kernel 編入，無呼叫者）。

---

# T5：SPI2／DMA、SD wiring、card detect／錯誤處理（R8, R9）

日期：2026-10-04。**沒有任何硬體驗收：SPI 時脈、DMA 傳輸、SD 實際 init、CD 極性、MISO pull-up 全未驗。**

## 16. 設計

### 16a. 分層（沿用 T4 的 host seam）

可 host 測的邏輯放 `board-logic`（韌體連結同一份），kernel 只放 esp-hal／embedded-sdmmc adapter：

| 位置 | 內容 |
|---|---|
| `board-logic/src/spi.rs` | 匯流排常數（`SPI_INIT_KHZ=400`、`SPI_OPERATING_KHZ=10_000`、`SPI_DMA_BUF_BYTES=4096`、`SD_PRELUDE_BYTES=10`，皆附 BSP 行號）、`Device{Epd,Sd}`、`ChipSelect` trait、`SpiArbiter::transaction`（assert CS → 執行 → release CS；已有 owner 時回 `BusError::Busy` 而非 panic；失敗也 release） |
| `board-logic/src/sd.rs` | card-detect 極性 `CD_PRESENT_WHEN_LOW`（BSP `board_c61.c:419`，pull-up `:284`）、`CardDetect` 去抖（`CD_DEBOUNCE_SAMPLES=3`、`CD_SAMPLE_INTERVAL_MS=20`）、`SdFault`、`SdProbe` trait、`probe_with_retry(&SdInitPermit, ..)`、`init_sd`（取 permit→重試→`finish_sd_init`）、`StorageStatus`／`StorageHealth` |
| `board-logic/src/power.rs`（T4 檔，小幅擴充） | 新增 `PeripheralPower::card_removed()`：`SdActive → PowerCycled`、rail 不動，讓重插卡可再取新的 `SdInitPermit`（T4 狀態機原本在 `SdActive` 後無法重 init）。`PowerCycled` 為 no-op，其餘狀態拒絕 |
| `kernel/src/board_c61/spi.rs` | `spi::init(SPI2, DMA_CH0, SpiPins)`：先把 EPD CS／SD CS 設成 `Output(High)`，`Spi::new` @400 kHz → `with_sck/mosi/miso` → 先送 10 byte 0xFF（≥74 clocks，與 X4 同作法）→ `dma_rx_buffer!/dma_tx_buffer!(4096)` → `with_dma(DMA_CH0).with_buffers`（HAL 1.2 `SpiDma`，無 unwrap，緩衝配置失敗回 `SpiInitError::DmaBuffers`）。匯流排放在 `critical_section::Mutex<RefCell<SharedSpi{bus, arbiter}>>`；`ArbitratedSpiDevice` 實作 `embedded_hal::spi::SpiDevice`，每次 `transaction` 都經 `SpiArbiter`（`try_borrow_mut` 失敗回 `Busy`，不 panic）。`SpiControl::speed_up()` 於 SD init 後把時脈切到 10 MHz |
| `kernel/src/board_c61/sd.rs` | `CardDetectPin`（GPIO28、`Pull::Up`）、`CardProbe`（embedded-sdmmc `SdCard::num_bytes` ／`mark_card_uninit` 對應 `SdProbe`）、`init`／`bring_up`（init 要求 GPIO27 permit，失敗回 `SdStorage::empty()` + `StorageHealth`）、`fault_to_error`／`fault_from_error`／`observe_storage_result`（對接既有 `crate::Error`） |
| `kernel/src/drivers/mod.rs`／`sdcard.rs`／`lib.rs` | `drivers::{sdcard, storage}` 現在 C61 也編譯（其餘 battery／input／ssd1677／strip 仍 X4-only，T6／T7／T9 再放寬）。`sdcard.rs` 的 `SdSpiDevice` 依 board cfg 選 `board::SdSpiDevice`（X4）或 `board_c61::spi::SdSpiDevice`；`init_card`（X4 的 raw GPIO12 CS 路徑，無 permit）標 `#[cfg(feature = "board-x4")]`。X4 程式碼路徑本身沒改 |
| `src/bin/c61_boot.rs` | power-cycle 之後：`spi::init` → `CardDetectPin` → `sd::bring_up`（permit→probe→mount）→ `speed_up` → `storage::list_root_files`（缺卡時回 `Error(NoCard)` 並 log `storage error: ...`，不 panic）→ 迴圈內以 20 ms 去抖 card-detect，Removed 時 `SdStorage::empty()` + `card_removed()`，Inserted 時重新 `bring_up`。ELF 內可見 `ArbitratedSpiDevice as SpiDevice>::transaction`、`SdCard<ArbitratedSpiDevice,..>` 的 `card_command`／`read_data`／`write_data`、`board_c61::sd::bring_up` |

### 16b. 設計決策

- **CS 由 device 層管理**：沿用 embassy／critical-section Mutex + `SpiDevice` 的形狀，但自寫 `ArbitratedSpiDevice` 取代 `embedded_hal_bus::CriticalSectionDevice`（X4 仍用後者，沒動），原因：CS 序列與「同時只能一個 CS」要放進可 host 測的 `SpiArbiter`，且重入時回 `Busy` 而非 `RefCell` panic（R9）。op 處理與 `CriticalSectionDevice` 等價（Read/Write/Transfer/TransferInPlace/DelayNs，失敗時仍 flush + release CS）。
- **CS 腳位不用 raw register**：EPD CS=GPIO25、SD CS=GPIO26 皆是 esp-hal `Output`；C61 的 esp-hal 有完整 GPIO 型別，不需要 X4 `board::raw_gpio`（C3 的 GPIO12 無 HAL 型別才用 raw register）。
- **SPI 時脈**：init 400 kHz → 操作 10 MHz（BSP `SPI_FREQ_HZ` `:44` 與 SD `max_freq_khz=10000` `:391`；X4 是 20 MHz，未沿用）。BSP 沒寫 400 kHz，這是 SD spec／ESP-IDF sdspi probe 慣例，與 X4 一致。
- **Card detect 不阻擋 init**：BSP `board_sd_present()` 的註解自己寫 "assume low = inserted"（`:419`），極性未驗證；若以 CD 擋 init 而極性反了，卡會永遠用不了。所以 init 一律嘗試，CD 只用來分類失敗（`NoCard` vs `InitFailed`）與產生插拔事件（`r9_card_detect_absent_does_not_block_an_init_attempt`）。BSP `board_sd_mount` 也不看 CD。
- **R6 在型別層強制**：`probe_with_retry(&SdInitPermit, ...)` 需要 permit；唯一出處是 `PeripheralPower::begin_sd_init()`。`init_sd` 在 power-cycle 前被呼叫會得到 `SdFault::PowerNotReady(Unpowered)` 且不碰 pin／delay／probe（`r6_init_sd_is_refused_before_power_cycle_and_touches_nothing`）。
- **重試策略**：5 次、失敗間隔 50 ms、失敗後 `mark_card_uninit`（與 X4 `init_card` 相同）。不重打 GPIO27（T4 §13c 的決定不變）；`SD init` 失敗後狀態回 `PowerCycled`，之後再試（插卡）無需動 rail。
- **錯誤型別與 UI 對接**：
  - 既有 X4 路徑（查證）：`SdStorage::empty()`（`probe_ok()=false`）→ `storage::borrow` 回 `Error(ErrorKind::NoCard)` → `FilesApp::load_failed(e)` 顯示；scheduler `sd_ok` 只用於 log。C61 `bring_up` 失敗時回 `SdStorage::empty()`，因此同一條路徑自然觸發。
  - 新增 `SdFault`（NoCard／InitFailed／MountFailed／ReadFailed／WriteFailed／PowerNotReady）→ `StorageStatus` → 訊息 `"SD: no card"` 等；`fault_to_error` 映射到既有 `ErrorKind`（NoCard／OpenVolume／ReadFailed／WriteFailed），沒有新增 `ErrorKind`（共用型別，不動 X4）。
  - 執行期讀寫失敗：`drivers::storage` 本來就回 `Result<_, Error>`；`observe_storage_result` 把卡層級錯誤（NoCard／OpenVolume／Read／Seek／Write／Delete／DirFull）寫進 `StorageHealth`，成功則清除 `IoError`。檔案層錯誤（NotFound／OpenFile／OpenDir）不算卡故障。
  - **未做**：把 `StorageStatus::message()` 接進 app 層畫面（需要 C61 的 scheduler／app lifecycle，屬 T12）。目前 UI 可見的是既有 `NoCard` Error 路徑；`StorageHealth` 為 T12 預備。
- **firmware 路徑無 unwrap／expect／panic**：`grep -n 'unwrap\|expect(\|panic!\|unreachable' kernel/src/board_c61/*.rs src/bin/c61_boot.rs` 無輸出。`spi::init` 失敗在 c61_boot 以 `park()`（log 後閒置）處理而非 panic。
- **MISO pull-up（與 BSP 的差異）**：BSP `:89`／`:385` 對 MISO 開內部 pull-up；esp-hal 1.2 `Spi::with_miso` 會以預設 `InputConfig`（無 pull）覆寫腳位設定，要補需繞過 HAL，所以**沒有套用**（SD DO 靠卡／板上上拉）。若實機 SD init 不穩，優先檢查這項。
- **X4 `board::mod.rs` 不動**：X4 的 SPI／SD 路徑（含 C3 raw GPIO12）保持原狀；T5 只放寬 `drivers::{sdcard,storage}` 的編譯條件。

### 16c. 移除 C61 對 C3 raw GPIO 的依賴

C3 raw register 只存在於 `kernel/src/board/raw_gpio.rs`（`0x6000_4008/0x6000_400C/0x6000_4024/0x6000_9000/0x6000_4554`），由 X4-gated 的 `mod board` 持有；C61 build 本來就不編譯它（T2），T5 的 C61 SPI／SD 路徑完全走 esp-hal 型別，並新增 `scripts/check-c61-no-c3-raw-gpio.sh` 自動檢查（見 17 第 5 項）。`drivers::sdcard` 改 cfg 後不再引用 `crate::board`（C61 build 下）。

## 17. T5 實際指令與結果

| # | 指令 | 結果 |
|---|---|---|
| 1 | `cargo test-board-logic` | **45 passed; 0 failed**（T4 的 20 項 + 新增 25 項，見下） |
| 2 | `CARGO_TARGET_DIR=target/t5-c61 cargo build-c61 --locked` | **成功**，無 warning；`size`：text 277536 / data 3248 / bss 76760（T4：238742 / 1800 / 66224；+38,794 text 為 embedded-sdmmc、SPI/DMA、SD init、storage 連入；bss +10,536 含 2 x 4096 B DMA 緩衝與描述符）。`nm -C` 見 `<ArbitratedSpiDevice as SpiDevice>::transaction`、`SdCard<ArbitratedSpiDevice, Delay>` 的 `card_command`／`read_data`／`write_data`、`board_c61::sd::bring_up`、`board_c61::spi::SPI_SHARED`、`init::BUFFER`／`DESCRIPTORS`；`strings` 見 `spi2: sck=GPIO22 mosi=GPIO23 miso=GPIO24, epd_cs=GPIO25 sd_cs=GPIO26, dma ch0 ...`、`sd: card detect GPIO28 reads`、`sd: init failed:`、`storage error:` |
| 3 | `CARGO_TARGET_DIR=target/t5-x4 cargo build-x4 --locked` | **成功**，無 warning；`size`：1222160 / 27608 / 348828，與 T4（及 T3 離線 X4）**完全相同**（差異 0） |
| 4 | `bash scripts/check-board-selection.sh` | 4 項 `ok` |
| 5 | `OFFLINE_CHECK_TARGET_ROOT=target/t5-check bash scripts/check-offline-boundary.sh` | exit 0，20 項 `ok`、0 FAIL |
| 6 | `X4_ELF=target/t5-x4/riscv32imc-unknown-none-elf/release/pulp-os bash scripts/check-c61-no-c3-raw-gpio.sh` | exit 0，全部 `ok`（見下） |

### host 測試（新增 25 項）

- `board-logic/src/spi.rs`（7）：`r8_bus_pins_are_the_bsp_spi2_pins`、`r8_bus_plan_matches_bsp_and_sd_spec`、`r8_new_releases_both_chip_selects`、`r8_transaction_is_assert_ops_release_for_the_right_device`、`r8_alternating_epd_and_sd_never_overlap`（重放 log：同時最多一個 CS、bus 動作只在 CS 為低時）、`r8_cs_is_released_when_the_transfer_fails`、`r8_reentrant_transaction_is_refused_without_driving_any_cs`。
- `board-logic/src/sd.rs`（16）：`r9_cd_polarity_is_bsp_low_means_inserted`、`r9_cd_change_needs_n_consecutive_samples`、`r9_cd_bounce_is_filtered`、`r9_cd_removal_event_after_debounce`、`r6_init_sd_is_refused_before_power_cycle_and_touches_nothing`、`r6_probe_runs_only_after_the_power_cycle_events`、`r9_init_retries_with_50ms_spacing_then_succeeds`、`r9_no_card_is_a_recoverable_error_not_a_panic`、`r9_card_present_but_unusable_is_init_failed`、`r9_card_detect_absent_does_not_block_an_init_attempt`、`r9_second_init_while_active_is_refused_not_panicking`、`r9_remove_then_reinsert_remounts_through_a_new_permit`、`r9_every_fault_maps_to_a_displayable_error_status`、`r9_io_error_recovers_on_next_success`、`r9_insert_while_ready_does_not_trigger_remount`、`r9_insert_after_failed_init_triggers_remount`。
- `board-logic/src/power.rs`（2，T4 擴充）：`r7_card_removed_reopens_sd_init_without_touching_gpio27`、`r7_card_removed_is_rejected_when_rail_not_up_or_shutting_down`。

注意：spi／sd 的「第二個 transaction 重入」在真實路徑下由 `&mut` 借用與 `try_borrow_mut` 擋下；`r8_reentrant_*` 用手動設 owner 模擬，只證明 arbiter 拒絕時不動 CS，不是真實重入場景的整合測試。

### C3 raw GPIO 依賴檢查（`scripts/check-c61-no-c3-raw-gpio.sh`）

方法（四個獨立探針 + X4 對照）：
1. 原始碼：對 C61 編譯集合（`kernel/src/board_c61`、`board-logic/src`、`drivers/{sdcard,storage,mod}.rs`、`error.rs`、`src/bin/c61_boot.rs`）grep `0x6000_?xxxx`／`raw_gpio`／`RawOutputPin`／`GPIO_OUT_W1T*`／`IO_MUX_BASE`（排除純註解行）→ 無命中。
2. 結構：`kernel/src/lib.rs` 的 `pub mod board;`（擁有 raw_gpio）在 `cfg(feature = "board-x4")` 之下。
3. C61 ELF `nm -C` 無 `raw_gpio`／`RawOutputPin` 符號（X4 ELF 對照：37 個）。
4. C61 ELF `objdump -d` 無 `lui rd,0x60004`（C3 GPIO 區塊；X4 ELF 對照：63 個）。C61 ELF 有 1 個 `lui s9,0x60009`：位置在 esp-hal 的 timer-group wdt 設定附近，`esp32c61-0.5.0` PAC 中 `0x6000_9000` 是 `TIMG1`，不是 C3 的 IO_MUX（C3 的 `0x6000_9000`）——這是撞位址而非依賴；腳本只印 info 不當作失敗。
結果：c61 4 項 ok、x4 對照 2 項 ok（證明探針有效）。

## 18. T5 尚未驗證項目

- **硬體全未驗**：SPI2 400 kHz／10 MHz 實際輸出、DMA 傳輸（含 EPD 大量資料）、SD 實際 init／mount、card-detect 極性（BSP 自己只 "assume"）與去抖時間、MISO 無內部 pull-up 是否影響 SD、10 MHz 下 SD 與 EPD 共用匯流排的穩定度、SD 在 GPIO27 power-cycle 後 20 ms 內能否 probe（T4 遺留）。
- `SpiDma` 在 C61 上的 `apply_config`（時脈切換）與 `SpiBus::flush` 行為僅靠編譯通過，未執行。
- 80 clocks prelude 在 `with_dma` 之前用 `Spi::write` 送出，與 X4 一致；C61 上是否需要等待 CS 穩定未驗。
- 與 BSP 的差異：MISO pull-up 未套用（見 16b）；未做 BSP `spi_bus_setup` 對 EPD CS／SD CS 的 `GPIO_PULLUP_ONLY`（CS 以 push-pull `Output(High)` 取代，boot 時 GPIO 在 `Output::new` 之前的浮動時間未量測）。
- `StorageStatus` 的畫面顯示未接（T12）；目前 UI 只能顯示既有的 `NoCard` Error 路徑。C61 沒有可跑的 Files app，故「畫面」層面完全未驗，只驗證錯誤值的產生與映射。
- 卡移除時 `SdStorage::empty()` 取代原 storage 是直接 drop 掉 FAT 狀態（沒有 `flush_and_close`），寫入中拔卡的檔案一致性未處理（T10／T12 範疇）。
- EPD 只預留 `EpdSpiDevice` handle，DC／BUSY／軟體 reset／SSD1677 命令屬 T6；SD CS／MOSI／SCK 在 deep sleep 前的靜音（board_c61.c:337-346）屬 T11。
- c61_boot 內 card-detect 迴圈只是整合示範，真正的輪詢責任屬後續 scheduler 整合。
- X4 行為：只驗證連結與 size 不變，SD／SPI 的實機行為與 T2 起同樣未重驗。

---

# T6：SSD1677 full refresh／rotation、有界 BUSY（R10, R11）

日期：2026-10-04。**沒有任何硬體驗收：實際顯示、BUSY 時間、殘影、waveform、rotation 方向在實機是否正確全未驗。**

## 19. 設計

### 19a. 面板尺寸／方向依據（結論：與 X4 相同，不新增 rotation）

| 項目 | C61 依據 | X4（現有驅動） |
|---|---|---|
| 原生尺寸 | `board_c61.c:50-51`：`EPD_W 800`（source）／`EPD_H 480`（gate），註解「480x800 is NOT drivable」；`README.md:22`：Osptek EPD0426A02 4.26" 800x480 SSD1677 | GDEQ0426T82 800x480，`WIDTH=800/HEIGHT=480` |
| gate scan | `:211` `gate_scan_dir = 0x02` | `DRIVER_OUTPUT_CONTROL` 第三位元組 `0x02` |
| gate 反向 | `:216` `mirror_y = true`「panel gates reversed: reverse Y data」 | 「gates wired in reverse; Y flipped, X inc / Y dec」（`DATA_ENTRY_MODE 0x01`） |
| portrait | `:222` `MOUI_ROTATION_270 -> portrait 480x800` | 預設 `Rotation::Deg270` → 480x800 |
| BUSY 極性 | `:147` `while (gpio_get_level(PIN_BUSY) == 1)`：高 = busy | `is_low()` 才算完成 |
| BUSY timeout | `:147-150`：1 ms 輪詢，超過 **5000 ms** 只 `ESP_LOGW("EPD busy timeout")` 後 break（不回錯） | 同步 `wait_busy` 到期靜默 return；async 路徑無上限 |
| reset | `:160-163` 註解：runtime EPD reset 用 SW_RESET 0x12，不切 SD 電；初始硬體 power-cycle 在 `board_init` | `reset()` toggle RST（C61 不可用，未被 C61 編入） |

所以 C61 重用 X4 的 `Rotation::Deg270`、strip 版面（physical 800x480，每條 40 列 x 100 B = 4000 B，共 12 條）與 blit 快路徑。方向「數學」與 X4 一致；moui `ROTATION_270` 與 X4 `Deg270` 是否同向只靠名稱相同與 mirror_y／gate 描述推論，**無法在紙上證明，需實機看 bring-up 方向卡**。

限制：BSP 的 SSD1677 實作在 `MoveCall/moui` 元件（`MOUI_USE_DRV_SSD1677`），**本機找不到其原始碼**（`find / -name 'moui*'` 無結果；`bsp_onepage_c61`、`crosspoint-onepage`、`onepage-reader` 只有呼叫端與 pin 定義）。因此 BSP 實際 init 序列（booster、溫度 `fast_temp = 0x5A`、OTP LUT／電壓、`anti_ghosting`）無法逐位元比對；T6 沿用 X4 現有 init／full-refresh 序列（task 指定「waveform／LUT 沿用既有 full refresh」），與 BSP 的差異（如 X4 用內建溫度感測器 `0x18 0x80`，BSP 固定溫度 `0x5A`）記為未驗項。

### 19b. 分層（沿用 T4／T5 host seam，韌體與測試是同一份程式碼）

| 位置 | 內容 |
|---|---|
| `board-logic/src/ssd1677.rs`（新） | `WIDTH/HEIGHT/Rotation`、`cmd`、`RenderState`、`transform_region`／`align_partial_region`、命令序列（`set_ram_area`／`soft_reset`／`configure`／`init_display`／`write_full_frame`／`start_full_update`）、`wait_busy_bounded`、`DisplayError{BusyTimeout,Bus,NotInitialized}`、`Epd`（C61 full-refresh 驅動）。硬體只透過 trait：`EpdBus`（command／data）、`power::DelayMs`、`BusyPin`（`is_busy`＋`now_ms`）、`StripSource` |
| `board-logic/src/strip.rs`（新，自 X4 `strip.rs` 搬出） | `StripCore`：strip buffer 版面、`to_physical`、`set_pixel_physical`、`blit_1bpp`（含 Deg270 快路徑）、`fill_logical_rect`、`draw_pixel_logical`、`logical_window_xywh`；`draw_bringup_pattern`（方向卡） |
| `kernel/src/drivers/strip.rs`（X4，改寫） | `StripBuffer(StripCore)` newtype：`Deref`／`DerefMut` 轉給 core，只保留 embedded-graphics `DrawTarget` 膠水、`ui::Region` 版的 `logical_window()`、`begin_window` 的截斷 warn（core 回傳 bool）。仍 X4-only（`ui::Region` 在 X4-only 的 `ui`；C61 的 app 層 T12 再放寬） |
| `kernel/src/drivers/ssd1677.rs`（X4，改寫） | `DisplayDriver` 保留自己的 SPI／DC／RST／BUSY／delay；init、RAM window、full frame、full update、`align_partial_region` 改呼叫 `board_logic::ssd1677`（`impl EpdBus for DisplayDriver` 吞掉錯誤，與原本 `let _ =` 一致）；`reset()`、`wait_busy`、partial／deep-sleep 流程不動 |
| `kernel/src/board_c61/epd.rs`（新） | esp-hal 轉接：`EpdHw{EpdSpiDevice, DC=GPIO8 Output(High), BUSY=GPIO29 Input(no pull), Delay}` 實作 `EpdBus`／`DelayMs`／`BusyPin`（`Instant` 毫秒時鐘）；`new`、`display_error`、`full_refresh_test_pattern`。沒有任何 RST 腳碼 |
| `src/bin/c61_boot.rs` | SD 之後：`epd::new(spi_board.epd, pins.epd_dc, pins.epd_busy)` → `pins.power.display_reset()` → `epd.init(reset)` → `epd::full_refresh_test_pattern`；錯誤只 log，不 panic |

### 19c. 設計決策

- **Full refresh 序列**（`Epd::full_refresh`）：`delay 1 ms` → 對 RED（0x26）再 BW（0x24）各：完整 RAM window（0x11/0x44/0x45/0x4E/0x4F）→ 寫命令 → `delay 1 ms` → 12 條 4000 B strip → `0x21 [40 00]`、`0x22 [F7]`、`0x20`（master activation）→ **有界 BUSY wait**。即 X4 既有序列（含 RED 與 BW 都寫、F7 為 full waveform＋power down）。Partial refresh 未移植（out of scope），`Epd` 沒有 partial API。
- **有界 BUSY**：`wait_busy_bounded`：先取樣再檢查期限，所以恰在上限時刻釋放仍算成功，超過一個輪詢才失敗（測試鎖定 99／100 成功、101 失敗）；輪詢 1 ms（BSP `vTaskDelay(1)`）；另有輪詢次數上限（`timeout/1 + 1`），時鐘卡死也不會 hang（`r11_a_stuck_clock_cannot_hang_the_wait`）。預設 `BUSY_TIMEOUT_MS = 5000`（引用 BSP `:147-150`，BSP 只 log，這裡改回錯誤），可由 `Epd::set_busy_timeout_ms` 配置。C61 不用 `wfi` 輪詢（X4 用）：C61 boot 尚無保證的喚醒中斷，改 `delay 1 ms`。
- **失敗後狀態**：任何 `Err`（BUSY 超時、bus 錯誤）→ `init_done=false`、`initial_refresh` 維持 true，下一次 `full_refresh` 回 `NotInitialized`，必須重新 `init`（SW_RESET 重置控制器）。SPI 錯誤一律回報、立即中止後續寫入（`r11_spi_failure_mid_frame_*`）。
- **Reset 只走軟體**：`Epd::init(reset: DisplayReset)` 需要 `PeripheralPower::display_reset()` 的 token（唯一 variant `Software`）；序列第一個命令是 SW_RESET 0x12。驅動 trait 沒有 RST 腳 API，所以型別層就無法 toggle GPIO27；`r10_display_init_after_sd_is_software_reset_and_never_touches_gpio27` 以 `PeripheralPower<FakeRail>` 驗證 SD 起用後 init＋refresh＋重新 init，rail 事件仍只有開機 power-cycle 的 Low、High。`init` 在 SW_RESET 前後各做一次有界 BUSY wait（X4 沒有，保守新增；BSP 內部流程看不到，未驗）。
- **錯誤對接**：現有 X4 顯示路徑沒有錯誤回報（所有 EPD 呼叫皆無回傳值、bus 錯誤被丟棄），沒有既有的 display 錯誤畫面可接；`ErrorKind` 是與 X4 共用的 `#[non_exhaustive]` enum 且無 display 變體，為了不動 X4（size 比對）不新增，改為 `board_c61::epd::display_error` 對應到 `crate::Error::new(ErrorKind::Other, "display busy timeout"|"display bus error"|"display not initialized")`（`Display` 印成 `error [display busy timeout]`）。把它顯示在畫面上屬 T12。
- **阻塞式**：`full_refresh` 是同步的，最壞阻塞約 5 s（BUSY 上限）＋資料傳輸；async（`embassy` 協作式）整合屬 T12。
- **X4 等價性**：X4 的 command 序列由 (a) host golden 測試 `x4_init_sequence_is_unchanged_golden`／`x4_partial_ram_window_commands_are_unchanged_golden`／`x4_full_update_sequence_is_unchanged_golden` 鎖定共用函式輸出，(b) 更強的端到端證明：`scripts/check-x4-driver-trace.sh` 把**真正的** `kernel/src/drivers/{ssd1677,strip}.rs` 在 host 上以 esp-hal 假件（Delay／Instant）編譯，驅動 init、full frame（含 fill_solid／draw_iter／fill_contiguous／blit_1bpp 圖案）、7 個 partial 區域（phase1_bw、phase1_bw_inv_red、start_du、phase3_sync）、deep sleep、再 init，記錄 SPI／DC／RST／delay 完整 trace（2122 行，data 以長度＋FNV-1a 雜湊）。**改寫前**（git HEAD 的原始檔）與**改寫後**得到同一個 sha256 `66a0447f8519e26704fda427c2fe1da4ad58bd6ae2a618214b0dfe16f6cc6ffb`，腳本以此為 pin（故意改一個 init 位元組會 FAIL，已驗）。

## 20. T6 實際指令與結果

| # | 指令 | 結果 |
|---|---|---|
| 1 | `cargo test-board-logic` | **77 passed; 0 failed**（T5 的 45 項 + 新增 32 項，名單見下） |
| 2 | 變異測試（暫時修改、跑測試、還原） | `>=`→`>`（BUSY 上限）→ 2 項 FAILED（boundary、zero_limit）；Deg270 y 翻轉拿掉 → 6 項 FAILED；RED／BW 順序對調 → 2 項 FAILED；還原後 77 passed |
| 3 | `CARGO_TARGET_DIR=target/t6-c61 cargo build-c61 --locked` | **成功**，無 warning；`size`：text 281600 / data 3280 / bss 76776（T5：277536 / 3248 / 76760；+4,064 text 為 EPD 序列、strip core、BUSY wait）。`nm -C` 見 `board_c61::epd::full_refresh_test_pattern`、`<EpdHw as EpdBus>::command／data`、`<EpdHw as BusyPin>::now_ms`、`ssd1677::wait_busy_bounded::<EpdHw>`、`ssd1677::set_ram_area::<EpdHw>`、`StripCore::fill_logical_rect`；`strings` 見 `epd: full refresh done ...`、`display busy timeout` |
| 4 | `CARGO_TARGET_DIR=target/t6-x4 cargo build-x4 --locked` | **成功**，無 warning；`size`：text 1222004 / data 27624 / bss 348812（T5：1222160 / 27608 / 348828；差 text −156、data +16、bss −16）。ELF section：`.text` −0xB4、`.rodata` +0x18、`.data` +0x10，`.bss` 0x23c80 不變（`size` 的 bss 差來自 `.rtc_fast` 等小段合計；未再追）。符號差異只是邏輯從 `kernel::drivers::{ssd1677,strip}` 移到 `pulp_board_logic::{ssd1677,strip}`（LTO fat 下 inline 邊界移動）。行為等價見 19c 與第 6 項 |
| 5 | `bash scripts/check-board-selection.sh` | 4 項 `ok` |
| 6 | `bash scripts/check-x4-driver-trace.sh` | `ok X4 driver trace identical to pre-T6 (2122 lines, sha256 66a0447f...)` |
| 7 | `OFFLINE_CHECK_TARGET_ROOT=target/t6-check bash scripts/check-offline-boundary.sh` | exit 0，20 項 `ok`（X4 離線 8、X4+wifi 對照 4、C61 離線 8） |
| 8 | `X4_ELF=target/t6-x4/riscv32imc-unknown-none-elf/release/pulp-os bash scripts/check-c61-no-c3-raw-gpio.sh` | 全部 `ok`（c61 4 項、x4 對照 2 項；1 項 info 同 T5） |
| 9 | `grep -n 'unwrap\|expect(\|panic!\|unreachable' kernel/src/board_c61/*.rs src/bin/c61_boot.rs` | 無輸出 |

Cargo.lock 沒有變動（`board-logic` 仍零依賴；`--locked` 通過）。

### 新增 host 測試（32 項，`board-logic/src/ssd1677.rs`）

- command trace（R10）：`x4_init_sequence_is_unchanged_golden`、`x4_partial_ram_window_commands_are_unchanged_golden`、`x4_full_update_sequence_is_unchanged_golden`、`r10_epd_init_trace_is_bounded_wait_soft_reset_wait_configure`、`r10_full_refresh_command_trace`（完整順序：delay → RED window／0x26／12x4000 B → BW window／0x24／12x4000 B → 0x21／0x22／0x20 → BUSY 輪詢 1600 次後清除）、`r10_full_refresh_writes_red_then_bw_each_full_frame`、`r10_full_refresh_before_init_is_refused_and_sends_nothing`、`r10_successful_refresh_clears_initial_refresh_flag`。
- strip 資料（R10）：`r10_portrait_logical_size_is_480x800`、`r10_portrait_four_corner_pixels_land_at_known_stream_offsets`（四角實際出現在 RED／BW 資料流的位置，且其餘位元組全 0xFF）、`r10_top_left_pixel_is_stream_byte_47900_value_7f`（邏輯 (0,0) → 實體 (0,479) → strip 11、列 39、byte 0、值 0x7F）、`r10_single_logical_row_is_one_physical_column_with_bit_order`、`r10_single_logical_column_is_one_physical_row`、`r10_every_rotation_maps_bijectively_onto_the_panel`（四種 rotation 對 800x480 全面雙射）、`r10_blit_fast_path_matches_pixel_path_for_portrait`（Deg270 快路徑 vs 逐像素，含邊緣裁切、寬度 1／5／13 等非 8 倍數、全部 12 條 strip）、`r10_fill_rect_matches_pixel_path_in_every_rotation`、`r10_offscreen_pixels_are_ignored`、`r10_partial_regions_align_to_byte_boundaries_with_edge_masks`（非 8 倍數寬度對齊與邊緣遮罩、四角、空區域）、`r10_bringup_pattern_marks_the_logical_corners_asymmetrically`。
- BUSY（R11）：`r11_default_limit_is_the_bsp_5000_ms`、`r11_stuck_busy_times_out_within_the_limit_and_does_not_hang`、`r11_busy_released_before_the_limit_succeeds`、`r11_boundary_release_at_limit_succeeds_one_ms_later_fails`、`r11_zero_limit_samples_once_then_fails`、`r11_a_stuck_clock_cannot_hang_the_wait`、`r11_stuck_busy_after_update_is_a_display_failure_not_a_hang`（超時 → `BusyTimeout`、狀態失效、`NotInitialized`、重新 init 後恢復）、`r11_busy_timeout_limit_is_configurable`、`r11_init_with_a_stuck_busy_line_fails_before_any_configuration`、`r11_spi_failure_mid_frame_is_reported_and_stops_the_stream`、`r11_errors_have_displayable_text`。
- 軟體 reset：`r10_display_init_after_sd_is_software_reset_and_never_touches_gpio27`、`r10_display_reset_is_refused_before_the_rail_is_up`。

注意：strip 測試驗證的是「方向數學與位元／位元組位置」，與 X4 實機已驗的 `Deg270` 一致；不證明 C61 實體面板方向正確。

## 21. T6 尚未驗證項目

- **硬體全未驗**：EPD 實際顯示、portrait 方向（bring-up 方向卡 `draw_bringup_pattern`：左上 64x64 實心、右上與左下 24x24、右下無；上述任一位置不對即 rotation／mirror 錯）、DC／BUSY 腳實際電位（GPIO8 是 strapping pin、需外部上拉）、BUSY 實際時間（full refresh 實際需要多久、5000 ms 上限是否合適）、waveform／LUT／溫度（X4 用內建溫度感測器，BSP 用固定 `0x5A` 與 OTP LUT；BSP 的 moui 驅動原始碼本機沒有，兩者 init 序列未逐位元比對）、殘影、10 MHz DMA 傳輸 4000 B strip、SW_RESET 後 BUSY 行為（`init` 的前後兩次 BUSY wait 為保守新增）。
- 軟體 reset 取代硬體 RST 後，控制器是否在 power-cycle 後正確啟動（BSP 也是同一假設：初始靠 power-cycle、runtime 靠 SW_RESET）未驗。
- 沒有 partial refresh（out of scope）、沒有 deep sleep／display park（T11，BSP `board_display_sleep`）、沒有 async／embassy 整合與 scheduler 對接（T12），`full_refresh` 為阻塞式。
- display 錯誤只轉成 `crate::Error`（`ErrorKind::Other` + 文字）；畫面顯示留 T12。
- C61 的 `StripBuffer`／`DrawTarget`／fonts／app 層尚未編入（`drivers::strip` 與 `ui::Region` 仍 X4-only，T12 放寬）；T6 的 C61 路徑只用 `StripCore`。
- X4 實機行為未重驗；僅有 host 上的 wire-trace 等價證明（指令序列、資料位元組）與 size 差異（見第 4 項）。`scripts/check-x4-driver-trace.sh` 需要主機有 stable／nightly rustc 與 cargo registry 快取（離線執行），不在 CI。


---

# T7：單 ADC ladder 與 GPIO keys（R12, R13）

日期：2026-10-04。**沒有任何硬體驗收：實際 ADC 電壓、閾值、按鍵手感、GPIO 極性、ADC 衰減／校正、GPIO9 strapping 影響皆未驗。**

## 22. 設計

### 22a. 分層（沿用 host seam；韌體與測試是同一份程式碼）

| 位置 | 內容 |
|---|---|
| `board-logic/src/action.rs`（新，自 X4 搬出） | `Action`／`ActionEvent`（純資料，無 HAL）。X4 `kernel/src/board/action.rs` 改為 `pub use` 再匯出，`ButtonMapper` 仍在 kernel。**沒有新增或刪除任何 Action**；單一定義，C61 對照表在 host 上直接以 apps 實際比對的型別測試 |
| `board-logic/src/input.rs`（新，自 X4 `input.rs` 搬出） | `InputCore<K>`：debounce／long-press／repeat／event queue／`reset_hold_state`，泛型於鍵型別；`Event<K>`、`InputTiming`、`RawSource<K>` trait（`read_raw`、`now_us`）；常數 `DEBOUNCE_MS=15`／`LONG_PRESS_MS=1000`／`REPEAT_MS=150`／`ADC_OVERSAMPLE=4`（X4 值；X4 `timing.rs` 改為引用同一組常數）。時間用微秒（X4 `Instant` 不被截成毫秒，行為精確一致）。逐行搬移：同樣 `>=`、同樣「raw 偏離 stable 時重置 hold 計時」、queue 非空時不讀硬體、先讀 raw 再讀時鐘 |
| `board-logic/src/keys.rs`（新） | `Key`（BSP 順序 7 鍵）、`FRONT_LADDER` 窗口表、`decode_front_ladder`、`decode_gpio`、`Key::action(swap)`／`map_event`、trait `AdcSample`（單次校正後 mV，失敗為 `None`）、`KeyPin`、`Clock`、`average_mv`、`KeyScanner`（grace＋latch＋解碼＋優先序，實作 `RawSource<Key>`）、`KeyInput`（scanner＋`InputCore`；`poll`／`poll_action`／`reset_hold_state`） |
| `kernel/src/drivers/input.rs`（X4，改寫） | `InputDriver` 只剩 X4 硬體側：`X4Source`（power 鍵優先、row1、row2，順序同前）實作 `RawSource<Button>`，其餘交給 `InputCore<Button>`；`Event` 是 `pulp_board_logic::input::Event<Button>` 的別名；LongPress 的 info log 保留 |
| `kernel/src/board_c61/keys.rs`（新） | esp-hal adapter：`FrontLadder`（`Adc<ADC1>`＋`AdcPin<GPIO4, ADC1, AdcCalCurve>`，`Attenuation::_11dB`，`read_oneshot` 以有上限自旋取代 `nb::block!`，失敗回 `None`）、`GpioKey`（`Input` 上拉）、`HalClock`（`Instant` 微秒）、`keys::new(ADC1, GPIO4, GPIO2, GPIO6, GPIO9)`（先核對 GPIO4 的 esp-hal ADC 通道＝`FRONT_ADC_CHANNEL`，不符回 `KeysError::AdcChannelMismatch`）、`adc_channels()` |
| `src/bin/c61_boot.rs` | 於 `esp_rtos::start` 後建立 key driver（grace 從此刻起算）並 log 兩個 ADC 通道；主迴圈改為 10 ms tick：每 tick `poll()` 並 log `Event` 與對映後的 `ActionEvent`，card detect 每 2 tick（20 ms，沿用 `CD_SAMPLE_INTERVAL_MS`）取樣一次。失敗只 log，不 panic |

### 22b. ADC 通道結論（實際編譯確認）

- **前面板 GPIO4 = ADC1_CH2**：`esp-metadata-generated 0.5.3`（Cargo.lock 唯一版本，esp-hal 1.2.0 使用）`_generated_esp32c61.rs:4141` `(ADC1_CH2, GPIO4)`、`:4149` `((ADC1_CH2, ADCn_CHm, 1, 2), GPIO4)`；`GPIO4<'static>` 實作 `AdcChannel + AnalogPin`、`AdcCalCurve<ADC1>` 於 C61 可編譯（`kernel/src/board_c61/keys.rs` 實際 `enable_pin_with_cal::<_, AdcCalCurve<ADC1>>(GPIO4, _11dB)` 並連結成功）。執行期再以 `GPIO4.adc_channel() == 2` 防呆。
- **GPIO5（電池）= ADC1_CH3**（同檔 `:4142`、`:4150`），與 BSP `board_c61.c:63`（`BAT_ADC_CHANNEL ADC_CHANNEL_3`）、`README.md:50`（BAT ADC GPIO5 = ADC1_CH3）一致。**T4 所述「矛盾」的來源**：`board_c61.c:315` 診斷函式的註解寫「front ADC-ladder node (GPIO5 = ADC1_CH2)」，但同函式程式碼 `:322` 用 `ADC_CHANNEL_2`、`board_keys.c:25-29,33` 與 README 都說前面板 = GPIO4 = CH2。結論：**註解筆誤，不是通道衝突**；GPIO4 → CH2、GPIO5 → CH3，兩者同在 ADC1，可共用一個 `Adc`。
- **給 T9**：電池取樣必須與 keys 共用同一個 ADC1 實例（BSP 共用同一 oneshot handle，`board_keys.c:119-121`）。esp-hal 要求所有 ADC 腳在 `Adc::new` 前以同一個 `AdcConfig` 啟用，所以 T9 要把 `keys::new` 的 ADC 建立部分拆出（建一個含 GPIO4＋GPIO5 的 `AdcConfig`，再把 `Adc` 以共享方式交給 `FrontLadder` 與電池讀取）。`pins.battery_adc`（GPIO5）T7 沒有被消耗；`c61_boot` 只用 `&pins.battery_adc` 呼叫 `adc_channel()` 取得 esp-hal 表的通道並 log（`adc: GPIO4 -> ADC1_CH2 (BSP 2), GPIO5 -> ADC1_CH3 (BSP 3)`）。BSP 電池以 `ADC_ATTEN_DB_12`＋curve fitting，與這裡前面板相同設定，且電池取樣須暫停充電（T9）。

### 22c. OnePage 實體鍵 → 語意 Action 對照（R12）

來源：BSP `board_keys_moui.c:21-29` 的 `s_map`（WAKE→BACK、PREV→UP、NEXT→DOWN、BACK→BACK、LEFT→LEFT、RIGHT→RIGHT、ENTER→ENTER）；再以 X4 既有 `ButtonMapper` 的語意落到 `Action`（上/prev→`Prev`、下/next→`Next`、左右→`PrevJump`/`NextJump`、ENTER→`Select`）。

| 實體鍵 | 腳位 | BSP 語意 | `Action`（預設） | `Action`（swap，左手版） |
|---|---|---|---|---|
| WAKE | GPIO2（active-low；T11 兼 wake） | BACK | `Back` | `Back` |
| PREV | GPIO6（active-low） | UP | `Prev` | `Prev` |
| NEXT | GPIO9（active-low，strapping pin） | DOWN | `Next` | `Next` |
| BACK | ladder 2400–2800 mV | BACK | `Back` | `PrevJump` |
| LEFT | ladder 1780–2140 mV | LEFT | `PrevJump` | `Back` |
| RIGHT | ladder 1140–1500 mV | RIGHT | `NextJump` | `Select` |
| ENTER | ladder 0–250 mV | ENTER | `Select` | `NextJump` |

swap 沿用 X4 規則（前排 Back↔Left、Confirm↔Right；側鍵與 WAKE 不 swap）。

**取捨（需使用者／T11／T12 決定）**：
- `Action::Menu` 在 C61 **沒有實體鍵**（X4 用 Power 短按 = Menu = quick menu；Power 長按 = 睡眠，`scheduler.rs` 以 `Event::LongPress(Button::Power)` 判斷）。BSP 把 WAKE 映成 BACK，所以 T7 遵 BSP，WAKE→`Back`，`Menu` 不可達（測試 `r12_every_existing_action_but_menu_is_reachable_and_none_is_new` 鎖定這個事實）。這代表 quick menu 與「長按進睡眠」在 C61 尚無入口；候選方案（WAKE 長按 = 睡眠、某鍵長按 = Menu 等）會是新的互動設計，**未在 T7 實作**，留給 T11／T12 決定。
- 與 X4 同為單鍵模型（見 22d 的同時按下規則）。

### 22d. 解碼規則與依據

- **前面板 ladder**（`board_keys.c:32-44`，BSP 實機量測，靜止約 3100 mV）：BACK 2400–2800（量測約 2592）、LEFT 1780–2140（約 1956）、RIGHT 1140–1500（約 1316）、ENTER 0–250（約 0）。窗口之間刻意留死區（BSP 註解：暫態掃過不同電位時應落在「無鍵」而非鄰鍵）。無鍵 = 窗口外（含 3100 靜止、飽和值、65535）。邊界：**含 min 與 max**（與 X4 `decode_ladder` 的 `>=`／`<=` 一致）；BSP 的 `espressif/button` button_adc 原始碼本機沒有，其是否含 max 未驗，只影響上緣 1 mV。
- **ENTER 帶包含 0 mV**：ADC 讀取失敗若被當成 0 就會變 ENTER（BSP 為此在 `board_keys_moui.c:54-66` 另做 ENTER 重讀確認 ≤300 mV）。這裡的處理：`AdcSample::sample_mv` 失敗回 `None`，`average_mv` 任一次失敗整體 `None`，decode 為無鍵（測試 `r12_adc_failure_is_not_zero_mv_and_not_enter`）；真正的 0 mV 仍是 ENTER。BSP 的 ENTER 重讀沒有移植，由 15 ms 去抖（同一讀值須持續 ≥15 ms，10 ms tick 下即連續 2 個 tick 以上）與 startup grace 取代，是否足夠需實機。
- **GPIO keys**：active-low、內部上拉（`board_keys.c:104`）。
- **同時按下**：輸入路徑是單鍵模型（同 X4：一次一鍵）。固定優先序 **GPIO（WAKE > PREV > NEXT）> ladder**；GPIO 有鍵時不取樣 ADC（同 X4 power 鍵優先並略過 ADC）。鍵集合改變時依 debounce 輸出 `Release(舊)`、`Press(新)`；贏家放開而另一鍵仍按著時，另一鍵會重新 `Press`。BSP 本身是每鍵獨立事件（無優先序），這是為了對齊既有單鍵語意的取捨（測試 `r12_two_gpio_keys_report_the_higher_priority_only`、`r12_ladder_plus_gpio_the_gpio_key_wins_and_skips_the_adc`）。
- **debounce／long-press／repeat 常數**：C61 沿用 X4（15／1000／150 ms，`InputTiming::PULP`），因為上層 app 的 `LongPress(Back)`→Home 等語意以 X4 手感調過。**與 BSP 的差異**：BSP 用 `espressif/button`，`long_press_time = 800` ms（`board_keys.c:93`），且 moui adapter 另有 120 ms 同鍵重觸發鎖（`NAV_DEBOUNCE_US`，`board_keys_moui.c:18`）；這兩個值沒有採用。要改只需換 `InputTiming`。15 ms 去抖是否足以濾掉 ladder 彈跳（BSP 當年需要 120 ms 鎖）**未驗**，是主要的實機風險。

### 22e. Startup grace（R13）

- 長度 2.5 s（`board_keys.c:22` `KEYS_STARTUP_GRACE_US 2500000`），從 `keys::new`（對應 BSP `board_keys_init`，`board_keys.c:91`）起算；`elapsed < 2_500_000 µs` 為 grace（BSP `:58` 同一個比較，所以**恰好 2.5 s 已結束**，2_499_999 µs 仍在 grace）。
- grace 內：**不取樣**ADC 與 GPIO，原始讀值視為無鍵，因此即使 ladder 讀到 0 mV（ENTER 帶，BSP 記載的開機 ADC 沉澱現象）或任何有效鍵值都不產生事件（`r13_enter_band_reading_during_grace_emits_nothing`、`r13_every_front_key_value_during_grace_emits_nothing`、`r13_long_hold_during_grace_never_long_presses`）。
- **GPIO keys 在 grace 內也被抑制**——依 BSP：`btn_trampoline` 對 7 個鍵共用同一個 grace 檢查（`board_keys.c:57`，註解「Drop all key events during this window」）。R13 只要求前面板；這是 BSP 忠實的超集（`r13_gpio_keys_are_suppressed_during_grace_like_the_bsp`）。副作用：開機後 2.5 s 內的側鍵操作被丟棄；也解決「從 deep sleep 以 WAKE 喚醒時，鍵仍被按著」不應變成 `Back` 的問題。
- **grace 結束時鍵已被按住**：第一次 grace 後取樣到的鍵會被「latch」，在放開或換成別的鍵之前都不輸出（不輸出 Press、LongPress、Repeat，放開時也不輸出 Release）。對應 BSP：它在 grace 內丟掉 DOWN，之後只會有 UP／LONG，moui 只用 DOWN，所以效果同為「不觸發」。換成不同的鍵則視為新按下（`r13_key_held_through_grace_end_is_ignored_until_released`、`r13_latched_key_is_replaced_by_a_different_key`、`r13_gpio_key_held_from_wake_up_through_grace_is_ignored_until_released`、`r13_key_pressed_just_after_grace_is_reported`）。邊界副作用：在 grace 結束前一刻（一個輪詢週期內）按下的鍵會被視為「已按住」而忽略到放開。

## 23. T7 實際指令與結果

| # | 指令 | 結果 |
|---|---|---|
| 1 | `cargo test-board-logic` | **125 passed; 0 failed**（T6 的 77 項 + 新增 48 項：`input` 11、`keys` 37）|
| 2 | `CARGO_TARGET_DIR=target/t7-c61 cargo build-c61 --locked` | **成功**，0 warning；`size`：text 285282 / data 3488 / bss 76888（T6：281600 / 3280 / 76776；+3,682 text 為 key 驅動、ADC＋校正、輸入邏輯、log 字串）。`nm -C` 見 `<KeyScanner<FrontLadder, GpioKey, HalClock>>::sample`、`<Event<Key> as Debug>::fmt`、`Option<KeyInput<FrontLadder, GpioKey, HalClock>>`；`strings` 見 `keys: ladder GPIO4 + GPIO2/6/9 ready, startup grace`、`key: `、`adc: GPIO4 -> ADC1_CH`。`c61_boot` 實際呼叫 `keys::new` 並於主迴圈 `poll()`（只證明編譯／連結，不宣稱能開機／按鍵可用） |
| 3 | `CARGO_TARGET_DIR=target/t7-x4 cargo build-x4 --locked` | **成功**，0 warning；`size`：text 1221824 / data 27624 / bss 348812（T6：1222004 / 27624 / 348812；text −180，data／bss 0）。差異來源：input 狀態機與 action 型別搬到 `pulp_board_logic`（LTO fat 下 inline 邊界與 queue 泛型化；`Event` 亦改為泛型別名），**未再逐符號追**；行為等價以第 4 項證明 |
| 4 | `bash scripts/check-x4-input-trace.sh`（新） | `ok X4 input trace identical to pre-T7 (26032 lines, sha256 2c3738aa...)`。把**真正的** `kernel/src/drivers/input.rs`、`board/button.rs`、`kernel/timing.rs` 在 host 上以 esp-hal 時間 shim（微秒時鐘）＋假 ADC／power pin 編譯，跑 5 個腳本情境（單鍵長按到 repeat、<15 ms 彈跳、1 ms 解析度的邊界掃描、power 優先／row2／換鍵、`reset_hold_state`）加 30 萬步偽隨機走訪（含長按段），記錄每個事件、其時間點與累計 ADC／power 讀取次數（鎖定「哪些輪詢碰了硬體」）。pin 由 **git HEAD 的舊 input.rs**（`INPUT_RS=/tmp/input_head.rs`）產出，同一個 sha256；改寫後的檔案得到相同 sha256 |
| 5 | `bash scripts/check-x4-driver-trace.sh` | `ok X4 driver trace identical to pre-T6 (2122 lines, sha256 66a0447f...)`（display 仍不變） |
| 6 | `bash scripts/check-board-selection.sh` | 4 項 `ok` |
| 7 | `OFFLINE_CHECK_TARGET_ROOT=target/t7-check bash scripts/check-offline-boundary.sh` | exit 0，20 項 `ok`（X4 離線 8、X4+wifi 對照 4、C61 離線 8） |
| 8 | `X4_ELF=target/t7-x4/riscv32imc-unknown-none-elf/release/pulp-os bash scripts/check-c61-no-c3-raw-gpio.sh` | exit 0，全部 `ok`（c61 4 項、x4 對照 2 項；1 項 info 同 T5） |
| 9 | `grep -n 'unwrap\|expect(\|panic!' kernel/src/board_c61/keys.rs src/bin/c61_boot.rs` | 無輸出（`board-logic` 非測試碼亦無） |

`cargo fmt --all -- --check` 無差異。Cargo.lock 未變動（`board-logic` 仍零依賴；`--locked` 通過）。

### 新增 host 測試（48 項）

- 時間邊界／狀態機（`input`，11；名稱含 R12）：`r12_debounce_press_needs_exactly_debounce_ms_of_stability`（t=24 不出、t=25 出）、`r12_debounce_bounce_shorter_than_window_makes_no_event`、`r12_debounce_restarts_on_every_change`、`r12_debounce_release_is_debounced_and_reported_once`、`r12_long_press_fires_exactly_at_threshold_from_the_press`（1014 不出、1015 出）、`r12_repeat_interval_and_boundary`（+149 不出、+150 出，之後每 150 ms）、`r12_release_stops_long_press_and_repeat`、`r12_reset_hold_state_suppresses_long_press_until_release`、`r12_sub_debounce_release_does_not_accumulate_into_long_press`、`r12_switching_keys_queues_release_then_press_without_reading_hardware`、`pulp_timing_constants_are_the_x4_values`、`held_us_counts_from_the_press`。
- ladder 解碼：`r12_ladder_bsp_measured_values_decode_to_their_keys`（2592／1956／1316／0）、`r12_ladder_window_edges_are_inclusive_and_one_mv_outside_is_none`（每窗 min、max、±1）、`r12_ladder_exact_bsp_thresholds`、`r12_ladder_rest_and_out_of_range_values_are_no_key`（3100…65535）、`r12_ladder_dead_zones_between_keys_are_no_key`（251–1139、1501–1779、2141–2399 全掃）、`r12_ladder_windows_are_ordered_and_disjoint_with_gaps`、`r12_ladder_every_mv_maps_to_the_one_window_that_contains_it`（0..=65535 全掃）、`r12_ladder_transient_across_bands_does_not_fire_a_neighbour`、`r12_average_is_the_truncated_mean_of_oversample_reads`、`r12_adc_failure_is_not_zero_mv_and_not_enter`、`r12_each_ladder_key_press_release_after_grace`、`r12_ladder_long_press_and_repeat_via_the_shared_core`。
- GPIO／同時按下：`r12_gpio_keys_are_active_low`、`r12_each_gpio_key_is_pressed_when_its_pin_is_low`、`r12_gpio_high_pin_is_released_not_pressed`、`r12_gpio_decode_priority_wake_prev_next_over_all_combinations`（8 種組合）、`r12_two_gpio_keys_report_the_higher_priority_only`、`r12_ladder_plus_gpio_the_gpio_key_wins_and_skips_the_adc`。
- 語意映射：`r12_key_to_action_table_default_layout`、`r12_key_to_action_table_swapped_layout`、`r12_every_existing_action_but_menu_is_reachable_and_none_is_new`、`r12_map_event_keeps_the_event_kind`、`r12_poll_action_maps_through_the_table`、`key_names_follow_the_bsp`。
- startup grace（R13）：`r13_grace_is_two_and_a_half_seconds`、`r13_enter_band_reading_during_grace_emits_nothing`、`r13_every_front_key_value_during_grace_emits_nothing`、`r13_long_hold_during_grace_never_long_presses`、`r13_grace_boundary_one_microsecond_before_and_exactly_at_2500ms`、`r13_grace_runs_from_creation_not_from_clock_zero`、`r13_key_held_through_grace_end_is_ignored_until_released`、`r13_latched_key_is_replaced_by_a_different_key`、`r13_key_pressed_just_after_grace_is_reported`、`r13_gpio_keys_are_suppressed_during_grace_like_the_bsp`、`r13_gpio_key_held_from_wake_up_through_grace_is_ignored_until_released`、`r13_gpio_key_pressed_after_grace_is_reported`。

### 變異測試（暫時修改、跑測試、還原；還原後 125 passed）

| # | 變異 | 結果 |
|---|---|---|
| M1 | grace 比較 `< STARTUP_GRACE_US` → `<=` | 3 項 FAILED（`r13_grace_boundary_*`、`r13_grace_runs_from_creation_*`、`r13_key_pressed_just_after_grace_is_reported`） |
| M2 | LEFT min 1780 → 1779 | 2 項 FAILED（`r12_ladder_dead_zones_*`、`r12_ladder_exact_bsp_thresholds`） |
| M3 | BACK max 2800 → 2801 | 2 項 FAILED（`r12_ladder_exact_*`、`r12_ladder_rest_and_out_of_range_*`） |
| M4 | ladder 上緣 `<=` → `<` | 2 項 FAILED（`r12_ladder_window_edges_*`、`r12_ladder_every_mv_*`） |
| M5 | debounce `>=` → `>` | 多項 FAILED（含 `r12_debounce_press_needs_exactly_*`） |
| M6 | long-press `>=` → `>` | 4 項 FAILED |
| M7 | repeat `>=` → `>` | 2 項 FAILED |
| M8 | 移除 grace-end latch | 3 項 FAILED（`r13_*_held_*`） |
| M9 | 預設 LEFT 的 Action 改成 NextJump | 4 項 FAILED |
| M10 | GPIO 優先序 PREV 先於 WAKE | 2 項 FAILED |
| X4-M | 共用核心 debounce `>=`→`>`；long-press `>=`→`>` | `check-x4-input-trace.sh` 兩次皆 FAIL（sha256 改變），還原後 ok |

## 24. T7 尚未驗證項目

- **硬體全未驗**：ladder 實際電壓（BSP 窗口是「這塊板」量測值，換板需用 `board_front_key_mv()` 重量；本專案沒有等價的 mV 診斷輸出，bring-up 時要靠 `c61_boot` 的事件 log 或在 T14 補診斷）、窗口邊界是否含 max、ADC 衰減 `_11dB`（BSP `ADC_ATTEN_DB_12`）與 curve-fitting 校正在 C61 上是否給出與 BSP 相同的 mV（esp-hal 校正與 ESP-IDF 校正是否一致未驗；C61 eFuse 校正資料是否存在未驗，缺資料時 `AdcCalCurve` 行為未驗）、開機 ~2 s ADC 沉澱現象的真實長度（2.5 s grace 是否足夠）、GPIO 極性與上拉、GPIO9 strapping pin 被按住開機的影響、GPIO2 在 T11 兼 wake、按鍵手感與 15 ms 去抖是否足以濾掉 ladder 彈跳（BSP 需要 120 ms 重觸發鎖）、同時按下的實際手感。
- 與 BSP 的有意差異（見 22d）：long-press 1000 ms（BSP 800）、無 120 ms 同鍵鎖、無 ENTER 重讀確認、單鍵優先序模型（BSP 每鍵獨立事件）、grace 結束時按住的鍵直到放開前都忽略。
- `Action::Menu` 無實體鍵、睡眠觸發鍵未定（22c）。
- 輪詢頻率自適應（X4 scheduler 的 10 ms／50 ms 切換）、與 scheduler／app 的接線、`swap_buttons` 設定來源屬 T12；`c61_boot` 只是固定 10 ms 輪詢並 log。
- ADC 與電池共用 `Adc` 實例（T9，見 22b）；`keys::new` 目前獨占 ADC1。
- `FrontLadder::sample_mv` 的自旋上限（20000 次）與實際轉換時間未實測。
- X4 實機行為未重驗；僅有 host 上的事件流等價證明與 size 差異（第 3、4 項）。`scripts/check-x4-input-trace.sh` 需要 host rustc 與 cargo registry 快取（離線），不在 CI。


---

# T8：PSRAM／internal-memory 分工、失敗處理、配置預算與 ELF 證據（R14, R15, R22, R23）

日期：2026-10-04。**沒有任何硬體驗收：PSRAM 初始化、40 MHz 穩定度、實際 heap 使用量、stack 高水位、cache／DMA 一致性全部未驗。** 本節不宣稱 PSRAM 能運作；`c61_boot` 只證明 init／註冊／預算配置的程式碼能編譯、連結。

## 25. 設計

### 25a. 分層（沿用 host seam）

| 位置 | 內容 |
|---|---|
| `board-logic/src/memory.rs`（新，host 測試） | C61 位址表（`addr_space`／`range_is_internal`）；配置類別 `MemClass`（`DmaBuffer`／`IsrData`／`Runtime` 只能 internal，`ChapterText`／`ImageData`／`PageTable`／`ZipToc` 可放 PSRAM）與型別 `ExternalClass`（**沒有** DMA／ISR／runtime 變體）；預算 `MemoryBudget`（每類別上限＋每 region 總池，`reserve`／`reserve_in`／`release`，checked arithmetic，錯誤是 `Err(MemError)`，無 panic）；PSRAM 狀態機 `PsramStatus`（`NotInitialised`／`Ready{bytes}`／`Degraded(PsramFault)`）、`evaluate_psram`、`window_fault`；PSRAM 冒煙測試 `selftest`（over `WordMem` trait）；ELF 預算規則 `check_image`／`section_placement_ok`；大配置盤點表 `INVENTORY`（供 T12） |
| `board-logic/examples/memreport.rs`（新） | host 小工具：`constants`（印預算常數）、`inventory`（印盤點表 markdown）、`check`（stdin 讀 ELF 事實，套用 `check_image`／`section_placement_ok`／`range_is_internal`，違規 exit 1）。`scripts/report-c61-memory.sh` 透過它套用規則，判定邏輯因此與韌體是同一份程式碼 |
| `kernel/src/board_c61/memory.rs`（新，只 C61 編入） | esp-hal 1.2／esp-alloc 0.11 adapter：`init(PSRAM)`、私有 `PSRAM_HEAP`（`EspHeap`）、`BUDGET`（critical-section mutex）、`MemBuf`（RAII：drop 時 free＋歸還預算，配置時清零）、`alloc`／`alloc_external`／`alloc_dma`、`status`／`log_report`；`Cache_WriteBack_Addr`／`Cache_Invalidate_Addr` ROM 函式的 extern 宣告（`esp_hal::soc::cache_*` 是 HAL 私有，同一組 ROM 函式 esp-hal DMA 也在用）給冒煙測試 |
| `src/bin/c61_boot.rs` | internal heap 改用預算常數（main 96 KiB＋`#[ram(reclaimed)]` 64000 B）；`memory::init(peripherals.PSRAM)`；`memory_demo()`：PSRAM 64 KiB 區塊、超出類別上限 1 byte 必須被拒、`MemClass::ImageData` 自動置放、`alloc_dma(256)`，全部只 log，錯誤不 panic |

X4 不編入 `board_c61`；`board-logic` 的 `memory` 模組在 X4 沒有任何引用（LTO 全移除）。

### 25b. esp-hal／esp-alloc 實際 API（讀 registry 原始碼並實際編譯確認，不是猜）

- `esp_hal::psram::{Psram, PsramConfig, FlashFreq, SpiRamFreq, PsramSize}`，需要 `unstable`（workspace 已開）。`Psram::new(peripherals.PSRAM, PsramConfig) -> Psram`，**不回傳 Result**；`raw_parts() -> (*mut u8, usize)` 是唯一可觀察的結果。C61 與 C5 共用 `psram/esp32c5_c61.rs`＋`quad.rs`（Quad SPI）；`esp-hal` 的 `esp_config.yml` **沒有** C61 的 `psram-*` 選項，時脈與 tuning 只透過 `PsramConfig` 結構設定。
- **預設值是 `flash_frequency = FlashFreq80m`、`ram_frequency = Freq40m`**，`init` 會 `mspi_timing_config_set_flash_clock(config.flash_frequency)`，也就是會把 flash 重設成 80 MHz。BSP README（`:77`、`:155`）寫明 80 MHz flash 在這塊板子會 image-hash boot loop，proposal 規定 bring-up 用 flash40／PSRAM40，所以 `init` 明確設 `FlashFreq40m`＋`Freq40m`（常數 `FLASH_MHZ`／`PSRAM_MHZ`，並有 `const _: () = assert!(FLASH_MHZ == 40 && PSRAM_MHZ == 40)` 與測試 `r14_clock_plan_is_flash40_psram40`）。`ram_frequency` 40 MHz 時 esp-hal 不做 PSRAM timing tuning（`need_timing_tuning` 只在 80 MHz）。
- 偵測：`size = AutoDetect`（預設）時讀 chip id，density 0 → 2 MiB；`dev_id & 0xffffff == 0xffffff`（無晶片）只 `warn!` 後 return，**`Psram::new` 仍正常回傳**，`raw_parts()` 的 size = 0。所以「沒偵測到」＝size 0，由 `evaluate_psram(0, _)` 轉成 `Degraded(NotDetected)`。
- esp-hal 對 PSRAM 用的腳：`PSRAM_CS_IO = GPIO14`、`PSRAM_SPIWP_SD3_IO = GPIO17`（`psram/esp32c5_c61.rs`），不在 `board-logic::pins` 的使用表內（無衝突）。
- esp-alloc 0.11：`EspHeap::empty()`、`add_region(HeapRegion::new(ptr, size, MemoryCapability::External.into()))`（每個 heap 最多 3 個 region）、`alloc_caps(caps, layout)`、`GlobalAlloc` for `EspHeap`；`psram_allocator!(PSRAM, esp_hal::psram[, config])` 與 `ExternalMemory`／`InternalMemory`／`DmaCompatible*Memory`（`allocator_api2::Allocator`）。**不採用 `psram_allocator!`**，原因見 25c。不需要 nightly `allocator_api` 或 `allocator-api2` 依賴（直接用 `GlobalAlloc` 與 `alloc_caps`），Cargo.lock 未動。
- C61 metadata：`psram.extmem_origin = 0x4200_0000`、`mmu.page_size = 65536`、`soc.internal_memory_cached = false`、有 `dma_can_access_psram`（**esp-hal 允許 GDMA 在 PSRAM buffer 上運作**，只要對齊並做 cache 同步，`dma/buffers/mod.rs`）。所以「DMA 只能用 internal」是**本專案的政策**，不是硬體限制；靠下面 25c 的三層約束落實。

### 25c. 政策（internal vs PSRAM）與理由

1. **全域 allocator 永遠只有 internal region**（`heap_allocator!`）。PSRAM 註冊在**私有的第二個 `EspHeap`（`PSRAM_HEAP`）**，不走 `psram_allocator!`。理由（讀 `esp-alloc/src/lib.rs:528-549`）：不指定 capability 的 `alloc` 會依序嘗試所有 region；`psram_allocator!` 把 `External` region 加進全域 `HEAP`，internal 滿了以後，一般 `Box`／`Vec`（含 runtime、executor、ISR 會碰的結構）會悄悄落到 PSRAM，R15 就破功。
2. PSRAM 只能經 `alloc_external(ExternalClass, ..)`（型別上就沒有 DMA／ISR／runtime 可選）或 `alloc(MemClass, ..)`（由 `MemoryBudget::region_for` 決定：只有 PSRAM-capable 類別且狀態 `Ready` 才進 PSRAM）取得；`reserve_in(Psram, DmaBuffer|IsrData|Runtime, ..)` 回 `Err(RegionForbidden)`，且 PSRAM 對這三類的上限為 0（兩道防線，都有測試）。
3. **DMA buffer／descriptor 三層約束**：(a) 靜態：`esp_hal::dma_*_buffer!` 放在 `aligned::InternalMemory` 型別的 `static`，連結器不會把任何 section 放到 PSRAM；(b) 執行期：esp-hal `DescriptorSet::new` 以 `is_slice_in_dram` 拒絕 DRAM 以外的 descriptor（`UnsupportedMemoryRegion`），buffer 則依 esp-hal 允許 PSRAM，所以由 (c) 負責；(c) 本專案：`alloc_dma()` 走 `DmaBuffer` 類別（internal only），配置後再以 `range_is_internal` 複驗位址；ELF 檢查以 `nm` 證明 SPI `BUFFER`／`DESCRIPTORS` 位址落在 internal range（§27）。
4. **ISR／critical-section／runtime 資料**：全是 static 或全域 internal heap；ELF 檢查要求所有可寫 section 完全落在 internal RAM、且沒有任何 data／bss 符號在 flash／PSRAM 視窗（`section_placement_ok`）。
5. **預算**：每類別上限＋每 region 總池；PSRAM 總池 = 偵測容量（夾到 2 MiB）− 保留 448 KiB；超過回 `ClassLimit`／`PoolExhausted`，不 panic。每次配置依 `max(align, 16)` 向上取整計費（allocator header／對齊不能鑽預算）。`size = 0`、非 2 的冪對齊、`usize` 溢位各有明確錯誤。
6. **失敗處理（降級而非中止）**：任何 PSRAM 問題（無晶片、容量 < 1 MiB、視窗不合法、冒煙測試失敗）→ `Degraded(PsramFault)`：不註冊 PSRAM heap、預算改用 internal 上限（章節 96 KiB／圖片 112 KiB／頁表 8 KiB／ZIP 32 KiB，即 X4 規模）、記 `warn!`，**繼續跑**。理由：X4 沒有 PSRAM 也能完整離線閱讀，所以 PSRAM 對「離線閱讀可用性」不是必要條件（R14 只要求「使用 PSRAM 時要受預算限制」）；PSRAM 在這塊板子的 40 MHz 時序未驗，最保守的做法是壞了就用較小的 internal 模式，而不是讓整機停在錯誤畫面。降級的代價：大章節或大圖會被預算拒絕（回 `Err`，由 T12 轉成既有的「章節太大／圖片略過」錯誤路徑）。`NotInitialised`（尚未 init）視同降級，所以忘記呼叫 `init` 是 fail-safe。狀態在還有 PSRAM-capable 類別的存活配置時不可變更（`StatusChangeWhileLive`），避免帳與實際 region 不一致。
7. **冒煙測試（`selftest`）**：對整個視窗寫 word 0、每個 2 的冪索引（抓位址線故障／別名）、最後一個 word 的獨立 pattern，再寫反相 pattern 讀回（抓 stuck bit）；寫入後對該 word 做 `Cache_WriteBack_Addr`＋`Cache_Invalidate_Addr`，讀回才真的來自晶片。它不是 RAM 測試，只抓「晶片不在／位址線死／資料線卡死」。ROM cache 函式在 C61 上的實際行為**未驗**。

## 26. 預算表（常數在 `board-logic/src/memory.rs`，腳本與測試共用）

### 26a. Internal（HP SRAM，memory.x：`RAM` 0x40800000+0x3EA70 = 256,624 B，`dram2_seg` 0x4083EA70+0x10000 = 65,536 B，合計 322,160 B；板子 `maximum_ram_size` 327,680 B）

| 項目 | 值 | 說明 |
|---|---|---|
| main heap（`.bss` 內 static） | 98,304 B（96 KiB） | `INTERNAL_HEAP_MAIN_BYTES`；`c61_boot` 已使用 |
| reclaimed heap（`.dram2_uninit`） | 64,000 B | `INTERNAL_HEAP_RECLAIMED_BYTES`，占 dram2_seg 97%；與 X4 相同做法（`#[ram(reclaimed)]`）。**bootloader 結束後 dram2 是否真的可用於 esp-bootloader-esp-idf 啟動的 C61 未驗** |
| internal 總 heap | 162,304 B | X4 是 110,592＋64,000 = 174,592 B |
| `.stack` 下限 `STACK_MIN_BYTES` | 49,152 B（48 KiB） | esp-hal 的連結期下限是 8 KiB；X4 註解規劃約 56 KB；這是本專案選的下限 |
| 靜態上限 `STATIC_RAM_MAX_BYTES` | 207,472 B | = `RAM` − `STACK_MIN_BYTES`；包含 main heap |
| internal 類別上限（API 配置） | DMA 16 KiB／ISR 4 KiB／runtime 32 KiB | 降級模式：章節 96 KiB／圖片 112 KiB／頁表 8 KiB／ZIP-TOC 32 KiB；受 internal 總池 162,304 B 共同限制 |

### 26b. PSRAM（2 MiB 零件；`PSRAM_HW_BYTES`，更大的晶片夾到 2 MiB；< `PSRAM_MIN_BYTES` 1 MiB 視為不可用）

| 類別 | 上限 | 預期用途（T12） |
|---|---|---|
| `ChapterText` | 786,432 B（768 KiB） | 章節文字快取（現 96 KiB 一份，PSRAM 可同時留數章）、prefetch、章節 inflate window |
| `ImageData` | 524,288 B（512 KiB） | 解碼後頁面圖片、PNG／JPEG decode 暫存 |
| `PageTable` | 65,536 B（64 KiB） | 頁偏移表（現 `MAX_PAGES` 512 × 4 B = 2 KiB，可放寬） |
| `ZipToc` | 262,144 B（256 KiB） | ZIP central directory／entry index／name pool、EPUB TOC |
| 保留（allocator metadata、碎片、T12 headroom） | 458,752 B（448 KiB） | 不配置 |
| 合計 | 2,097,152 B | 編譯期 `const _: () = assert!(sum + reserve <= PSRAM_HW_BYTES)` |

PSRAM 總池 = 偵測容量 − 保留 = 1,638,400 B（2 MiB 零件）。各類別上限只是 T8 依盤點表提出的**起始值**；T12 實際搬遷後可在類別間調整，總和斷言與測試會擋住超出晶片。

## 27. 盤點表（>= 4 KiB 的 static／Box／Vec／stack，另列 DMA descriptor 與 STRIP 因為 R15／DMA 相關；供 T12 使用）

大小來源：X4 release ELF `nm -C -S --size-sort`（static，精確）、smol-epub／app 原始碼常數（heap／stack，精確到常數，但 heap 實際用量依書而定）、C61 T8 boot ELF。`Required` = 必須 internal（R15）；`Keep` = 留 internal（延遲／大小理由，PSRAM 搬遷非必要）；`Candidate` = 可搬 PSRAM（T12，受類別上限限制）。標「(in READER)」的列包含在 `READER` 18,972 B 內，不可相加。

| # | allocation | where | kind | bytes | class | placement | note |
|---|---|---|---|---|---|---|---|
| 1 | SPI DMA RX buffer | kernel/src/board_c61/spi.rs dma_rx_buffer!(4096); X4 board/mod.rs:213 | Static | 4096 | dma | Required | DMA; statics are internal by construction |
| 2 | SPI DMA TX buffer | kernel/src/board_c61/spi.rs dma_tx_buffer!(4096); X4 board/mod.rs:214 | Static | 4096 | dma | Required | DMA; STRIP (4,014 B) is copied through this, never DMA'd directly |
| 3 | SPI DMA descriptors | kernel/src/board_c61/spi.rs DESCRIPTORS | Static | 28 | dma | Required | below 4 KiB, listed for R15; nm-checked internal |
| 4 | C61 main heap | src/bin/c61_boot.rs heap_allocator!(INTERNAL_HEAP_MAIN_BYTES) | Static | 98304 | runtime | Required | global allocator, internal region only (inside .bss) |
| 5 | C61 reclaimed heap | src/bin/c61_boot.rs heap_allocator!(#[ram(reclaimed)] INTERNAL_HEAP_RECLAIMED_BYTES) | Static | 64000 | runtime | Required | global allocator, dram2_seg (.dram2_uninit), internal |
| 6 | X4 main heap | src/bin/main.rs heap_allocator!(110_592) | Static | 110592 | runtime | Required | X4 only |
| 7 | X4 reclaimed heap | src/bin/main.rs heap_allocator!(#[ram(reclaimed)] 64_000) | Static | 64000 | runtime | Required | X4 only; dram2 on both chips |
| 8 | embassy executor task arena | src/bin/main.rs __embassy_main POOL (X4 11,288; C61 boot 2,320) | Static | 11288 | runtime | Required | task futures; internal |
| 9 | main stack (.stack) | linker .stack = _stack_start - _stack_end (C61 T8 boot ELF: 132,768; whatever RAM the statics leave) | Stack | 132768 | runtime | Required | esp-rtos main task; size = RAM left after statics |
| 10 | dir_cache title scan buffer | kernel/src/kernel/dir_cache.rs:50 [0u8; 4096] | Stack | 4096 | runtime | Required | stack local |
| 11 | inflate read buffer | smol-epub async_io.rs:316 / cache.rs READ_BUF_SIZE | Stack | 4096 | runtime | Required | sync path: stack local; async path: inside the task future (counted in the executor POOL) |
| 12 | inflate strip buffer | smol-epub async_io.rs:317 / cache.rs STRIP_BUF_SIZE | Stack | 4096 | runtime | Required | same as the read buffer |
| 13 | StripBuffer STRIP | src/bin/main.rs STRIP (X4 nm: 4,014 B) | Static | 4014 | runtime | Keep | below 4 KiB, listed because it is the pixel source of every SPI DMA transfer; copied through the DMA TX buffer |
| 14 | READER (ReaderApp) | src/bin/main.rs READER | Static | 18972 | runtime | Keep | contains the 'in READER' rows below |
| 15 | page text buffer (in READER) | src/apps/reader/mod.rs PageState.buf PAGE_BUF | Static | 8192 | chapter-text | Keep | hot: touched per glyph while wrapping |
| 16 | DirCache | kernel/src/kernel/dir_cache.rs DIR_CACHE | Static | 10764 | zip-toc | Keep | 128 x DirEntry; UI list, low value to move |
| 17 | chapter cache Vec | src/apps/reader/epubs.rs ch_cache (CHAPTER_CACHE_MAX) | Heap | 98304 | chapter-text | Candidate | largest reader allocation; read-mostly |
| 18 | page prefetch Vec | src/apps/reader/paging.rs prefetch (PAGE_BUF) | Heap | 8192 | chapter-text | Candidate | sequential fill/read |
| 19 | chapter inflate window | smol-epub cache.rs WINDOW_SIZE | Heap | 32768 | chapter-text | Candidate | decompression scratch; hot inner loop, measure first |
| 20 | page offset table (in READER) | src/apps/reader/mod.rs PageState.offsets MAX_PAGES x u32 | Static | 2048 | page-table | Candidate | below 4 KiB; PSRAM lets MAX_PAGES grow (limit 64 KiB) |
| 21 | ZIP entry index (in READER) | smol-epub zip.rs ZipIndex.entries 256 x 20 | Static | 5120 | zip-toc | Candidate | estimate (entry size not measured); box it on PSRAM |
| 22 | ZIP name pool | smol-epub zip.rs names (try_reserve <= 8192) | Heap | 8192 | zip-toc | Candidate | already heap |
| 23 | EPUB TOC | src/apps/reader/mod.rs Box<EpubToc> 256 x 52 | Heap | 13316 | zip-toc | Candidate | already boxed on demand |
| 24 | central directory buffer | src/apps/reader/epubs.rs:76, files.rs:561 cd_buf | Heap | 32768 | zip-toc | Candidate | estimate; equals CD size, bounded by try_reserve and the zip-toc limit |
| 25 | decoded page image | src/apps/reader/images.rs DecodedImage.data (480x800 1bpp max) | Heap | 48000 | image-data | Candidate | also in work_queue Channel |
| 26 | PNG decode dictionary | smol-epub png.rs DICT_SIZE | Heap | 32768 | image-data | Candidate | scratch; hot loop, measure first |
| 27 | PNG zip inflate window | smol-epub png.rs ZIP_DEFLATE_WINDOW | Heap | 32768 | image-data | Candidate | scratch |
| 28 | JPEG header read | smol-epub jpeg.rs HEADER_READ | Heap | 32768 | image-data | Candidate | scratch |
| 29 | JPEG inflate window | smol-epub jpeg.rs DEFLATE_WINDOW | Heap | 32768 | image-data | Candidate | scratch |

PSRAM candidates per class (rows marked 'in READER' included; they are limits-relevant when moved):
- chapter-text: candidates 139264 B of PSRAM limit 786432 B
- image-data: candidates 179072 B of PSRAM limit 524288 B
- page-table: candidates 2048 B of PSRAM limit 65536 B
- zip-toc: candidates 59396 B of PSRAM limit 262144 B

說明：
- 「Required」列的 stack／heap／static 在 X4 與 C61 兩處各自存在，表不是加總用的記憶體預算，而是歸類。
- **Candidate 合計（章節 139,264 B／圖片 179,072 B／頁表 2,048 B／ZIP-TOC 59,396 B）都在各自 PSRAM 上限內**（測試 `r14_inventory_candidates_have_psram_capable_classes_and_fit_their_limits`、`r14_the_current_reader_allocations_fit_the_psram_plan_with_headroom`：把所有 Candidate 依類別同時在 `Ready` 預算上配置，必須成功且總池未滿）。
- 熱路徑提醒（T12 需實測而非假設）：`chapter inflate window`、`PNG dictionary`、inflate window 是逐 byte 隨機存取，PSRAM（Quad 40 MHz、經 cache）比 internal SRAM 慢得多；先放 PSRAM 再量測，若太慢改 `Keep`。
- **預估 C61 完整韌體靜態用量（估算，未連結）**：T8 boot ELF statics 123,856 B ＋ X4 的 app 靜態（`READER` 18,972＋`DIR_CACHE` 10,764＋`STRIP` 4,014＋`FILES` 1,284＋`HOME` 1,712＋executor arena 增量 8,968）≈ 45.7 KB ⇒ 約 169.6 KB，低於 207,472 B 預算（餘約 37.9 KB），之後 T9–T11 還會加東西。T12 整合時以 `scripts/report-c61-memory.sh` 實測取代此估算。

## 28. ELF 記憶體證據（C61 T8 boot ELF）與 stack headroom

### 28a. 如何計算

- 連結器把 `RAM`（0x40800000..0x4083EA70）依序放 `.trap`、`.rwtext`、`.data`、`.bss`（含 main heap static）、`.noinit`，**`.stack` 是「靜態結束處到 `RAM` 結尾」的剩餘空間**：`_stack_end` = 靜態結尾、`_stack_start` = `ORIGIN(RAM)+LENGTH(RAM)` = 0x4083EA70（stack 向下長）。所以 `.stack` 大小不是設定值，是 `RAM_LEN − statics`；headroom 定義為 `.stack − STACK_MIN_BYTES`（= statics 預算還能長多少）。
- 兩個 heap：main heap = `.bss` 內的 static `HEAP`（起 0x408036f0、長 98,304）；reclaimed heap = `.dram2_uninit`（起 0x4083ea70、長 64,000，在 `RAM` 之外、`dram2_seg` 之內）。
- `size`（Berkeley）在這個 target 把 `.stack`（flags `A`，無 `W`）算進 **text** 欄，所以 T5–T7 記錄的 C61 `text` 數字（281600→285282）含 `.stack`（T7：168,848 B），**不是程式碼大小**；真正的 flash image 大小用 `.text .rodata .rwtext .trap .data .appdesc` 加總（腳本輸出）。X4 的 `.stack` 是 `WA`，算進 bss（這就是 §28d 的來源）。T8 起 C61 以腳本輸出的 section 表為準。

### 28b. `scripts/report-c61-memory.sh` 實際輸出（節錄；完整輸出見指令 #5）

```
ok   image: statics 123856 B (48.2% of 256624 B main RAM), stack 132768 B, headroom over 49152 B minimum = 83616 B
HP SRAM usable (memory.x RAM + dram2_seg)               322160 B
  RAM (linker main region)                              256624 B
    statics (.trap .rwtext .data .bss .noinit)          123856 B  (48.2% of RAM)
    .stack (= RAM - statics)                            132768 B
    STACK_MIN_BYTES                                      49152 B
    stack headroom over STACK_MIN_BYTES                  83616 B
    statics budget (STATIC_RAM_MAX_BYTES)               207472 B
    statics budget left                                  83616 B
  dram2_seg used by .dram2_uninit (heap)                 64000 B  (97% of dram2_seg)
image bytes (.text .rodata .rwtext .trap .data .appdesc)    136476 B
```

| section | 位址 | 大小 B |
|---|---|---|
| `.trap` | 0x40800000 | 2,656 |
| `.rwtext`（RAM 內程式，含 esp-hal 的 `#[ram]` PSRAM init） | 0x40800a60 | 7,608 |
| `.data` | 0x40802818 | 3,800 |
| `.bss`（含 main heap 98,304） | 0x408036f0 | 109,792 |
| `.noinit` | 0x4081e3d0 | 0 |
| `.stack` | 0x4081e3d0..0x4083ea70 | 132,768 |
| `.dram2_uninit`（reclaimed heap） | 0x4083ea70 | 64,000 |
| `.flash.appdesc` / `.rodata` / `.text`（flash 視窗） | 0x42000020 / 0x42000120 / 0x420076fc | 256 / 30,136 / 92,020 |

LOAD segments（`readelf -l`）：RAM R E 0x2818；RAM RW FileSiz 0xed8 MemSiz 0x1bbb8（`.data`＋`.bss`）；flash R 0x76dc；flash R E 0x16774；`.stack` 0x206a0（R，NOBITS）；`.dram2_uninit` 0xfa00（RW，NOBITS）。

與 T7 ELF 的 section 差異：`.rwtext` +2,868（esp-hal PSRAM init 是 `#[ram]`）、`.data` +312、`.bss` +32,904（main heap 64→96 KiB 的 +32,768，其餘 +136 為 `BUDGET`／`PSRAM_HEAP`／esp-hal psram 範圍）、`.rodata` +3,456、`.text` +9,954（esp-hal quad PSRAM 驅動、`selftest`、budget、log 字串）、`.stack` −36,080（statics 變大 → 剩餘變小）、新增 `.dram2_uninit` 64,000。`size`：text 265480 / data 3800 / bss 173792（T7：285282 / 3488 / 76888；text 下降是因為 `.stack` 縮小，見 28a）。

### 28c. nm 證明 R15（DMA 資料在 internal range）

```
ok   dma     pulp_kernel::board_c61::spi::init::BUFFER      0x4081c16c +0x1000 internal RAM   (RX/TX 各一)
ok   dma     pulp_kernel::board_c61::spi::init::BUFFER      0x4081d16c +0x1000 internal RAM
ok   dma     pulp_kernel::board_c61::spi::init::DESCRIPTORS 0x4081e1ec +0x001c internal RAM
ok   dma     pulp_kernel::board_c61::spi::init::DESCRIPTORS 0x4081e208 +0x001c internal RAM
```

internal range = `C61_RAM_START 0x40800000 .. C61_RAM_END 0x4084EA70`（與 esp-hal `ld/esp32c61/memory.x` 的 `RAM`／`dram2_seg` 一致；腳本會逐項對照 `RAM ORIGIN`／`LENGTH`／`dram2 end`／`DROM 0x42000000..0x46000000`，皆 ok）。另外：所有可寫 section 都在 internal range、沒有任何 data／bss 符號落在 flash／PSRAM 視窗（0x42000000..0x46000000）、`_stack_end/_stack_start` 與 `.stack` section 吻合。負向驗證：把 DMA 位址換成 0x42100000、把 `.bss` 放進 PSRAM 視窗、statics 多 1 byte，`memreport check` 都 exit 1（見指令 #7）；用 T7 的舊 ELF 跑腳本（heap 與預算不符）exit 1。

### 28d. 「離線 X4 的 bss 比含 wifi 版多約 39 KB」的來源（已查明）

指令：`scripts/report-c61-memory.sh --x4-diff <離線 X4 ELF> <wifi X4 ELF>`（T3 兩個 ELF）。

| section | 離線 | wifi | wifi−離線 |
|---|---|---|---|
| `.trap` | 1,936 | 1,936 | 0 |
| `.rwtext` | 6,268 | 6,600 | +332 |
| `.rwdata_dummy` | 8,204 | 42,064 | +33,860 |
| `.data` | 27,608 | 32,584 | +4,976 |
| `.bss` | 146,560 | 167,216 | +20,656 |
| `.stack` | 138,136 | 78,152 | **−59,984** |
| `.dram2_uninit` | 64,000 | 64,000 | 0 |
| 上列 RAM section 合計（不含 dram2） | 328,712 | 328,552 | −160（對齊） |

`size` 的 bss 欄 = `.bss`＋`.stack`＋`.dram2_uninit`＋`.rtc_fast.persistent`＋`.noinit`：離線 146,560＋138,136＋64,000＋132 = **348,828**；wifi 167,216＋78,152＋64,000＋132 = **309,500**。差 +39,328 = `.bss` −20,656 與 `.stack` +59,984 的和。**結論：不是離線版多配置了靜態記憶體；離線版靜態反而少約 60 KB（wifi 版多了 `.rwdata_dummy` +33.9 KB、`.bss` +20.7 KB、`.data` +5.0 KB、executor arena 11,256→18,328 B 與 radio 靜態如 `g_cnxMgr` 3,976 B 等），而主 RAM 區總量固定（兩者合計都 ≈ 328.6 KB），`.stack` 吃掉剩餘，所以 `size` 的 bss 欄變大。** 這也是為什麼 C61 的「stack headroom」要以 `RAM_LEN − statics` 計（28a）。`.rwdata_dummy` 在 wifi 版為何增加到 42,064 B 未再追（esp-hal c3 linker 的保留區，與離線／C61 無關）。

## 29. T8 實際指令與結果

| # | 指令 | 結果 |
|---|---|---|
| 1 | `cargo test-board-logic` | **174 passed; 0 failed**（T7 的 125 項 + 新增 49 項，全在 `memory::tests`） |
| 2 | `CARGO_TARGET_DIR=target/t8-c61 cargo build-c61 --locked` | **成功**，0 warning；`size`：text 265480 / data 3800 / bss 173792（text 含 `.stack`，見 28a；section 明細 28b）；`strings` 見 `memory: over-limit request refused as expected`、`psram: budget refused status`、`psram freqdiv: `；`nm` 見 `pulp_kernel::board_c61::memory::{PSRAM_HEAP,BUDGET}`、`esp_hal::psram::MAPPED_PSRAM_{START,END}`；`pulp_os_c61_boot` 實際呼叫 `memory::init`／`alloc_external`／`alloc`／`alloc_dma`（只證明編譯／連結） |
| 3 | `CARGO_TARGET_DIR=target/t8-x4 cargo build-x4 --locked` | **成功**，0 warning；`size`：text 1221824 / data 27624 / bss 348812，**與 T7 完全相同**（差異 0）。進一步以 `objcopy -O binary --only-section=<s>` 比較 `.trap` `.rwtext` `.data` `.rodata` `.text`：**位元組完全相同**；僅 `.flash.appdesc` 有 3 byte 不同（app descriptor 的建置時間欄）。原因：X4 不編入 `board_c61`，`board-logic::memory` 在 X4 無引用 |
| 4 | `bash scripts/check-x4-input-trace.sh`／`bash scripts/check-x4-driver-trace.sh` | `ok X4 input trace identical to pre-T7 (26032 lines, sha256 2c3738aa...)`／`ok X4 driver trace identical to pre-T6 (2122 lines, sha256 66a0447f...)` |
| 5 | `bash scripts/report-c61-memory.sh`（新） | exit 0，`RESULT: all memory budget checks ok`；輸出含 `size -A`、`readelf -S/-l`、預算常數、`image`／`section`／`dma` 檢查、memory.x 對照、internal RAM 摘要、PSRAM 計畫、最大靜態物件（top 12：兩個 heap、兩個 4 KiB DMA `BUFFER`、executor `POOL` 2,320 B …） |
| 6 | `scripts/report-c61-memory.sh --x4-diff <T3 離線> <T3 wifi>` | 28d 的表 |
| 7 | `memreport check` 負向（stdin 事實） | DMA 位址 0x42100000 → `FAIL dma ... not inside internal RAM (ExternalMapped)` exit 1；`.bss` 放在 0x42100000 → `FAIL section ... writable data outside internal RAM` exit 1；statics 207,473 B → `FAIL image: StaticsOverBudget { used: 207473, max: 207472 }` exit 1；statics 恰 207,472 B → ok exit 0（stack 剛好 49,152、headroom 0）。`C61_ELF=<T7 ELF> scripts/report-c61-memory.sh` → 2 項 FAIL（沒有 98,304 B heap、`.dram2_uninit` = 0）exit 1 |
| 8 | `bash scripts/check-board-selection.sh` | 4 項 `ok` |
| 9 | `OFFLINE_CHECK_TARGET_ROOT=target/t8-check bash scripts/check-offline-boundary.sh` | exit 0，20 項 `ok` |
| 10 | `X4_ELF=target/t8-x4/riscv32imc-unknown-none-elf/release/pulp-os bash scripts/check-c61-no-c3-raw-gpio.sh` | exit 0（預設檢查 `target/t5-c61` 的舊 ELF；另以 `C61_TARGET_DIR=target/t8-c61` 對 T8 ELF 重跑，全部 `ok`，1 項 info 同 T5） |
| 11 | `cargo fmt --all -- --check` | 無差異 |
| 12 | `grep -n 'unwrap\|expect(\|panic!' kernel/src/board_c61/memory.rs` | 無輸出（`board-logic/src/memory.rs` 非測試碼亦無） |

Cargo.lock 未變動（`--locked` 通過；`board-logic` 仍零依賴，`memreport` 是 example 不影響依賴）。

### 新增 host 測試（49 項，節錄代表）

- 預算（R14）：`r14_exactly_filling_a_class_limit_succeeds`（剛好用滿）、`r14_one_byte_over_the_class_limit_is_refused`（+1 byte 被拒，且拒絕不改帳）、`r14_fill_then_one_more_granule_is_refused_then_release_allows_again`（釋放後可重配）、`r14_each_class_has_its_own_limit`（章節／圖片／頁表各自上限）、`r14_pool_exhaustion_is_reported_when_the_class_still_has_room`、`r14_all_classes_at_their_limits_fit_the_pool_exactly_below_reserve`、`r14_zero_size_is_rejected_in_every_state`、`r14_bad_alignment_is_rejected`（0／3／24）、`r14_usize_overflow_is_an_error_not_a_wrap`（`usize::MAX`、`MAX-3`、2^63 對齊）、`r14_charge_rounds_up_to_the_granule_or_the_alignment`、`r14_alignment_is_charged_so_small_blocks_cannot_dodge_the_budget`、`r14_release_underflow_is_refused_and_changes_nothing`（含雙重釋放、錯 region）。
- 失敗處理／降級（R14, R15）：`r14_evaluate_psram_state_machine`（無晶片／< 1 MiB／恰 1 MiB／2 MiB／8 MiB 夾到 2 MiB／冒煙失敗／大小問題優先）、`r14_fresh_budget_is_not_initialised_and_places_internally`、`r14_degraded_budget_places_psram_classes_internally_with_internal_limits`（降級後改用 internal 上限，200 KiB 章節在 PSRAM 模式成功、降級被拒）、`r14_degraded_internal_pool_caps_the_sum_of_classes`、`r14_status_change_with_live_reservations_is_refused`、`r14_internal_only_reservations_do_not_block_a_status_change`、`r14_selftest_failure_degrades_the_budget`、`r14_bad_window_degrades_with_a_readable_reason`、`r14_window_fault_*`（區間邊界、錯位、wrap）、`r14_selftest_*`（健康記憶體、stuck bit、死位址線、太小、走訪索引不重複）。
- 分類規則（R15）：`r15_dma_isr_runtime_never_allow_psram`、`r15_explicit_psram_request_for_internal_only_class_is_refused`（PSRAM `Ready` 且空間充足時仍拒絕）、`r15_auto_placement_keeps_internal_only_classes_internal_even_when_ready`、`r15_psram_limit_of_internal_only_classes_is_zero`、`r15_addr_space_boundaries`、`r15_range_is_internal_edges`（含 wrap、空區間）、`r15_psram_and_flash_window_is_never_internal`、`r15_memory_map_matches_memory_x`、`r15_internal_plan_fits_ram_with_stack`、`r15_writable_sections_must_be_internal_read_only_may_be_flash_mapped`、`r15_inventory_*`（Required 列不得為 PSRAM-capable 類別、DMA 列必為 Required＋Static、SPI DMA 列與 `SPI_DMA_BUF_BYTES` 同值）。
- ELF 預算（R22）：`r22_check_image_reports_headroom_over_the_stack_minimum`、`r22_check_image_statics_exactly_at_the_budget_pass_one_byte_more_fails`、`r22_check_image_rejects_inconsistent_layouts`。

### 變異測試（暫時修改 `memory.rs`、跑 `cargo test-board-logic`、還原；還原後 174 passed）

| # | 變異 | 結果 |
|---|---|---|
| M1 | `allows_psram` 加入 `DmaBuffer` | 5 項 FAILED（`r15_dma_isr_runtime_never_allow_psram` 等） |
| M2 | `reserve_in` 移除 `RegionForbidden` 檢查 | FAILED（`r15_explicit_psram_request_for_internal_only_class_is_refused`） |
| M3 | 類別上限 `<=` → `<` | 6 項 FAILED |
| M4 | 池上限 `<=` → `<` | 2 項 FAILED |
| M5 | `charge` 不做對齊／granule 向上取整 | 4 項 FAILED |
| M6 | PSRAM 池不扣保留量 | 2 項 FAILED |
| M7 | `region_for` 不看 PSRAM 狀態 | 6 項 FAILED |
| M8 | `release` 不檢查 underflow | FAILED |
| M9 | `range_is_internal` 結尾 `<=` → `<` | FAILED |
| M10 | `range_is_internal` 的 `checked_add` → `wrapping_add` | FAILED |
| M11 | `set_status` 不擋存活配置 | FAILED |
| M12 | `evaluate_psram` 忽略冒煙測試結果 | 2 項 FAILED |
| M13 | `window_fault` 全部接受 | FAILED |
| M14 | `check_image` 不檢查 statics 預算 | FAILED |
| M15 | `selftest` 略過反相 pass | 3 項 FAILED |
| M16 | `section_placement_ok` 允許可寫 section 在 PSRAM 視窗 | FAILED |
| M17 | internal 章節上限 96 → 256 KiB | 2 項 FAILED |
| M18 | `window_fault` 不檢查 4-byte 對齊 | FAILED |

18 個變異全部被擊殺，無存活。

## 30. T8 尚未驗證項目

- **硬體全未驗**：PSRAM 偵測與初始化（含 `Psram::new` 在這塊 2 MB 零件上實際回傳的 size，以及 chip id／density 判斷是否適用）、40 MHz PSRAM／40 MHz flash 同時工作的穩定度（esp-hal 預設 flash 80 MHz 已被覆寫為 40，但 `flash_tuning`／`ram_tuning` 的預設值 `din_mode 3, din_num 1, extra_dummy 2` 在 40 MHz 下是否合適未驗）、`selftest` 的 ROM cache 呼叫（`Cache_WriteBack_Addr`／`Cache_Invalidate_Addr`）在 C61 上的行為、PSRAM 實際讀寫速度（影響 T12 熱路徑歸類）、實際 heap 使用量與碎片（`reserve` 計的是請求量，allocator 內部 overhead 以保留 448 KiB 吸收，未實測）、stack 使用高水位（headroom 只是靜態空間，不是實測用量）、cache／DMA 一致性（本專案政策是 DMA 不碰 PSRAM，所以沒有對 PSRAM buffer 做 DMA 的路徑，但 CPU 經 cache 存取 PSRAM 與 flash 共用 MSPI 的互相影響未驗）、dram2 reclaimed heap 在 bootloader 之後是否真的可用。
- 設計取捨需使用者／T12 確認：PSRAM 類別上限是起始值；降級模式採用 X4 規模的 internal 上限；`alloc*` 目前零初始化（768 KiB 清零在 40 MHz Quad PSRAM 上可能要數十 ms，T12 若在熱路徑配置需改 uninit 版本）。
- 範圍外（T12）：reader／image pipeline 尚未使用 `memory::alloc*`（T8 只提供機制、預算常數、盤點表與 `c61_boot` 的入口驗證）；`MemoryBudget` 只管 API 配置，static 由 ELF 檢查把關，兩者的總和與完整韌體的 statics（約 169.6 KB 估算）T12 以腳本實測。
- `report-c61-memory.sh` 需要 GNU awk（`strtonum`）、binutils（`size`／`readelf`／`nm`）、host rustc 與 cargo registry 快取（memory.x 對照在 registry 缺檔時降為 warn）；不在 CI。


---

# T9：電池取樣／充電控制與 USB detect（R16, R17, R23）

日期：2026-10-04。**沒有任何硬體驗收：分壓比、ADC 校正、充電暫停後的沉澱時間、USB 實際極性、電池放電曲線全部未驗。** `c61_boot` 只證明共用 ADC、keys、battery、USB 的程式碼能編譯、連結並被呼叫，不宣稱可用。

## 31. 設計

### 31a. 分層（沿用 host seam）

| 位置 | 內容 |
|---|---|
| `board-logic/src/battery.rs`（新，host 測試） | 常數 `BATTERY_ADC_CHANNEL=3`／`CHARGE_SETTLE_MS=30`／`SAMPLE_COUNT=16`／`DIVIDER_MULT=2`；`LIPO_DISCHARGE_CURVE`（X4 表原樣）與 `percentage_from_curve`（自 X4 `drivers::battery::battery_percentage` 逐行搬出）；`cell_mv_from_adc_mv`（x2，**飽和**而非 wrap）；trait `ChargePin`（`set_charging(bool)`）；內部 `ChargePause` drop guard；`BatteryMonitor<C,A,D>`（擁有充電腳、ADC、延時；`new` 先驅動 enabled；`measure(&mut self)`）；`BatteryError::AdcFailed{sample}`、`BatteryReading{adc_mv,cell_mv}.percent()`。ADC trait 重用 `keys::AdcSample`（回 `Option<u16>`，`None`=失敗，不是 0 mV），延時重用 `power::DelayMs` |
| `board-logic/src/usb.rs`（新，host 測試） | `UsbPolarity::{ActiveHigh,ActiveLow}`＋`plugged(level)`；**board 設定值 `USB_POLARITY`**（來源表在檔頭註解）；`UsbDetect`（首次取樣定義初始狀態、無事件；N 次連續不同才確認；只在確認狀態改變時回 `UsbEvent::{Plugged,Unplugged}`）；`USB_DEBOUNCE_SAMPLES=3` |
| `kernel/src/board_c61/adc.rs`（新） | **單一 ADC1**：`init(ADC1, GPIO4, GPIO5)` 先檢查兩腳的 esp-hal channel（2／3），在**同一個 `AdcConfig`** 以 `_11dB`＋`AdcCalCurve` 啟用兩腳後才 `Adc::new`，放進 `StaticCell<critical_section::Mutex<RefCell<Adc>>>`（重複 init 回 `AlreadyInitialised`，不 panic）；`SharedAdc`（Copy handle）的 `read_mv` 在**一個 critical section 內**完成整次轉換（含 20000 次自旋上限），所以 keys 與 battery 即使在不同 task 也不會交錯使用轉換器；失敗回 `None` |
| `kernel/src/board_c61/keys.rs`（改） | ADC 建立部分移到 `adc.rs`；`FrontLadder` 改持 `SharedAdc`＋`FrontPin`；`keys::new(adc, front, wake, prev, next) -> C61Input`（不再回 `Result`；原 `KeysError::AdcChannelMismatch`／`adc_channels` 隨檢查移到 `adc::init`／`adc::channels`）。解碼、grace、debounce 全在 `board-logic`，**沒有改**，T7 的 48 項測試未動且全過 |
| `kernel/src/board_c61/battery.rs`（新） | `Gpio10Charge`（`Output`，**建立時 `Level::High`**，無開機暫停 glitch，BSP `:268-270`）、`BatteryAdc`（`SharedAdc`＋GPIO5 pin）、`C61Battery = BatteryMonitor<…, HalDelay>`、`new(GPIO10, SharedAdc, BatteryPin)`。GPIO10 只存在於 monitor 內，沒有其他寫入路徑 |
| `kernel/src/board_c61/usb.rs`（新） | `UsbPort`：GPIO11 input＋內部上拉（BSP `:274-279`），`UsbDetect::for_board(首次電平)`；`poll()`／`plugged()`／`level_high()`（診斷用，bring-up 判斷極性） |
| `src/bin/c61_boot.rs` | `adc::init` → `keys::new`＋`battery::new`；開機量一次電池、之後每 3000 tick（30 s，同 X4）量一次；USB 每 2 tick（20 ms）poll，事件 log。量測失敗以 `error!` 記錄，不當 0 V |
| X4 | `kernel/src/board/battery.rs` 的 `DISCHARGE_CURVE` 改為 re-export `pulp_board_logic::battery::LIPO_DISCHARGE_CURVE`；`drivers/battery.rs::battery_percentage` 改呼叫 `percentage_from_curve`；`adc_to_battery_mv` 與 `DIVIDER_MULT` 原封不動（X4 取樣仍是 `input.rs` 的 `read_battery_mv`，無充電控制） |

不做：`board_charge_enable()`（BSP `:308`，任意開關充電）沒有 consumer 且無策略，不移植；deep sleep 時 charge／USB 處理（T11）；電量／USB UI（T12）。

### 31b. 充電暫停／恢復契約（R16）：BSP 依據

`board_c61.c:289-304` `board_battery_mv()`：

| 步驟 | BSP | 本實作 |
|---|---|---|
| 1 暫停 | `gpio_set_level(PIN_CHG_EN, 0)`（`:291`，`:60` 註解「pull low to read true battery V」） | `ChargePause::new` → `set_charging(false)` |
| 2 沉澱 | `vTaskDelay(pdMS_TO_TICKS(30))`（`:292`） | `delay_ms(CHARGE_SETTLE_MS=30)` |
| 3 取樣 | 16 次 `adc_oneshot_read`，平均（`:295-297`） | 16 次 `AdcSample::sample_mv`，截斷平均 |
| 4 恢復 | `gpio_set_level(PIN_CHG_EN, 1)`（`:299`） | `ChargePause::drop` → `set_charging(true)` |
| 5 換算 | `mv * 2`（`:303`，註解 1:1 divider） | `cell_mv_from_adc_mv`（x2，飽和） |
| 初始 | `power_init` 把 GPIO10 設輸出並拉高「allow charging by default」（`:268-270`） | `GPIO10` 以 `Level::High` 建立＋`BatteryMonitor::new` 再驅動 enabled |

其他參考：`../crosspoint-onepage/lib/hal/HalPowerManager.cpp:153-158` 同樣 low→`delay(5)`→讀→high；沉澱時間與 BSP 不同（5 ms vs 30 ms），**採 BSP 的 30 ms**，5 ms 是否足夠不在 R16 範圍。

與 BSP 的**有意差異**（BSP 行為有缺陷或與 Rust 型別模型不同）：

1. BSP 的失敗讀值被略過但仍 `/16`（失敗當 0，低估）；`s_cali_ok` 為 false 時整體回 `0 mV`。本實作任一次 ADC 讀取失敗即回 `Err(AdcFailed{sample})`（**不是 0 V**），並停止後續讀取（不浪費時間）；恢復充電仍會執行。
2. 恢復由 drop guard 保證（含 sampler panic 的 unwind）；BSP 是線性程式，無失敗路徑。
3. 平均的是校準後 mV（esp-hal `read_oneshot` 回 mV），BSP 是平均 raw 再校準；兩者在線性區差異小，**未驗**。
4. 沒有 `board_charge_enable()`：恢復一律回 enabled（BSP 預設）。

不變條件的型別保證：`BatteryMonitor` 擁有 `ChargePin`，`measure(&mut self)` 以獨佔借用把腳只借給 `ChargePause`；`ChargePause` 存活期間沒有別的路徑能拿到該腳，也不可能有第二個重入的 `measure`。

### 31c. ADC 共用（承 T7 結論）

- GPIO4 = ADC1_CH2、GPIO5 = ADC1_CH3：`esp-metadata-generated 0.5.3 _generated_esp32c61.rs:4141-4142`（`_for_each_inner_analog_function!((ADC1_CH2, GPIO4))`／`((ADC1_CH3, GPIO5))`），與 BSP `board_c61.c:63`、README `:49-50` 一致；`board_c61.c:315` 註解把前面板寫成 GPIO5/CH2 是筆誤（`:322` 實際讀 `ADC_CHANNEL_2`）。`adc::init` 於執行期對兩腳檢查，表一變就回 `AdcError::ChannelMismatch`，不讀錯節點。
- BSP 同樣只建一個 ADC1 oneshot handle，由 battery 的 `power_init` 建立、keys 借用（`board_c61.c:310-313`、`board_keys.c:119-121`）；本實作等價：board 層建立、keys 與 battery 各拿一個 `SharedAdc`。
- 兩腳都用 11 dB（BSP `ADC_ATTEN_DB_12`，`:259`、`:264`）＋curve-fitting 校正。

### 31d. USB polarity（R17）：矛盾與來源表

| 來源 | 位置 | 說法 |
|---|---|---|
| BSP 實作 | `board_c61.c:306` `gpio_get_level(PIN_USB_DET) == 0` | LOW = plugged |
| BSP 註解 | `board_c61.c:61`、`:272-273` | LM66200 ST open-drain、外部上拉：low = USB present；需要上拉才讓「未插」讀 high |
| BSP README | `README.md:52` `USB_DET \| 11 \| USB insertion detection (high = plugged)` | **HIGH = plugged（與實作矛盾）** |
| 原理圖 | `../onepage-reader/electronics/c61/SCH_Sch_OnePage_V1_2026-08-19.pdf`（PSW 區塊，本次以 `pdftoppm` 目視） | U12 `LM66200DRLR` pin 8 `ST` → `USB_DET`；`R29 10kΩ` 把 `USB_DET` 上拉到 `+3V3`；U12 旁標註 `USB=0` |
| 另一份韌體 | `../crosspoint-onepage/lib/hal/HalGPIO.cpp:245-249`、`:368-370`；`HalGPIO.h:18` | LOW = USB present；兩處註解皆稱「Verified on hardware」（BATDIAG 在插 USB 時讀到 LOW） |

**預設值選 `ActiveLow`**：BSP 實作＋BSP 註解＋原理圖（open-drain ST＋上拉，`USB=0`）＋另一份韌體的實機註解四者一致；只有 README 一句相反，且不符合電路（ST 是 open-drain、外部上拉，無法在「插入」時主動拉高）。`USB_POLARITY` 是單一常數（`board-logic/src/usb.rs`），解碼路徑只讀它；測試用兩種 polarity 都跑，並以「同一電平序列在兩種設定下得到相反事件」證明設定確實生效。**待實機確認**：第三方 log 的「Verified on hardware」我們無法獨立驗證；bring-up 時看 `c61_boot` 的 `usb: GPIO11 reads high/low -> plugged/unplugged` log 與實際插拔對照，不符就只改 `USB_POLARITY`。

去抖：BSP 沒有（隨需讀電平，crosspoint 也每次 `update()` 直接比較）。`USB_DEBOUNCE_SAMPLES=3`（20 ms 週期 → 60 ms）是**本專案新增**，防止插頭接觸時抖動產生連串事件；設 1（或 0）即 BSP 等價的立即模式。這個值沒有硬體依據。

初始狀態：`UsbDetect::new` 要求傳入「現在讀到的電平」，初始狀態由它決定且不發事件；沒有「未知」狀態或未初始化假設。

### 31e. 電壓→百分比與分壓比的依據

- 分壓比 **1:1**：BSP `board_c61.c:303`（`mv * 2`）、README `:158`；原理圖 CHARGE 區塊 `R7 5.1MΩ`（VBAT→`BAT_ADC`）、`R12 5.1MΩ`（`BAT_ADC`→GND）、`C22 100nF` 與 R12 並聯，三者一致。X4 為 100K/100K（也 x2），所以 `DIVIDER_MULT` 數值相同但為各自板子的常數。
- **BSP 沒有電壓→百分比曲線**，crosspoint 的 `BatteryMonitor` 原始碼本機沒有（只有 `simulator/shims/BatteryMonitor.h` 空殼），所以 C61 **沿用 X4 表**（`LIPO_DISCHARGE_CURVE`，12 個點、3000→0 %、4200→100 %），**列為未驗**（C61 的電芯規格未見於 BSP／README／schematic 本機檔）。
- 注意：分壓電阻合計 10.2 MΩ，ADC 腳的來源阻抗約 2.55 MΩ，並聯 100 nF；ESP32 ADC 對這種阻抗的取樣誤差與 30 ms 沉澱是否足夠**未驗**（BSP 的 30 ms 是否經實測不明）。

## 32. T9 實際指令與結果

| # | 指令 | 結果 |
|---|---|---|
| 1 | `cargo test-board-logic` | **210 passed; 0 failed**（T8 的 174 項 + 36 項：`battery` 24、`usb` 12；T4–T8 測試未動） |
| 2 | `CARGO_TARGET_DIR=target/t9-c61 cargo build-c61 --locked` | **成功**，0 warning；`size`：text 268400 / data 3816 / bss 173848（T8：265480 / 3800 / 173792；text 欄含 `.stack`，見 §28）。`nm -C`：`pulp_os_c61_boot::report_battery`（`BatteryMonitor::measure` 內嵌其中）、`pulp_kernel::board_c61::adc::ADC1_CELL`（單一共用 ADC 的 static）、`AdcCalCurve<ADC1>::new_cal_with_channel`、`BatteryError`／`UsbEvent`／`UsbPolarity` 的 `Debug::fmt`；`strings`：`adc: GPIO4 -> ADC1_CH`、`battery: GPIO5 + charge control GPIO10 ready (charging enabled)`、`battery: pin `、`battery: sample failed: `、`usb: GPIO11 reads `。`c61_boot` 實際呼叫 `adc::init`／`keys::new`／`battery::new`／`UsbPort::new`，並於迴圈 `poll()`／`measure()`（只證明編譯／連結） |
| 3 | `C61_TARGET_DIR=target/t9-c61 bash scripts/report-c61-memory.sh` | exit 0，`RESULT: all memory budget checks ok`；`image: statics 123928 B (48.2% of 256624 B), stack 132696 B, headroom over 49152 B minimum = 83544 B`（T8：123856／132768／83616；statics +72 B）；DMA `BUFFER`／`DESCRIPTORS` 仍在 internal。（注意：此腳本預設讀 `target/t8-c61`，必須帶 `C61_TARGET_DIR`，否則會檢查舊 ELF） |
| 4 | `CARGO_TARGET_DIR=target/t9-x4 cargo build-x4 --locked` | **成功**，0 warning；`size`：text **1221824** / data **27624** / bss **348812**，與 T8 **完全相同**（差 0／0／0）。`objcopy --only-section` 比較 T8 與 T9：`.trap` `.rwtext` `.data` `.rodata` 位元組相同；`.text` 同為大小但有 358 個位元組不同，**全部落在 `<Kernel>::log_stats` 一個函式內**（`0x4209ae2c`，692 B；`scheduler.rs:560` 是 `battery::battery_percentage` 的唯一呼叫者，被內嵌進去），差異即該百分比函式改走共用的泛型 `percentage_from_curve` 後的指令排列。行為等價見第 5 項 |
| 5 | `bash scripts/check-x4-battery-equiv.sh`（新） | `ok   X4 battery conversions identical to pre-T9 (65536 inputs, sha256 272e9905…)`。把 **git HEAD 的舊 `drivers/battery.rs`＋`board/battery.rs`** 與**工作樹的真檔案**各自在 host 編成 harness，對 `mv = 0..=65535` 輸出 `adc_to_battery_mv` 與 `battery_percentage`，兩份輸出同一個 sha256（腳本內 pin 為舊檔輸出）。負向：把新 `drivers/battery.rs` 暫改為 `battery_mv as u32 + 1` → `FAIL X4 battery conversions changed`（差異首行 `3079 6158 0` vs `1`），exit 1，還原後 ok。另外 `board-logic` 內 `x4_curve_table_is_unchanged`、`x4_percentage_matches_the_pre_move_algorithm_for_every_u16`（0..=65535 對舊演算法全掃） |
| 6 | `bash scripts/check-x4-input-trace.sh`／`bash scripts/check-x4-driver-trace.sh` | `ok X4 input trace identical to pre-T7 (26032 lines, sha256 2c3738aa…)`／`ok X4 driver trace identical to pre-T6 (2122 lines, sha256 66a0447f…)` |
| 7 | `bash scripts/check-board-selection.sh` | 4 項 `ok` |
| 8 | `OFFLINE_CHECK_TARGET_ROOT=target/t9-check bash scripts/check-offline-boundary.sh` | exit 0，20 項 `ok` |
| 9 | `X4_ELF=target/t9-x4/riscv32imc-unknown-none-elf/release/pulp-os C61_TARGET_DIR=target/t9-c61 bash scripts/check-c61-no-c3-raw-gpio.sh` | exit 0，7 項 `ok`＋1 項 `info`（同 T5：esp32c61 PAC TIMG1） |
| 10 | `cargo fmt --all -- --check` | 無差異（第一次檢查有差異，已 `cargo fmt --all` 修正後重建並重跑全部驗證） |
| 11 | `grep -n 'unwrap\|expect(\|panic!' kernel/src/board_c61/{adc,battery,usb,keys}.rs src/bin/c61_boot.rs` | 無輸出（`board-logic` 非測試碼亦無；`panic!` 只出現在測試的 `FakeAdc`） |

Cargo.lock 未被 T9 變動（`--locked` 通過；`board-logic` 仍零依賴）。

### 新增 host 測試（36 項，代表）

- R16 序列（`battery`）：`r16_sequence_is_enable_pause_settle_16_reads_resume`（精確事件序：`Charge(false), Delay(30), Sample(0..15), Charge(true)`）、`r16_constructor_drives_charging_enabled_before_anything_else`、`r16_contract_constants_are_the_bsp_values`、`r16_no_read_happens_before_the_settle_delay_or_while_charging`、`r16_charge_pin_is_not_written_between_pause_and_resume`。
- R16 失敗／重入：`r16_adc_failure_at_every_index_stops_reading_and_still_resumes`（失敗 index 0..15 各一次，事件序精確且之後無讀取）、`r16_adc_failure_is_an_error_not_zero_mv`、`r16_zero_mv_reads_are_a_valid_reading_not_an_error`、`r16_repeated_failures_leave_charging_enabled_and_monitor_reusable`（連續 5 次失敗後第 6 次成功，pause／resume 嚴格交替）、`r16_charging_is_resumed_even_if_the_sampler_panics`（`catch_unwind`，證明 RAII）、`r16_repeated_calls_are_independent_complete_sequences`、`r16_pause_is_never_nested_across_a_failure_then_success_mix`。
- R16 換算／百分比：`r16_cell_voltage_is_twice_the_truncated_mean`（1000.9375 → 1000 → 2000）、`r16_mean_uses_all_16_reads`、`r16_sum_does_not_overflow_for_max_reads`、`r16_cell_mv_saturates_at_the_u16_edge`（32767→65534、32768→65535）、`r16_percent_clamps_at_both_ends`（0、2999、3000、4200、4201、65535）、`r16_percent_hits_every_knot_exactly`、`r16_percent_between_knots_floors`（3439→5、3440→6、4130→95、3079→0、3080→1）、`r16_percent_is_monotonic_and_bounded_over_the_whole_range`（0..=65535）、`r16_reading_percent_uses_the_cell_voltage_not_the_pin_voltage`、`x4_curve_table_is_unchanged`、`x4_percentage_matches_the_pre_move_algorithm_for_every_u16`。
- R17（`usb`，12）：`r17_mapping_table_for_both_polarities`、`r17_board_polarity_is_active_low_per_bsp_code_and_schematic`、`r17_flipping_the_polarity_flips_every_result`（同一電平序列兩種設定得相反事件）、`r17_initial_state_comes_from_the_first_sample_without_an_event`、`r17_events_only_when_the_state_changes`（兩種 polarity×兩種初值各 100 次穩定取樣無事件）、`r17_change_is_reported_once_with_the_right_direction`、`r17_debounce_needs_n_consecutive_disagreeing_samples`、`r17_debounce_glitch_resets_the_count`、`r17_debounce_applies_to_unplug_too`、`r17_zero_and_one_debounce_are_immediate`、`r17_board_debounce_constant_is_used_by_for_board`、`r17_pin_is_gpio11_in_the_shared_pin_map`。

### 變異測試（暫時修改、跑 `cargo test-board-logic`、還原；還原後 210 passed）

| # | 變異 | 結果 |
|---|---|---|
| M1 | 充電序列順序：settle 延時移到暫停之前 | 3 項 FAILED（`r16_sequence_*`、`r16_no_read_happens_before_*`、`r16_adc_failure_at_every_index_*`） |
| M2 | 移除 resume（`Drop` 空實作） | 8 項 FAILED |
| M2b | **只移除失敗路徑的 resume**（`Drop` 空實作，成功路徑才明確 resume） | 4 項 FAILED（`r16_adc_failure_at_every_index_*`、`r16_charging_is_resumed_even_if_the_sampler_panics`、`r16_pause_is_never_nested_*`、`r16_repeated_failures_*`） |
| M3 | 取樣前就 resume（`drop(_pause)` 移到取樣之前） | 3 項 FAILED |
| M4 | settle 30 → 5 ms | 4 項 FAILED |
| M5 | 失敗讀值當 0 mV 累加 | 3 項 FAILED（`r16_adc_failure_is_an_error_not_zero_mv` 等） |
| M6 | `DIVIDER_MULT` 2 → 1 | 4 項 FAILED |
| M7 | 平均除以 15 | 5 項 FAILED |
| M8 | `percentage_from_curve` 下限 `<=` → `<` | **存活，等價變異**：mv == 3000 時改走內插迴圈，結果仍是 `0 + 0 = 0`，對外行為不變（全 u16 對舊演算法測試也過） |
| M9 | X4 曲線一個點 3830 → 3835 | 2 項 FAILED（`x4_curve_table_is_unchanged`、`x4_percentage_matches_the_pre_move_algorithm_for_every_u16`） |
| M10 | **翻轉 polarity 映射**（`ActiveLow => level_high`） | 8 項 FAILED |
| M11 | board 設定常數改 `ActiveHigh` | 2 項 FAILED（`r17_board_polarity_is_active_low_*`、`r17_board_debounce_constant_*`） |
| M12 | 解碼忽略設定（寫死 `!level_high`） | 3 項 FAILED（`r17_flipping_the_polarity_flips_every_result` 等） |
| M13 | 去抖 `<` → `<=` | 7 項 FAILED |
| M14 | 去抖計數在狀態一致時不歸零 | 1 項 FAILED（`r17_debounce_glitch_resets_the_count`） |
| M15 | 確認變更時不更新狀態 | 4 項 FAILED |
| X4-M | 真 `drivers/battery.rs` 加 `+ 1` | `check-x4-battery-equiv.sh` FAIL（見第 5 項），還原後 ok |

M1–M7、M9–M15 全被擊殺；M8 為等價變異（已說明）。

## 33. T9 尚未驗證項目

- **硬體全未驗**：
  - 分壓比（原理圖 1:1，兩顆 5.1 MΩ；實際阻值公差、漏電流、ADC 輸入阻抗負載造成的低估）與 ESP32-C61 ADC 在 ~2.55 MΩ 來源阻抗（並聯 100 nF）下的精度。
  - ADC 校正：esp-hal curve-fitting 與 ESP-IDF 校正是否給出相同 mV、C61 eFuse 校正資料是否存在（缺資料時 `AdcCalCurve` 行為，同 T7）；取樣是平均校準後 mV 而非 BSP 的「平均 raw 再校準」，差異未量。
  - 充電暫停沉澱時間：BSP 30 ms、crosspoint 5 ms，哪一個足以讓腳位電壓穩定、暫停充電 IC（U3 = `LGS4056HDA`，BOM `BOM_Board1_PCB_OnePage_V1_2026-08-19.csv:43`；`CE` 腳 R19 10 kΩ 上拉，原理圖 CHARGE 區塊；未讀其 datasheet）後的行為，皆未驗；暫停期間 30 s 一次的熱／電量影響未驗。
  - USB polarity 實際極性：預設 `ActiveLow`（理由見 31d），README 與實作矛盾**由原理圖與另一份韌體的註解傾向 ActiveLow，但沒有在這塊板子上實測**；bring-up 以 log 對照插拔確認。
  - 電池放電曲線：沿用 X4 表，C61 的電芯規格／曲線未驗。
  - `USB_DEBOUNCE_SAMPLES=3`（60 ms @20 ms）沒有硬體依據。
- **GPIO10／GPIO11 同時是 UART0 的預設腳**：`esp-metadata-generated 0.5.3 _generated_esp32c61.rs:4063-4064` 與 `:4274-4275`，`GPIO10 = U0RXD`、`GPIO11 = U0TXD`（IOMUX function 0）。本程式把它們設為 GPIO（充電輸出／USB 輸入）後，UART0 就不會再接到這兩個腳；若 bring-up 時 log 走 UART0 而不是 USB-Serial-JTAG，會與充電控制／USB 偵測互相干擾。`esp-println`（`esp32c61`＋`auto`）實際選哪個 channel 本次**沒有查證**，bring-up 時需確認（BSP README `:159` 提到 USB-Serial-JTAG 燒錄，暗示開發者使用該介面，但未查證 log 走哪個 channel）。ROM／bootloader 在 `esp_hal::init` 前的 UART 輸出是否在 GPIO11 上短暫輸出也未驗。
- 設計取捨需使用者／T12 確認：`measure()` 是**阻塞式**（~30 ms 沉澱＋16 次轉換在 critical section 中逐次進行，每次只有數十 µs）；T12 決定由哪個 task 以什麼週期呼叫，若不能容忍 30 ms 阻塞需改成 async 版延時。BSP 的 `board_charge_enable()` 沒有移植。`c61_boot` 的 30 s 週期只是 bring-up 迴圈。
- `Kernel::cached_battery_mv`／`tasks::BATTERY_MV`／`scheduler.rs` 對 C61 電量的消費、UI 顯示、USB 事件的使用者介面（T12）；deep sleep 前後的 charge／USB 處理、GPIO10 在 deep sleep 的保持狀態（T11）。
- `scripts/check-x4-battery-equiv.sh` 需要 host rustc 與 git（無 registry 依賴），不在 CI。
- X4 實機行為未重驗；僅有 host 上對 65536 個輸入的等價證明與 size 差異（第 4、5 項）。

---

# T10：C61 session 的 SD 持久化與有效／損壞狀態回歸（R18, R20；R19 只提供「保存」元件）

日期：2026-10-04。**沒有任何硬體驗收：SD 實際寫入、斷電半寫行為、FAT 一致性、寫入延遲、卡磨損全部未驗。** `c61_boot` 只證明 save／restore 的程式碼能編譯、連結並被呼叫，不宣稱 SD 寫入可用。**R19 的 shutdown 順序（EPD／SD／SPI 靜音、GPIO27 拉低、GPIO2 wake）與 wake 由 T11 完成**；T10 只交付「SD 仍上電時才能保存」的型別約束與保存／恢復元件，不實作 deep sleep，也未接進 app／scheduler（T12）。

## 34. 設計

### 34a. 分層（沿用 host seam；韌體與測試是同一份程式碼）

| 位置 | 內容 |
|---|---|
| `board-logic/src/session.rs`（新，host 測試 49 項） | `crc32`（CRC-32/IEEE 小實作，檢查值 `"123456789"` → `0xCBF43926`）；`SessionState`／`encode`／`decode`（固定 80 B little-endian）；`DecodeError`／`FieldError`；`Slot`／`SessionStore` trait（`read`／`write`／`delete`）／`StoreError`；`save_session`／`restore_session`／`clear_session`；`SdActiveProof`（`PeripheralPower::sd_active()`，只在 `RailState::SdActive` 發出，借用 `&PeripheralPower`，所以保存期間 `begin_shutdown`／`cut_peripheral_power`（`&mut`）無法同時進行） |
| `kernel/src/board_c61/session.rs`（新） | `SdSessionStore`：`SessionStore` 實作在現有 `drivers::storage`（`read_chunk_in_pulp`／`write_in_pulp`／`delete_in_pulp`／`file_size_in_pulp`）；`save_session(&PeripheralPower<Gpio27Rail>, &SdStorage, &SessionState)`、`restore_session(...)`、`clear_session(...)` 只是把 `SdSessionStore` 傳給 board-logic 的版本；沒有 `unwrap`／`expect`／`panic!` |
| `board-logic/src/lib.rs`、`kernel/src/board_c61/mod.rs` | 註冊兩個模組 |
| `src/bin/c61_boot.rs` | SD 掛載後 `session_demo`：`ensure_pulp_dir_async` → `restore_session`（log 決策）→ 若尚無有效 session，存一筆只含 Home 的無害狀態 → 再 `restore_session`（模擬重啟）；所有失敗只 log |
| X4 | **未改**（`rtc_session.rs`／scheduler／app 完全沒動；X4 的 RTC 路徑繼續用自己的 `RtcSession`） |

### 34b. session 欄位對照 X4 `RtcSession`（語意一致；C61 獨立實作，沒有共用 X4 結構）

X4 來源：`kernel/src/kernel/rtc_session.rs`（`#[repr(C, align(4))]` 256 B 以內，放 `.rtc_fast.persistent`；`is_valid_session()` 只比 magic，`save()` 寫入時補 magic，`load()`；呼叫點：`scheduler.rs::boot` 開機讀、`sleep_with_session` 睡前由 `AppLayer::collect_session` 收集後 `save`；`src/apps/manager.rs::apply_session` 還原）。

| X4 欄位 | C61 欄位（檔案 offset） | 驗證 |
|---|---|---|
| `magic` = `0x504C5053`（"PLPS"） | `magic` u32 LE @0，**同一常數** | 必須相等；X4 只有 magic，無版本／CRC |
| `wake_count` | `wake_count` u32 @12（呼叫端管理；X4 在 zeroed 結構上 `+1`，所以永遠是 1，是 X4 既有行為，不移植該缺陷） | 無 |
| `flags`／`_header_pad` | `version` u16 @4（=1）、`flags` u16 @6（必須 0）、`seq` u32 @8（新增，雙槽用） | version 必須相等；flags 必須 0 |
| `nav_depth` | u8 @16 | 1..=4（X4 `apply_session` 的檢查） |
| `nav_stack[4]`（Home0／Files1／Reader2／Settings3／Upload4） | u8×4 @17 | depth 內 id ≤ 4；depth 之後必須 0；`stack[0]` 必須 Home（X4 註解「Home 永遠在底」，**比 X4 嚴格**） |
| `reader_filename[32]`／`_len` | u8×32 @21、len u8 @53 | len ≤ 32；name 區域內為可印 ASCII 0x21–0x7E 且非 `/ \ : * ?`；len 之後必須 0；stack 含 Reader 時 len ≥ 1（後兩項為新增驗證） |
| `reader_is_epub` | u8 @54 | 0 或 1 |
| `reader_chapter` u16／`reader_page` u16／`reader_byte_offset` u32／`reader_font_size` u8 | @55／@57／@59／@63 | font ≤ 4（`FONT_SIZE_NAMES` 有 5 項）；其餘全範圍合法（X4 `restore_state` 也忽略 `_page`，只用 byte offset） |
| `files_scroll` u16／`files_selected` u8／`files_total` u16 | @64／@66／@67 | 全範圍合法（X4 對應 getter 轉 `u16`／`u8` 截斷，無更窄的語意範圍可驗證） |
| `home_state`／`home_selected`／`home_bm_selected`／`home_bm_scroll` | u8×4 @69–72 | `home_state` ≤ 1（0=Menu，1=ShowBookmarks） |
| `settings_*` 快取（sleep timeout、ghost clear、fonts、valid） | **不保存** | X4 的快取只是為了喚醒時省 SD 讀取；C61 的 session 本身就在 SD 上，SETTINGS.TXT 維持單一事實來源 |
| `_pad`／`_reserved` | reserved 3 B @73 必須 0；`crc32` u32 @76（涵蓋 @0..76） | 非零或 CRC 不符即拒絕 |

檔案總長固定 **80 B**；`MAX_NAV_STACK = 4`、`MAX_FILENAME_LEN = 32`、magic 與 X4 數值相同，由測試 `r18_format_constants_match_the_x4_session_semantics` 鎖定（跨板編譯期斷言不可行：`rtc_session.rs` 只在 `board-x4` 編譯）。

### 34c. 檔案位置與命名

`_PULP/SESSA.BIN`、`_PULP/SESSB.BIN`（8.3；`PULP_DIR = "_PULP"` 與 `SETTINGS.TXT`／`BKMK.BIN`／`TITLES.BIN` 同一目錄，沿用既有 `storage::*_in_pulp`）。`_PULP` 由開機路徑 `ensure_pulp_dir_async` 建立（X4 `main.rs:113` 同；`c61_boot` 的 demo 也先呼叫）；目錄不存在時 `read` 回「不存在」、`save` 回寫入錯誤，不 panic。

### 34d. 原子性策略：雙槽＋單調序號＋CRC＋寫後讀回（不用 temp＋rename）

**選擇理由**：`embedded-sdmmc`（本 repo 鎖定的 `hansmrtn/embedded-sdmmc-rs` async 分支 `0bf12548`）**沒有 rename API**（grep `fn rename` 無結果；只有 `delete_entry_in_dir`／`open_file_in_dir`／`flush_file`），所以 temp＋rename 不可行；而 `storage::write_*` 是 `ReadWriteCreateOrTruncate`（先截斷再寫），單檔覆寫在斷電時會把唯一的好檔變成半寫檔。雙槽：

1. `save` 先讀兩個槽並 decode；選出最新的有效紀錄（序號用 RFC 1982 風格的 wrapping 比較，`u32::MAX → 0` 仍判新；兩槽序號相同時固定取 A）。
2. 目標 = **另一個槽**（最新有效槽絕不被覆寫；沒有有效槽時寫 A，序號從 1 開始）；新序號 = 最新序號 + 1。無效／損壞的槽自然排在被覆寫的優先順序（因為它不是「最新有效槽」）。
3. 寫入目標槽後**讀回並 decode**，必須與剛寫的 seq 與內容完全相同，否則回 `SaveError::Verify`（`write_in_pulp` 對 `close_file` 的錯誤是忽略的，所以讀回是唯一能抓到「回報成功但沒落地」的機制）。
4. 斷電只可能毀掉被取代的那個槽；CRC 擋下半寫檔，舊的最新槽仍然有效，開機還原舊位置。

`restore`：兩槽都 decode，取最新有效者；若另一槽讀取失敗（不明）而這槽有效，仍回傳這槽（已知最佳位置）。兩槽皆無效：不存在 → `NoSession`，有檔但壞 → `Corrupt(原因)`，讀取失敗 → `StorageUnavailable`，SD 非 active → `SdNotActive`；**一律是正常開機**（不恢復位置、不 panic）。壞檔不刪除，下次 `save` 會優先覆寫它（`clear_session` 刪除兩槽，供 T12 決定是否採「一次性還原」，兩次刪除之間失敗可能留下較舊的紀錄）。

`save` 的其他失敗政策：讀槽失敗 → 中止、**不寫**（回 `SaveError::Read`，因為下一個序號未知）；狀態不合法 → 在任何卡存取前拒絕（`InvalidState`）；寫失敗 → `Write`，舊紀錄不受影響。保存失敗只回報；是否仍睡眠由 T11 決定。

已知限制：`drivers::storage` 的巨集把所有 open 失敗都標為 `OpenFile`／`OpenDir`，所以「檔案不存在」與「開檔暫時失敗」無法區分，一律映成 `NotFound`。最壞情況：暫時性 open 失敗讓 `save` 以為某槽不存在而把它當目標；但另一槽仍是有效（舊一代）紀錄，所以永遠仍有可還原的 session（最多回到前一次保存的位置）。

### 34e. R18 順序約束

`save_session`／`restore_session`／`clear_session` 都先呼叫 `PeripheralPower::sd_active()`；只有 `RailState::SdActive`（SD 已初始化、尚未 `begin_shutdown`、卡未被拔）才通過。`Unpowered`／`PowerCycled`（含 `card_removed()` 之後）／`SdInitializing`／`ShuttingDown`／`PoweredOff` 一律拒絕，且**沒有任何讀寫發生**（測試以 fake store 的 read／write 計數驗證）。T11 的呼叫順序必須是：`save_session` →（成功或失敗都記錄）→ `begin_shutdown` → 停 EPD／SD → `cut_peripheral_power`。

## 35. T10 實際指令與結果

| # | 指令 | 結果 |
|---|---|---|
| 1 | `cargo test-board-logic` | **259 passed; 0 failed**（T9 的 210 項 + `session` 49 項；T4–T9 測試未動）；無 warning |
| 2 | `CARGO_TARGET_DIR=target/t10-c61 cargo build-c61 --locked` | **成功**，0 warning。`size`：text 309822 / data 3992 / bss 173848（T9：268400 / 3816 / 173848；**C61 的 text 欄含 `.stack`**）。實際區段（`readelf -S`）：`.text` 94,428 → 131,354（**+36,926**）、`.rodata` 30,720 → 35,392（+4,672）、statics 123,928 → 124,104（+176）。flash 增加的主因是 C61 首次連入 embedded-sdmmc 的寫入／刪除／mkdir 路徑（`nm` 可見 `async_make_dir`、`ensure_pulp_dir_async` 的 drop glue；T5–T9 的 C61 只有讀目錄），加上 session 邏輯與 `Debug` 格式；這是推論，沒有逐函式拆帳 |
| 3 | `nm -C`／`strings` 佐證 c61_boot 實際呼叫 | `nm -C`：`pulp_kernel::board_c61::session::restore_session`（`42028394`）、`pulp_board_logic::session::{read_slot::<SdSessionStore>, newest, crc32, SessionState::validate}`、`SessionState::eq`、`FieldError`／`StoreError`／`DecodeError`／`NormalBootReason`／`SaveError`／`Slot` 的 `Debug::fmt`。`save_session` 被內嵌進 `session_demo`（不是獨立 symbol）。`strings`：`session[`、`session: saved to slot `、`session: save failed: `、`session: _PULP dir: `、`NoSession`／`Corrupt`／`StorageUnavailable`／`SdNotActive` |
| 4 | `C61_TARGET_DIR=target/t10-c61 bash scripts/report-c61-memory.sh` | exit 0，`RESULT: all memory budget checks ok`；`image: statics 124104 B (48.3% of 256624 B main RAM), stack 132520 B, headroom over 49152 B minimum = 83368 B`（T9：123928／132696／83544；statics +176 B，headroom −176 B） |
| 5 | `CARGO_TARGET_DIR=target/t10-x4 cargo build-x4 --locked` | **成功**，0 warning；`size`：text **1221824** / data **27624** / bss **348812**，與 T9／T8／T7 **完全相同**（差 0／0／0）。`objcopy -O binary --only-section` 比較 T9 與 T10：`.trap` `.rwtext` `.data` `.rodata` `.text` 皆 `cmp` 位元組相同（X4 沒有編入 session 模組） |
| 6 | `bash scripts/check-x4-input-trace.sh`／`check-x4-driver-trace.sh`／`check-x4-battery-equiv.sh` | `ok X4 input trace identical to pre-T7 (26032 lines, sha256 2c3738aa…)`／`ok X4 driver trace identical to pre-T6 (2122 lines, sha256 66a0447f…)`／`ok X4 battery conversions identical to pre-T9 (65536 inputs, sha256 272e9905…)` |
| 7 | `bash scripts/check-board-selection.sh` | 4 項 `ok` |
| 8 | `OFFLINE_CHECK_TARGET_ROOT=target/t10-check bash scripts/check-offline-boundary.sh` | exit 0，全 `ok`（C61 無 radio／net／upload symbol） |
| 9 | `X4_ELF=target/t10-x4/riscv32imc-unknown-none-elf/release/pulp-os C61_TARGET_DIR=target/t10-c61 bash scripts/check-c61-no-c3-raw-gpio.sh` | exit 0，7 項 `ok`＋1 項 `info`（同 T5：esp32c61 PAC TIMG1） |
| 10 | `cargo fmt --all -- --check` | 無差異 |
| 11 | `grep -n 'unwrap\|expect(\|panic!' kernel/src/board_c61/session.rs`；`board-logic/src/session.rs` 在 `#[cfg(test)]` 之前的部分 | 皆無輸出 |

Cargo.lock 未被 T10 變動（`--locked` 通過；`board-logic` 仍零依賴）。

### 新增 host 測試（49 項，`board-logic/src/session.rs`，代表）

- 格式／CRC：`crc32_matches_the_standard_check_values`、`r18_format_constants_match_the_x4_session_semantics`（magic／4／32／8.3 檔名）、`r18_layout_is_the_documented_little_endian_layout`。
- R20 round-trip：`r20_round_trip_restores_the_same_position`、`r20_round_trip_field_boundary_values`（u16／u32 最大值、`seq` ∈ {0,1,MAX-1,MAX}）、`r20_round_trip_every_nav_depth_and_every_app_id`、`r20_round_trip_every_file_name_length`（0..=32）。
- R20 損壞矩陣：`r20_decode_empty_file`、`r20_decode_truncated_to_every_length_is_rejected`（長度 1..79 各一次）、`r20_decode_trailing_data_is_rejected`、`r20_decode_bad_magic_with_a_valid_crc`、`r20_decode_wrong_version_with_a_valid_crc`、`r20_decode_crc_mismatch_is_rejected`、`r20_every_single_bit_flip_in_the_record_is_rejected`（80 B × 8 bit 全掃，另含逐位元組反相與歸零）、`r20_with_the_crc_repaired_a_flip_is_rejected_or_canonical`（CRC 重算後全掃：被接受者必須能 byte-for-byte 重新編碼回原樣，即無「被接受的垃圾」）、`r20_illegal_field_values_with_a_valid_crc_are_rejected`（20 種非法欄位）、`r20_decode_never_panics_on_pseudo_random_input`（8000 筆 xorshift）、`r20_every_corruption_of_the_only_slot_is_a_normal_boot`（空檔、每個截斷長度、每個位元組、尾隨垃圾 × 兩個槽，決策一律 `NormalBoot(Corrupt)`）。
- R20 恢復決策：`r20_valid_session_is_restored_to_the_same_position`、`r20_no_session_files_is_a_normal_boot`、`r20_a_corrupt_slot_does_not_hide_a_valid_older_one`、`r20_newest_valid_sequence_wins_in_both_slot_orders`、`r20_sequence_wraparound_still_picks_the_newer_record`、`r20_equal_sequence_in_both_slots_is_resolved_deterministically_to_a`、`r9_r20_sd_errors_are_a_normal_boot_not_a_panic`（缺卡／I/O 錯誤）、`r20_restore_is_refused_without_an_active_sd`。
- R18 原子性／失敗：`r18_saves_alternate_slots_with_increasing_sequence`、`r18_a_save_never_overwrites_the_newest_valid_record`、`r18_write_failure_keeps_the_previous_session_restorable`（寫入失敗／截斷後失敗／靜默丟棄三種）、`r18_power_cut_at_every_byte_of_the_write_never_yields_a_wrong_session`（斷電於寫入第 0..=80 位元組：舊 session 完好，或整筆落地時為新 session，不會出現第三種）、`r18_power_cut_during_the_very_first_save_is_a_normal_boot`、`r18_repeated_torn_writes_never_corrupt_the_good_slot`、`r18_read_failure_before_save_aborts_without_writing`、`r18_verify_catches_a_read_back_that_differs`、`r18_an_invalid_state_is_refused_before_any_card_access`、`r18_a_misbehaving_store_reporting_too_many_bytes_does_not_panic`。
- R18 順序約束：`r18_save_is_refused_unless_the_sd_is_active`（5 個非 active 狀態 × 不讀不寫）、`r18_save_before_shutdown_works_and_after_shutdown_begins_is_refused`（完整 T11 順序：save → begin_shutdown → 拒絕 → cut power → 拒絕；下一次開機看到先前保存的位置）、`r18_card_removal_blocks_saving_until_the_card_is_back`、`r18_sd_active_proof_is_only_minted_in_sd_active`；`r20_clear_*` 2 項。

### 變異測試（暫時修改 `board-logic/src/session.rs`、跑 `cargo test-board-logic`、還原；還原後 259 passed）

| # | 變異 | 結果 |
|---|---|---|
| M1 | CRC 只比低 16 位元 | 2 項 FAILED（`r20_every_single_bit_flip_in_the_record_is_rejected`、`r20_every_corruption_of_the_only_slot_is_a_normal_boot`） |
| M1b | 完全移除 CRC 檢查 | 3 項 FAILED（`r20_decode_crc_mismatch_is_rejected` 等） |
| M2 | 跳過 version 檢查 | 2 項 FAILED（`r20_decode_wrong_version_with_a_valid_crc`、`r20_with_the_crc_repaired_a_flip_is_rejected_or_canonical`） |
| M3 | 雙槽選擇改取**較舊**序號 | 7 項 FAILED（`r18_saves_alternate_slots_with_increasing_sequence` 等） |
| M4 | 移除 SD-active 檢查 | 6 項 FAILED（`r18_save_is_refused_unless_the_sd_is_active`、`r18_save_before_shutdown_works_and_after_shutdown_begins_is_refused` 等） |
| M5 | 略過寫後讀回驗證 | 2 項 FAILED（`r18_verify_catches_a_read_back_that_differs`、`r18_write_failure_keeps_the_previous_session_restorable`） |
| M6 | `save` 覆寫最新槽而非另一槽 | 7 項 FAILED（`r18_a_save_never_overwrites_the_newest_valid_record`、`r18_power_cut_at_every_byte_of_the_write_*` 等） |
| M7 | 跳過 magic 檢查 | 2 項 FAILED |
| M8 | 接受尾隨資料（截到 80 B） | 2 項 FAILED（`r20_decode_trailing_data_is_rejected` 等） |
| M9 | 移除 flags／reserved 檢查 | 2 項 FAILED |
| M10 | 序號比較改成普通 `>`（無 wraparound） | 2 項 FAILED（`r20_sequence_wraparound_still_picks_the_newer_record`、`r20_a_saved_wraparound_sequence_continues_correctly`） |
| M11 | `save` 忽略讀槽失敗 | 1 項 FAILED（`r18_read_failure_before_save_aborts_without_writing`） |
| M12 | 移除 nav_depth 上限檢查 | 3 項 FAILED |
| M13 | 移除檔名尾端零檢查 | 1 項 FAILED（`r20_illegal_field_values_with_a_valid_crc_are_rejected`） |
| M14 | 兩槽序號相同時改取 B | 第一次**存活**（258 passed）→ 補 `r20_equal_sequence_in_both_slots_is_resolved_deterministically_to_a` 後 1 項 FAILED |

全部變異皆被擊殺（M14 在補測試後）。

## 36. T10 尚未驗證項目

- **硬體全未驗**：
  - SD 實際寫入：`write_in_pulp`（truncate＋write＋close）在 ESP32-C61 的 SPI2／DMA 路徑上是否成功、寫後讀回是否一致；`close_file` 錯誤在 `drivers::storage` 被忽略，只靠讀回偵測。
  - 斷電半寫行為：fake store 以「截斷後只寫前 k 位元組」模擬，真實 FAT（目錄項大小更新、簇配置、FAT 表、`embedded-sdmmc` 的 write-back 快取）在斷電時可能留下與模擬不同的狀態（例如檔案長度正確但內容為舊資料、cross-linked cluster）。雙槽＋CRC 的保證僅限「被取代的槽會被 CRC 拒絕」這個模型；卡本身掉電時的 flash translation layer 行為不在範圍。
  - FAT 一致性：不假設斷電後檔案系統可掛載；掛載失敗走 R9 路徑（無 session、正常開機）。
  - 寫入延遲（含讀兩槽＋寫＋讀回，共 4 次小檔案 I/O 與一次目錄查找）與對睡前流程時間的影響；卡磨損（每次睡眠兩個 80 B 檔案覆寫，每次仍落在同一個目錄項／簇，未量）。
  - `_PULP` 目錄是否在實機上被建立、`file_size_in_pulp`／`delete_in_pulp` 對不存在檔案的實際回傳（程式假設為 `OpenFile`／`OpenDir` 類錯誤）。
- 設計取捨：
  - **序號與重啟後仍會還原**：與 X4（RTC，電源循環後歸零）不同，SD session 在一般斷電（非 deep sleep）後也存在，下次開機會還原上次睡前位置；是否採「還原後立即 `clear_session`」（避免壞資料造成開機迴圈）由 T12 決定，`clear_session` 已提供。
  - `save` 在讀槽失敗時**中止不寫**（保守）；暫時性讀取錯誤會造成該次保存失敗並回報，T11 決定是否仍睡眠。
  - 存取 `drivers::storage` 是同步＋`poll_once`（SPI blocking）；`restore_session`／`save_session` 在 async 開機流程中會阻塞數十 ms（未量）。
- 尚未整合：`AppLayer::collect_session`／`apply_session` 仍綁 X4 的 `RtcSession`（`kernel` 模組在 C61 不編譯）；C61 需要在 T12 把 app 層的收集／套用改成 `SessionState`（欄位一對一，見 34b）。deep sleep／GPIO2 wake／外設 shutdown 順序（R19）屬 T11。
- `scripts/` 沒有新增 X4 等價檢查，因為 T10 完全沒有改 X4 原始碼；X4 section 與 T9 byte-identical 為證。

---

# T11：deep-sleep 外設 shutdown／GPIO2 wake、power-order contract（R18, R19, R20）

日期：2026-10-04。**沒有任何硬體驗收：實際睡眠電流、GPIO2 是否真能喚醒、GPIO27 斷電後 pad 狀態、RTC hold、EPD 殘影／park、喚醒後 SD 重新初始化全未驗。**不宣稱可睡眠／喚醒。

## 37. 設計

### 37a. 分層（沿用 host seam；韌體與測試是同一份程式碼）

| 位置 | 內容 |
|---|---|
| `board-logic/src/sleep.rs`（新，host 測試 27 項） | `SleepSequence::enter_deep_sleep`（整個順序與失敗政策的唯一實作）；traits：`SessionSaver`（＋`StoreSaver<S: SessionStore>` 轉 T10 `save_session`）、`DisplayPark`、`SdShutdown`、`ChargeRestore`（為 `BatteryMonitor`、`Option<T>`、`&mut T` 實作）、`LineSilencer`、`WakeConfig`、`SleepEntry`；`WakeSpec`／`WAKE_SPEC`（GPIO2、`Low`、`Up`）；證明 token `WakeArmed`（只有序列在 `arm` 回 `Ok` 後能建立）；`SleepReport`／`SleepAbort`／`AbortReason`；R20：`WakeCause`／`WakeBits`／`classify_wake`、`BootPlan`／`plan_boot` |
| 「PowerRail」 | 不另立 trait：就是 T4 的 `PeripheralPower<P: RailPin>`（序列持有 `&mut`，GPIO27 沒有第二條路徑）。新增 `PoweredOffProof<'a>`（`powered_off()` 只在 `PoweredOff` 回 `Ok`；借用 power、不可 Clone） |
| `board-logic/src/ssd1677.rs` | 新 `deep_sleep(bus)`（0x10 ＋ 0x01）；新 `Epd::enter_deep_sleep()`（有界等 BUSY → 0x10 mode 1；BUSY 超時／bus 失敗仍嘗試送命令並回傳第一個錯誤；之後視為未初始化） |
| `board-logic/src/spi.rs`、`battery.rs` | `SpiArbiter::park_selects_low()`（兩個 CS 拉低，有 transaction 時回 `Busy` 且不動腳）；`BatteryMonitor::ensure_charging()`（冪等，GPIO10 = 允許充電） |
| `kernel/src/board_c61/sleep.rs`（新） | esp-hal 轉接：`WakeKey`（GPIO2 pull-up `Input`、`listen(LowLevel)`、`apply_wakeup_config(low_power_path)`）、`EpdPark`、`SdFlush`（`flush_and_close`）、`C61Lines`、`C61SleepEntry`（`LowPower::sleep_deep(RtcSleepConfig::deep())`，`Entered = Infallible`，不返回）、`wake_cause()`、`enter_deep_sleep(..) -> SleepAbort`（只在 abort 時返回，成功就睡著不返回） |
| `kernel/src/board_c61/spi.rs` | `SpiControl::silence_lines()`：SCK 在 mode 0 閒置為低、無 CS 時送一個 0x00 讓 MOSI 低、再經 arbiter 把兩個 CS 拉低 |
| `kernel/src/drivers/ssd1677.rs`（**X4**） | `enter_deep_sleep` 結尾的 `send_command(DEEP_SLEEP); send_data(&[0x01])` 改呼叫共用 `shared::deep_sleep(self)`（其餘不動）；`scripts/check-x4-driver-trace.sh` 的 stimulus 已含兩次 `enter_deep_sleep()`，trace sha256 不變、`.text` 位元組相同（見 §38） |
| `src/bin/c61_boot.rs` | 開機讀 `sleep::wake_cause()` 並 log；SD init 後 `plan_boot(cause, restore_session(..))` 決定恢復或正常開機（log）；睡眠序列連結進映像，**預設停用**（見 37g） |

### 37b. BSP 順序對照表（`../bsp_onepage_c61/board_c61.c`）

BSP `board_sleep_enter`（:335-357）實際順序，與本實作逐步對照：

| BSP（行號） | 動作 | 本實作步驟 | 備註 |
|---|---|---|---|
| —（BSP 無此步） | 存閱讀位置 | **1 save_session**（`SdActive`、GPIO27 仍高） | R18；T10 的 `save_session` 要 `SdActiveProof` |
| :337 `board_display_sleep()` → `be->sleep` | 停 EPD 控制器 | 4 `Epd::enter_deep_sleep`（0x10 mode 1） | BSP 的 moui SSD1677 `sleep` 原始碼本機沒有（T6 同），命令序列取自 X4（`kernel/src/drivers/ssd1677.rs` `enter_deep_sleep`）；full update 的 0xF7 已含 power-down，所以不另送 0x83 |
| —（BSP 無此步） | 停 SD | 5 `flush_and_close` | X4 的 CMD0 不送：C61 隨即斷電（BSP 也不送）；只 flush 避免寫到一半斷電 |
| —（BSP 無此步；:299 `board_battery_mv` 自己會恢復） | 充電 | 6 `ensure_charging`（GPIO10 高） | 保險：睡前最後一次寫 GPIO10 一定是「允許充電」 |
| :340-344 `gpio_set_level(SCLK,MOSI,CS,SD_CS,PDM_CLK, 0)` | 靜音共享線 | 7 `silence_lines`＋PDM CLK 低 | esp-hal 的 SPI 腳在 `SpiDma` 內，不能當 GPIO 輸出：以 SCK idle low＋送 0x00＋CS 拉低達成（見 `spi.rs`） |
| :346 `board_peripherals_power(false)` | GPIO27 低 | 8 `cut_peripheral_power`（需先 `begin_shutdown`） | T4 狀態機 |
| :348-352 `esp_sleep_enable_gpio_wakeup_on_hp_periph_powerdown(GPIO2, LOW)` | 配置 GPIO2 低電位喚醒 | **2 arm wake（提前到斷電之前）** | **刻意偏離 BSP**，理由見下 |
| :353-355 timer wake | 計時喚醒 | 不實作 | 離線閱讀不需要 |
| :356 `esp_deep_sleep_start()` | 進 deep sleep | 9 `SleepEntry::enter`（需要 `PoweredOffProof`＋`WakeArmed`） | |

**偏離 BSP 的一點：wake 配置在斷電之前（步驟 2），BSP 在斷電之後（:348-352 在 :346 之後）。** 任務指定「GPIO2 wake 必須在 GPIO27 斷電前完成」，本實作更進一步放到所有不可逆步驟之前：(1) wake 配置和 rail 沒有電氣相依（GPIO2 是 LP pad，不在 GPIO27 供電域，且 pull-up 是內部的），所以前移不改變 BSP 在電氣上重要的順序（park → silence → cut → sleep）。(2) wake 配置是唯一「失敗後無法救」的步驟：esp-hal 的 `sleep_deep` 在沒有任何 wake source 時 **panic**（`no wakeup source is enabled`，見 37d），rail 已斷＋沒有喚醒源等於只能靠 reset／插 USB 才回得來；而 EPD 進 deep sleep mode 1 後只有硬體 reset 能喚醒，在這塊板子上硬體 reset 就是 GPIO27 開機 power-cycle，狀態機禁止 runtime 使用，所以步驟 3 起全部不可逆。先 arm 讓「無法配置 wake」成為乾淨的 abort（裝置仍完整可用）。（3）任務的失敗矩陣要求「每一步單獨失敗仍到達 wake 已配置＋外設斷電＋進 sleep」：對 wake 配置本身這不可能成立，所以 wake 失敗的政策是 abort，見 37c。其餘 BSP 順序全部保留。

### 37c. 失敗政策（皆有測試）

| 情況 | 行為 | 理由 |
|---|---|---|
| rail 不在 `PowerCycled`／`SdActive`（`Unpowered`／`SdInitializing`／`ShuttingDown`／`PoweredOff`） | **abort，完全不動任何東西**（含不 save、不 arm），回 `AbortReason::Rail` | 序列只能在開機完成後呼叫；`PoweredOff` 後再呼叫是 no-op 並拒絕（不重複 arm、不重複斷電） |
| save 失敗或被拒（拔卡 → `SdNotActive`、SD 錯誤、寫入驗證失敗） | **記錄在 `SleepReport.save`，仍睡眠** | 不睡眠會讓 CPU 醒著耗電到電池沒電；遺失位置只損失一頁，且舊的雙槽紀錄仍有效（T10）。被拒時沒有任何 SD 讀寫（測試以 store 的 I/O 計數驗證） |
| EPD park／SD shutdown／charge restore／silence 任一失敗 | **記錄，序列繼續**到斷電與進睡眠 | 後面的步驟不依賴前面的成功；停在半斷電狀態比帶著殘缺外設睡眠更糟 |
| wake 配置失敗（`NoLowPowerPath`、不支援的 spec） | **abort**（尚無不可逆動作；save 結果保留在 `SleepAbort.save`） | 見 37b；沒有 wake source 的 sleep 會 panic 或回不來 |
| wake 電位已存在（`AlreadyAsserted`：GPIO2 按鍵還按著） | **abort**，放開後可重試（測試：同一 rail 之後 sleep 成功） | `sleep_deep` 不支援 rejection，pad 已在 wake 電位時會立刻醒或睡過頭；這也提醒 T12：睡眠觸發必須等 GPIO2 放開 |
| `cut_peripheral_power` 失敗 | 不可能（`ShuttingDown` 必成功），仍映射成 abort | 型別完整性 |

型別層強制：`SleepEntry::enter` 需要 `PoweredOffProof`（只有 GPIO27 已低的 `PoweredOff` 才能建立，`PeripheralPower::powered_off()`）與 `WakeArmed`（只有序列在 wake 配置成功後能建立）；「SD 仍 active 且未保存」不可能走到 enter，因為 `SdActive` 到 `PoweredOff` 必經 `begin_shutdown`／`cut`，而序列的第一個硬體動作就是 save（公開 API 仍可手動 `begin_shutdown` 而不 save：型別系統不強制「save 一定在 shutdown 前」，只有序列強制；T12 必須走序列而不是自己呼叫 `begin_shutdown`）。

### 37d. esp-hal 1.2.0 對 ESP32-C61 的 deep-sleep 喚醒（讀原始碼＋實際編譯確認，非猜測）

- API：`esp_hal::rtc_cntl::sleep::{LowPower, RtcSleepConfig}`（`unstable`，workspace 已開）。`LowPower::sleep_deep(config) -> !`。**沒有舊版的 wake-source 清單參數**：喚醒源由各 driver 自行宣告；`Input::listen(Event)`＋`Input::apply_wakeup_config(&WakeupConfig::default().with_low_power_path(true))` 宣告一個 pad。原始碼：`esp-hal-1.2.0/src/rtc_cntl/sleep/mod.rs:119-150`、`gpio/wakeup/mod.rs`。
- C61 metadata（`esp-metadata-generated-0.5.3/_generated_esp32c61.rs`）：`sleep.deep_sleep = true`、`sleep.ext1_version = 2`、`sleep.deep_sleep_needs_gpio_isolation = false`；LP pad = GPIO0–GPIO6（`LP_GPIO0..6`），**GPIO2 = LP_GPIO2，是 LP pad**，可用 low-power path（編譯通過 `apply_wakeup_config` 的 `NoLowPowerPath` 路徑；實機未驗）。同族 PDM DIN GPIO3、前面 ADC GPIO4、電池 ADC GPIO5、PREV GPIO6 也是 LP pad，GPIO27／GPIO10／EPD DC 不是。
- 實際路徑：listening 且有 low-power path 的 pad，在 sleep 進入時由 entry hook 分配到 **EXT1**（`gpio/wakeup/ext1_v2.rs`，C61／C5／C6／H2 共用）：`ext_wakeup_sel` 以 LP 編號選 pad、`ext_wakeup_lv` 每個 pad 一個電位；`Event::LowLevel`／`FallingEdge` 都轉成 Low（`armed_level`）。`prepare_pad` 把數位 pull 複製到 LP IO MUX，並在 deep sleep 時 `pad_hold`；開機時 `wake_io_reset` 釋放 hold。因此 **GPIO2 必須是 Pull::Up**（wake level 低，需上拉對抗），`WakeKey` 正是這樣建立。BSP 側：GPIO2 active-low、內部上拉（`board_keys.c:104-106`），`esp_sleep_enable_gpio_wakeup_on_hp_periph_powerdown(.., GPIO_LOW)`（`board_c61.c:352`）。**注意**：BSP 那個 IDF 6 API 與 esp-hal 的 EXT1 是否為同一硬體路徑，本機沒有 IDF 原始碼可對照，只能說兩者都針對「HP 週邊斷電時仍可偵測的 LP pad 低電位」；實機未驗。
- wake cause：`esp_hal::rtc_cntl::wakeup_cause()`（非 deep-sleep reset 回空集合）。C61 的 LP pad 喚醒回報為 `WakeupSource::Ext1`（數位 pad 路徑為 `Gpio`）；`sleep::wake_cause()` 把兩者都當作 `WakeCause::Key`，`Timer`→`Timer`，其餘→`Other`，空→`PowerOn`，優先序同 BSP `board_wake_cause`（`board_c61.c:359-380`）。
- `sleep_deep` **沒有 rejection**：若 wake 電位在進入時已存在，晶片可能睡過頭（`sleep_deep_with_rejection` 才會回報）。已由 `AlreadyAsserted` 預檢擋掉（GPIO2 低就 abort）；沒有改用 `sleep_deep_with_rejection`：即使被拒回傳，EPD 此時已 park 且 GPIO27 已低（不可逆），回傳也無法復原，所以改在不可逆步驟之前預檢。
- 無 wake source 時 `sleep_deep` panic：由 `WakeArmed` 型別＋先 arm 的順序避免。
- hold／isolate：C61 `deep_sleep_needs_gpio_isolation = false`，esp-hal 不做全 pad isolate；只 hold 被 arm 的 wake pad。BSP 沒有任何 `gpio_hold`／isolate 呼叫，本移植也沒有，所以 **GPIO27、GPIO10、EPD DC、CS 在 deep sleep 中的電位完全取決於 SoC 預設與板上外部電阻，未驗**（可能的後續：`Output::set_pad_hold(true)` 在 GPIO27 低後鎖住；esp-hal 1.2 有此 API，**未採用**，因為 BSP 沒有而且硬體行為未知，留待實機決定）。
- RTC FAST／RTC 記憶體：C61 session 在 SD，不需要 RTC 記憶體；`RtcSleepConfig::deep()` 使用預設（X4 另外 `set_rtc_fastmem_pd_en(false)`，C61 不需要）。

### 37e. X4 sleep 行為等價性重審（esp-hal 1.0.0 → 1.2.0，ESP32-C3，只讀原始碼，X4 無實機）

比對 1.0.0 `rtc_cntl/sleep/esp32c3.rs`（`RtcioWakeupSource`）與 1.2.0 `gpio/wakeup/per_pin.rs`＋`lp_io/low_level/v3.rs`（C3 路徑）：

| 項目 | 1.0（舊 X4 程式） | 1.2（現 X4 程式） | 結論 |
|---|---|---|---|
| wake pad／電位 | GPIO3、`GPIO_INTR_LOW_LEVEL`（`WakeupLevel::Low`） | GPIO3 `listen(FallingEdge)`，`armed_level` 轉 Low；`apply_wakeup(lp, true, Low)` | 等價（同一 LPWR `gpio_wakeup` 欄位） |
| pad hold | `rtcio_pad_hold(true)` | `pad_hold(lp, true)`（`LPWR pad_hold` 的 `gpio_pin3_hold`）；開機時 `wake_io_reset` 釋放 | 等價；1.2 多了開機釋放 hold（正向修正） |
| gpio clk gate／filter／status clear | 都有 | `prepare_gpio_wakeup` 都有 | 等價 |
| pull | 明確寫 RTC_IO 的 `rue=1`、`rde=0` | **不再明確寫**；依賴 `Input` 的 Pull::Up 與 hold（esp-hal 文件：「sleep keeps the pull resistors the pin is configured with」） | **差異**：X4 的 GPIO3 `Input::new(.., Pull::Up)` 仍在（`POWER_BTN` 常駐、`Board::init_input`），所以數位 pull-up 會被 hold 住；C3 的 IDF 也沒有 RTC IO 驅動，`gpio_deep_sleep_wakeup_prepare` 用數位 `gpio_pullup_en`＋hold，1.2 與 IDF 一致。若實機發現立即喚醒／無法喚醒，首先檢查此點 |
| 全 pad isolate | `isolate_digital_gpio()` 對所有未 hold 的 pad 做（含 GPIO0–2、4、5） | `isolate_pads_for_deep_sleep`（C3 metadata `needs_gpio_isolation = true`）**跳過 LP pad**（GPIO0–5 有自己供電） | **差異**：X4 的 GPIO0（電池 ADC）、GPIO1／GPIO2（按鍵 ADC）在 1.2 不再被 isolate；esp-hal 的註解稱 LP pad 不漏電。只影響睡眠電流（可能略差），不影響喚醒；未驗 |
| 沒有 wake source | `rtc.sleep` 照睡（無法喚醒） | `sleep_deep` panic | X4 的 `listen(FallingEdge)` 在 `init_input` 設定，`gpio_handler` 只 `clear_interrupt`（**不 unlisten**），所以 listening 一直在；正常路徑不會 panic。若有人 `unlisten` 該腳就會 panic（目前原始碼 grep 無 `unlisten`） |
| 進入後 | 1.0 的 `loop { spin_loop }` 備援 | `-> !` | 等價 |
| RTC FAST 保持供電 | `RtcSleepConfig::deep()` ＋ `set_rtc_fastmem_pd_en(false)` | 相同 | 等價（編譯通過；C3 的 `RtcSleepConfig` 欄位未變） |

**結論**：喚醒源、電位、hold、clk gate 等價；兩個差異（pull 不再明確寫入、LP pad 不 isolate）都已在上表標明，沒有發現必須修正的功能性風險，**沒有改動 X4 的 sleep 行為**，也沒有為此改 X4 程式。X4 睡眠／喚醒仍為未實機驗證（`T2` 起的既有風險）。

X4 唯一被動到的程式是 `kernel/src/drivers/ssd1677.rs` 的兩行（改成呼叫共用 `deep_sleep`）；等價證明見 §38。

### 37f. R20 喚醒後決策（`plan_boot`）

`plan_boot(cause, decision)`：有效 session → `Restore`，**不論 cause**；其他一律 `Normal`（保留 `NormalBootReason`）。理由：C61 的 session 在 SD 上，與 X4（RTC 記憶體、電源循環後歸零）不同，冷開機後仍存在；R20 要求「重新啟動且有效就恢復」。因此冷開機也會恢復上一次睡前位置（例如更換韌體後）；T12 須在 app 層驗證檔案仍存在，並決定恢復後是否 `clear_session`（避免壞位置造成開機迴圈）。cause 只用於 log。決策表（`r20_boot_plan_decision_table_all_causes_by_all_session_outcomes`）：4 種 cause（PowerOn／Key／Timer／Other）×（有效／無檔／CRC 壞／magic 壞／儲存不可用／SD 非 active）。

### 37g. 睡眠觸發方案建議（**未決，T12 決定；T11 沒有動鍵盤層**）

C61 沒有實體 Menu／Power 鍵（T7）。X4 的觸發是：Power 長按（`power held`）與 idle timeout（`scheduler.rs` 的 `sleep_with_session`）。建議（由 T12 擇一或併用）：
1. **idle timeout 自動睡眠**（建議為主）：沿用 X4 的 idle timeout 設定與流程，把 `sleep_with_session` 的 C61 版改為：app 層收集 `SessionState` → `sleep::enter_deep_sleep(..)`。不需要新按鍵。
2. **長按 GPIO2（WAKE／Back）**：與 X4 的 Power 長按對應；但 GPIO2 同時是 Back 與 wake 鍵，觸發時使用者手指一定還按著 → `WakeKey::arm` 會回 `AlreadyAsserted`。T12 必須等放開再呼叫序列，且喚醒時按住的那一下要被 T7 的 startup grace／latch 吃掉，否則一醒來就又睡。
3. 長按某個 ladder 鍵（如 ENTER 長按在 Home）：需要在 `Action` 加新語意，會動鍵盤層，不建議。
無論選哪個：app 層只呼叫 `board_c61::sleep::enter_deep_sleep`，**不要自己呼叫 `begin_shutdown`／`cut_peripheral_power`**，否則失去 save-first 保證；回傳（`SleepAbort`）代表沒有睡著，UI 要繼續運作。

### 37h. c61_boot 示範路徑（預設不睡眠）

`DEMO_IDLE_SLEEP_SECS = 0`（預設，不自動睡眠，不影響後續 bring-up）；改成 N > 0 重編後，N 秒無按鍵事件時序列執行一次（存 Home-only 的 `SessionState`、`wake_count = 1`）。以 `core::hint::black_box` 讀常數，避免常數為 0 時整段被 DCE（否則 `nm` 看不到序列）。序列只會跑一次；abort 時 log `sleep demo aborted`，之後 log 「already attempted」。沒有新增任何按鍵互動。

## 38. T11 實際指令與結果

| # | 指令 | 結果 |
|---|---|---|
| 1 | `cargo test-board-logic` | **294 passed; 0 failed**（T10：259；+35：`sleep` 27、`ssd1677` +6、`power` +1、`spi` +1）；0 warning |
| 2 | `CARGO_TARGET_DIR=target/t11-c61 cargo build-c61 --locked` | **成功**，0 warning。`size`：text 316504 / data 4072 / bss 174056（T10：309822 / 3992 / 173848；C61 text 欄含 `.stack`）。`readelf -S`：`.text` 0x2011a → 0x213b2（+4,760 B）、`.rodata` 0x8a40 → 0x9208（+1,992 B）、`.rwtext` 0x1db8 → 0x2b1c（+3,428 B：esp-hal 的睡眠進入程式碼 `#[ram]` 放在主 RAM，是 statics 增加的主因） |
| 3 | `C61_TARGET_DIR=target/t11-c61 bash scripts/report-c61-memory.sh` | exit 0，`RESULT: all memory budget checks ok`；`image: statics 127824 B (49.8% of 256624 B main RAM), stack 128800 B, headroom over 49152 B minimum = 79648 B`（T10：124104／132520／83368；statics +3,720 B，headroom −3,720 B） |
| 4 | `nm -C`／`strings` 佐證序列被連結 | `nm -C`：`<esp_hal::rtc_cntl::sleep::LowPower>::sleep_deep`（`4201c282`）、`<pulp_kernel::board_c61::sleep::C61SleepEntry as pulp_board_logic::sleep::SleepEntry>::enter`（`4202a39c`）、`esp_hal::gpio::wakeup::entry_hook`、`<WakeCause as Debug>::fmt`、`<AbortReason as Debug>::fmt`、`<WakeError as Debug>::fmt`、`<WakeLevel as Debug>::fmt`。`SleepSequence::enter_deep_sleep`、`Epd::enter_deep_sleep`、`WakeKey::arm` 被內嵌進 `main`，沒有獨立 symbol，改以 `strings` 佐證：`sleep: GPIO2 wake armed (low level, pull-up, low-power path)`、`sleep: epd parked (deep sleep mode 1)`、`sleep: sd flushed and closed`、`sleep: shared lines driven low`、`sleep: session saved before power-off`、`sleep: entering deep sleep, wake = GPIO`、`sleep: aborted, staying awake: `、`boot: wake cause `、`boot plan: RESTORE position`、`boot plan: normal boot` |
| 5 | `CARGO_TARGET_DIR=target/t11-x4 cargo build-x4 --locked` | **成功**，0 warning；`size`：text **1221824** / data **27624** / bss **348812**（與 T10 完全相同）。`objcopy -O binary --only-section` 對 T10 ELF：`.trap`（1936 B）`.rwtext`（6268 B）`.data`（27624 B）`.text`（308028 B）差異 0 個位元組；`.rodata`（438380 B）差 5 個位元組——都是 panic `Location` 的行號低位元組少 1（`kernel/src/drivers/ssd1677.rs` 兩行改成一行使其後行號 −1），不是邏輯 |
| 6 | `bash scripts/check-x4-input-trace.sh`／`check-x4-driver-trace.sh`／`check-x4-battery-equiv.sh` | `ok X4 input trace identical to pre-T7 (26032 lines, sha256 2c3738aa…)`／`ok X4 driver trace identical to pre-T6 (2122 lines, sha256 66a0447f…)`／`ok X4 battery conversions identical to pre-T9 (65536 inputs, sha256 272e9905…)`。driver trace 的 stimulus 已含兩次 `enter_deep_sleep()`（`scripts/x4-driver-trace/stim.rs:91-92`）與其後 re-init，所以 X4 改呼叫共用 `deep_sleep` 已被同一 pinned sha256 覆蓋，不需擴充 |
| 7 | `bash scripts/check-board-selection.sh` | 4 項 `ok` |
| 8 | `OFFLINE_CHECK_TARGET_ROOT=target/t11-check bash scripts/check-offline-boundary.sh` | exit 0，20 項 `ok`（C61 無 radio／net／upload symbol） |
| 9 | `X4_ELF=target/t11-x4/riscv32imc-unknown-none-elf/release/pulp-os C61_TARGET_DIR=target/t11-c61 bash scripts/check-c61-no-c3-raw-gpio.sh` | exit 0，7 項 `ok`＋1 項 `info`（同 T5：esp32c61 PAC TIMG1） |
| 10 | `cargo fmt --all -- --check` | 無差異 |

Cargo.lock 未變動（`--locked` 通過）。

### 新增 host 測試（代表）

- 順序／R19：`r19_full_sequence_trace_in_bsp_order`（完整 trace：Save(SdActive) → Wake(GPIO2/Low/Up) → Park → SdStop → Charge → Silence → Gpio27Low → Enter）、`r19_every_peripheral_step_precedes_the_gpio27_cut`、`r19_park_precedes_silence_precedes_cut_like_the_bsp`、`r19_sd_is_stopped_before_its_power_goes`、`r19_gpio27_is_never_driven_high_and_goes_low_exactly_once`、`r19_powered_off_proof_exists_only_after_the_cut`。
- R18：`r18_save_is_the_first_event_and_runs_with_sd_active_and_rail_high`、`r18_save_precedes_every_power_down_event`、`r18_save_is_refused_when_the_card_was_removed_and_sleep_still_happens`（拔卡：存檔被拒、無 SD I/O、仍睡眠）、`r18_save_failure_is_reported_and_the_device_still_sleeps`、`r18_a_real_store_failure_surfaces_through_the_sequence`。
- 失敗矩陣：`r19_each_single_step_failure_still_reaches_wake_armed_rail_off_sleep`（save／park／sd／charge／lines 各單獨失敗）、`r19_all_peripheral_steps_failing_together_still_sleeps`、`r19_failure_is_recorded_in_the_matching_report_field_only`、`r19_wake_config_failure_aborts_before_any_irreversible_step`、`r19_a_held_wake_key_aborts_cleanly_and_a_retry_after_release_sleeps`。
- wake 設定：`r19_wake_spec_is_gpio2_active_low_with_pull_up_as_in_the_bsp`、`r19_the_sequence_arms_exactly_the_bsp_spec_once_and_enters_with_it`、`r19_a_second_sequence_after_the_rail_is_off_is_refused_and_does_nothing`（重複呼叫不重 arm）、`r19_a_rail_that_cannot_shut_down_aborts_without_any_side_effect`（未配置就進 sleep 在型別層不可能：`WakeArmed` 無公開建構子；runtime 拒絕在 abort 測試）。
- EPD：`r19_epd_park_waits_for_idle_then_sends_deep_sleep_mode_1`、`r19_epd_park_never_cuts_in_on_a_running_update`、`r19_epd_park_with_a_stuck_busy_still_tries_deep_sleep_and_reports`、`r19_epd_park_bus_failure_is_reported`、`x4_deep_sleep_sequence_is_unchanged_golden`。
- R20：`r20_wake_cause_classification_matches_the_bsp_priority`、`r20_boot_plan_decision_table_all_causes_by_all_session_outcomes`、`r20_restore_plan_carries_the_exact_saved_position`、`r18_r20_save_sleep_reboot_restores_the_same_reading_position`（序列 → 斷電後 restore 被拒 → 新開機 power-cycle＋SD active → 還原同一位置）、`r20_a_failed_save_before_sleep_boots_normally_next_time`。

### 變異測試（暫時修改原始碼、跑 `cargo test-board-logic`、還原；還原後 294 passed）

| # | 變異 | 結果 |
|---|---|---|
| M1 | save 放到 GPIO27 cut 之後 | 10 項 FAILED（`r18_save_is_the_first_event_and_runs_with_sd_active_and_rail_high`、`r18_r20_save_sleep_reboot_*` 等） |
| M2 | wake 配置放到 GPIO27 cut 之後 | 6 項 FAILED（`r19_every_peripheral_step_precedes_the_gpio27_cut`、`r19_each_single_step_failure_*` 等） |
| M3／M3b／M3c | EPD park／SD shutdown／silence 失敗時停止序列（移除「失敗後繼續」） | 各 3／2／2 項 FAILED（`r19_each_single_step_failure_still_reaches_*`、`r19_all_peripheral_steps_failing_together_*`） |
| M4 | wake 極性翻成 High | 2 項 FAILED（`r19_full_sequence_trace_in_bsp_order`、`r19_wake_spec_is_gpio2_active_low_*`） |
| M4b | pull 改 Down | 2 項 FAILED（同上） |
| M5 | silence 移到 cut 之後 | 3 項 FAILED |
| M6 | EPD park 移到 cut 之後 | 3 項 FAILED |
| M7 | save 失敗即放棄睡眠 | 6 項 FAILED（`r18_save_failure_is_reported_and_the_device_still_sleeps` 等） |
| M8 | 移除 rail 狀態預檢 | 2 項 FAILED |
| M9 | wake 失敗被忽略仍睡眠 | 1 項 FAILED（`r19_wake_config_failure_aborts_before_any_irreversible_step`） |
| M10 | SD 停止移到 cut 之後 | 3 項 FAILED |
| M11 | wake cause 優先序 timer 勝過 key | 1 項 FAILED |
| M12 | 冷開機時忽略有效 session | 1 項 FAILED（決策表） |
| M13 | 略過 charge restore | 4 項 FAILED |
| M14 | wake 重複 arm 兩次 | 4 項 FAILED |
| P1 | EPD park 略過 BUSY 等待 | 3 項 FAILED |
| P2 | deep sleep 資料改 0x03（mode 2） | 2 項 FAILED（含 X4 golden） |
| P3 | park 吞掉 BUSY 超時 | 1 項 FAILED |
| P4 | BUSY 失敗後不送 deep sleep | 1 項 FAILED |

全部 21 個變異（M 系列 17、P 系列 4）皆被擊殺，沒有存活。

## 39. T11 尚未驗證項目

- **硬體全未驗**：
  - 實際 deep sleep 電流（含 EPD mode 1 ~3 µA、SD／MIC 斷電後、GPIO27 低後的漏電）。
  - GPIO2 是否真能喚醒（EXT1 低電位、LP pad 上拉被 hold、BSP 的 IDF 6 `..._on_hp_periph_powerdown` 與 esp-hal EXT1 是否等價）；wake 時按鍵去抖；喚醒當下手指仍按著的行為。
  - **GPIO27 在 deep sleep 中的 pad 狀態**（BSP 與本移植都沒有 hold／isolate；若浮接或回到預設，SD／MIC／EPD 可能被重新供電而吃電）、GPIO10（充電啟用）在睡眠中的電位（非 LP pad，沒有 hold：睡眠期間是否仍允許充電取決於板上外部電阻）、EPD DC／CS 與 SPI 腳在斷電後是否反灌電（`silence_lines` 只做 SCK idle low＋0x00＋CS 低，BSP 的 `gpio_set_level` 對 SPI 路由腳本身效果存疑）。
  - EPD：BSP 的 moui `sleep` 原始碼本機沒有，park 序列取自 X4（0x10 mode 1）；影像是否保留、有無殘影、喚醒後（GPIO27 power-up 即 EPD 硬體 reset）是否正常 init。
  - 喚醒後的 SD 重新初始化（冷啟動路徑已存在：power-cycle → SD init → `restore_session`），`flush_and_close` 後再斷電的資料完整性。
  - `sleep_deep` 實際不 panic、LPWR 被 `peripherals.LPWR` 正常取得（編譯通過）。
- 設計取捨需實機／T12 確認：wake 配置提前到斷電前（偏離 BSP，37b）；冷開機也恢復位置（37f）；`AlreadyAsserted` 預檢要求睡眠觸發等按鍵放開（37g）。
- X4：只重審了 esp-hal 1.0 → 1.2 的原始碼（37e），兩個差異（pull 不再明確寫入、LP pad 不 isolate）未在實機驗證；X4 本次只動了 EPD deep-sleep 命令的呼叫點（程式碼 `.text` 位元組相同）。
- 尚未整合：app 層的睡眠觸發與 `collect_session`／`apply_session`（T12）；睡眠畫面（X4 會先畫 `(sleep)`）、`bm_cache` flush（X4 睡前 flush 書籤）都沒有在 C61 序列中，T12 需要在呼叫序列前自行完成（序列會在步驟 1 前不做任何 SD 以外的 flush）。
- `c61_boot` 睡眠示範預設停用，沒有在任何環境實際執行；`black_box` 只保證連結，不保證執行正確。

# T12：完整離線 main／tasks／app lifecycle，C61 與 X4 離線 release link（R1, R2, R3, R21）

日期：2026-10-04。**沒有任何硬體驗收**：本節所有「行為」只證明可編譯、可連結、邏輯部分有 host 測試。接手說明：前一個 T12 agent 留下的半成品（`board_c61/{api,hw}.rs`、`board-logic/src/lifecycle.rs`、`kernel/{app,mod,tasks}.rs`、`sleep.rs` 的 `&mut` 實作）已審查並沿用（`lifecycle.rs` 的 22 項測試在審查時全過；`api.rs` 的 `CX_FRONT` 改成與 X4 widget 同名的常數；`mod scheduler_c61;` 指向不存在的檔案，已補上實作）。

## 40. 設計

### 40a. C61 入口結構
- **新增 `src/bin/main_c61.rs`（bin `pulp-os-c61`）= C61 完整離線韌體**；`src/bin/c61_boot.rs`（bin `pulp-os-c61-boot`）原樣保留為最小 bring-up 映像（T14 用）；`src/bin/main.rs`（X4）只改了 `DecodedImage` 的建構（見 40f）。
- **為什麼新 bin 而不是讓 `main.rs` 以 cfg 分流**：兩塊板的 bring-up 完全不同（X4：`Board::init`、`InputDriver`、RTC session；C61：`take_c61_pins!`、GPIO27 power-cycle、共用 ADC1、SPI 仲裁、PSRAM、SD session），在一個檔案裡以 cfg 交錯會讓 X4 的 `main.rs` 變得難讀且每次動都有 X4 回歸風險；分開後 X4 的 `main.rs` 幾乎不動（X4 RAM section 與 T11 逐項相同，見 41）。代價：兩個 main 的 static／AppManager 組裝約 40 行重複。
- 啟動順序（`main_c61.rs`）：`esp_hal::init` → `paint_stack` → wake cause → internal heap（`INTERNAL_HEAP_*`）→ `memory::init(PSRAM)`（失敗降級）→ `take_c61_pins!` + GPIO27 power-cycle（R6，早於任何 SD）→ `esp_rtos::start` → ADC1（keys + battery；失敗 `park`，沒有鍵就無法操作）→ USB detect → SPI2/DMA（失敗 `park`）→ `sd::bring_up`（缺卡／壞卡走 `NoCard` 路徑不 panic）→ `speed_up` → `ensure_pulp_dir_async` → `epd::new` → 首次電池量測 → `C61Hw::new` → `Kernel::new` → `AppManager::new` → `Kernel::boot(app_mgr, wake)` → 註冊 image decoder → spawn 4 個 task（input／housekeeping／idle_timeout／worker）→ `Kernel::run`。
- **沒有 boot console**：X4 會先畫一張 console 再畫 Home；C61 每次 refresh 都是 full refresh（閃爍、數秒），多畫一張只增加開機時間與閃爍。進度只在 log。
- alias：`cargo build-c61`／`check-c61` 不再指定 `--bin`，一次建兩個映像；`run-c61` 跑完整韌體，新增 `run-c61-boot` 跑 bring-up 映像（`.cargo/config.toml`、`Cargo.toml` `[[bin]] pulp-os-c61`、`README.txt` 已更新）。
- 腳本：`report-c61-memory.sh` 預設改為完整韌體 ELF（`C61_ELF=…/pulp-os-c61-boot` 可指定舊映像）；`check-offline-boundary.sh` 同時探測 C61 兩個映像；`check-c61-no-c3-raw-gpio.sh` 探測兩個映像，source 掃描範圍加入 `main_c61.rs`、`kernel/src/{kernel,ui,util}`、`drivers/strip.rs`、`src/{apps,ui}`。

### 40b. cfg 放寬清單（C61 現在編入的東西）
| 檔案 | 改動 |
|---|---|
| `src/lib.rs` | `board`／`drivers`／`kernel`、`apps`、`fonts`、`ui` 不再是 `board-x4` only（`wifi`+C61 的 `compile_error!` 保留） |
| `kernel/src/lib.rs` | C61 的 `crate::board` = `board_c61::api`（`pub use board_c61::api as board`），apps／kernel 以同一路徑編譯 |
| `kernel/src/board_c61/api.rs`（新） | `SCREEN_W/H`、`Epd`、`StripBuffer`、`button::Button`（= `keys::Key`）、`action::{Action, ActionEvent, ButtonMapper}`（swap 規則來自 `pulp_board_logic::keys`，T7 已測）、`layout::{CX_*, CY_*}`、`lifecycle` 重新匯出 |
| `kernel/src/board_c61/hw.rs`（新） | `C61Hw`：GPIO27 `PeripheralPower`、`Option<C61Battery>`、`CardHw`（GPIO28、去抖、health、SD SPI handle、`SpiControl`）、`SleepParts`（lines／wake／entry）、`DisplayHealth`、`wake_count`、兩個 `Periodic` |
| `kernel/src/kernel/mod.rs` | `Kernel` 在 C61 多一個 `hw` 欄位、`Kernel::new` 多一個 `hw` 參數；`rtc_session` 只在 X4；`bigbuf`（兩板）；`scheduler_c61`（C61） |
| `kernel/src/kernel/app.rs` | `SessionData` = X4 `RtcSession`／C61 `SessionState`；`AppLayer::{collect,apply}_session` 改用 `SessionData` |
| `kernel/src/kernel/scheduler.rs` | X4-only：`show_boot_console`、`boot`、partial `render`、`busy_wait_with_background`、`sleep_with_session`、`enter_sleep`、`sd_card_sleep`、Power 長按；共用：`run`／`handle_input`／`poll_housekeeping`；C61 加：`poll_card`、`poll_battery`、顯示失敗後下一次輸入要求重畫 |
| `kernel/src/kernel/scheduler_c61.rs`（新） | C61 的 `boot`／`render`／`sleep_with_session`／`poll_card`／`poll_battery`（40c–40g） |
| `kernel/src/kernel/tasks.rs` | C61 `input_task(C61Input, UsbPort)`：與 X4 相同的 10 ms／50 ms 自適應輪詢（`INPUT_IDLE_TICKS`）、同樣的 `INPUT_EVENTS`／`IDLE_RESET`／`RESET_HOLD` 訊號；USB 每圈取樣（3 次去抖）寫入 `USB_PLUGGED`（無 UI）；電池**不在**此讀 |
| `kernel/src/kernel/work_queue.rs` | `DecodedImage.data: Vec<u8>` → `BigBuf`（兩板） |
| `kernel/src/board_c61/spi.rs` | `SpiControl::slow_down()`（執行中插卡要回 400 kHz 再 init；T5 的 `c61_boot` 沒做這件事） |
| `src/apps/manager.rs` | `collect/apply_session` 改接 `SessionData`（只差 `reader_is_epub` 型別，由 `session_fields` 處理；settings 快取欄位只存在 X4 的 `RtcSession`）；`opens_quick_menu`（40d） |
| `src/apps/widgets/button_feedback.rs` | 以 `keys::{BACK, CONFIRM, LEFT, RIGHT, SIDE_UP, SIDE_DOWN}` 別名吃兩板不同的 `Button` 名稱（X4：Confirm／VolUp／VolDown，C61：Enter／Prev／Next） |
| `src/apps/reader/mod.rs` | `showing_toc()`；`prefetch`／`ch_cache` 型別改 `BigBuf` |

### 40c. 睡眠觸發與序列（預設決定 1，待使用者確認）
- **只用 idle timeout**：重用 X4 的 `idle_timeout_task`／`IDLE_SLEEP_DUE`／設定的 `sleep_timeout`（分鐘），不新增鍵盤層互動、不新增 `Action`。`scheduler.rs` 的 `run` 原本就在 `poll_housekeeping` 為真時呼叫 `sleep_with_session`；C61 沒有 Power 長按，所以只有 idle 一條路。
- `sleep_with_session`（C61）：flush `bm_cache` → `collect_session`（`wake_count = 上次 restore 的值 + 1`）→ 畫 `(sleep)`（與 X4 同文字同位置，best effort，失敗不阻擋睡眠）→ **`board_c61::sleep::enter_deep_sleep`**（T11 序列：save → arm wake → begin_shutdown → park EPD → flush SD → charge restore → silence lines → GPIO27 低 → sleep）。kernel 沒有自行呼叫 `begin_shutdown`／`cut_peripheral_power`（`grep` 可驗）。
- 各部件以 `&mut` 持有（`impl SleepEntry for &mut C61SleepEntry`、`WakeConfig for &mut WakeKey`、`LineSilencer for &mut C61Lines`），所以序列中止後裝置仍完整可用。
- **中止（含喚醒鍵仍按住 `AlreadyAsserted`）**：log `sleep: not entered (…)`、`tasks::IDLE_RESET.signal(())`（idle 計時重來，否則 `IDLE_SLEEP_DUE` 只會響一次）、`request_full_redraw`（面板此時顯示 `(sleep)`，要畫回來）。下一次 idle timeout 會再試。
- **取捨**：`(sleep)` 先畫、序列後跑，所以中止時面板已被覆蓋成 `(sleep)`，要多一次 full refresh 才能還原；若要避免，需要在畫面前預檢 GPIO2（`WakeKey` 目前延遲建立 `Input`，無法預讀）。idle timeout 時手指按著 WAKE 的機率低，故不處理。
- 沒有「手動睡眠」鍵：使用者無法即時睡眠，只能等 idle timeout（設定裡 `sleep_timeout`＝0 代表永不睡）。**這是已知功能缺口**；候選方案（WAKE 長按睡眠）會與 `Back` 長按＝回 Home 衝突，故未做。

### 40d. `Action::Menu` audit 與 C61 對應（預設決定 2，待使用者確認）
- audit：`Action::Menu` 只有一個消費點（`AppManager::dispatch_event`：`Press(Menu)` 開 quick menu）；quick menu 內 `Menu|Back` 關閉。X4 的來源只有 Power 短按。
- quick menu 內容：Reader＝Book Font（循環）、Prev Ch／Next Ch、Contents（EPUB 有 TOC 時）；Files＝Delete File／Delete Cache；Home／Settings＝無。另有 Refresh Screen 與 Go Home（`handle_quick_menu` 內建）。
- 其他路徑可達：Book Font＝Settings；Prev/Next Chapter＝NextJump／PrevJump（LEFT／RIGHT）；Refresh＝C61 每次都是 full refresh；Go Home＝Back 長按。**沒有其他路徑的只有「Contents」（EPUB 目錄）**，以及 Files 的兩個刪除功能。
- 決定（最保守者）：**只在 apps 層**，`ENTER`（`Action::Select`）**長按**於 Reader 且不在 TOC、quick menu 未開時開啟 quick menu（`pulp_board_logic::lifecycle::opens_quick_menu`，4 項測試）。理由：Reader 頁面上 `Select` 完全沒有處理（`LongPress(Select)` 在 reader 是被註解保留給「書籤切換 Phase 6」），因此不會與既有功能衝突；TOC 內 ENTER 是選擇所以排除。選單開啟後 `request_hold_reset` 已由 scheduler 觸發，不會有 Repeat(Select) 直接選到第一項。
- **不映射 Files 的刪除**（破壞性，且不該用長按隱性觸發）；因此 C61 沒有「從裝置刪書／刪快取」功能。**已知限制**。
- trade-off：1) 佔用 `LongPress(Select)`，日後做「書籤切換」要換鍵；2) 使用者要知道「長按 ENTER = 選單」，畫面沒有提示（button feedback 只畫短按標籤）；3) 沒有新 board 鍵語意，`pulp_board_logic::keys` 與 `Action` 一行未改。

### 40e. session restore 與 `clear_session`（預設決定 3，待使用者確認）
- `Kernel::boot`（C61）：`restore_session`（T10）→ `plan_boot(cause, decision)`（T11，有效 session 不論 cause 都 Restore）→ `check_restorable`（`board-logic::lifecycle`：堆疊含 Upload 而本韌體無 → 拒絕；堆疊含 Reader 而 `file_size(name)` 失敗 → 拒絕）→ `apply_session` → `post_restore(applied)`。
- **成功套用：不刪除**（下次睡眠會寫更新的紀錄；保留可在一般斷電後仍能還原）。**套用失敗（檔案不存在、本韌體不能顯示、`apply_session` 回 false）：`clear_session` 並正常開機到 Home**，避免壞資料每次開機都重試造成開機迴圈。章節／頁碼越界由 reader 自己夾住（`reader/mod.rs` 的 `spine_len` 夾制，既有行為）。
- 刪除失敗只 log（不阻擋開機）。壞檔（CRC／欄位不合法）在 T10 已是 `NormalBoot(Corrupt)`，不刪除，下次 save 覆寫。
- 副作用（同 T10 提醒）：C61 session 在 SD 上，冷開機（更換韌體、斷電）也會還原上次睡前位置。

### 40f. PSRAM 搬遷表（R14；`BigBuf`）
新增 `kernel/src/kernel/bigbuf.rs`：`BigBuf`（`Deref<[u8]>`）＋ `BufClass{ChapterText, ImageData, ZipToc}`（**沒有** DMA／ISR／runtime 變體，型別層不可能從 PSRAM 取得，R15）。X4：包 `Vec<u8>`（`try_reserve_exact`＋`resize(n,0)`，與原碼逐行等價；X4 size／RAM section 不變，見 41）。C61：`memory::alloc(class, len, 16)`（T8 的自動放置：PSRAM `Ready` 用 PSRAM 並受類別上限；降級／未 init 用 internal，降級上限＝X4 規模，所以 PSRAM 壞了仍能讀書），被預算拒絕回 `Err` → 呼叫端走既有「章節太大（`false`）／圖片略過」路徑，不 panic。

| T8 盤點 # | allocation | T12 處置 |
|---|---|---|
| 17 | `ch_cache`（章節文字快取，最大 96 KiB） | **遷移**，`ChapterText`（`epubs.rs`、`images.rs` 的釋放點、`reader/mod.rs`） |
| 18 | prefetch（8 KiB） | **遷移**，`ChapterText`（`paging.rs`、`images.rs` 的 `ensure_len`；配置失敗＝本頁不預取） |
| 24 | central directory buffer | **遷移**，`ZipToc`（`epubs.rs` `epub_init_zip`、`files.rs` title scan） |
| 25 | 解碼後頁面圖片 `DecodedImage.data`（≤48,000 B）與 cached image 載入 | **遷移**，`ImageData`（`images.rs` `from_smol_image`／`load_cached_image`、兩個 main 的 decoder 註冊；C61 為複製進預算區塊後釋放 smol-epub 的 `Vec`，降級模式下暫時雙倍） |
| 19, 22, 26–29 | smol-epub 內部 inflate／PNG／JPEG 暫存（32 KiB 級）與 name pool | **未遷移**：配置在外部 crate `../smol-epub` 內，本 change 不改該 crate（`smol` revision 固定於 T1）。仍在 internal heap（與 X4 相同量級，且是逐 byte 隨機存取的熱路徑，原本 T8 也建議先量測再決定） |
| 21 | ZIP entry index（in READER static） | 未遷移（static，已含在 `READER` 18,972 B） |
| 23 | EPUB TOC（`Box<EpubToc>` 13 KB） | 未遷移：型別放置需要 `unsafe` 就地建構，且在 internal 可接受 |
| 20, 15, 16 | 頁偏移表、頁文字緩衝、DirCache | Keep（T8 已歸 Keep 或低於門檻） |
| 1–3, 10–12 | SPI DMA、descriptor、stack 暫存 | Required internal，未動 |

- `alloc*` 目前零初始化；章節快取是「配置 n 位元組後立刻整段從 SD 讀入」，與 X4 的 `resize(n,0)` 同成本，**不新增 uninit 版本**（預設決定 4 要求「需要時才新增」；PSRAM 零填速度未實測，若實機顯示章節載入明顯變慢再加 `alloc_external_uninit`，屬 T13/T14 的量測項）。
- 熱路徑（`ch_cache` 逐頁複製、prefetch）現在走 40 MHz Quad PSRAM＋cache；速度**未驗**。

### 40g. 顯示、輸入、電池、card detect 接線
- **顯示**：C61 一律 full refresh（`Redraw::Partial` 升級為 full，`ghost_clear_every` 無作用）；`Epd::full_refresh` 阻塞（最壞約 5 s BUSY 上限），期間 input／housekeeping task 不會跑，**完全在一次 refresh 內發生的短按會遺失**（X4 的 partial 約 400 ms 且 refresh 中會收輸入）。每次 refresh 後 `yield_now` 讓 task 補跑。後續建議：把 BUSY 等待改成 async（需動 T6 的 `wait_busy_bounded` 介面），列為 T13/T14 以後的改進。
- **顯示失敗**：`board-logic::lifecycle::run_refresh`（5 項 host 測試）＝失敗 → 軟 reset 重新 init → 重試一次；第二次失敗或 init 失敗 → 放棄這一幀並標 stale，**下一次輸入事件**強制 `request_full_redraw`（不用計時器，避免面板死掉時背靠背 5 s 超時卡住主迴圈）。面板壞掉時無法顯示自己的錯誤，所以只有 log（`ErrorKind` 沒有 display 變體，沒新增以免動 X4）。
- **輸入**：`swap_buttons` 來源＝設定（`sync_button_config` → `ButtonMapper::set_swap`，與 X4 同）；輪詢頻率 10 ms／50 ms 自適應在 C61 `input_task`。
- **電池**：`BatteryMonitor::measure` 阻塞約 30 ms＋16 次轉換；**由主迴圈的 `poll_housekeeping_inner` 每 30 s 呼叫**（`Periodic`，遲到只觸發一次，不補發），不放在 task：睡眠序列需要同一個 monitor 的 `&mut`（charge restore），放 task 要跨阻塞持鎖。失敗保留上一個值（不寫 0 mV）。開機先量一次。
- **card detect**：`poll_card`（async，因為重新 mount 是 async），每 `CD_SAMPLE_INTERVAL_MS`；拔卡 → `SdStorage::empty()`、`card_removed()`、`dir_cache.invalidate()`、full redraw；插卡 → 400 kHz → `bring_up` → 10 MHz → `ensure_pulp_dir_async` → invalidate → full redraw。
- **`StorageStatus` UI（T5 留下）**：**部分完成**。狀態文字（`SD: no card`…）進 log；螢幕上看得到的是既有 `NoCard` 錯誤路徑（Files 清單錯誤、Reader 錯誤畫面）。沒有新的狀態列元件；Files 的錯誤會留到重新進入 app 才清除（插卡後 `dir_cache` 已失效、整頁重畫，但 Files 自己的 `error` 欄位不會被通知）。
- USB：`USB_PLUGGED` 靜態有值，**沒有 UI**（偏離 X4 沒有 USB 概念，本版不新增）。

## 41. T12 實際指令與結果

(1) host 測試
```
$ cargo test-board-logic
test result: ok. 321 passed; 0 failed        # T11 後 294；+27 = lifecycle 27 項（審查接手時 22，新增 5 項 run_refresh）
```
新增測試代表：`r20_reader_session_needs_the_book_on_this_card`、`r20_upload_on_the_stack_is_rejected_by_offline_firmware`、`r20_session_is_cleared_when_applying_fails_so_it_cannot_loop`、`r20_session_is_kept_after_a_successful_apply`、`menu_enter_long_press_in_the_reader_opens_the_menu`、`menu_does_not_reopen_while_open_or_in_the_toc`、`r11_refresh_stuck_panel_gives_up_after_exactly_two_attempts`、`r11_failed_reinit_stops_without_a_second_refresh`、`periodic_late_caller_fires_once_not_a_burst`、`display_stale_frame_is_redrawn_once_on_the_next_input`。
變異測試（`board-logic/src/lifecycle.rs`，逐一改壞後跑 `cargo test-board-logic`，6 個全被擊殺、還原後 321 全過）：L1 `post_restore` 反相（2 項 FAILED）、L2 重試不設 attempt（2 項）、L3 init 失敗仍重試（1 項）、L4 TOC 內也開選單（1 項）、L5 不檢查書是否存在（2 項）、L6 Periodic 補發 burst（1 項）。

(2) C61 完整離線 release link
```
$ CARGO_TARGET_DIR=target/t12-c61 cargo build-c61 --locked     # 建出 pulp-os-c61 與 pulp-os-c61-boot
    Finished `release` profile [optimized + debuginfo] target(s)
$ file target/t12-c61/riscv32imac-unknown-none-elf/release/pulp-os-c61
ELF 32-bit LSB executable, UCB RISC-V, RVC, soft-float ABI, version 1, statically linked, with debug_info, not stripped
$ readelf -h … | grep -E "Class|Machine"      Class: ELF32   Machine: RISC-V
$ size pulp-os-c61 pulp-os-c61-boot
   text	   data	    bss	    dec	    hex	filename
 882410	  28872	 199520	1110802	 10f312	pulp-os-c61
 314580	   4072	 174056	 492708	  784a4	pulp-os-c61-boot
```
連結證據（`nm -C`／`strings`，LTO 後多數函式被 inline，所以用倖存符號與 log 字串）：`HomeApp`／`ReaderApp`（41 個符號）／`FilesApp`／`SettingsApp`／`BookmarkCache`／`AppManager` 符號皆在；字串 `Continue`／`Files`／`Bookmarks`／`Settings`／`Contents`／`Book Font`；PSRAM 路徑：`pulp_kernel::board_c61::memory::PSRAM_HEAP`、`MemoryBudget` 符號、`psram: budget refused status`；睡眠序列：`<C61SleepEntry>::sleep`、`<LowPower>::sleep_deep`、字串 `sleep: GPIO2 wake armed`／`sleep: epd parked`／`sleep: session saved`／`sleep: entering deep sleep`；session：`session::read_slot::<SdSessionStore>`、字串 `boot: session restore`／`boot: saved session not applicable`；lifecycle：`display: giving up on this frame`、`sd: card inserted`、`battery: sample failed`、`chapter cache: OOM`。
```
$ cargo tree --locked -e normal -i esp-radio --target riscv32imac-unknown-none-elf --features board-onepage-c61
error: package ID specification `esp-radio` did not match any packages     # 不含 radio
```
(3) 記憶體實測（`C61_TARGET_DIR=target/t12-c61 bash scripts/report-c61-memory.sh`，完整韌體）：`RESULT: all memory budget checks ok`
```
statics (.trap .rwtext .data .bss .noinit)   178080 B  (69.3% of RAM)   # T8 估算 169.6 KB；實測多 8.5 KB（task arena POOL 12,192 B、SpiControl／hw／scheduler 等）
.stack (= RAM - statics)                      78544 B
STACK_MIN_BYTES                               49152 B
stack headroom over STACK_MIN_BYTES           29392 B
statics budget (STATIC_RAM_MAX_BYTES)        207472 B     # 未放寬；剩 29392 B
dram2 .dram2_uninit (heap)                    64000 B
```
最大 static：main heap 98,304、reclaimed heap 64,000、`READER` 18,972、task `POOL` 12,192、`DIR_CACHE` 10,764、DMA BUFFER 4,096×2、`STRIP` 4,014。stack 高水位（實際用量）只有硬體能量，**未驗**。

(4) X4 離線 release link（R3）
```
$ CARGO_TARGET_DIR=target/t12-x4 cargo build-x4 --locked
   text	   data	    bss
1221660	  27624	 348812         # T11：1221824 / 27624 / 348812
```
data／bss 與 T11 完全相同；`scripts/report-c61-memory.sh --x4-diff T11 T12` 顯示所有 RAM section（`.trap .rwtext .data .bss .stack .dram2_uninit .rtc_fast.persistent`）差 0；text −164 B（`RtcSession` 欄位改走 `session_fields` 輔助函式與 `BigBuf` 包裝後 LTO 的小幅差異，無行為變更）。
```
$ bash scripts/check-x4-input-trace.sh   ok  (26032 lines, sha256 2c3738aa…)
$ bash scripts/check-x4-driver-trace.sh  ok  (2122 lines, sha256 66a0447f…)
$ bash scripts/check-x4-battery-equiv.sh ok  (65536 inputs, sha256 272e9905…)
$ CARGO_TARGET_DIR=target/t12-x4w cargo build-x4-wifi --locked
   text	   data	    bss
1787968	  33096	 309484        # 含 wifi 仍可連結
```
(5) 邊界與格式
```
$ bash scripts/check-board-selection.sh     ok no board / ok both boards / ok x4 on imac target / ok c61 on imc target
$ OFFLINE_CHECK_TARGET_ROOT=target/t12-check bash scripts/check-offline-boundary.sh   全 ok（X4 離線、X4+wifi 正向對照、C61 完整韌體與 boot 映像皆無 radio／net 符號、無 upload 字串、無 Upload 選單）
$ X4_ELF=target/t12-x4/…/pulp-os C61_TARGET_DIR=target/t12-c61 bash scripts/check-c61-no-c3-raw-gpio.sh   全 ok（兩個 C61 映像；X4 對照組如預期命中）
$ cargo fmt --all -- --check               無差異
```

## 42. T12 尚未驗證項目與待使用者確認的決定

- **硬體全未驗**：整個完整韌體從未在 OnePage 上執行過——boot、PSRAM、SD、ADC、EPD、USB、sleep／wake、session 還原、reader 的分頁與導航行為、頁面載入速度（PSRAM 熱路徑）、電流。英文 TXT／EPUB 的回歸驗收矩陣（R21）屬 T13，本 task 只證明路徑可編譯連結且分頁／導航／設定／書籤的程式碼（`src/apps/**`）除型別換成 `BigBuf`、session 欄位、quick menu 開啟條件外未改。
- 任何 C61 build 都**沒有被 host 執行**；`scheduler_c61.rs`／`main_c61.rs` 的流程只有編譯檢查，決策函式（`check_restorable`／`post_restore`／`run_refresh`／`opens_quick_menu`／`Periodic`）才有 host 測試。
- 待使用者確認的決定（本 session 非互動，採預設）：
  1. **睡眠只靠 idle timeout**，沒有手動睡眠鍵；`(sleep)` 畫面先畫，序列中止時要多一次 full refresh（40c）。
  2. **Reader 內長按 ENTER 開 quick menu**（只在 apps 層）；Files 的刪除功能在 C61 不可用；日後要用 `LongPress(Select)` 做書籤切換需換鍵（40d）。
  3. **restore 成功不刪 session、失敗才刪並正常開機**（40e）。
  4. 熱路徑配置**不加 uninit 版本**（40f）；smol-epub 內部暫存不遷移（需要改外部 crate）。
  5. 無 boot console；C61 一律 full refresh，`Partial` 升級為 full。
- 已知限制：full refresh 阻塞期間的短按遺失；Files 的 `error` 欄位在插卡後不自動清除；`StorageStatus` 文字只進 log；`USB_PLUGGED` 無 UI；按鍵標籤（button feedback）的 bezel 位置沿用 X4 且未驗證；喚醒時手指仍按著 WAKE 由 T7 的 grace／latch 吞掉（grace 從 `keys::new` 起算 2.5 s，若開機比 2.5 s 久，仍按著的鍵在首次取樣被 latch 到放開為止，沒有假事件，但此行為未在實機驗）。
- 未做（範圍外）：partial refresh、CJK、Wi-Fi／BLE、T13 回歸矩陣、T14 bring-up 文件。
