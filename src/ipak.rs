//! IPAK reader (Black Ops II texture archives, `zone/all/*.ipak`).
//!
//! Layout, as found in the shipped files:
//! ```text
//! header (16): "IPAK", version 0x50000, file size, section count
//! sections (16 each): type, offset, size, count      type 1 = index, 2 = data, 3 and 4 = further tables
//! index entry (16): data checksum, name hash, offset in the data section, stored size
//! ```
//! The index is sorted by name hash. Names are not stored, only a hash of the image name, so images are shown by
//! hash. Entries with the same hash but different checksums are different versions of one image.
//!
//! The data section is a series of 0x80-aligned blocks. A block starts with a 128-byte header: one word
//! (low 24 bits = where in the image this block's data goes, high 8 bits = command count, at most 31) followed by
//! one word per command (low 24 bits = stored length, high 8 bits = kind: 0 raw, 1 LZO1X, other values carry
//! no image data). The commands' data follows the header back to back. An image is rebuilt by placing each
//! command's bytes at the running position; the result is a complete IWI file (version 27).

use crate::lzo;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Mutex;

const MAGIC: u32 = 0x4950_414B; // the bytes "KAPI" read as a little-endian word
const BLOCK_ALIGN: u64 = 0x80;
const BLOCK_HEADER: usize = 128;
const MAX_COMMANDS: usize = 31;
/// No command expands to more than this.
const MAX_CHUNK: usize = 0x40000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpakEntry {
    pub checksum: u32,
    pub name_hash: u32,
    pub offset: u32,
    pub size: u32,
}

pub struct Ipak {
    file: Mutex<File>,
    data_offset: u64,
    pub entries: Vec<IpakEntry>,
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

impl Ipak {
    pub fn open(path: &Path) -> Result<Self> {
        let mut f = File::open(path).with_context(|| path.display().to_string())?;
        let len = f.metadata()?.len();
        let mut head = [0u8; 16];
        f.read_exact(&mut head).context("file too short for an IPAK header")?;
        if u32_at(&head, 0) != MAGIC {
            bail!("not an IPAK file");
        }
        let sections = u32_at(&head, 12) as usize;
        if sections == 0 || sections > 64 {
            bail!("implausible IPAK section count {sections}");
        }
        let (mut index, mut data) = (None, None);
        for k in 0..sections {
            let mut s = [0u8; 16];
            f.seek(SeekFrom::Start(16 + 16 * k as u64))?;
            f.read_exact(&mut s).context("IPAK section table is cut off")?;
            match u32_at(&s, 0) {
                1 => index = Some((u32_at(&s, 4) as u64, u32_at(&s, 12) as usize)),
                2 => data = Some(u32_at(&s, 4) as u64),
                _ => {}
            }
        }
        let (Some((index_offset, count)), Some(data_offset)) = (index, data) else { bail!("IPAK has no index or data section") };
        if index_offset + 16 * count as u64 > len || data_offset > len {
            bail!("IPAK sections lie outside the file");
        }
        f.seek(SeekFrom::Start(index_offset))?;
        let mut raw = vec![0u8; 16 * count];
        f.read_exact(&mut raw)?;
        let entries = raw
            .chunks_exact(16)
            .map(|c| IpakEntry { checksum: u32_at(c, 0), name_hash: u32_at(c, 4), offset: u32_at(c, 8), size: u32_at(c, 12) })
            .collect();
        Ok(Self { file: Mutex::new(f), data_offset, entries })
    }

    /// Rebuilds the image stored for `entry` (a complete IWI file).
    pub fn read(&self, entry: &IpakEntry) -> Result<Vec<u8>> {
        self.read_prefix(entry, usize::MAX)
    }

