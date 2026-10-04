// R21 / bookmarks: the real BookmarkCache (16 slots, LRU by generation, 48-byte
// records in _PULP/BKMK.BIN) on the in-memory card, and its use by the reader.
use pulp_os_host::apps::probe;
use pulp_os_host::board::action::Action;
use pulp_os_host::fixtures::*;
use pulp_os_host::kernel::bookmarks::{BmListEntry, BOOKMARK_FILE, FILE_LEN, RECORD_LEN, SLOTS};
use pulp_os_host::kernel::Kernel;
use pulp_os_host::rig::*;
use pulp_kernel::drivers::sdcard::{FakeFs, SdStorage};

fn kernel_with(fs: FakeFs) -> Kernel {
    Kernel::new(SdStorage::mounted(fs))
}

fn bkmk(k: &Kernel) -> Option<Vec<u8>> {
    k.sd().with_fs(|fs| fs.get("_PULP", BOOKMARK_FILE).cloned()).flatten()
}

// independent FNV-1a (32 bit) over the lower-cased name
fn fnv(name: &str) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for b in name.bytes() {
        h ^= b.to_ascii_lowercase() as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn names(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("BOOK{:02}.TXT", i)).collect()
}

#[test]
fn constants_are_the_baseline_format() {
    assert_eq!((SLOTS, RECORD_LEN, FILE_LEN), (16, 48, 768));
    assert_eq!(BOOKMARK_FILE, "BKMK.BIN");
}

#[test]
fn not_loaded_means_no_lookup_and_no_save() {
    let mut k = kernel_with(card());
    // the cache is only usable after the kernel loaded it at boot
    k.bookmarks().save(b"A.TXT", 5, 0);
    assert!(!k.bookmarks().is_dirty(), "save before load is ignored");
    assert!(k.bookmarks().find(b"A.TXT").is_none());
    let mut out = [BmListEntry::EMPTY; 16];
    assert_eq!(k.bookmarks().load_all(&mut out), 0);
    k.bookmarks_load();
    assert!(k.bookmarks().is_loaded());
    assert!(k.bookmarks().find(b"A.TXT").is_none());
}

#[test]
fn save_find_update_and_remove() {
    let mut k = kernel_with(card());
    k.bookmarks_load();
    k.bookmarks().save(b"BOOK.TXT", 1234, 0);
    k.bookmarks().save(b"NOVEL.EPU", 99, 7);
    assert!(k.bookmarks().is_dirty());
    let a = k.bookmarks().find(b"BOOK.TXT").unwrap();
    assert_eq!((a.byte_offset, a.chapter, a.valid), (1234, 0, true));
    assert_eq!(a.filename_str(), "BOOK.TXT");
    let b = k.bookmarks().find(b"NOVEL.EPU").unwrap();
    assert_eq!((b.byte_offset, b.chapter), (99, 7));
    // lookup is ASCII case-insensitive (FAT names)
    assert_eq!(k.bookmarks().find(b"book.txt").unwrap().byte_offset, 1234);
    assert!(k.bookmarks().find(b"OTHER.TXT").is_none());

    // saving the same book updates in place and bumps its generation
    let g0 = a.generation;
    k.bookmarks().save(b"BOOK.TXT", 4321, 0);
    let a2 = k.bookmarks().find(b"BOOK.TXT").unwrap();
    assert_eq!(a2.byte_offset, 4321);
    assert!(a2.generation > g0);
    let mut out = [BmListEntry::EMPTY; 16];
    assert_eq!(k.bookmarks().load_all(&mut out), 2, "still two books");

    k.bookmarks().remove(b"BOOK.TXT");
    assert!(k.bookmarks().find(b"BOOK.TXT").is_none());
    assert_eq!(k.bookmarks().load_all(&mut out), 1);
    // removing something that is not there is a no-op
    k.bookmarks_flush();
    assert!(!k.bookmarks().is_dirty());
    k.bookmarks().remove(b"NOPE.TXT");
    assert!(!k.bookmarks().is_dirty());
}

