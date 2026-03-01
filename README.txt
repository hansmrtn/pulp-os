ABOUT
    pulp-os - bare-metal e-reader firmware for the XTEink X4

    Embedded e-reader operating system targeting the XTEink X4 board
    (ESP32-C3 + SSD1677 e-paper).  Written in Rust.  No OS, no std,
    no framebuffer.  Async runtime provided by Embassy via esp-rtos.

HARDWARE
    MCU         ESP32-C3, single-core RISC-V RV32IMC, 160 MHz
    RAM         400 KB DRAM; 140 KB heap, rest for stack + radio
    Display     800x480 SSD1677 mono e-paper, DMA-backed SPI, portrait
    Storage     MicroSD over shared SPI bus (400 kHz probe, 20 MHz run)
    Input       2 ADC ladders (GPIO1, GPIO2) + power button (GPIO3 IRQ)
    Battery     Li-ion via ADC, 100K/100K divider on GPIO0

    Pin map:
      GPIO0   battery ADC          GPIO6   EPD BUSY
      GPIO1   button row 1 ADC     GPIO7   SPI MISO
      GPIO2   button row 2 ADC     GPIO8   SPI SCK
      GPIO3   power button         GPIO10  SPI MOSI
      GPIO4   EPD DC               GPIO12  SD CS (raw register GPIO)
      GPIO5   EPD RST              GPIO21  EPD CS

BUILDING
    Requires stable Rust >= 1.88 and the riscv32imc-unknown-none-elf
    target.  rust-toolchain.toml handles both automatically.

        cargo build --release
        cargo espflash flash --release --monitor

FEATURES
    txt reader      lazy page-indexed, read-ahead prefetch
    epub reader     ZIP/OPF/HTML-strip, chapter cache on SD,
                    proportional fonts, inline PNG/JPEG (dithered 1-bit)
    bookmarks       16-slot LRU in RAM, flushed to SD every 30s
    wifi upload     HTTP file upload + mDNS (pulp.local)
    fonts           regular/bold/italic TTFs rasterised at build time
                    via fontdue; three sizes (small/medium/large)
    display         partial DU refresh (~400 ms page turn),
                    periodic full GC refresh (configurable interval)
    quick menu      per-app actions + screen refresh + go home
    status bar      battery, uptime, heap, stack (debug builds only)
    settings        sleep timeout, ghost clear interval,
                    book font size, UI font size, wifi credentials
    sleep           idle timeout + power long-press; EPD deep sleep
                    (~3 uA) + ESP32-C3 deep sleep (~5 uA); GPIO3 wake

CONTROLS
    Prev / Next         scroll or turn page
    PrevJump / NextJump page skip (files: full page; reader: chapter)
    Select              open item
    Back                go back; long-press goes home
    Power (short)       open quick-action menu
    Power (long)        deep sleep

