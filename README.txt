ABOUT
    pulp-os is a bare-metal e-reader firmware for the XTEink X4

    Embedded e-reader operating system targeting the XTEink X4 board
    (ESP32-C3 + SSD1677 e-paper). Written in Rust. No std no framebuffer.
    Async runtime provided by Embassy via esp-rtos.

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
    target. rust-toolchain.toml handles both automatically.

        cargo build --release
        espflash flash --monitor --chip esp32c3 /path/to/target/image

        or

        cargo run --release

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

RUNTIME ARCHITECTURE
    Embassy async executor on esp-rtos.  Four concurrent tasks:

    main            event loop: input dispatch, app work, rendering
    input_task      10 ms ADC poll, debounce, battery read (30 s)
    housekeeping    status bar (5 s), SD check (30 s), bookmark flush (30 s)
    idle_timeout    configurable idle timer, signals deep sleep

    CPU sleeps (WFI) whenever all tasks are waiting.

NOTES
    No dyn dispatch.  with_app!() macro matches AppId, expands to
    concrete calls per app struct.  All monomorphised; no vtable.

    Apps never touch hardware.  Services mediates all I/O (SD, dir
    cache, bookmarks) and is only passed in via on_work().

    Dirty-region tracking.  Apps call ctx.mark_dirty(region); regions
    are unioned per frame.  Partial DU or full GC issued accordingly.

    Strip rendering.  12 x 40-row strips (4 KB each) instead of a
    48 KB framebuffer.  Draw callback fires per strip during DMA.
    Windowed mode for partial refresh; widgets use logical coords.

    Heavy statics.  Large structs live in ConstStaticCell / StaticCell
    so the async future stays ~200 B.  Taken once, passed as &'static mut.

    Nav stack.  Launcher holds a 4-deep AppId stack.  Transitions
    (Push/Pop/Replace/Home) drive on_suspend / on_enter lifecycle.

    Quick menu.  Power button opens a per-app overlay; drawn inline
    during the strip pass.  Refresh and go-home always available.

    Heap budget.  140 KB heap; used only for epub chapter text and
    image decode (alloc::vec).  Peak ~79 KB.  Rest is stack/static.

    smol-epub.  Companion no_std crate: ZIP/DEFLATE, OPF spine,
    streaming HTML strip, 1-bit Floyd-Steinberg PNG/JPEG decoders.
    All I/O via generic read closure; storage-agnostic.

    Input.  ADC ladders sampled at 100 Hz, debounced, long-press and
    repeat detected in driver.  ButtonMapper maps to semantic actions.

    Fonts.  build.rs rasterises TTFs via fontdue into 1-bit bitmaps
    at three sizes.  Book and UI sizes independently hot-swappable.

LICENSE
    MIT
