pulp-os -- e-reader firmware for the XTEink X4

bare-metal e-reader operating system for the XTEink X4 board
(ESP32-C3 + SSD1677 e-paper). written in Rust. no std, no
framebuffer, no dyn dispatch. async runtime via Embassy on
esp-rtos.

hardware
    mcu         ESP32-C3, single-core RISC-V RV32IMC, 160 MHz
    ram         400 KB DRAM; ~172 KB heap (108 KB main + 64 KB reclaimed)
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

    EPD and SD share SPI2, arbitrated by CriticalSectionDevice.

building
    requires stable Rust >= 1.88 and the riscv32imc-unknown-none-elf
    target. rust-toolchain.toml handles both automatically.

        cargo build --release
        espflash flash --monitor --chip esp32c3 target/...

    or:

        cargo run --release

    local path dependencies (sibling dirs):
      embedded-sdmmc    async FAT filesystem over SD/SPI (local fork)
      smol-epub         no_std epub/zip/html/image processing

features
    txt reader      lazy page-indexed, read-ahead prefetch,
                    proportional font wrapping
    epub reader     ZIP/OPF/HTML-strip pipeline, chapter cache on SD,
                    proportional fonts with bold/italic/heading styles,
                    inline PNG/JPEG (1-bit Floyd-Steinberg dithered),
                    TOC browser (NCX or inline), chapter navigation
    file browser    paginated SD listing, background EPUB title
                    scanner (resolves titles from OPF metadata)
    bookmarks       16-slot LRU in RAM, flushed to SD every 30 s;
                    home screen bookmarks browser sorted by recency
    wifi upload     HTTP file upload + mDNS (pulp.local);
                    drag-and-drop web UI with delete support
    fonts           regular/bold/italic TTFs rasterised at build time
                    via fontdue; five sizes, book and UI independently
                    configurable
    display         partial DU refresh (~400 ms page turn), periodic
                    full GC refresh (configurable interval)
    quick menu      per-app actions + screen refresh + go home,
                    triggered by power button
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
    kernel/                 pulp-kernel workspace crate (zero app imports)
      src/
        lib.rs              crate root, re-exports
        kernel/
          mod.rs            Kernel struct, resource ownership
          app.rs            App trait, AppLayer trait, AppIdType,
                            Transition, Redraw, AppContext, Launcher,
                            QuickAction protocol types
          console.rs        boot console (FONT_6X13, no fontdue)
          scheduler.rs      main loop, render pipeline, sleep
          handle.rs         KernelHandle (app I/O API)
          tasks.rs          spawned embassy tasks
          work_queue.rs     background work with generation cancellation
          bookmarks.rs      LRU bookmark cache
          config.rs         settings parser/writer
          dir_cache.rs      sorted directory cache with title resolution
          wake.rs           uptime helper (embassy monotonic clock)
        board/              board support (pin map, SPI wiring, button layout)
          mod.rs            Board::init, peripheral splitting
          action.rs         ActionEvent (semantic button actions)
          battery.rs        voltage-to-percentage mapping
          button.rs         physical button enum, ButtonMapper
          layout.rs         button-to-action table
          raw_gpio.rs       register-level GPIO for SD CS
        drivers/            hardware drivers
          mod.rs            driver re-exports
          ssd1677.rs        EPD display driver, 3-phase partial refresh
          strip.rs          4 KB strip buffer, rotation, glyph blitting
          sdcard.rs         SD card init and SPI wiring
          storage.rs        FAT filesystem ops, poll_once, with_fs! macros
          input.rs          ADC button polling, debounce, repeat
          battery.rs        ADC battery voltage sampling
        ui/                 font-independent primitives
          mod.rs            Region, Alignment, stack measurement
          stack_fmt.rs      no_alloc formatting (StackFmt)
          statusbar.rs      status bar rendering
          widget.rs         widget trait and helpers

    src/                    distro / app layer
      bin/main.rs           entry point, hardware init, boot
      lib.rs                crate root
      ui/
        mod.rs              app-side UI helpers
      fonts/
        mod.rs              font size tiers, FontSet lookups
        bitmap.rs           build-time bitmap font data
      apps/
        mod.rs              AppId enum, type aliases binding kernel generics
        manager.rs          AppLayer impl, with_app! dispatch, lifecycle
        home.rs             launcher menu + bookmarks browser
        files.rs            SD file browser + background title scanner
        settings.rs         settings UI
        upload.rs           wifi upload server
        reader/
          mod.rs            state machine, lifecycle, draw, quick actions
          paging.rs         text wrapping, page navigation, load/prefetch
          epub_pipeline.rs  ZIP/OPF parsing, chapter caching, background strip
          images.rs         image detection, decode dispatch, dithering
        widgets/
          mod.rs            widget re-exports
          bitmap_label.rs   proportional text label (uses fonts/)
          quick_menu.rs     power-button overlay menu
          button_feedback.rs  button press visual feedback

    build.rs                fontdue TTF rasterisation at compile time
    assets/fonts/           TTF files (regular, bold, italic)
    assets/upload.html      web UI for wifi upload mode