#[test]
fn list_is_most_recently_saved_first() {
    let mut k = kernel_with(card());
    k.bookmarks_load();
    for (i, n) in names(5).iter().enumerate() {
        k.bookmarks().save(n.as_bytes(), i as u32, i as u16);
    }
    k.bookmarks().save(b"BOOK04.TXT", 111, 4); // re-save the newest: stays first, offset updated
    let mut out = [BmListEntry::EMPTY; 16];
    let n = k.bookmarks().load_all(&mut out);
    let order: Vec<_> = out[..n].iter().map(|e| e.filename_str().to_string()).collect();
    assert_eq!(order, ["BOOK04.TXT", "BOOK03.TXT", "BOOK02.TXT", "BOOK01.TXT", "BOOK00.TXT"]);
    assert_eq!(out[0].chapter, 4);
    // baseline: a short output buffer receives the first N slots in file order (not
    // the N newest), sorted by recency among themselves
    let mut two = [BmListEntry::EMPTY; 2];
    assert_eq!(k.bookmarks().load_all(&mut two), 2);
    assert_eq!(two[0].filename_str(), "BOOK01.TXT");
    assert_eq!(two[1].filename_str(), "BOOK00.TXT");
}

#[test]
fn resaving_an_early_slot_does_not_always_make_it_the_newest_baseline_quirk() {
    // Baseline behaviour (identical in the pre-port code, see baseline.md T13): the
    // generation of an updated slot is max(generation of the slots up to and
    // including it) + 1, because the scan stops at the match. Re-saving BOOK01
    // (slot 1) therefore gets generation 3 and only ties with BOOK02.
    let mut k = kernel_with(card());
    k.bookmarks_load();
    for (i, n) in names(5).iter().enumerate() {
        k.bookmarks().save(n.as_bytes(), i as u32, 0);
    }
    assert_eq!(k.bookmarks().find(b"BOOK04.TXT").unwrap().generation, 5);
    k.bookmarks().save(b"BOOK01.TXT", 111, 0);
    assert_eq!(k.bookmarks().find(b"BOOK01.TXT").unwrap().generation, 3);
    let mut out = [BmListEntry::EMPTY; 16];
    let n = k.bookmarks().load_all(&mut out);
    let order: Vec<_> = out[..n].iter().map(|e| e.filename_str().to_string()).collect();
    assert_eq!(order, ["BOOK04.TXT", "BOOK03.TXT", "BOOK01.TXT", "BOOK02.TXT", "BOOK00.TXT"]);
}

#[test]
fn full_cache_evicts_the_lowest_generation_book() {
    let mut k = kernel_with(card());
    k.bookmarks_load();
    let all = names(SLOTS + 2);
    for (i, n) in all[..SLOTS].iter().enumerate() {
        k.bookmarks().save(n.as_bytes(), 1000 + i as u32, 0);
    }
    let mut out = [BmListEntry::EMPTY; 32];
    assert_eq!(k.bookmarks().load_all(&mut out), 16);
    // the 17th book replaces the oldest (BOOK00, generation 1), the 18th the next (BOOK01)
    k.bookmarks().save(all[16].as_bytes(), 7, 0);
    assert!(k.bookmarks().find(all[0].as_bytes()).is_none(), "oldest evicted");
    assert_eq!(k.bookmarks().find(all[16].as_bytes()).unwrap().generation, 17);
    k.bookmarks().save(all[17].as_bytes(), 8, 0);
    assert!(k.bookmarks().find(all[1].as_bytes()).is_none());
    assert_eq!(k.bookmarks().load_all(&mut out), 16, "never more than 16");
    for i in 2..16 {
        assert_eq!(k.bookmarks().find(all[i].as_bytes()).unwrap().byte_offset, 1000 + i as u32);
    }
    // the newest two are first in the list
    assert_eq!(out[0].filename_str(), all[17]);
    assert_eq!(out[1].filename_str(), all[16]);
}

