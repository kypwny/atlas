//! KAPI packages of the newest Call of Duty (`cod26`): `.xpak` (index) and `.xsub` (data) files.
//!
//! Layout as found in the files of `Call of Duty HQ` (header version 23):
//! - `0x000`: `KAPI`, u16 3, u16 version (23), u64 1, u32 kind (2 = xpak, 3 = xsub, 4 = xpak of a CDN set), u32 (xpak: 9, else 0),
//!   u64 file size, u64 package id, u64 (xpak: number of xsub records, else 0). The rest up to `0x7a0` is zero or a signature.
//! - `0x7a0`: the entry table descriptor: u64 entry count, u64 table offset, u64 table bytes (= count * 20).
//! - `0x800`: in an xpak, the xsub records `{u64 id, u32 a, u16 index}` (14 bytes each), padded with `0xa7` to the page.
//! - the table (xsub: at the end of the file; xpak: at `0x8000`): `count` entries of 20 bytes `{u64 key, u64 packed, u32 ex}`
//!   sorted by key. `packed`: bit 0 = the payload has a block header, bits 1..30 = payload size, bits 32..54 = offset >> 7
//!   (payloads sit on 128-byte boundaries), bits 55.. = the xsub's index (xpak only; an xsub's own table has 0). `ex` is the
//!   size of the decoded payload.
//! - a payload without the bit is stored: `size == ex`. With the bit it is a series of segments, each starting with
//!   `{u16 0xf01d, u64 key, ...}`, count byte at `+0x16` and `count` 21-byte block descriptors from `+0x17`:
//!   `{u8 codec, u32 compressed, u32 raw, u32 offset (from the segment), u32 raw offset, u32 check}`. Codec 6 = Oodle, 3 = LZ4,
//!   0 = stored. Blocks sit 8-byte aligned. A segment fills one 1 MiB page of the payload (small filler blocks pad it to within
//!   16 bytes, `0xcd` after that); the next segment starts on the next page. The decoded size (`ex`) says when to stop.

use crate::oodle::{self, Oodle};
use anyhow::{Context, Result, bail};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const MAGIC: [u8; 4] = *b"KAPI";
pub const SEGMENT_MAGIC: u16 = 0xf01d;
const HEADER_SIZE: usize = 0x800;
const TABLE_DESCRIPTOR: usize = 0x7a0;
const BLOCK_DESCRIPTOR: usize = 21;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: u64,
    pub packed: u64,
    pub ex: u32,
}

impl Entry {
    /// The payload starts with segment headers (otherwise it is stored as is).
    pub fn has_blocks(&self) -> bool {
        self.packed & 1 != 0
    }
    /// Bytes the payload takes in its xsub.
    pub fn size(&self) -> u64 {
        (self.packed >> 1) & 0x3fff_ffff
    }
    pub fn offset(&self) -> u64 {
        ((self.packed >> 32) & 0x7f_ffff) << 7
    }
    /// Which record of the xpak's xsub table holds the payload (always 0 in an xsub's own table); the record has the file number.
    pub fn sub(&self) -> u32 {
        (self.packed >> 55) as u32
    }
    /// The name of the entry's key in a hash index (the low 60 bits are compared), if the index has it.
    pub fn name<'a>(&self, index: &'a crate::hashindex::HashIndex) -> Option<&'a str> {
        index.lookup(self.key)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SubRecord {
    pub id: u64,
    pub a: u32,
    pub index: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Pak,
    Sub,
    CdnPak,
    Other(u32),
}