design notes
    kernel / app split. the kernel crate (kernel/) has zero imports
    from apps/ or fonts/. the scheduler is generic over AppLayer;
    it never names a concrete app. AppId is defined by the distro,
    not the kernel -- the kernel only knows AppIdType::HOME.

    no dyn dispatch. with_app!() macro matches AppId, expands to
    concrete calls per app struct. all monomorphised; no vtable,
    no Box.

    strip rendering. 12 x 40-row strips (4 KB each) instead of a
    48 KB framebuffer. draw callback fires per strip during SPI
    transfer. blit_1bpp_270 fast path walks physical memory linearly
    for the portrait rotation. windowed mode for partial refresh.

    3-phase partial refresh. write BW RAM, kick DU waveform, collect
    input during ~400 ms refresh, then sync RED RAM. phase3 skipped
    during rapid navigation (RED marked stale; next partial uses
    inv_red recovery). full GC promoted after configurable number
    of partials to clear ghosting.

    SPI bus sharing. EPD and SD share one SPI2 bus. all SD I/O
    completes before any EPD render pass. busy_wait_with_input()
    collects only input events, no background work. violating the
    ordering panics (RefCell double-borrow), never corrupts.

    poll_once. embedded-sdmmc's async API wraps blocking SPI+DMA
    that never pends. poll_once drives every future to completion
    in a single poll, avoiding task spawn overhead.

    KernelHandle. apps never touch hardware. KernelHandle borrows
    the Kernel for one lifecycle method and exposes file I/O, dir
    cache, bookmarks. every async method does sync work then
    yield_now() for executor fairness.

    smol-epub sync bridge. smol-epub I/O uses closures, not async.
    with_sync_reader() provides a scoped closure that completes
    all storage access before returning -- no borrows across await.

    heavy statics. large structs (ReaderApp ~28 KB, DirCache ~10 KB,
    StripBuffer ~4 KB) live in ConstStaticCell / StaticCell so the
    async future stays ~200 B.

    nav stack. Launcher<Id> holds a 4-deep stack. transitions
    (Push/Pop/Replace/Home) drive on_suspend / on_enter / on_resume
    lifecycle. Push degrades to Replace when stack is full.

    dirty-region tracking. apps call ctx.mark_dirty(region); regions
    are unioned per frame. partial DU or full GC issued accordingly.

    work queue. dedicated embassy task for CPU-heavy work (HTML strip,
    image decode). generation-based cancellation: bump a counter and
    drain channels; worker checks generation before and after
    processing. channel capacity 1 for back-pressure.

    input. ADC ladders at 100 Hz, 4-sample oversampling, 15 ms
    debounce, 1 s long-press, 150 ms repeat. ButtonMapper translates
    physical buttons to semantic actions. apps never see hardware.

    fonts. build.rs rasterises TTFs via fontdue into 1-bit bitmaps
    at five sizes (xsmall through xlarge), three styles (regular,
    bold, italic). ASCII direct-indexed, extended unicode binary-
    searched. book and UI sizes independently hot-swappable.

    boot console. kernel renders text during hardware init using
    built-in FONT_6X13 mono font. works with zero fontdue, zero
    TTFs. if the SD card is missing, user still sees boot progress.

    bookmarks. 16-slot LRU, RAM-resident, binary format on SD.
    flushed every 30 s if dirty, plus on sleep. lookup by fnv1a
    hash + case-insensitive name comparison.

    settings. key=value text in _PULP/SETTINGS.TXT. parsed at boot,
    saved on change. font size changes propagate to all apps.

    wifi upload. bypasses normal dispatch. HTTP server on port 80,
    mDNS on 5353 (pulp.local). multipart upload with 8.3 filename
    sanitisation. radio torn down before returning to app loop.

    memory budget. ~172 KB heap for epub text and image decode
    (alloc::vec). everything else is static or stack. ~56 KB stack,
    painted 0xDEAD_BEEF at boot, high-water mark logged every 5 s.

    forkable kernel. designed to be extracted as a standalone crate.
    a fork defines its own AppId, implements AppLayer, brings its
    own fonts and apps, writes a main.rs. the kernel provides
    drivers, scheduling, storage, bookmarks, config, and a working
    EPD with mono boot console.

license
    MIT
