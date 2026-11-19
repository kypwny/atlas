//! Fastfiles of the newest Call of Duty (`cod26`, `Call of Duty HQ`): container and inventory only.
//!
//! What the files of the install look like (all 3,264 `.ff` share it):
//! - Header: `IWffa100`, u32 version 25, u32 1, u32 `0x00040001`, u32 payload size (file size minus the offset of the magic below),
//!   then a table of u64 words (image size at `0x38`, block sizes at `0x50`.. `0xc0`, ...) and a `TAFA` section; its length varies
//!   (`0x32c` .. over `0x30000` bytes).
//! - The payload starts at the word `0x43574902`, followed by the signature header (`IWffs100`, 0x100 bytes of signature, a name
//!   at `+0x12c`) that fills `0x8000` bytes. Then a preamble `{u32 image size, u32 codec << 24 | block size}` (codec 6 = Oodle,
//!   block size `0x10000`) and the blocks `{u32 compressed, u32 raw, u32 0}` + data, each 4-byte aligned. After the 511th block and
//!   then after every 512 blocks, `0x4000` bytes of signature data sit between two blocks.
//! - The blocks inflate to the zone image: 0x30 header bytes (`+0x10` u32 asset count, `+0x18` u64 pointer, `+0x20` u32, `+0x24`
//!   u32 asset count, `+0x28` u64 pointer), then the asset list: 16-byte records `{u64 type, u64 pointer}` (type = a 32-bit value,
//!   pointer = `-1`, the data follows in the stream), then the asset data. Rawfile assets hold zlib streams (`78 da`).
//! - Zones whose first magic is not `IWffa100` (`IWff0100` DLC zones, `IWffd100` patches) are refused; this build has none of
//!   the first kind.
//!
//! Oodle comes from the game's `oo2core_8_win64.dll` ([`crate::oodle`]); nothing is decrypted.

use crate::hashindex::{self, HashIndex};
use crate::kapi::{self, Kapi, Kind};
use crate::oodle::{self, Oodle};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 8] = b"IWffa100";
pub const VERSION: u32 = 25;
const PAYLOAD_MAGIC: [u8; 4] = [0x02, 0x49, 0x57, 0x43];
const SIGNATURE_SIZE: usize = 0x8000;
const GAP_SIZE: usize = 0x4000;
const FIRST_GAP_AFTER: usize = 511;
const GAP_EVERY: usize = 512;
const NAME_OFFSET: usize = 0x12c;

/// The input is a container this reader leaves alone (encrypted, patch, other flavour).
#[derive(Debug)]
pub struct Refused(pub String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "refused: {}", self.0)
    }
}

impl std::error::Error for Refused {}