#[test]
fn a_removed_slot_is_reused_before_anything_is_evicted() {
    let mut k = kernel_with(card());
    k.bookmarks_load();
    let all = names(SLOTS + 1);
    for n in &all[..SLOTS] {
        k.bookmarks().save(n.as_bytes(), 1, 0);
    }
    k.bookmarks().remove(all[5].as_bytes());
    k.bookmarks().save(all[16].as_bytes(), 2, 0);
    for (i, n) in all.iter().enumerate() {
        assert_eq!(k.bookmarks().find(n.as_bytes()).is_some(), i != 5, "{n}");
    }
}

#[test]
fn flush_writes_the_baseline_record_layout() {
    let mut k = kernel_with(card());
    k.bookmarks_load();
    assert!(bkmk(&k).is_none());
    k.bookmarks().save(b"BOOK.TXT", 0x0102_0304, 7);
    k.bookmarks().save(b"B2.EPU", 9, 0);
    k.bookmarks_flush();
    assert!(!k.bookmarks().is_dirty());
    let f = bkmk(&k).unwrap();
    assert_eq!(f.len(), 2 * RECORD_LEN, "file holds exactly the used slots");
    let r = &f[..RECORD_LEN];
    assert_eq!(&r[0..4], &fnv("BOOK.TXT").to_le_bytes(), "name hash (FNV-1a, case folded)");
    assert_eq!(&r[4..8], &0x0102_0304u32.to_le_bytes(), "byte offset");
    assert_eq!(&r[8..10], &7u16.to_le_bytes(), "chapter");
    assert_eq!(&r[10..12], &1u16.to_le_bytes(), "flags: valid");
    assert_eq!(&r[12..14], &1u16.to_le_bytes(), "generation");
    assert_eq!(r[14], 8, "name length");
    assert_eq!(r[15], 0);
    assert_eq!(&r[16..24], b"BOOK.TXT");
    assert!(r[24..48].iter().all(|&b| b == 0));
    assert_eq!(&f[RECORD_LEN + 12..RECORD_LEN + 14], &2u16.to_le_bytes(), "second save: generation 2");
}

#[test]
fn flush_is_skipped_when_clean_and_retried_when_the_card_fails() {
    let mut k = kernel_with(card());
    k.bookmarks_load();
    k.bookmarks_flush();
    assert!(bkmk(&k).is_none(), "clean cache writes nothing");
    k.bookmarks().save(b"A.TXT", 1, 0);
    k.sd().eject();
    k.bookmarks_flush();
    assert!(k.bookmarks().is_dirty(), "failed flush keeps the dirty flag");
    // the card comes back (same content) and the next flush succeeds
    let mut fs = card();
    fs.put("", "A.TXT", b"x");
    let mut k2 = kernel_with(fs);
    k2.bookmarks_load();
    k2.bookmarks().save(b"A.TXT", 1, 0);
    k2.bookmarks_flush();
    assert!(bkmk(&k2).is_some());
}

#[test]
fn persisted_bookmarks_survive_a_reboot_exactly() {
    let mut k = kernel_with(card());
    k.bookmarks_load();
    let all = names(SLOTS);
    for (i, n) in all.iter().enumerate() {
        k.bookmarks().save(n.as_bytes(), 100 * i as u32, i as u16);
    }
    k.bookmarks().remove(all[3].as_bytes());
    k.bookmarks().save(all[0].as_bytes(), 31337, 2);
    k.bookmarks_flush();
    let bytes = bkmk(&k).unwrap();
    assert_eq!(bytes.len(), FILE_LEN);

    // power cycle: new kernel, same card contents
    let mut fs = card();
    fs.put("_PULP", BOOKMARK_FILE, &bytes);
    let mut k2 = kernel_with(fs);
    k2.bookmarks_load();
    for (i, n) in all.iter().enumerate() {
        let got = k2.bookmarks().find(n.as_bytes());
        match i {
            3 => assert!(got.is_none(), "removed stays removed"),
            0 => assert_eq!((got.unwrap().byte_offset, got.unwrap().chapter), (31337, 2)),
            _ => assert_eq!((got.unwrap().byte_offset, got.unwrap().chapter), (100 * i as u32, i as u16)),
        }
    }
    let mut a = [BmListEntry::EMPTY; 16];
    let mut b = [BmListEntry::EMPTY; 16];
    let na = k.bookmarks().load_all(&mut a);
    let nb = k2.bookmarks().load_all(&mut b);
    assert_eq!(na, nb);
    for i in 0..na {
        assert_eq!(a[i].filename_str(), b[i].filename_str(), "list order identical after reload");
    }
    // saving again after the reload keeps generations increasing (no collision)
    k2.bookmarks().save(b"NEW.TXT", 1, 0);
    assert_eq!(k2.bookmarks().find(b"NEW.TXT").unwrap().generation, 17, "max remaining generation was 16");
}

