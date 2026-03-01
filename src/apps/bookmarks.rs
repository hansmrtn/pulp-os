// RAM-resident bookmark cache: 16 slots x 48 bytes held permanently in RAM (~780B).
// Loaded from SD once; reads served from RAM with zero SD I/O.
// Writes mark dirty; flush() persists to SD only when dirty.
//
// Record layout (little-endian, 48 bytes per slot):
//   [0..4)   name_hash    u32
//   [4..8)   byte_offset  u32   font-independent file/chapter position
//   [8..10)  chapter      u16   epub chapter; 0 for txt
//   [10..12) flags        u16   bit 0 = valid
//   [12..14) generation   u16   LRU counter (higher = more recent)
//   [14]     name_len     u8
//   [15]     _pad         u8
//   [16..48) filename     [u8;32]

use crate::drivers::sdcard::SdStorage;
use crate::drivers::storage;
pub use smol_epub::cache::fnv1a;

pub const BOOKMARK_FILE: &str = "BKMK.BIN";
pub const SLOTS: usize = 16;
pub const RECORD_LEN: usize = 48;
pub const FILE_LEN: usize = SLOTS * RECORD_LEN; // 768B
pub const FILENAME_CAP: usize = 32;

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

// lightweight bookmark list entry (35B vs 48B for BookmarkSlot)
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

// in-memory bookmark table; loaded once, reads from RAM, writes mark dirty (~780B)
pub struct BookmarkCache {
    slots: [BookmarkSlot; SLOTS],
    count: usize, // slots present in file; new saves past this extend count
    dirty: bool,
    loaded: bool,
}

impl Default for BookmarkCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BookmarkCache {
    pub const fn new() -> Self {
        Self {
            slots: [BookmarkSlot::EMPTY; SLOTS],
            count: 0,
            dirty: false,
            loaded: false,
        }
    }

    // true if in-memory state has changed since the last flush
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    // read bookmark file from SD; idempotent; no-op if already loaded
    pub fn ensure_loaded<SPI: embedded_hal::spi::SpiDevice>(&mut self, sd: &SdStorage<SPI>) {
        if self.loaded {
            return;
        }
        self.force_load(sd);
    }

    // reload from SD, discarding in-memory changes
    pub fn force_load<SPI: embedded_hal::spi::SpiDevice>(&mut self, sd: &SdStorage<SPI>) {
        let mut buf = [0u8; FILE_LEN];
        let slot_count = match storage::read_pulp_file_start(sd, BOOKMARK_FILE, &mut buf) {
            Ok((_, n)) => (n / RECORD_LEN).min(SLOTS),
            Err(_) => 0,
        };

        for i in 0..slot_count {
            let base = i * RECORD_LEN;
            self.slots[i] = BookmarkSlot::decode(&buf[base..base + RECORD_LEN]);
        }
        for i in slot_count..SLOTS {
            self.slots[i] = BookmarkSlot::EMPTY;
        }

        self.count = slot_count;
        self.dirty = false;
        self.loaded = true;

        log::info!("bookmarks: loaded {} slots from SD", slot_count);
    }

    // find bookmark by filename; None if not found or not loaded
    pub fn find(&self, filename: &[u8]) -> Option<BookmarkSlot> {
        if !self.loaded {
            return None;
        }

        let key = fnv1a(filename);
        for i in 0..self.count {
            let slot = &self.slots[i];
            if slot.valid && slot.name_hash == key && slot.matches_name(filename) {
                return Some(*slot);
            }
        }
        None
    }

    // copy valid bookmarks into out, sorted by generation descending; return count written
    pub fn load_all(&self, out: &mut [BmListEntry]) -> usize {
        if !self.loaded {
            return 0;
        }

        // collect valid entries with their generation for sorting
        let mut gens = [0u16; SLOTS];
        let mut count = 0usize;

        for i in 0..self.count {
            if count >= out.len() {
                break;
            }
            let slot = &self.slots[i];
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

        // insertion sort by generation descending (most recent first)
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

    // save bookmark; update cache + mark dirty; call flush() to persist.
    // handles LRU eviction, generation increment, hash+name matching.
    pub fn save(&mut self, filename: &[u8], byte_offset: u32, chapter: u16) {
        if !self.loaded {
            log::warn!("bookmarks: save called before load, ignoring");
            return;
        }

        let key = fnv1a(filename);

        // scan for: target slot, max generation, first free, LRU
        let mut max_gen: u16 = 0;
        let mut target: Option<usize> = None;
        let mut first_free: Option<usize> = None;
        let mut lru_slot: usize = 0;
        let mut lru_gen: u16 = u16::MAX;

        for i in 0..self.count {
            let slot = &self.slots[i];

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

        let write_slot = target.or(first_free).unwrap_or(if self.count >= SLOTS {
            lru_slot
        } else {
            self.count
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

        self.slots[write_slot] = new_slot;

        // extend count if we wrote past the current end
        if write_slot >= self.count {
            self.count = write_slot + 1;
        }

        self.dirty = true;

        log::info!(
            "bookmark: cached off={} ch={} gen={} for {:?}",
            byte_offset,
            chapter,
            generation,
            core::str::from_utf8(filename).unwrap_or("?"),
        );
    }

    // write cache to SD if dirty; no-op if clean; 768B stack buffer
    pub fn flush<SPI: embedded_hal::spi::SpiDevice>(&mut self, sd: &SdStorage<SPI>) {
        if !self.dirty || !self.loaded {
            return;
        }

        let file_len = self.count * RECORD_LEN;
        let mut buf = [0u8; FILE_LEN];

        for i in 0..self.count {
            let base = i * RECORD_LEN;
            let rec = self.slots[i].encode();
            buf[base..base + RECORD_LEN].copy_from_slice(&rec);
        }

        match storage::write_pulp_file(sd, BOOKMARK_FILE, &buf[..file_len]) {
            Ok(_) => {
                self.dirty = false;
                log::info!("bookmarks: flushed {} slots to SD", self.count);
            }
            Err(e) => {
                log::warn!("bookmarks: flush failed: {}", e);
                // leave dirty=true so we retry next time
            }
        }
    }
}