fn refuse<T>(why: impl Into<String>) -> Result<T> {
    Err(Refused(why.into()).into())
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// The fixed part of the file header.
#[derive(Debug, Clone)]
pub struct Header {
    pub version: u32,
    pub word_0c: u32,
    pub word_10: u32,
    /// Declared bytes from the payload magic to the end of the file.
    pub payload_size: u32,
    /// Offset of the payload magic.
    pub magic_pos: usize,
    /// Non-zero u64 words of `0x18..0x100`.
    pub words: Vec<(usize, u64)>,
}

pub struct Zone {
    pub header: Header,
    /// Name in the signature header (`+0x12c`), when there is a printable one.
    pub signature_name: Option<String>,
    pub image: Vec<u8>,
    pub codec: u8,
    pub block_size: u32,
    pub blocks: usize,
    pub stored_blocks: usize,
    pub signature_gaps: usize,
    pub payload_bytes: u64,
    /// Bytes after the last block.
    pub trailer: usize,
    /// Things that are odd but did not stop the read.
    pub notes: Vec<String>,
}

fn entropy(b: &[u8]) -> f64 {
    if b.is_empty() {
        return 0.0;
    }
    let mut c = [0usize; 256];
    for &x in b {
        c[x as usize] += 1;
    }
    let n = b.len() as f64;
    -c.iter().filter(|&&v| v > 0).map(|&v| (v as f64 / n) * (v as f64 / n).log2()).sum::<f64>()
}

fn find_payload(b: &[u8]) -> Option<usize> {
    let limit = b.len().min(16 << 20);
    let mut fallback = None;
    let mut i = 0;
    while i + 8 <= limit {
        match b[i..limit].iter().position(|&c| c == PAYLOAD_MAGIC[0]) {
            None => break,
            Some(p) => i += p,
        }
        if i + 8 > b.len() {
            break;
        }
        if b[i..i + 4] == PAYLOAD_MAGIC {
            if &b[i + 4..i + 8] == b"IWff" {
                return Some(i);
            }
            fallback.get_or_insert(i);
        }
        i += 1;
    }
    fallback
}

/// Reads a zone: the container, the blocks (Oodle through `oodle`), and the image.
pub fn open_bytes(bytes: &[u8], oodle: Option<&Oodle>) -> Result<Zone> {
    if bytes.len() < 0x40 {
        bail!("too short for a fastfile ({} bytes)", bytes.len());
    }
    match &bytes[..8] {
        m if m == MAGIC => {}
        b"IWff0100" => return refuse("IWff0100: a signed/encrypted DLC zone (not decrypted)"),
        b"IWffd100" => return refuse("IWffd100: a patch (.fp) container, not a zone"),
        m => return refuse(format!("unknown fastfile magic {:?}", String::from_utf8_lossy(m))),
    }
    let version = u32_at(bytes, 8);
    if version != VERSION {
        return refuse(format!("fastfile version {version} (25 expected)"));
    }
    let magic_pos = find_payload(bytes).context("no payload magic 0x43574902 in the file")?;
    let mut words = Vec::new();
    for o in (0x18..0x100.min(magic_pos)).step_by(8) {
        let v = u64_at(bytes, o);
        if v != 0 {
            words.push((o, v));
        }
    }
    let header = Header { version, word_0c: u32_at(bytes, 0x0c), word_10: u32_at(bytes, 0x10), payload_size: u32_at(bytes, 0x14), magic_pos, words };
    let mut notes = Vec::new();
    if header.payload_size as usize != bytes.len() - magic_pos {
        notes.push(format!("payload size word {:#x} != file size - magic offset {:#x}", header.payload_size, bytes.len() - magic_pos));
    }
    let mut pos = magic_pos + 4;
    let mut signature_name = None;
    if &bytes[pos..pos + 4] == b"IWff" {
        if &bytes[pos..pos + 8] != b"IWffs100" {
            return refuse(format!("signature header {:?}", String::from_utf8_lossy(&bytes[pos..pos + 8])));
        }
        let at = pos + NAME_OFFSET;
        if let Some(n) = bytes.get(at..at + 0x40) {
            let end = n.iter().position(|&c| c == 0).unwrap_or(n.len());
            if end > 0 && n[..end].iter().all(|&c| c.is_ascii_graphic()) {
                signature_name = Some(String::from_utf8_lossy(&n[..end]).into_owned());
            }
        }
        pos += SIGNATURE_SIZE;
    }
    if pos + 8 > bytes.len() {
        bail!("file ends inside the signature header");
    }
    let total = u32_at(bytes, pos) as usize;
    let word = u32_at(bytes, pos + 4);
    let (codec, block_size) = ((word >> 24) as u8, word & 0xff_ffff);
    pos += 8;
    if block_size == 0 || block_size > 0x100000 {
        // no plausible block size: looks like data that is not a block stream
        if entropy(&bytes[pos.min(bytes.len())..(pos + 4096).min(bytes.len())]) > 7.8 {
            return refuse("the data after the signature header is high-entropy and has no block headers: encrypted or signed zone");
        }
        bail!("implausible block size {block_size:#x}");
    }
    if !matches!(codec, 0 | 3 | 6) {
        bail!("unknown block codec {codec}");
    }
    let mut image: Vec<u8> = Vec::new();
    image.try_reserve(total + 256).map_err(|_| anyhow::anyhow!("cannot allocate {total} bytes"))?;
    let (mut blocks, mut stored_blocks, mut gaps, mut payload_bytes) = (0usize, 0usize, 0usize, 0u64);
    let mut nonzero_word = 0usize;
    while image.len() < total {
        if blocks == FIRST_GAP_AFTER || (blocks > FIRST_GAP_AFTER && (blocks - FIRST_GAP_AFTER) % GAP_EVERY == 0) {
            pos += GAP_SIZE;
            gaps += 1;
        }
        let Some(h) = bytes.get(pos..pos + 12) else { bail!("file ends inside the block header of block {blocks} (image {}/{total})", image.len()) };
        let (comp, raw, z) = (u32_at(h, 0) as usize, u32_at(h, 4) as usize, u32_at(h, 8));
        if raw == 0 || raw > block_size as usize || comp == 0 || comp > raw + 64 {
            let tail = &bytes[(pos).min(bytes.len())..(pos + 4096).min(bytes.len())];
            if blocks == 0 && entropy(tail) > 7.8 {
                return refuse("the blocks do not parse and the data is high-entropy: encrypted zone");
            }
            bail!("bad block header at {pos:#x} (block {blocks}: compressed {comp:#x}, raw {raw:#x})");
        }
        if z != 0 {
            nonzero_word += 1;
        }
        let Some(data) = bytes.get(pos + 12..pos + 12 + comp) else { bail!("block {blocks} runs past the end of the file") };
        if comp == raw {
            image.extend_from_slice(data);
            stored_blocks += 1;
        } else {
            match codec {
                6 => oodle.context("an Oodle zone needs the game's oo2core DLL")?.decompress_append(data, raw, &mut image).with_context(|| format!("block {blocks} at {pos:#x}"))?,
                3 => oodle::lz4_block_append(data, raw, &mut image).with_context(|| format!("block {blocks} at {pos:#x}"))?,
                _ => bail!("block {blocks} is compressed but the preamble says codec {codec}"),
            }
        }
        payload_bytes += comp as u64;
        blocks += 1;
        pos = (pos + 12 + comp + 3) & !3;
    }
    if image.len() != total {
        notes.push(format!("image is {} bytes, the preamble says {total}", image.len()));
    }
    if nonzero_word > 0 {
        notes.push(format!("{nonzero_word} block headers have a non-zero third word"));
    }
    if let Some(&(_, v)) = header.words.iter().find(|(o, _)| *o == 0x38) {
        if v as usize != image.len() {
            notes.push(format!("header size word {v:#x} != image {:#x}", image.len()));
        }
    }
    let trailer = bytes.len().saturating_sub(pos);
    Ok(Zone { header, signature_name, image, codec, block_size, blocks, stored_blocks, signature_gaps: gaps, payload_bytes, trailer, notes })
}

pub fn open(path: &Path, oodle: Option<&Oodle>) -> Result<Zone> {
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    open_bytes(&bytes, oodle)
}

/// The asset list at the start of the image.
///
/// The image header holds counted arrays: `+0x00` u32 count of u64 pointers (`0x70..` = data follows) with a pointer at `+0x08`,
/// `+0x10` u32 asset count with a pointer at `+0x18`, `+0x20` u32 and `+0x24` u32 (the asset count again) with a pointer at
/// `+0x28`. When there are no pointers (small zones) the assets are 16-byte records `{u64 type, u64 pointer}` from `+0x30`. When
/// there are, the pointers fill `+0x30` onwards and the assets follow them as a compact stream of variable-length items
/// (`81 xx ...`) that this reader does not decode: only the counts are known.
#[derive(Debug, Clone, Default)]
pub struct AssetList {
    /// u32 at `+0x00`: number of 8-byte pointers right after the header (0 for plain lists).
    pub pointer_count: u32,
    pub count: u32,
    pub count_b: u32,
    pub word_20: u32,
    pub pointer_18: u64,
    pub pointer_28: u64,
    /// `{type, pointer}` in file order (plain lists only).
    pub records: Vec<(u64, u64)>,
    /// Offset of the first byte after the records (plain) or after the pointer array (compact).
    pub data_start: usize,
    pub compact: bool,
}

impl Zone {
    pub fn asset_list(&self) -> Result<AssetList> {
        let img = &self.image;
        if img.len() < 0x30 {
            bail!("image of {} bytes has no header", img.len());
        }
        let (pointer_count, count, count_b) = (u32_at(img, 0), u32_at(img, 0x10), u32_at(img, 0x24));
        if count != count_b {
            bail!("asset counts differ: {count} at +0x10, {count_b} at +0x24");
        }
        let mut l = AssetList { pointer_count, count, count_b, word_20: u32_at(img, 0x20), pointer_18: u64_at(img, 0x18), pointer_28: u64_at(img, 0x28), ..Default::default() };
        if pointer_count > 0 {
            let end = 0x30usize.checked_add(pointer_count as usize * 8).filter(|&e| e <= img.len()).with_context(|| format!("{pointer_count} pointers run past the image"))?;
            l.compact = true;
            l.data_start = end;
            return Ok(l);
        }
        let end = 0x30usize.checked_add(count as usize * 16).filter(|&e| e <= img.len()).with_context(|| format!("{count} asset records run past the image"))?;
        l.records = (0..count as usize).map(|i| (u64_at(img, 0x30 + 16 * i), u64_at(img, 0x38 + 16 * i))).collect();
        l.data_start = end;
        Ok(l)
    }
}

impl AssetList {
    pub fn histogram(&self) -> BTreeMap<u64, usize> {
        let mut h = BTreeMap::new();
        for (t, _) in &self.records {
            *h.entry(*t).or_default() += 1;
        }
        h
    }
}

/// Text-like content found in an image.
#[derive(Debug, Default, Clone)]
pub struct StringReport {
    pub plain: usize,
    pub plain_samples: Vec<String>,
    pub zlib_streams: usize,
    pub zlib_bytes: u64,
    pub zlib_text: usize,
    pub zlib_samples: Vec<String>,
}

fn wordy(s: &[u8]) -> bool {
    if !s.iter().all(|&c| c.is_ascii_alphanumeric() || b"_./\\:- ".contains(&c)) {
        return false;
    }
    let letters = s.iter().filter(|c| c.is_ascii_alphabetic()).count();
    let lower_run = s.split(|c| !c.is_ascii_lowercase()).any(|w| w.len() >= 4);
    letters * 10 >= s.len() * 6 && (lower_run || s.iter().any(|c| matches!(c, b'_' | b'.' | b'/')))
}

/// Plain strings (printable runs of 8 or more bytes that read like words) and the contents of zlib streams.
pub fn find_strings(image: &[u8], samples: usize) -> StringReport {
    let mut r = StringReport::default();
    let mut i = 0;
    while i < image.len() {
        if !(0x20..0x7f).contains(&image[i]) {
            i += 1;
            continue;
        }
        let s = i;
        while i < image.len() && (0x20..0x7f).contains(&image[i]) {
            i += 1;
        }
        if i - s >= 8 && wordy(&image[s..i]) {
            r.plain += 1;
            if r.plain_samples.len() < samples {
                r.plain_samples.push(String::from_utf8_lossy(&image[s..i.min(s + 80)]).into_owned());
            }
        }
    }
    let mut p = 0;
    while p + 2 < image.len() {
        if image[p] == 0x78 && matches!(image[p + 1], 0x01 | 0x5e | 0x9c | 0xda) {
            let mut d = flate2::read::ZlibDecoder::new(&image[p..]);
            let mut out = Vec::new();
            if (&mut d).take(256 << 20).read_to_end(&mut out).is_ok() && d.total_in() > 6 {
                r.zlib_streams += 1;
                r.zlib_bytes += out.len() as u64;
                let text = !out.is_empty() && out.iter().take(512).filter(|&&c| c == b'\n' || c == b'\r' || c == b'\t' || (0x20..0x7f).contains(&c)).count() * 10 >= out.len().min(512) * 9;
                if text {
                    r.zlib_text += 1;
                    if r.zlib_samples.len() < samples {
                        let line = out.split(|&c| c == b'\n').next().unwrap_or(&[]);
                        r.zlib_samples.push(String::from_utf8_lossy(&line[..line.len().min(70)]).into_owned());
                    }
                }
                p += d.total_in() as usize;
                continue;
            }
        }
        p += 1;
    }
    r
}

// ---------------------------------------------------------------------------------------------------------------------------
// the command

const USAGE: &str = "usage: atlas --hqinfo FILE|FOLDER [-v] [--strings] [--deep] [--sample N] [--dump DIR] [--oodle DLL] [--hashes FILE.wni]\n\
  opens Call of Duty HQ (cod26) containers offline: .ff fastfiles and .xpak/.xsub packages; prints one line per file and a survey\n\
  --hashes (or ATLAS_HASHES): a WNI hash index (ate47 HashIndex); package entries get their name when the key resolves, --dump DIR then also writes names.tsv";

pub struct ZoneReport {
    /// The zone without its image (that is dropped once the lists are read).
    pub zone: Zone,
    pub image_len: usize,
    pub file_size: u64,
    pub list: Result<AssetList, String>,
    pub strings: Option<StringReport>,
}

#[derive(Default)]
pub struct PackReport {
    pub kapi_kind: Option<Kind>,
    pub file_size: u64,
    pub id: u64,
    pub stats: kapi::PackageStats,
    pub subs: usize,
    /// Entries whose block headers were checked (without reading the blocks).
    pub headers_checked: usize,
    pub header_failures: Vec<String>,
    pub codecs: [usize; 8],
    pub segments: usize,
    pub blocks: usize,
    /// Entries decoded completely, how many matched the table, and the bytes they gave.
    pub decoded: usize,
    pub decode_failures: Vec<String>,
    pub decoded_bytes: u64,
    /// xpak: payload files present, entries found in their table with the same offset and size, others.
    pub subs_present: usize,
    pub subs_missing: usize,
    /// xsub files whose header id differs from the id in the xpak's record.
    pub subs_id_mismatch: usize,
    pub cross_matched: usize,
    pub cross_mismatched: usize,
    pub cross_unknown: usize,
    /// Entries whose key the hash index resolves (0 without an index), the first few of them, and with `--dump` all of them
    /// as (key, name, decoded size).
    pub names_resolved: usize,
    pub name_samples: Vec<(u64, String)>,
    pub names: Vec<(u64, String, u32)>,
}

pub enum Report {
    Zone(Box<ZoneReport>),
    Pack(Box<PackReport>),
    Patch,
}

#[derive(Clone)]
pub struct Options {
    pub verbose: bool,
    pub strings: bool,
    pub deep: bool,
    pub sample: usize,
    /// Write the inflated zone images here (`NAME.img`).
    pub dump: Option<PathBuf>,
    /// Hash index used to name package entries and record types.
    pub hashes: Option<std::sync::Arc<HashIndex>>,
}

/// Name of a record type: the built-in table of type names (FNV-1a 32 of the name), then the index.
fn type_name(t: u64, ix: Option<&HashIndex>) -> Option<String> {
    if t <= u32::MAX as u64 {
        if let Some(n) = hashindex::asset_type_name(t as u32) {
            return Some(n.to_string());
        }
    }
    ix.and_then(|i| i.lookup(t)).map(str::to_string)
}

fn fmt_n(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn inspect(path: &Path, oodle: Option<&Oodle>, opt: &Options) -> Result<Report> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "ff" => {
            let file_size = std::fs::metadata(path)?.len();
            let mut zone = open(path, oodle)?;
            let list = zone.asset_list().map_err(|e| format!("{e:#}"));
            let strings = opt.strings.then(|| find_strings(&zone.image, 6));
            let image_len = zone.image.len();
            if let Some(dir) = &opt.dump {
                std::fs::create_dir_all(dir)?;
                std::fs::write(dir.join(format!("{}.img", path.file_stem().unwrap().to_string_lossy())), &zone.image)?;
            }
            zone.image = Vec::new();
            Ok(Report::Zone(Box::new(ZoneReport { zone, image_len, file_size, list, strings })))
        }
        "fp" => {
            let mut h = [0u8; 8];
            std::fs::File::open(path)?.read_exact(&mut h)?;
            if &h == b"IWffd100" { Ok(Report::Patch) } else { refuse(format!("unknown patch magic {:?}", String::from_utf8_lossy(&h))) }
        }
        "xpak" | "xsub" => inspect_pack(path, oodle, opt).map(|p| Report::Pack(Box::new(p))),
        e => bail!("not a .ff, .xpak or .xsub file ({e:?})"),
    }
}