pub struct Kapi {
    pub path: PathBuf,
    pub version: u16,
    pub kind: Kind,
    pub file_size: u64,
    pub id: u64,
    pub table_offset: u64,
    pub entries: Vec<Entry>,
    pub subs: Vec<SubRecord>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

impl Kapi {
    /// Reads the header and the entry table.
    pub fn open(path: &Path) -> Result<Kapi> {
        let mut f = std::fs::File::open(path).with_context(|| path.display().to_string())?;
        let file_size = f.metadata()?.len();
        if file_size < HEADER_SIZE as u64 {
            bail!("too short for a KAPI package ({file_size} bytes)");
        }
        let mut h = vec![0u8; HEADER_SIZE];
        f.read_exact(&mut h)?;
        if h[0..4] != MAGIC {
            bail!("not a KAPI package (magic {:02x?})", &h[0..4]);
        }
        let version = u16_at(&h, 6);
        if version != 23 {
            bail!("KAPI header version {version} (23 expected)");
        }
        let kind = match u32_at(&h, 0x10) {
            2 => Kind::Pak,
            3 => Kind::Sub,
            4 => Kind::CdnPak,
            k => Kind::Other(k),
        };
        let declared = u64_at(&h, 0x18);
        if declared != file_size {
            bail!("header says {declared} bytes, the file has {file_size}");
        }
        let (count, table_offset, table_bytes) = (u64_at(&h, TABLE_DESCRIPTOR), u64_at(&h, TABLE_DESCRIPTOR + 8), u64_at(&h, TABLE_DESCRIPTOR + 16));
        if count.checked_mul(20) != Some(table_bytes) || table_offset.checked_add(table_bytes).is_none_or(|e| e > file_size) {
            bail!("entry table out of range ({count} entries at {table_offset:#x}, {table_bytes} bytes, file {file_size})");
        }
        f.seek(SeekFrom::Start(table_offset))?;
        let mut t = vec![0u8; table_bytes as usize];
        f.read_exact(&mut t)?;
        let entries: Vec<Entry> = t.chunks_exact(20).map(|c| Entry { key: u64_at(c, 0), packed: u64_at(c, 8), ex: u32_at(c, 16) }).collect();
        let mut subs = Vec::new();
        let sub_count = u64_at(&h, 0x28);
        if kind != Kind::Sub && sub_count > 0 && sub_count <= 2340 {
            let mut s = vec![0u8; sub_count as usize * 14];
            f.seek(SeekFrom::Start(HEADER_SIZE as u64))?;
            if f.read_exact(&mut s).is_ok() {
                subs = s.chunks_exact(14).map(|c| SubRecord { id: u64_at(c, 0), a: u32_at(c, 8), index: u16_at(c, 12) }).collect();
            }
        }
        Ok(Kapi { path: path.to_path_buf(), version, kind, file_size, id: u64_at(&h, 0x20), table_offset, entries, subs })
    }

    /// The xsub that holds the payload of `e`: this file for an xsub, a sibling `<name>-NNNNN.xsub` for an xpak.
    pub fn payload_path(&self, e: &Entry) -> PathBuf {
        if self.kind == Kind::Sub {
            return self.path.clone();
        }
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        // the entry names a record of the xpak's xsub table; that record holds the file number
        let number = self.subs.get(e.sub() as usize).map_or(e.sub(), |r| r.index as u32);
        self.path.with_file_name(format!("{stem}-{number:05}.xsub"))
    }

    /// The payload bytes as stored in the xsub (block headers still in place).
    pub fn read_raw(&self, e: &Entry) -> Result<Vec<u8>> {
        let p = self.payload_path(e);
        let mut f = std::fs::File::open(&p).with_context(|| p.display().to_string())?;
        read_at(&mut f, e.offset(), e.size() as usize)
    }

