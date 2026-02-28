// Shared bookmark record format and helpers
//
// 48-byte slots in _PULP/BOOKMARKS, 16 max. Full filename per slot
// for bookmark browsing. LRU eviction via generation counter.
// File I/O uses a heap-allocated temporary buffer (768 bytes, freed
// on return) to keep the stack light.
//
// Record layout (little-endian):
//   [0..4)   name_hash    u32
//   [4..8)   byte_offset  u32   font-independent file/chapter position
//   [8..10)  chapter      u16   epub chapter; 0 for txt
//   [10..12) flags        u16   bit 0 = valid
//   [12..14) generation   u16   LRU counter (higher = more recent)
//   [14]     name_len     u8
//   [15]     _pad         u8
//   [16..48) filename     [u8;32]

extern crate alloc;

use alloc::vec;

use crate::apps::Services;

pub const BOOKMARK_FILE: &str = "BKMK.BIN";
pub const SLOTS: usize = 16;
pub const RECORD_LEN: usize = 48;
pub const FILE_LEN: usize = SLOTS * RECORD_LEN; // 768
pub const FILENAME_CAP: usize = 32;

// full decoded slot — used transiently, not stored in arrays
#[derive(Clone, Copy)]
pub struct BookmarkSlot {
    pub name_hash: u32,
    pub byte_offset: u32,
    pub chapter: u16,
    pub valid: bool,
    pub generation: u16,
    pub name_len: u8,
    pub filename: [u8; FILENAME_CAP],
}

impl BookmarkSlot {
    pub const EMPTY: Self = Self {
        name_hash: 0,
        byte_offset: 0,
        chapter: 0,
        valid: false,
        generation: 0,
        name_len: 0,
        filename: [0u8; FILENAME_CAP],
    };

    pub fn filename_str(&self) -> &str {
        core::str::from_utf8(&self.filename[..self.name_len as usize]).unwrap_or("?")
    }

    fn decode(rec: &[u8]) -> Self {
        if rec.len() < RECORD_LEN {
            return Self::EMPTY;
        }
        let name_hash = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
        let byte_offset = u32::from_le_bytes([rec[4], rec[5], rec[6], rec[7]]);
        let chapter = u16::from_le_bytes([rec[8], rec[9]]);
        let flags = u16::from_le_bytes([rec[10], rec[11]]);
        let generation = u16::from_le_bytes([rec[12], rec[13]]);
        let name_len = rec[14].min(FILENAME_CAP as u8);

        let mut filename = [0u8; FILENAME_CAP];
        let n = name_len as usize;
        filename[..n].copy_from_slice(&rec[16..16 + n]);

        Self {
            name_hash,
            byte_offset,
            chapter,
            valid: flags & 1 != 0,
            generation,
            name_len,
            filename,
        }
    }

    fn encode(&self) -> [u8; RECORD_LEN] {
        let flags: u16 = if self.valid { 1 } else { 0 };
        let mut rec = [0u8; RECORD_LEN];
        rec[0..4].copy_from_slice(&self.name_hash.to_le_bytes());
        rec[4..8].copy_from_slice(&self.byte_offset.to_le_bytes());
        rec[8..10].copy_from_slice(&self.chapter.to_le_bytes());
        rec[10..12].copy_from_slice(&flags.to_le_bytes());
        rec[12..14].copy_from_slice(&self.generation.to_le_bytes());
        rec[14] = self.name_len;
        rec[15] = 0;
        let n = self.name_len as usize;
        rec[16..16 + n].copy_from_slice(&self.filename[..n]);
        rec
    }

    fn matches_name(&self, name: &[u8]) -> bool {
        self.name_len as usize == name.len() && self.filename[..self.name_len as usize] == *name
    }
}

// lightweight entry for the bookmark list in HomeApp (35 bytes vs 48)
#[derive(Clone, Copy)]
pub struct BmListEntry {
    pub filename: [u8; FILENAME_CAP],
    pub name_len: u8,
    pub chapter: u16,
}

impl BmListEntry {
    pub const EMPTY: Self = Self {
        filename: [0u8; FILENAME_CAP],
        name_len: 0,
        chapter: 0,
    };

    pub fn filename_str(&self) -> &str {
        core::str::from_utf8(&self.filename[..self.name_len as usize]).unwrap_or("?")
    }
}