fn inspect_pack(path: &Path, oodle: Option<&Oodle>, opt: &Options) -> Result<PackReport> {
    let k = Kapi::open(path)?;
    let mut r = PackReport { kapi_kind: Some(k.kind), file_size: k.file_size, id: k.id, stats: k.stats(), subs: k.subs.len(), ..Default::default() };
    if let Some(ix) = &opt.hashes {
        for e in &k.entries {
            if let Some(n) = e.name(ix) {
                r.names_resolved += 1;
                if r.name_samples.len() < 5 {
                    r.name_samples.push((e.key, n.to_string()));
                }
                if opt.dump.is_some() {
                    r.names.push((e.key, n.to_string(), e.ex));
                }
            }
        }
    }
    fn note(codecs: &mut [usize; 8], i: &kapi::PayloadInfo) {
        for (a, b) in codecs.iter_mut().zip(i.codecs) {
            *a += b;
        }
    }
    if k.kind == Kind::Sub {
        let mut f = std::fs::File::open(path)?;
        // block headers: all entries with --deep, otherwise a sample
        let with_blocks: Vec<&kapi::Entry> = k.entries.iter().filter(|e| e.has_blocks()).collect();
        let step = if opt.deep { 1 } else { (with_blocks.len() / opt.sample.max(1)).max(1) };
        for e in with_blocks.iter().step_by(step) {
            r.headers_checked += 1;
            match kapi::payload_info_file(&mut f, e) {
                Ok(i) => {
                    r.segments += i.segments;
                    r.blocks += i.blocks;
                    note(&mut r.codecs, &i);
                }
                Err(err) => {
                    if r.header_failures.len() < 5 {
                        r.header_failures.push(format!("{:#018x}: {err:#}", e.key));
                    }
                }
            }
        }
        // decode a sample completely: compressed ones spread over the table and some stored ones
        let stored: Vec<&kapi::Entry> = k.entries.iter().filter(|e| !e.has_blocks()).collect();
        let mut picks: Vec<&kapi::Entry> = with_blocks.iter().step_by((with_blocks.len() / opt.sample.max(1)).max(1)).copied().collect();
        picks.extend(stored.iter().step_by((stored.len() / 8).max(1)).take(8));
        for e in picks {
            if e.size() > 256 << 20 {
                continue;
            }
            match k.read_entry(e, oodle) {
                Ok(v) if v.len() == e.ex as usize => {
                    r.decoded += 1;
                    r.decoded_bytes += v.len() as u64;
                }
                Ok(v) => r.decode_failures.push(format!("{:#018x}: {} bytes, table says {}", e.key, v.len(), e.ex)),
                Err(err) => {
                    if r.decode_failures.len() < 5 {
                        r.decode_failures.push(format!("{:#018x}: {err:#}", e.key));
                    }
                }
            }
        }
    } else if k.kind == Kind::Pak {
        // cross-check against the xsub tables next to it
        let mut tables: BTreeMap<u32, Option<std::collections::HashMap<u64, kapi::Entry>>> = BTreeMap::new();
        for sub in k.stats().per_sub.keys() {
            let e = kapi::Entry { key: 0, packed: (*sub as u64) << 55, ex: 0 };
            let p = k.payload_path(&e);
            let opened = Kapi::open(&p).ok();
            if let Some(o) = &opened {
                if k.subs.get(*sub as usize).is_some_and(|rec| rec.id != o.id) {
                    r.subs_id_mismatch += 1;
                }
            }
            let t = opened.map(|s| s.entries.into_iter().map(|e| (e.key, e)).collect());
            if t.is_some() {
                r.subs_present += 1;
            } else {
                r.subs_missing += 1;
            }
            tables.insert(*sub, t);
        }
        for e in &k.entries {
            match tables.get(&e.sub()) {
                Some(Some(t)) => match t.get(&e.key) {
                    Some(s) if s.offset() == e.offset() && s.size() == e.size() && s.ex == e.ex && s.has_blocks() == e.has_blocks() => r.cross_matched += 1,
                    Some(_) => r.cross_mismatched += 1,
                    None => r.cross_unknown += 1,
                },
                _ => {}
            }
        }
    }
    Ok(r)
}

