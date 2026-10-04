// host shim of kernel/src/drivers/storage.rs: same function names and
// signatures handle.rs / bookmarks.rs call, backed by the in-memory FakeFs.
use crate::drivers::sdcard::SdStorage;
use crate::error::Error;

pub const PULP_DIR: &str = "_PULP";
pub const TITLES_FILE: &str = "TITLES.BIN";
pub const TITLE_CAP: usize = 64;

pub type StorageError = Error;

#[derive(Clone, Copy)]
pub struct DirEntry;

pub struct DirPage {
    pub total: usize,
    pub count: usize,
}

type R<T> = crate::error::Result<T>;

fn sub(dir: &str) -> String {
    format!("{}/{}", PULP_DIR, dir)
}

// root
pub fn file_size(sd: &SdStorage, name: &str) -> R<u32> {
    sd.run_read(|fs| fs.size("", name))
}
pub fn read_file_chunk(sd: &SdStorage, name: &str, offset: u32, buf: &mut [u8]) -> R<usize> {
    sd.run_read(|fs| fs.read("", name, offset, buf))
}
pub fn read_file_start(sd: &SdStorage, name: &str, buf: &mut [u8]) -> R<(u32, usize)> {
    sd.run_read(|fs| fs.read_start("", name, buf))
}
pub fn delete_file(sd: &SdStorage, name: &str) -> R<()> {
    sd.run(|fs| fs.delete("", name))
}

// single-directory (used with PULP_DIR)
pub fn write_file_in_dir(sd: &SdStorage, dir: &str, name: &str, data: &[u8]) -> R<()> {
    sd.run(|fs| fs.write(dir, name, data))
}
pub fn append_file_in_dir(sd: &SdStorage, dir: &str, name: &str, data: &[u8]) -> R<()> {
    sd.run(|fs| fs.append(dir, name, data))
}
pub fn read_file_start_in_dir(sd: &SdStorage, dir: &str, name: &str, buf: &mut [u8]) -> R<(u32, usize)> {
    sd.run_read(|fs| fs.read_start(dir, name, buf))
}

// _PULP subdirectories
pub fn ensure_pulp_subdir(sd: &SdStorage, name: &str) -> R<()> {
    sd.run(|fs| {
        fs.mkdir(&sub(name));
        Ok(())
    })
}
pub fn write_in_pulp_subdir(sd: &SdStorage, dir: &str, name: &str, data: &[u8]) -> R<()> {
    sd.run(|fs| fs.write(&sub(dir), name, data))
}
pub fn append_in_pulp_subdir(sd: &SdStorage, dir: &str, name: &str, data: &[u8]) -> R<()> {
    sd.run(|fs| fs.append(&sub(dir), name, data))
}
pub fn read_chunk_in_pulp_subdir(sd: &SdStorage, dir: &str, name: &str, offset: u32, buf: &mut [u8]) -> R<usize> {
    sd.run_read(|fs| fs.read(&sub(dir), name, offset, buf))
}
pub fn file_size_in_pulp_subdir(sd: &SdStorage, dir: &str, name: &str) -> R<u32> {
    sd.run_read(|fs| fs.size(&sub(dir), name))
}
pub fn delete_in_pulp_subdir(sd: &SdStorage, dir: &str, name: &str) -> R<()> {
    sd.run(|fs| fs.delete(&sub(dir), name))
}

// _PULP/ direct files
pub fn read_chunk_in_pulp(sd: &SdStorage, name: &str, offset: u32, buf: &mut [u8]) -> R<usize> {
    sd.run_read(|fs| fs.read(PULP_DIR, name, offset, buf))
}
pub fn write_in_pulp(sd: &SdStorage, name: &str, data: &[u8]) -> R<()> {
    sd.run(|fs| fs.write(PULP_DIR, name, data))
}
pub fn append_in_pulp(sd: &SdStorage, name: &str, data: &[u8]) -> R<()> {
    sd.run(|fs| fs.append(PULP_DIR, name, data))
}
pub fn file_size_in_pulp(sd: &SdStorage, name: &str) -> R<u32> {
    sd.run_read(|fs| fs.size(PULP_DIR, name))
}
pub fn delete_in_pulp(sd: &SdStorage, name: &str) -> R<()> {
    sd.run(|fs| fs.delete(PULP_DIR, name))
}
pub fn write_at_in_pulp(sd: &SdStorage, name: &str, offset: u32, data: &[u8]) -> R<()> {
    sd.run(|fs| fs.write_at(PULP_DIR, name, offset, data))
}

pub fn save_title(sd: &SdStorage, filename: &str, title: &str) -> R<()> {
    let mut line = Vec::new();
    line.extend_from_slice(filename.as_bytes());
    line.push(b'\t');
    line.extend_from_slice(&title.as_bytes()[..title.len().min(TITLE_CAP)]);
    line.push(b'\n');
    append_file_in_dir(sd, PULP_DIR, TITLES_FILE, &line)
}