fn load_from(file: &[u8]) -> Kernel {
    let mut fs = card();
    fs.put("_PULP", BOOKMARK_FILE, file);
    let mut k = kernel_with(fs);
    k.bookmarks_load();
    k
}

fn record(name: &[u8], off: u32, ch: u16, flags: u16, gen_: u16, name_len: u8) -> Vec<u8> {
    let mut r = vec![0u8; RECORD_LEN];
    r[0..4].copy_from_slice(&fnv(std::str::from_utf8(name).unwrap()).to_le_bytes());
    r[4..8].copy_from_slice(&off.to_le_bytes());
    r[8..10].copy_from_slice(&ch.to_le_bytes());
    r[10..12].copy_from_slice(&flags.to_le_bytes());
    r[12..14].copy_from_slice(&gen_.to_le_bytes());
    r[14] = name_len;
    r[16..16 + name.len()].copy_from_slice(name);
    r
}

#[test]
fn damaged_bookmark_files_are_tolerated() {
    let mut out = [BmListEntry::EMPTY; 16];

    // empty file / missing file: no bookmarks, cache is usable
    let mut k = load_from(b"");
    assert!(k.bookmarks().is_loaded());
    assert_eq!(k.bookmarks().load_all(&mut out), 0);
    k.bookmarks().save(b"A.TXT", 1, 0);
    assert!(k.bookmarks().find(b"A.TXT").is_some());

    // shorter than one record: nothing decoded
    let mut k = load_from(&[0xAA; 47]);
    assert_eq!(k.bookmarks().load_all(&mut out), 0);

    // trailing partial record is ignored
    let mut f = record(b"A.TXT", 10, 1, 1, 1, 5);
    f.extend_from_slice(&[0xFF; 20]);
    let mut k = load_from(&f);
    assert_eq!(k.bookmarks().find(b"A.TXT").unwrap().byte_offset, 10);
    assert_eq!(k.bookmarks().load_all(&mut out), 1);

    // flags without the valid bit: slot present but not a bookmark
    let f = record(b"A.TXT", 10, 1, 0, 1, 5);
    let mut k = load_from(&f);
    assert!(k.bookmarks().find(b"A.TXT").is_none());
    assert_eq!(k.bookmarks().load_all(&mut out), 0);

    // name_len beyond the 32-byte field is clamped (no out-of-range panic)
    let f = record(b"A.TXT", 10, 1, 1, 1, 255);
    let mut k = load_from(&f);
    assert!(k.bookmarks().is_loaded());

    // wrong hash for the stored name: that slot never matches
    let mut f = record(b"A.TXT", 10, 1, 1, 1, 5);
    f[0] ^= 0xFF;
    let mut k = load_from(&f);
    assert!(k.bookmarks().find(b"A.TXT").is_none());

    // pure garbage of the maximum size and beyond: at most 16 slots are read
    let garbage: Vec<u8> = (0..2000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let mut k = load_from(&garbage);
    let n = k.bookmarks().load_all(&mut out);
    assert!(n <= 16);
    k.bookmarks().save(b"OK.TXT", 77, 3);
    assert_eq!(k.bookmarks().find(b"OK.TXT").unwrap().byte_offset, 77);

    // 17 valid records in the file: only the first 16 are loaded
    let mut f = Vec::new();
    for (i, n) in names(17).iter().enumerate() {
        f.extend_from_slice(&record(n.as_bytes(), i as u32, 0, 1, i as u16 + 1, n.len() as u8));
    }
    let mut k = load_from(&f);
    assert!(k.bookmarks().find(b"BOOK15.TXT").is_some());
    assert!(k.bookmarks().find(b"BOOK16.TXT").is_none());
}

#[test]
fn a_read_error_while_loading_leaves_an_empty_usable_cache() {
    let mut fs = card();
    fs.put("_PULP", BOOKMARK_FILE, &record(b"A.TXT", 10, 1, 1, 1, 5));
    let mut k = kernel_with(fs);
    k.sd().fail_next_reads(1);
    k.bookmarks_load();
    assert!(k.bookmarks().is_loaded());
    assert!(k.bookmarks().find(b"A.TXT").is_none());
}

// ----------------------------------------------------- reader integration

fn txt_rig() -> Rig {
    let mut r = Rig::with_book("BOOK.TXT", &english_txt(40_000, Eol::Lf, 1));
    r.configure(2, 1);
    r.open("BOOK.TXT");
    r
}

#[test]
fn reader_bookmark_round_trip_through_the_card() {
    let mut r = txt_rig();
    for _ in 0..8 {
        r.press(Action::Next);
    }
    let want = r.pos();
    r.save_position();
    r.k.bookmarks_flush();
    r.exit();

    // reboot: new kernel + new reader on the same card contents
    let bytes = bkmk(&r.k).unwrap();
    let mut fs = card();
    fs.put("", "BOOK.TXT", &english_txt(40_000, Eol::Lf, 1));
    fs.put("_PULP", BOOKMARK_FILE, &bytes);
    let mut r2 = Rig::new(fs);
    r2.configure(2, 1);
    r2.open("BOOK.TXT");
    assert_eq!(r2.pos(), want, "same page and same text after a reboot");
}

#[test]
fn removing_the_bookmark_reopens_at_the_first_page() {
    let mut r = txt_rig();
    for _ in 0..4 {
        r.press(Action::Next);
    }
    r.save_position();
    r.exit();
    r.k.bookmarks().remove(b"BOOK.TXT");
    r.open("BOOK.TXT");
    assert_eq!(r.page(), 0);
}

#[test]
fn bookmarks_are_per_book() {
    let mut fs = card();
    fs.put("", "A.TXT", &english_txt(30_000, Eol::Lf, 1));
    fs.put("", "B.TXT", &english_txt(30_000, Eol::Lf, 2));
    let mut r = Rig::new(fs);
    r.configure(2, 1);
    r.open("A.TXT");
    for _ in 0..5 {
        r.press(Action::Next);
    }
    r.save_position();
    r.exit();
    r.open("B.TXT");
    assert_eq!(r.page(), 0, "B has no bookmark");
    r.press(Action::Next);
    r.save_position();
    r.exit();
    r.open("A.TXT");
    assert_eq!(r.page(), 5);
    r.exit();
    r.open("B.TXT");
    assert_eq!(r.page(), 1);
    assert_eq!(probe::total_pages(&r.app) > 1, true);
}

#[test]
fn reader_save_position_does_not_touch_the_session_files() {
    // bookmarks (BKMK.BIN) and the C61 session slots (SESSA/SESSB.BIN) are separate files
    let mut r = txt_rig();
    r.k.sd().with_fs(|fs| {
        fs.put("_PULP", "SESSA.BIN", &[1, 2, 3]);
        fs.put("_PULP", "SESSB.BIN", &[4, 5, 6]);
    });
    r.press(Action::Next);
    r.save_position();
    r.k.bookmarks_flush();
    let (a, b) = r
        .k
        .sd()
        .with_fs(|fs| (fs.get("_PULP", "SESSA.BIN").cloned(), fs.get("_PULP", "SESSB.BIN").cloned()))
        .unwrap();
    assert_eq!(a.unwrap(), vec![1, 2, 3]);
    assert_eq!(b.unwrap(), vec![4, 5, 6]);
    assert!(bkmk(&r.k).is_some());
}