fn describe(label: &str, rep: &Report, opt: &Options) -> String {
    match rep {
        Report::Patch => format!("{label}: patch container IWffd100 (not opened)"),
        Report::Zone(z) => {
            let zone = &z.zone;
            let assets = match &z.list {
                Ok(l) if l.compact => format!("assets {} (compact list after {} pointers, types not decoded)", l.count, l.pointer_count),
                Ok(l) => format!("assets {} in {} types", l.count, l.histogram().len()),
                Err(e) => format!("asset list unreadable ({e})"),
            };
            let mut s = format!(
                "{label}: ok  file {} -> image {}  blocks {} (+{} signature gaps)  {assets}",
                fmt_n(z.file_size),
                fmt_n(z.image_len as u64),
                zone.blocks,
                zone.signature_gaps
            );
            if let Some(n) = &zone.signature_name {
                s += &format!("  name {n}");
            }
            if let Some(st) = &z.strings {
                s += &format!("  strings {} plain, {} zlib streams ({} text)", st.plain, st.zlib_streams, st.zlib_text);
            }
            if !zone.notes.is_empty() {
                s += &format!("  notes: {}", zone.notes.join("; "));
            }
            if opt.verbose {
                s += &format!(
                    "\n    header: version {} word_0c {} word_10 {:#x} payload size {:#x} magic at {:#x}; codec {} block size {:#x}, {} stored blocks, trailer {} bytes",
                    zone.header.version, zone.header.word_0c, zone.header.word_10, zone.header.payload_size, zone.header.magic_pos, zone.codec, zone.block_size, zone.stored_blocks, zone.trailer
                );
                let words: Vec<String> = zone.header.words.iter().map(|(o, v)| format!("{o:#x}={v:#x}")).collect();
                s += &format!("\n    header words: {}", words.join(" "));
                if let Ok(l) = &z.list {
                    s += &format!(
                        "\n    image header: count {} word_20 {} pointers {:#x} {:#x}; records end at {:#x}",
                        l.count, l.word_20, l.pointer_18, l.pointer_28, l.data_start
                    );
                    let mut ptrs = BTreeMap::new();
                    for (_, p) in &l.records {
                        *ptrs.entry(*p).or_insert(0usize) += 1;
                    }
                    let p: Vec<String> = ptrs.iter().take(4).map(|(p, n)| format!("{p:#x} x{n}")).collect();
                    s += &format!("\n    record pointers: {}", p.join(", "));
                    for (t, n) in l.histogram() {
                        match type_name(t, opt.hashes.as_deref()) {
                            Some(name) => s += &format!("\n    type {t:#010x} ({name}): {n}"),
                            None => s += &format!("\n    type {t:#010x}: {n}"),
                        }
                    }
                }
                if let Some(st) = &z.strings {
                    for x in &st.plain_samples {
                        s += &format!("\n    string: {x}");
                    }
                    for x in &st.zlib_samples {
                        s += &format!("\n    zlib text: {x}");
                    }
                }
            }
            s
        }
        Report::Pack(p) => {
            let st = &p.stats;
            let kind = match p.kapi_kind {
                Some(Kind::Pak) => "xpak",
                Some(Kind::Sub) => "xsub",
                Some(Kind::CdnPak) => "xpak (CDN set)",
                _ => "?",
            };
            let mut s = format!(
                "{label}: ok  {kind}  file {}  entries {} ({} with blocks {} packed -> {} decoded, {} stored {})",
                fmt_n(p.file_size),
                fmt_n(st.entries as u64),
                fmt_n(st.with_blocks as u64),
                fmt_n(st.packed_bytes),
                fmt_n(st.ex_bytes - st.stored_bytes),
                fmt_n(st.stored as u64),
                fmt_n(st.stored_bytes)
            );
            match p.kapi_kind {
                Some(Kind::Sub) => {
                    s += &format!(
                        "  headers checked {} ({} segments, {} blocks; Oodle {} LZ4 {} stored {}), decoded {} ({} bytes)",
                        p.headers_checked, p.segments, p.blocks, p.codecs[6], p.codecs[3], p.codecs[0], p.decoded, fmt_n(p.decoded_bytes)
                    );
                    if !p.header_failures.is_empty() || !p.decode_failures.is_empty() {
                        s += &format!("  FAILURES header {} decode {}", p.header_failures.len(), p.decode_failures.len());
                    }
                }
                Some(Kind::Pak) => {
                    s += &format!(
                        "  xsub records {}, xsub files present {} missing {} (id mismatch {}), entries matching their xsub table {} (mismatch {}, unknown {})",
                        p.subs, p.subs_present, p.subs_missing, p.subs_id_mismatch, p.cross_matched, p.cross_mismatched, p.cross_unknown
                    );
                }
                _ => {}
            }
            if opt.hashes.is_some() {
                s += &format!("  names resolved {} of {} ({:.1}%)", fmt_n(p.names_resolved as u64), fmt_n(st.entries as u64), 100.0 * p.names_resolved as f64 / st.entries.max(1) as f64);
            }
            if st.unsorted || st.duplicate_keys > 0 || st.out_of_range > 0 {
                s += &format!("  odd: unsorted {} duplicate keys {} out of range {}", st.unsorted, st.duplicate_keys, st.out_of_range);
            }
            if opt.verbose {
                s += &format!("\n    id {:#018x}; entries per xsub {:?}", p.id, st.per_sub);
                for f in p.header_failures.iter().chain(&p.decode_failures) {
                    s += &format!("\n    failure: {f}");
                }
                for (key, name) in &p.name_samples {
                    s += &format!("\n    name {key:#018x} {name}");
                }
            }
            s
        }
    }
}