SD CARD LAYOUT
    /                       root; place .txt and .epub files here
    /_PULP/                 app data (created at boot)
    /_PULP/SETTINGS.TXT     settings (key=value text, editable)
    /_PULP/BKMK.BIN         bookmarks (16 slots x 48 bytes)
    /_PULP/TITLES.BIN       title index (append-only, tab-separated)
    /_PULP/RECENT.BIN       last opened filename
    /_PULP/<hash>/          epub chapter cache directories

    SETTINGS.TXT format (lines starting with # are ignored):
      sleep_timeout=10      minutes idle before sleep; 0 = never
      ghost_clear=10        partial refreshes before full GC
      book_font=1           0=Small  1=Medium  2=Large
      ui_font=1             0=Small  1=Medium  2=Large
      wifi_ssid=MyNetwork   SSID for upload mode
      wifi_pass=secret      password for upload mode

    Bookmark slot layout (48 bytes each):
      0:4   name_hash       FNV-1a of filename
      4:4   byte_offset     file/chapter position
      8:2   chapter         epub chapter; 0 for txt
     10:2   flags           bit 0 = valid
     12:2   generation      LRU counter
     14:1   name_len
     15:1   pad
     16:32  filename

SOURCE LAYOUT
    src/
      bin/main.rs           async entry point, event loop, rendering
      lib.rs                crate root
      kernel/
        tasks.rs            spawned tasks (input, housekeeping, idle)
        wake.rs             uptime helper
      board/
        mod.rs              SPI/DMA init, peripheral wiring
        action.rs           semantic actions, button-to-action mapper
        button.rs           button enum, ADC ladder decode
        pins.rs             GPIO pin map (reference)
        raw_gpio.rs         register-level GPIO for unmapped pins
      drivers/
        ssd1677.rs          e-paper controller driver
        strip.rs            4 KB strip render buffer (no framebuffer)
        input.rs            debounced ADC + GPIO input, long press, repeat
        sdcard.rs           SD card over SPI, FAT volume manager
        storage.rs          file ops, directory cache, _PULP helpers
        battery.rs          ADC-to-mV, discharge curve LUT
      fonts/
        mod.rs              font selection, FontSet (regular/bold/italic)
        bitmap.rs           1-bit glyph blit, string measurement
      ui/
        widget.rs           Region, Alignment, wrap helpers
        bitmap_label.rs     proportional-font label widgets
        statusbar.rs        top bar, stack painting, heap stats
        quick_menu.rs       overlay menu (cycle + trigger actions)
        button_feedback.rs  edge button labels
      apps/
        mod.rs              App trait, Launcher nav stack, Services
        home.rs             launcher menu + bookmark browser
        files.rs            paginated SD file browser
        reader.rs           txt + epub reader
        settings.rs         persistent settings editor
        upload.rs           wifi HTTP upload server
        bookmarks.rs        RAM-resident bookmark cache

    smol-epub/              no_std epub parser crate
      src/
        zip.rs              ZIP central directory, streaming DEFLATE
        xml.rs              minimal XML tag/attribute scanner
        css.rs              CSS property parser
        epub.rs             container.xml, OPF spine, NCX/NAV TOC
        html_strip.rs       streaming HTML-to-styled-text converter
        cache.rs            chapter decompress + strip pipeline
        png.rs              PNG decoder, Floyd-Steinberg dither
        jpeg.rs             JPEG decoder, Floyd-Steinberg dither

    build.rs                TTF rasterisation, linker config
    assets/fonts/           source TTFs (Regular, Bold, Italic)

RUNTIME ARCHITECTURE
    Embassy async executor on esp-rtos.  Four concurrent tasks:

    main            event loop: input dispatch, app work, rendering
    input_task      10 ms ADC poll, debounce, battery read (30 s)
    housekeeping    status bar (5 s), SD check (30 s), bookmark flush (30 s)
    idle_timeout    configurable idle timer, signals deep sleep

    CPU sleeps (WFI) whenever all tasks are waiting.

DESIGN NOTES
    No dyn dispatch.  The with_app!() macro statically dispatches to
    each concrete app struct.  No vtable, no heap indirection.

    Apps never touch hardware.  All I/O goes through the Services
    handle passed to on_work().  Clean syscall boundary.

    Strip-buffered rendering.  The display is driven in 4 KB horizontal
    strips (40 rows each, 12 strips total) instead of a 48 KB
    framebuffer.  Widgets draw to logical coordinates; the strip
    buffer handles rotation and clipping.

    Heavy statics in .bss.  Large structs (ReaderApp, StripBuffer,
    BookmarkCache) are placed in static storage via ConstStaticCell
    so the async future stays small (~200 B).

    Partial refresh owns the render path.  Apps call mark_dirty(region)
    for targeted updates.  Full GC refresh is forced periodically and
    on screen transitions.  DU waveform runs concurrently with input
    processing and page prefetch.

    Heap is used only for EPUB chapter text and image decode buffers
    (alloc::vec).  Everything else is stack or static.

LICENSE
    MIT
