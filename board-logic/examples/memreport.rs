// Host helper for `scripts/report-c61-memory.sh`: applies the memory budget
// rules of `pulp_board_logic::memory` (the code the firmware links) to numbers
// the script extracted from a linked ELF.
//
//   memreport constants          print the budget constants as NAME=value
//   memreport inventory          print the large-allocation inventory as a markdown table
//   memreport check < facts      check facts read from stdin, exit 1 on a fault
//
// Fact lines (hex without 0x):
//   image  <static_end> <stack_addr> <stack_size>
//   section <name> <addr> <size> <W|R>      allocated ELF section
//   dma    <symbol> <addr> <size>           DMA buffer / descriptor symbol

use pulp_board_logic::memory::*;
use std::io::BufRead;

fn hex(s: &str) -> usize {
    usize::from_str_radix(s, 16).unwrap_or_else(|_| {
        eprintln!("memreport: bad hex number {s:?}");
        std::process::exit(2);
    })
}

fn constants() {
    let kv: &[(&str, usize)] = &[
        ("C61_RAM_START", C61_RAM_START),
        ("C61_RAM_LEN", C61_RAM_LEN),
        ("C61_RECLAIMED_START", C61_RECLAIMED_START),
        ("C61_RECLAIMED_LEN", C61_RECLAIMED_LEN),
        ("C61_RAM_END", C61_RAM_END),
        ("C61_EXTMEM_START", C61_EXTMEM_START),
        ("C61_EXTMEM_END", C61_EXTMEM_END),
        ("STACK_MIN_BYTES", STACK_MIN_BYTES),
        ("STATIC_RAM_MAX_BYTES", STATIC_RAM_MAX_BYTES),
        ("INTERNAL_HEAP_MAIN_BYTES", INTERNAL_HEAP_MAIN_BYTES),
        (
            "INTERNAL_HEAP_RECLAIMED_BYTES",
            INTERNAL_HEAP_RECLAIMED_BYTES,
        ),
        ("INTERNAL_HEAP_BYTES", INTERNAL_HEAP_BYTES),
        ("PSRAM_HW_BYTES", PSRAM_HW_BYTES),
        ("PSRAM_MIN_BYTES", PSRAM_MIN_BYTES),
        ("PSRAM_RESERVE_BYTES", PSRAM_RESERVE_BYTES),
        ("PSRAM_CHAPTER_TEXT_BYTES", PSRAM_CHAPTER_TEXT_BYTES),
        ("PSRAM_IMAGE_DATA_BYTES", PSRAM_IMAGE_DATA_BYTES),
        ("PSRAM_PAGE_TABLE_BYTES", PSRAM_PAGE_TABLE_BYTES),
        ("PSRAM_ZIP_TOC_BYTES", PSRAM_ZIP_TOC_BYTES),
        ("INTERNAL_DMA_BYTES", INTERNAL_DMA_BYTES),
        ("INTERNAL_ISR_BYTES", INTERNAL_ISR_BYTES),
        ("INTERNAL_RUNTIME_BYTES", INTERNAL_RUNTIME_BYTES),
        ("INTERNAL_CHAPTER_TEXT_BYTES", INTERNAL_CHAPTER_TEXT_BYTES),
        ("INTERNAL_IMAGE_DATA_BYTES", INTERNAL_IMAGE_DATA_BYTES),
        ("INTERNAL_PAGE_TABLE_BYTES", INTERNAL_PAGE_TABLE_BYTES),
        ("INTERNAL_ZIP_TOC_BYTES", INTERNAL_ZIP_TOC_BYTES),
        ("FLASH_MHZ", FLASH_MHZ as usize),
        ("PSRAM_MHZ", PSRAM_MHZ as usize),
    ];
    for (k, v) in kv {
        println!("{k}={v}");
    }
}

fn inventory() {
    println!("| # | allocation | where | kind | bytes | class | placement | note |");
    println!("|---|---|---|---|---|---|---|---|");
    for (i, it) in INVENTORY.iter().enumerate() {
        println!(
            "| {} | {} | {} | {:?} | {} | {} | {:?} | {} |",
            i + 1,
            it.name,
            it.source,
            it.kind,
            it.bytes,
            it.class.name(),
            it.placement,
            it.note
        );
    }
    println!();
    println!(
        "PSRAM candidates per class (rows marked 'in READER' included; they are limits-relevant when moved):"
    );
    let st = PsramStatus::Ready {
        bytes: PSRAM_HW_BYTES,
    };
    for class in [
        MemClass::ChapterText,
        MemClass::ImageData,
        MemClass::PageTable,
        MemClass::ZipToc,
    ] {
        let sum: usize = INVENTORY
            .iter()
            .filter(|i| i.placement == Placement::Candidate && i.class == class)
            .map(|i| i.bytes)
            .sum();
        println!(
            "- {}: candidates {} B of PSRAM limit {} B",
            class.name(),
            sum,
            class_limit(st, Region::Psram, class)
        );
    }
}

fn check() -> bool {
    let mut ok = true;
    let mut seen_image = false;
    let mut dma_seen = 0usize;
    for line in std::io::stdin().lock().lines() {
        let line = line.expect("stdin");
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.as_slice() {
            ["image", end, addr, size] => {
                seen_image = true;
                let img = ImageMemory {
                    static_end: hex(end),
                    stack_addr: hex(addr),
                    stack_size: hex(size),
                };
                match check_image(&img) {
                    Ok(r) => println!(
                        "ok   image: statics {} B ({}.{}% of {} B main RAM), stack {} B, headroom over {} B minimum = {} B",
                        r.static_bytes,
                        r.static_permille / 10,
                        r.static_permille % 10,
                        C61_RAM_LEN,
                        r.stack_bytes,
                        STACK_MIN_BYTES,
                        r.stack_headroom
                    ),
                    Err(e) => {
                        println!("FAIL image: {e:?}");
                        ok = false;
                    }
                }
            }
            ["section", name, addr, size, flag] => {
                let (a, n, w) = (hex(addr), hex(size), *flag == "W");
                if section_placement_ok(a, n, w) {
                    println!(
                        "ok   section {name:<22} {a:#010x} +{n:#08x} {flag} {:?}",
                        addr_space(a)
                    );
                } else {
                    println!(
                        "FAIL section {name} {a:#010x} +{n:#x} {flag}: writable data outside internal RAM"
                    );
                    ok = false;
                }
            }
            ["dma", name, addr, size] => {
                dma_seen += 1;
                let (a, n) = (hex(addr), hex(size));
                if range_is_internal(a, n) {
                    println!("ok   dma     {name:<42} {a:#010x} +{n:#06x} internal RAM");
                } else {
                    println!(
                        "FAIL dma     {name} {a:#010x} +{n:#x}: not inside internal RAM ({:?})",
                        addr_space(a)
                    );
                    ok = false;
                }
            }
            [] => {}
            other => {
                println!("FAIL unknown fact line: {other:?}");
                ok = false;
            }
        }
    }
    if !seen_image {
        println!("FAIL no image fact given");
        ok = false;
    }
    if dma_seen == 0 {
        println!("FAIL no DMA symbol given (nothing proves R15)");
        ok = false;
    }
    ok
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("constants") => constants(),
        Some("inventory") => inventory(),
        Some("check") => {
            if !check() {
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("usage: memreport constants | inventory | check < facts");
            std::process::exit(2);
        }
    }
}