#[derive(Default)]
struct Survey {
    files: usize,
    zones_ok: usize,
    packs_ok: usize,
    patches: usize,
    refused: BTreeMap<String, Vec<String>>,
    failed: BTreeMap<String, Vec<String>>,
    file_bytes: u64,
    image_bytes: u64,
    blocks: u64,
    gaps: u64,
    assets: u64,
    types: BTreeMap<u64, (usize, usize)>,
    zone_notes: BTreeMap<String, usize>,
    unreadable_lists: usize,
    plain_zones: usize,
    plain_assets: u64,
    compact_zones: usize,
    compact_assets: u64,
    compact_pointers: u64,
    named: usize,
    named_as_file: usize,
    trailers: BTreeMap<usize, usize>,
    strings: StringReport,
    pack_entries: u64,
    pack_blocks: u64,
    pack_codecs: [usize; 8],
    pack_packed: u64,
    pack_decoded_total: u64,
    pack_stored: u64,
    pack_checked: usize,
    pack_decoded: usize,
    pack_failures: usize,
    cross_matched: usize,
    cross_bad: usize,
    subs_missing: usize,
    by_kind: BTreeMap<&'static str, (usize, u64, u64)>,
    /// Payload keys of the xsub files and how many of them the hash index resolves.
    name_keys: u64,
    name_resolved: u64,
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_dir() {
        let mut v: Vec<PathBuf> = std::fs::read_dir(path)?.flatten().map(|e| e.path()).collect();
        v.sort();
        for p in v {
            if p.is_dir() {
                collect(&p, out)?;
            } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ff" | "xpak" | "xsub" | "fp")) {
                out.push(p);
            }
        }
    } else {
        out.push(path.to_path_buf());
    }
    Ok(())
}