// FNV-1a 32-bit hash
pub fn fnv1a(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ── File I/O (heap-buffered, one SD read per call) ────────────────────

// read the bookmark file into a heap Vec; returns (buf, slot_count).
// caller must drop the Vec when done to free the heap.
fn read_file<SPI: embedded_hal::spi::SpiDevice>(
    svc: &mut Services<'_, SPI>,
) -> (alloc::vec::Vec<u8>, usize) {
    let mut buf = vec![0u8; FILE_LEN];
    match svc.read_pulp_start(BOOKMARK_FILE, &mut buf) {
        Ok((_, n)) => {
            let count = (n / RECORD_LEN).min(SLOTS);
            (buf, count)
        }
        Err(_) => (buf, 0),
    }
}

#[inline]
fn slot_at(buf: &[u8], i: usize) -> BookmarkSlot {
    let base = i * RECORD_LEN;
    BookmarkSlot::decode(&buf[base..base + RECORD_LEN])
}

// ── Public API ────────────────────────────────────────────────────────

// load all valid bookmarks into BmListEntry array, sorted by generation
// descending (most recent first). returns count written.
pub fn load_all<SPI: embedded_hal::spi::SpiDevice>(
    svc: &mut Services<'_, SPI>,
    out: &mut [BmListEntry],
) -> usize {
    let (buf, slot_count) = read_file(svc);
    let mut gens = [0u16; SLOTS];
    let mut count = 0usize;

    for i in 0..slot_count {
        if count >= out.len() {
            break;
        }
        let slot = slot_at(&buf, i);
        if slot.valid && slot.name_len > 0 {
            gens[count] = slot.generation;
            out[count] = BmListEntry {
                filename: slot.filename,
                name_len: slot.name_len,
                chapter: slot.chapter,
            };
            count += 1;
        }
    }
    // buf dropped here — heap freed

    // insertion sort by generation descending
    for i in 1..count {
        let key_gen = gens[i];
        let key_entry = out[i];
        let mut j = i;
        while j > 0 && gens[j - 1] < key_gen {
            gens[j] = gens[j - 1];
            out[j] = out[j - 1];
            j -= 1;
        }
        gens[j] = key_gen;
        out[j] = key_entry;
    }

    count
}

// find a bookmark by filename; returns None if not found.
pub fn find<SPI: embedded_hal::spi::SpiDevice>(
    svc: &mut Services<'_, SPI>,
    filename: &[u8],
) -> Option<BookmarkSlot> {
    let key = fnv1a(filename);
    let (buf, slot_count) = read_file(svc);

    for i in 0..slot_count {
        let slot = slot_at(&buf, i);
        if slot.valid && slot.name_hash == key && slot.matches_name(filename) {
            return Some(slot);
        }
    }
    // buf dropped here — heap freed

    None
}

// save a bookmark for the given file. handles LRU eviction, generation
// increment, and matching by hash + full filename.
pub fn save<SPI: embedded_hal::spi::SpiDevice>(
    svc: &mut Services<'_, SPI>,
    filename: &[u8],
    byte_offset: u32,
    chapter: u16,
) {
    let key = fnv1a(filename);
    let (mut buf, slot_count) = read_file(svc);

    // scan for target slot, max generation, and LRU candidate
    let mut max_gen: u16 = 0;
    let mut target: Option<usize> = None;
    let mut first_free: Option<usize> = None;
    let mut lru_slot: usize = 0;
    let mut lru_gen: u16 = u16::MAX;

    for i in 0..slot_count {
        let slot = slot_at(&buf, i);

        if !slot.valid {
            if first_free.is_none() {
                first_free = Some(i);
            }
            continue;
        }

        if slot.generation > max_gen {
            max_gen = slot.generation;
        }
        if slot.generation < lru_gen {
            lru_gen = slot.generation;
            lru_slot = i;
        }

        if slot.name_hash == key && slot.matches_name(filename) {
            target = Some(i);
            break;
        }
    }

    let write_slot = target.or(first_free).unwrap_or(if slot_count >= SLOTS {
        lru_slot
    } else {
        slot_count
    });

    let generation = max_gen.wrapping_add(1);
    let name_len = filename.len().min(FILENAME_CAP);
    let mut new_slot = BookmarkSlot {
        name_hash: key,
        byte_offset,
        chapter,
        valid: true,
        generation,
        name_len: name_len as u8,
        filename: [0u8; FILENAME_CAP],
    };
    new_slot.filename[..name_len].copy_from_slice(&filename[..name_len]);

    // patch the slot in the buffer
    let base = write_slot * RECORD_LEN;
    let new_count = (write_slot + 1).max(slot_count);
    let file_len = new_count * RECORD_LEN;

    // ensure buf is large enough if we're appending past the old end
    if file_len > buf.len() {
        buf.resize(file_len, 0);
    }

    let rec = new_slot.encode();
    buf[base..base + RECORD_LEN].copy_from_slice(&rec);

    match svc.write_pulp(BOOKMARK_FILE, &buf[..file_len]) {
        Ok(_) => log::info!(
            "bookmark: saved off={} ch={} gen={} for {:?}",
            byte_offset,
            chapter,
            generation,
            core::str::from_utf8(filename).unwrap_or("?"),
        ),
        Err(e) => log::warn!("bookmark: save failed: {}", e),
    }
    // buf dropped here — heap freed
}