    /// The decoded payload. Oodle blocks need `oodle`.
    pub fn read_entry(&self, e: &Entry, oodle: Option<&Oodle>) -> Result<Vec<u8>> {
        let raw = self.read_raw(e)?;
        decode_entry(&raw, e, oodle)
    }
}

pub fn read_at(f: &mut std::fs::File, offset: u64, len: usize) -> Result<Vec<u8>> {
    f.seek(SeekFrom::Start(offset))?;
    let mut v = vec![0u8; len];
    f.read_exact(&mut v).with_context(|| format!("reading {len} bytes at {offset:#x}"))?;
    Ok(v)
}

/// What a payload's block headers say.
#[derive(Debug, Default, Clone)]
pub struct PayloadInfo {
    pub segments: usize,
    pub blocks: usize,
    /// Blocks per codec number (index = codec byte).
    pub codecs: [usize; 8],
    pub compressed: u64,
    pub raw: u64,
}

/// One block descriptor of a segment.
#[derive(Clone, Copy, Debug)]
pub struct Block {
    pub codec: u8,
    pub comp: u32,
    pub raw: u32,
    /// Offset of the compressed data from the start of the segment.
    pub off: u32,
    /// Offset of the block's output in the decoded payload.
    pub raw_off: u32,
    pub check: u32,
}

/// Longest segment header: 255 descriptors.
pub const MAX_SEGMENT_HEADER: usize = 0x17 + 255 * BLOCK_DESCRIPTOR;

fn align(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

/// Parses the header of the segment that starts `buf`.
pub fn parse_segment(buf: &[u8], key: u64) -> Result<Vec<Block>> {
    let h = buf.get(..0x18).context("segment header runs past the payload")?;
    if u16_at(h, 0) != SEGMENT_MAGIC || u64_at(h, 2) != key {
        bail!("bad segment header (magic {:#06x}, key {:#018x})", u16_at(h, 0), u64_at(h, 2));
    }
    let count = h[0x16] as usize;
    if count == 0 {
        bail!("segment without blocks");
    }
    let descs = buf.get(0x17..0x17 + count * BLOCK_DESCRIPTOR).context("block descriptors run past the payload")?;
    Ok(descs
        .chunks_exact(BLOCK_DESCRIPTOR)
        .map(|d| Block { codec: d[0], comp: u32_at(d, 1), raw: u32_at(d, 5), off: u32_at(d, 9), raw_off: u32_at(d, 13), check: u32_at(d, 17) })
        .collect())
}

/// Segments start on 1 MiB pages of the payload; a page is filled up to the last 16 bytes with small filler blocks, the rest is `0xcd`.
const PAGE: usize = 0x10_0000;

fn account(info: &mut PayloadInfo, blocks: &[Block], seg_start: usize, raw_done: &mut u64) -> Result<usize> {
    let mut end = seg_start + align(0x17 + blocks.len() * BLOCK_DESCRIPTOR, 4);
    for b in blocks {
        if b.raw_off as u64 != *raw_done {
            bail!("block at raw offset {:#x} where {:#x} was expected", b.raw_off, *raw_done);
        }
        end = end.max(seg_start + b.off as usize + b.comp as usize);
        *raw_done += b.raw as u64;
        info.blocks += 1;
        if let Some(c) = info.codecs.get_mut(b.codec as usize) {
            *c += 1;
        }
        info.compressed += b.comp as u64;
    }
    info.segments += 1;
    Ok(end)
}

/// Where the next segment can start after a segment that ends at `end`: the next 16-byte boundary or the next page boundary.
fn next_candidates(end: usize) -> [usize; 2] {
    [align(end, 16), align(end, PAGE)]
}

/// Walks the segments of a payload that has block headers and hands every block to `each(codec, compressed bytes, raw size)`.
/// It stops after `ex` raw bytes (what is left of the payload is padding).
pub fn walk_blocks(payload: &[u8], key: u64, ex: u64, mut each: impl FnMut(u8, &[u8], usize) -> Result<()>) -> Result<PayloadInfo> {
    let mut info = PayloadInfo::default();
    let mut seg = 0usize;
    let mut raw_done = 0u64;
    while raw_done < ex {
        let blocks = parse_segment(payload.get(seg..).unwrap_or(&[]), key).with_context(|| format!("segment at {seg:#x} (raw {raw_done:#x} of {ex:#x})"))?;
        for b in &blocks {
            let start = seg + b.off as usize;
            let data = payload.get(start..start + b.comp as usize).with_context(|| format!("block data {start:#x}+{:#x} runs past the payload", b.comp))?;
            each(b.codec, data, b.raw as usize)?;
        }
        let end = account(&mut info, &blocks, seg, &mut raw_done).with_context(|| format!("segment at {seg:#x}"))?;
        seg = next_candidates(end).into_iter().find(|&c| parse_segment(payload.get(c..).unwrap_or(&[]), key).is_ok()).unwrap_or(align(end, 16));
    }
    info.raw = raw_done;
    Ok(info)
}

/// Checks the block headers of an entry in its file without reading any block data (a few KiB per segment).
pub fn payload_info_file(f: &mut std::fs::File, e: &Entry) -> Result<PayloadInfo> {
    if !e.has_blocks() {
        return Ok(PayloadInfo::default());
    }
    let size = e.size() as usize;
    let ex = e.ex as u64;
    let mut info = PayloadInfo::default();
    let (mut seg, mut raw_done) = (0usize, 0u64);
    while raw_done < ex {
        let buf = read_at(f, e.offset() + seg as u64, MAX_SEGMENT_HEADER.min(size.saturating_sub(seg)))?;
        let blocks = parse_segment(&buf, e.key).with_context(|| format!("segment at {seg:#x} (raw {raw_done:#x} of {ex:#x})"))?;
        let end = account(&mut info, &blocks, seg, &mut raw_done).with_context(|| format!("segment at {seg:#x}"))?;
        if end > size {
            bail!("blocks of the segment at {seg:#x} end at {end:#x}, the payload is {size:#x} bytes");
        }
        let mut next = align(end, 16);
        for c in next_candidates(end) {
            if c + 0x18 <= size && read_at(f, e.offset() + c as u64, 0x18).is_ok_and(|h| u16_at(&h, 0) == SEGMENT_MAGIC && u64_at(&h, 2) == e.key) {
                next = c;
                break;
            }
        }
        seg = next;
    }
    info.raw = raw_done;
    if raw_done != ex {
        bail!("blocks add up to {raw_done} raw bytes, the table says {ex}");
    }
    Ok(info)
}

/// Header-only view of a payload (no decoding).
pub fn payload_info(payload: &[u8], e: &Entry) -> Result<PayloadInfo> {
    walk_blocks(payload, e.key, e.ex as u64, |_, _, _| Ok(()))
}

/// Decodes a payload fully. A stored payload is returned as is.
pub fn decode_entry(payload: &[u8], e: &Entry, oodle: Option<&Oodle>) -> Result<Vec<u8>> {
    if !e.has_blocks() {
        return Ok(payload.to_vec());
    }
    let mut out = Vec::with_capacity(e.ex as usize + 256);
    let info = walk_blocks(payload, e.key, e.ex as u64, |codec, data, raw| match codec {
        0 => {
            if data.len() != raw {
                bail!("stored block of {} bytes for {raw}", data.len());
            }
            out.extend_from_slice(data);
            Ok(())
        }
        3 => oodle::lz4_block_append(data, raw, &mut out),
        6 => oodle.context("an Oodle block needs the game's oo2core DLL")?.decompress_append(data, raw, &mut out),
        c => bail!("unknown block codec {c}"),
    })?;
    if info.raw != e.ex as u64 || out.len() as u64 != info.raw {
        bail!("blocks give {} bytes, the table says {}", out.len(), e.ex);
    }
    Ok(out)
}

/// Totals of one package, for the survey.
#[derive(Debug, Default, Clone)]
pub struct PackageStats {
    pub entries: usize,
    pub with_blocks: usize,
    pub stored: usize,
    pub stored_bytes: u64,
    pub packed_bytes: u64,
    pub ex_bytes: u64,
    pub unsorted: bool,
    pub duplicate_keys: usize,
    /// Entries per xsub index.
    pub per_sub: std::collections::BTreeMap<u32, usize>,
    pub out_of_range: usize,
}

impl Kapi {
    pub fn stats(&self) -> PackageStats {
        let mut s = PackageStats { entries: self.entries.len(), ..Default::default() };
        let mut prev: Option<u64> = None;
        for e in &self.entries {
            if e.has_blocks() {
                s.with_blocks += 1;
                s.packed_bytes += e.size();
            } else {
                s.stored += 1;
                s.stored_bytes += e.size();
            }
            s.ex_bytes += e.ex as u64;
            *s.per_sub.entry(e.sub()).or_default() += 1;
            if let Some(p) = prev {
                if e.key < p {
                    s.unsorted = true;
                } else if e.key == p {
                    s.duplicate_keys += 1;
                }
            }
            prev = Some(e.key);
            if self.kind == Kind::Sub && e.offset() + e.size() > self.file_size {
                s.out_of_range += 1;
            }
        }
        s
    }
}