pub fn command(args: &[String]) -> Result<()> {
    let mut opt = Options { verbose: false, strings: false, deep: false, sample: 24, dump: None, hashes: None };
    let mut index_arg: Option<String> = None;
    let mut path: Option<&String> = None;
    let mut dll: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-v" => opt.verbose = true,
            "--strings" => opt.strings = true,
            "--deep" => opt.deep = true,
            "--sample" => {
                i += 1;
                opt.sample = args.get(i).and_then(|v| v.parse().ok()).context("--sample needs a number")?;
            }
            "--dump" => {
                i += 1;
                opt.dump = Some(PathBuf::from(args.get(i).context("--dump needs a folder")?));
            }
            "--hashes" => {
                i += 1;
                index_arg = Some(args.get(i).context("--hashes needs a .wni file")?.clone());
            }
            "--oodle" => {
                i += 1;
                dll = Some(PathBuf::from(args.get(i).context("--oodle needs a path")?));
            }
            a if a.starts_with('-') => bail!("unknown option {a}\n{USAGE}"),
            _ => path = Some(&args[i]),
        }
        i += 1;
    }
    let root = Path::new(path.with_context(|| USAGE.to_string())?);
    let mut files = Vec::new();
    collect(root, &mut files)?;
    if files.is_empty() {
        bail!("no .ff, .xpak or .xsub files under {}", root.display());
    }
    let oodle = match dll.or_else(|| Oodle::find(root)) {
        Some(p) => match Oodle::load(&p) {
            Ok(o) => {
                eprintln!("Oodle: {}", p.display());
                Some(o)
            }
            Err(e) => {
                eprintln!("Oodle: {e:#}; Oodle blocks cannot be decoded");
                None
            }
        },
        None => {
            eprintln!("Oodle: no oo2core_*_win64.dll next to the game (or set ATLAS_OODLE_DLL / --oodle); Oodle blocks cannot be decoded");
            None
        }
    };
    if let Some(p) = hashindex::index_path(index_arg.as_deref()) {
        let t = std::time::Instant::now();
        let ix = HashIndex::open(&p)?;
        eprintln!("Hash index: {} ({} hashes, {} skipped, {:.1} s)", p.display(), fmt_n(ix.len() as u64), ix.skipped, t.elapsed().as_secs_f64());
        opt.hashes = Some(std::sync::Arc::new(ix));
    }
    let mut names_out = match (&opt.dump, &opt.hashes) {
        (Some(dir), Some(_)) => {
            std::fs::create_dir_all(dir)?;
            let mut w = std::io::BufWriter::new(std::fs::File::create(dir.join("names.tsv"))?);
            writeln!(w, "key\tname\tsize\tpackage")?;
            Some(w)
        }
        _ => None,
    };
    let started = std::time::Instant::now();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 6);
    let mut sv = Survey::default();
    let batch = threads * 3;
    for chunk in files.chunks(batch) {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<std::sync::Mutex<Option<Result<Report>>>> = chunk.iter().map(|_| std::sync::Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for _ in 0..threads.min(chunk.len()) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= chunk.len() {
                            break;
                        }
                        let r = inspect(&chunk[i], oodle.as_ref(), &opt);
                        *results[i].lock().unwrap() = Some(r);
                    }
                });
            }
        });
        for (f, r) in chunk.iter().zip(results) {
            let label = if root.is_dir() { f.strip_prefix(root).unwrap_or(f).to_string_lossy().replace('\\', "/") } else { f.file_name().unwrap().to_string_lossy().into_owned() };
            let r = r.into_inner().unwrap().unwrap();
            sv.files += 1;
            sv.file_bytes += std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
            match r {
                Ok(rep) => {
                    println!("{}", describe(&label, &rep, &opt));
                    if let (Some(w), Report::Pack(p)) = (names_out.as_mut(), &rep) {
                        for (key, name, size) in &p.names {
                            writeln!(w, "{key:#018x}\t{}\t{size}\t{label}", name.replace(['\t', '\r', '\n'], " "))?;
                        }
                    }
                    sv.add(f, &rep);
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let key = if msg.len() > 90 { format!("{}...", msg.chars().take(87).collect::<String>()) } else { msg.clone() };
                    // group failures by the first words, the numbers differ per file
                    let key = key.split(|c: char| c.is_ascii_digit()).next().unwrap_or("").to_string() + if key.chars().any(|c| c.is_ascii_digit()) { "N..." } else { "" };
                    if let Some(re) = e.downcast_ref::<Refused>() {
                        println!("{label}: REFUSED ({})", re.0);
                        sv.refused.entry(re.0.clone()).or_default().push(label);
                    } else {
                        println!("{label}: FAILED: {msg}");
                        sv.failed.entry(key).or_default().push(format!("{label}: {msg}"));
                    }
                }
            }
        }
    }
    sv.print(started.elapsed().as_secs_f64(), opt.verbose, opt.hashes.as_deref());
    if let Some(w) = names_out.as_mut() {
        w.flush()?;
    }
    Ok(())
}

