pulp-os
=======

Bare-metal e-reader firmware for the XTEink X4 (ESP32-C3, SSD1677 e-paper).
Written in Rust. No OS, no std, no framebuffer.

HARDWARE
--------
  MCU      ESP32-C3, single-core RISC-V RV32IMC, up to 160 MHz
  RAM      400 KB DRAM; ~250 KB available to the app
  Display  800×480 SSD1677 monochrome e-paper (GDEQ0426T82)
  Storage  MicroSD via SPI
  Input    4 front buttons (dual ADC ladder) + 1 power button (GPIO IRQ)
  Battery  Li-ion, read via ADC with 100K/100K divider

BUILDING
--------
  Requires stable Rust and the riscv32imc-unknown-none-elf target.
  rust-toolchain.toml handles both automatically.

  cargo build --release

  Flash with espflash or cargo-espflash:

  cargo espflash flash --release --monitor

FEATURES
--------
  - Plain text (.txt) reader with lazy page indexing and read-ahead prefetch
  - EPUB reader (ZIP + OPF + HTML strip + proportional font rendering)
  - Bookmarks: position saved to BOOKMARKS on SD card on exit; restored on open
  - Proportional bitmap fonts rasterised at build time from Bookerly TTFs
  - Cooperative scheduler (High / Normal / Low priority job queues)
  - Partial e-paper refresh for fast page turns; periodic full refresh for ghosting
  - Persistent settings written to settings.bin on SD card

SD CARD LAYOUT
--------------
  /                 root — place .txt and .epub files here
  /settings.bin     saved preferences (8 bytes, little-endian struct)
  /BOOKMARKS        saved reading positions (32 slots × 12 bytes)

CONTROLS
--------
  Prev / Next       scroll selection or turn page
  PrevJump / NextJump   page jump (files: full page; reader: ±10 pages or chapter)
  Select            open highlighted item
  Back              go back; long-press returns to home
  Menu (Power)      open quick-action overlay

SOURCE LAYOUT
-------------
  src/bin/main.rs       entry point, main loop, job dispatch
  src/kernel/           scheduler and wake/ISR signalling
  src/board/            SPI init, GPIO, display, SD card
  src/drivers/          input debounce, storage helpers, battery ADC
  src/fonts/            bitmap font structs and build-time glyph tables
  src/formats/          ZIP, EPUB/OPF, XML scanner, HTML stripper
  src/ui/               widget toolkit (Region, Button, Label, StatusBar)
  src/apps/             Home, Files, Reader, Settings
  build.rs              host-side TTF rasterisation via fontdue

DESIGN NOTES
------------
  Jobs are signals, not data carriers. State lives in the subsystem that
  handles the job; the scheduler carries only the job variant.

  No dyn dispatch in the app framework. The with_app! macro statically
  dispatches to each concrete app struct; no vtable, no heap indirection.

  Apps never touch hardware directly. All I/O goes through the Services
  handle passed into on_work(). The kernel and apps are decoupled at a
  clean syscall boundary.

  Partial refresh owns the render path. Apps call mark_dirty(region)
  rather than requesting full redraws. A full GC refresh is forced
  periodically (configurable) and on screen transitions.

  Stack allocation by default. The four app structs live on the stack in
  main(). Heap is used only where size is unknown at compile time
  (EPUB chapter text).
