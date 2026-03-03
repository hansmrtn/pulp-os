pulp-os -- e-reader firmware for the XTEink X4

bare-metal e-reader operating system for the XTEink X4 board
(ESP32-C3 + SSD1677 e-paper). written in Rust. no std, no
framebuffer. async runtime via Embassy on esp-rtos.

hardware
    mcu         ESP32-C3, single-core RISC-V RV32IMC, 160 MHz
    ram         400 KB DRAM; ~172 KB heap (main + reclaimed), rest stack + radio
    display     800x480 SSD1677 mono e-paper, DMA-backed SPI, portrait
    storage     microSD over shared SPI bus (400 kHz probe, 20 MHz run)
    input       2 ADC ladders (GPIO1, GPIO2) + power button (GPIO3 IRQ)
    battery     li-ion via ADC, 100K/100K divider on GPIO0

    pin map:
      GPIO0   battery ADC          GPIO6   EPD BUSY
      GPIO1   button row 1 ADC     GPIO7   SPI MISO
      GPIO2   button row 2 ADC     GPIO8   SPI SCK
      GPIO3   power button         GPIO10  SPI MOSI
      GPIO4   EPD DC               GPIO12  SD CS (raw register GPIO)
      GPIO5   EPD RST              GPIO21  EPD CS

building
    requires stable Rust >= 1.88 and the riscv32imc-unknown-none-elf
    target. rust-toolchain.toml handles both automatically.

        cargo build --release
        espflash flash --monitor --chip esp32c3 target/...

        or

        cargo run --release

    local path dependencies (sibling dirs):
      embedded-sdmmc    async FAT filesystem over SD/SPI (local fork)
      smol-epub         no_std epub/zip/html/image processing

features
    txt reader      lazy page-indexed, read-ahead prefetch,
                    proportional or monospace wrapping
    epub reader     ZIP/OPF/HTML-strip, chapter cache on SD,
                    proportional fonts with bold/italic/heading,
                    inline PNG/JPEG (1-bit Floyd-Steinberg dithered),
                    TOC browser (NCX or inline), chapter navigation
    file browser    paginated SD listing, background EPUB title
                    scanner (resolves titles from OPF metadata)
    bookmarks       16-slot LRU in RAM, flushed to SD every 30 s;
                    home screen bookmarks browser sorted by recency
    wifi upload     HTTP file upload + mDNS (pulp.local);
                    drag-and-drop web UI with delete support
    fonts           regular/bold/italic TTFs rasterised at build time
                    via fontdue; five sizes (xsmall/small/medium/large/xlarge)
    display         partial DU refresh (~400 ms page turn),
                    periodic full GC refresh (configurable interval)
    quick menu      per-app actions + screen refresh + go home
    settings        sleep timeout, ghost clear interval,
                    book font size, UI font size, wifi credentials
    sleep           idle timeout + power long-press; EPD deep sleep
                    (~3 uA) + ESP32-C3 deep sleep (~5 uA); GPIO3 wake

controls
    Prev / Next         scroll or turn page
    PrevJump / NextJump page skip (files: full page; reader: chapter)
    Select              open item
    Back                go back; long-press goes home
    Power (short)       open quick-action menu
    Power (long)        deep sleep

runtime
    embassy async executor on esp-rtos. five concurrent tasks:

    main            event loop: input dispatch, app work, rendering
    input_task      10 ms ADC poll, debounce, battery read (30 s)
    housekeeping    status bar (5 s), SD check (30 s), bookmark flush (30 s)
    idle_timeout    configurable idle timer, signals deep sleep
    worker_task     background CPU-heavy work (HTML strip, image decode)

    CPU sleeps (WFI) whenever all tasks are waiting.

directory layout
    src/
      bin/main.rs       entry point, hardware init, boot
      lib.rs            crate root
      apps/             application layer (App trait + concrete apps)
        home.rs         launcher menu + bookmarks browser
        files.rs        SD file browser + background title scanner
        reader/         TXT/EPUB reader (paging, epub pipeline, images)
        settings.rs     settings UI
        upload.rs       wifi upload server
        manager.rs      lifecycle dispatch, font propagation
      board/            board support (pin map, SPI wiring, button layout)
      drivers/          hardware drivers (EPD, SD, ADC, strip buffer)
      fonts/            build-time bitmap font data + runtime lookups
      kernel/           resource ownership, scheduling, syscall boundary
        scheduler.rs    main loop, render pipeline, sleep
        handle.rs       KernelHandle (app I/O API)
        tasks.rs        spawned embassy tasks
        work_queue.rs   background work with generation cancellation
        bookmarks.rs    LRU bookmark cache
        config.rs       settings parser/writer
        dir_cache.rs    sorted directory cache with title resolution
    build.rs            fontdue TTF rasterisation at compile time
    assets/fonts/       TTF files (regular, bold, italic)
    assets/upload.html  web UI for wifi upload mode

design notes
    no dyn dispatch. with_app!() macro matches AppId, expands to
    concrete calls per app struct. all monomorphised; no vtable.

    apps never touch hardware. KernelHandle mediates all I/O (SD,
    dir cache, bookmarks) and is only passed in via on_enter/background.

    dirty-region tracking. apps call ctx.mark_dirty(region); regions
    are unioned per frame. partial DU or full GC issued accordingly.

    strip rendering. 12 x 40-row strips (4 KB each) instead of a
    48 KB framebuffer. draw callback fires per strip during DMA.
    windowed mode for partial refresh; widgets use logical coords.

    heavy statics. large structs live in ConstStaticCell / StaticCell
    so the async future stays ~200 B. taken once, passed as &'static mut.

    nav stack. Launcher holds a 4-deep AppId stack. transitions
    (Push/Pop/Replace/Home) drive on_suspend / on_enter lifecycle.

    quick menu. power button opens a per-app overlay; drawn inline
    during the strip pass. refresh and go-home always available.

    heap budget. ~172 KB heap (108 KB main + 64 KB reclaimed from
    bootloader). used only for epub chapter text and image decode
    (alloc::vec). rest is stack/static.

    smol-epub. companion no_std crate: ZIP/DEFLATE, OPF spine,
    streaming HTML strip, 1-bit Floyd-Steinberg PNG/JPEG decoders.
    all I/O via generic read closure; storage-agnostic.

    input. ADC ladders sampled at 100 Hz, 4-sample oversampling,
    15 ms debounce, long-press and repeat detected in driver.
    ButtonMapper maps physical buttons to semantic actions.

    fonts. build.rs rasterises TTFs via fontdue into 1-bit bitmaps
    at five sizes. book and UI sizes independently hot-swappable.

    work queue. background embassy task for CPU-heavy work (HTML strip,
    image decode). generation-based cancellation; channel capacity 1
    for natural back-pressure.

    SPI bus sharing. EPD and SD card share a single SPI bus via
    CriticalSectionDevice. background SD I/O finishes before any
    EPD render pass to avoid RefCell borrow conflicts.

    layered architecture. drivers (layer 0) know nothing of apps.
    kernel (layer 1) owns hardware resources. apps (layer 2-3)
    interact only through KernelHandle. board/ and fonts/ are
    cross-cutting but never import from apps.

license
    MIT