impl Survey {
    fn add(&mut self, f: &Path, rep: &Report) {
        match rep {
            Report::Patch => self.patches += 1,
            Report::Zone(z) => {
                self.zones_ok += 1;
                let zone = &z.zone;
                self.image_bytes += z.image_len as u64;
                self.blocks += zone.blocks as u64;
                self.gaps += zone.signature_gaps as u64;
                *self.trailers.entry(zone.trailer).or_default() += 1;
                for n in &zone.notes {
                    let k: String = n.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
                    *self.zone_notes.entry(k).or_default() += 1;
                }
                if let Some(n) = &zone.signature_name {
                    self.named += 1;
                    if f.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case(n)) {
                        self.named_as_file += 1;
                    }
                }
                match &z.list {
                    Ok(l) => {
                        self.assets += l.count as u64;
                        if l.compact {
                            self.compact_zones += 1;
                            self.compact_assets += l.count as u64;
                            self.compact_pointers += l.pointer_count as u64;
                        } else {
                            self.plain_zones += 1;
                            self.plain_assets += l.count as u64;
                        }
                        for (t, n) in l.histogram() {
                            let e = self.types.entry(t).or_default();
                            e.0 += n;
                            e.1 += 1;
                        }
                    }
                    Err(_) => self.unreadable_lists += 1,
                }
                if let Some(s) = &z.strings {
                    self.strings.plain += s.plain;
                    self.strings.zlib_streams += s.zlib_streams;
                    self.strings.zlib_bytes += s.zlib_bytes;
                    self.strings.zlib_text += s.zlib_text;
                }
            }
            Report::Pack(p) => {
                self.packs_ok += 1;
                let k = match p.kapi_kind {
                    Some(Kind::Pak) => "xpak",
                    Some(Kind::Sub) => "xsub",
                    Some(Kind::CdnPak) => "xpak (CDN set)",
                    _ => "other",
                };
                let e = self.by_kind.entry(k).or_default();
                e.0 += 1;
                e.1 += p.stats.entries as u64;
                e.2 += p.file_size;
                if p.kapi_kind == Some(Kind::Sub) {
                    self.pack_entries += p.stats.entries as u64;
                    self.pack_packed += p.stats.packed_bytes;
                    self.pack_stored += p.stats.stored_bytes;
                    self.pack_decoded_total += p.stats.ex_bytes;
                    self.pack_checked += p.headers_checked;
                    self.pack_blocks += p.blocks as u64;
                    for (a, b) in self.pack_codecs.iter_mut().zip(p.codecs) {
                        *a += b;
                    }
                    self.pack_decoded += p.decoded;
                    self.pack_failures += p.header_failures.len() + p.decode_failures.len();
                    self.name_keys += p.stats.entries as u64;
                    self.name_resolved += p.names_resolved as u64;
                }
                self.cross_matched += p.cross_matched;
                self.cross_bad += p.cross_mismatched + p.cross_unknown;
                self.subs_missing += p.subs_missing;
            }
        }
    }

    fn print(&self, secs: f64, verbose: bool, ix: Option<&HashIndex>) {
        println!("\nsurvey: {} files, {} bytes on disk, {:.1} s", self.files, fmt_n(self.file_bytes), secs);
        let refused: usize = self.refused.values().map(Vec::len).sum();
        let failed: usize = self.failed.values().map(Vec::len).sum();
        println!("  opened: {} zones + {} packages   refused: {}   failed: {}   patches (.fp, header only): {}", self.zones_ok, self.packs_ok, refused, failed, self.patches);
        for (why, files) in &self.refused {
            println!("  refused x{}: {why} (e.g. {})", files.len(), files[0]);
        }
        for (why, files) in &self.failed {
            println!("  failed x{}: {why}  e.g. {}", files.len(), files[0]);
        }
        if self.zones_ok > 0 {
            println!(
                "  zones: {} bytes inflated, {} blocks, {} signature gaps, {} assets, {} distinct record types in plain lists ({} lists unreadable)",
                fmt_n(self.image_bytes),
                fmt_n(self.blocks),
                self.gaps,
                fmt_n(self.assets),
                self.types.len(),
                self.unreadable_lists
            );
            println!(
                "  zones: plain asset lists {} ({} assets, 16-byte records), compact lists {} ({} assets, {} leading pointers; types not decoded)",
                self.plain_zones,
                fmt_n(self.plain_assets),
                self.compact_zones,
                fmt_n(self.compact_assets),
                fmt_n(self.compact_pointers)
            );
            println!("  zones: signature header names {} (equal to the file name: {}), bytes after the last block {:?}", self.named, self.named_as_file, self.trailers);
            if !self.zone_notes.is_empty() {
                println!("  zones: notes {:?}", self.zone_notes);
            }
            let mut top: Vec<(&u64, &(usize, usize))> = self.types.iter().collect();
            top.sort_by(|a, b| b.1.0.cmp(&a.1.0));
            let n = if verbose { top.len() } else { 15 };
            for (t, (count, zones)) in top.into_iter().take(n) {
                let name = type_name(*t, ix).map(|n| format!(" ({n})")).unwrap_or_default();
                println!("    type {t:#010x}{name}: {} assets in {zones} zones", fmt_n(*count as u64));
            }
            if self.strings.zlib_streams > 0 || self.strings.plain > 0 {
                println!(
                    "  strings: {} plain strings, {} zlib streams ({} text) inflating to {} bytes",
                    fmt_n(self.strings.plain as u64),
                    fmt_n(self.strings.zlib_streams as u64),
                    fmt_n(self.strings.zlib_text as u64),
                    fmt_n(self.strings.zlib_bytes)
                );
            }
        }
        if self.packs_ok > 0 {
            for (k, (n, entries, bytes)) in &self.by_kind {
                println!("  {k}: {n} files, {} entries listed, {} bytes", fmt_n(*entries), fmt_n(*bytes));
            }
            println!(
                "  xsub payloads: {} entries ({} bytes packed + {} stored -> {} bytes decoded); headers checked {} entries ({} blocks: Oodle {} LZ4 {} stored {}); decoded {} entries completely; failures {}",
                fmt_n(self.pack_entries),
                fmt_n(self.pack_packed),
                fmt_n(self.pack_stored),
                fmt_n(self.pack_decoded_total),
                fmt_n(self.pack_checked as u64),
                fmt_n(self.pack_blocks),
                fmt_n(self.pack_codecs[6] as u64),
                fmt_n(self.pack_codecs[3] as u64),
                fmt_n(self.pack_codecs[0] as u64),
                self.pack_decoded,
                self.pack_failures
            );
            if ix.is_some() {
                println!(
                    "  names: {} of {} xsub payload keys resolved by the hash index ({:.2}%)",
                    fmt_n(self.name_resolved),
                    fmt_n(self.name_keys),
                    100.0 * self.name_resolved as f64 / self.name_keys.max(1) as f64
                );
            }
            println!("  xpak cross-check: {} entries match their xsub table, {} do not, {} xsub files missing", fmt_n(self.cross_matched as u64), fmt_n(self.cross_bad as u64), self.subs_missing);
        }
    }
}