    /// Like [`read`](Self::read) but stops once at least `limit` bytes are rebuilt (enough for a header), which
    /// skips decompressing the rest of the image.
    pub fn read_prefix(&self, entry: &IpakEntry, limit: usize) -> Result<Vec<u8>> {
        let mut f = self.file.lock().unwrap();
        let start = self.data_offset + entry.offset as u64;
        let end = start + entry.size as u64;
        let mut pos = start;
        let mut out: Vec<u8> = Vec::new();
        let mut hdr = [0u8; BLOCK_HEADER];
        loop {
            pos = pos.div_ceil(BLOCK_ALIGN) * BLOCK_ALIGN;
            if pos >= end {
                break;
            }
            f.seek(SeekFrom::Start(pos))?;
            f.read_exact(&mut hdr).context("IPAK block header is cut off")?;
            let word = u32_at(&hdr, 0);
            let mut cur = (word & 0xFF_FFFF) as usize;
            let count = (word >> 24) as usize;
            if count == 0 || count > MAX_COMMANDS {
                bail!("corrupt IPAK block at {pos:#x} (command count {count})");
            }
            let mut p = pos + BLOCK_HEADER as u64;
            for k in 0..count {
                let cmd = u32_at(&hdr, 4 + 4 * k);
                let stored = (cmd & 0xFF_FFFF) as usize;
                let kind = cmd >> 24;
                if kind <= 1 {
                    let mut chunk = vec![0u8; stored];
                    f.seek(SeekFrom::Start(p))?;
                    f.read_exact(&mut chunk).context("IPAK data is cut off")?;
                    let piece = if kind == 0 { chunk } else { lzo::decompress(&chunk, MAX_CHUNK)? };
                    if out.len() < cur {
                        out.resize(cur, 0);
                    }
                    let to = cur + piece.len();
                    if out.len() < to {
                        out.resize(to, 0);
                    }
                    out[cur..to].copy_from_slice(&piece);
                    cur = to;
                }
                p += stored as u64;
            }
            pos = p;
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A one-image archive: a raw command followed by an LZO command, so both kinds are exercised.
    fn tiny() -> Vec<u8> {
        let mut f = vec![0u8; 0x210];
        let mut put = |at: usize, words: &[u32]| {
            for (i, w) in words.iter().enumerate() {
                f[at + 4 * i..at + 4 * i + 4].copy_from_slice(&w.to_le_bytes());
            }
        };
        put(0, &[MAGIC, 0x50000, 0x210, 2]);
        put(16, &[1, 0x200, 16, 1]); // index
        put(32, &[2, 0x80, 139, 1]); // data
        put(0x200, &[0xAABB_CCDD, 0x1234_5678, 0, 139]); // checksum, name hash, offset, stored size
        put(0x80, &[2 << 24, 4, (1 << 24) | 7]); // block: 2 commands at output offset 0
        f[0x80 + 128..0x80 + 132].copy_from_slice(b"IWi\x1b");
        f[0x80 + 132..0x80 + 139].copy_from_slice(&[20, b'a', b'b', b'c', 0x11, 0, 0]);
        f
    }

    #[test]
    fn rebuilds_an_image_from_raw_and_lzo_commands() {
        let path = std::env::temp_dir().join(format!("codol-tiny-{}.ipak", std::process::id()));
        std::fs::File::create(&path).unwrap().write_all(&tiny()).unwrap();
        let ipak = Ipak::open(&path).unwrap();
        assert_eq!(ipak.entries, vec![IpakEntry { checksum: 0xAABB_CCDD, name_hash: 0x1234_5678, offset: 0, size: 139 }]);
        assert_eq!(ipak.read(&ipak.entries[0]).unwrap(), b"IWi\x1babc");
        // asking for a short prefix stops after the first block, which here holds everything
        assert_eq!(ipak.read_prefix(&ipak.entries[0], 4).unwrap(), b"IWi\x1babc");
        drop(ipak);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_other_files() {
        let path = std::env::temp_dir().join(format!("codol-notipak-{}.ipak", std::process::id()));
        std::fs::write(&path, vec![0u8; 64]).unwrap();
        assert!(Ipak::open(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
