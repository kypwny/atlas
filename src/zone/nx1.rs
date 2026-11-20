//! Future Warfare (NX1) fastfiles of the Xbox 360 development build (December 2011): the container and the asset list.
//!
//! Layout (everything big-endian):
//!
//! ```text
//! 0x00  "NXff0100" (signed) or "NXffu100"     0x08  version (357)      0x0e  creation time (FILETIME)
//! 0x1a  n: number of chunk records             0x1e  n records of 12 bytes: 0, start, end (file offsets)
//! then  two words: the file offset where the main stream ends (= the start of the first chunk), and a second offset
//! signed files: "NXffs100" and 0x4000 bytes of signatures, then the main stream
//! ```
//!
//! The main stream is one raw deflate stream (a zlib header, no usable Adler trailer) holding the zone image: the `XFile`
//! header (size, external size, six block sizes), then the asset stream. In a signed file the stream is cut into 2 MiB
//! pieces and an 8 KiB hash table follows every full piece (the last piece has none). The chunk records describe the
//! rest of the file: texture data in four lanes (record `i` belongs to lane `i % 4`), each chunk one deflate stream. The
//! asset stream never refers to it.

use anyhow::{Result, bail};
use flate2::read::DeflateDecoder;
use std::io::Read;

pub const MAGIC_SIGNED: &[u8; 8] = b"NXff0100";
pub const MAGIC_UNSIGNED: &[u8; 8] = b"NXffu100";
const PIECE: usize = 0x20_0000;
const TABLE: usize = 0x2000;
const SIGNATURES: usize = 0x4000;
/// Number of memory blocks in the `XFile` header.
pub const BLOCKS: usize = 6;

pub struct NxFile {
    pub version: u32,
    pub signed: bool,
    /// The zone image: `XFile` header and asset stream.
    pub image: Vec<u8>,
    /// File offset where the main stream ends.
    pub main_end: usize,
    /// (start, end) file offsets of the texture chunks, by lane.
    pub lanes: [Vec<(usize, usize)>; 4],
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}

/// Opens an NX1 fastfile: the zone image and the chunk table.
pub fn open(raw: &[u8]) -> Result<NxFile> {
    if raw.len() < 0x26 {
        bail!("too short for a fastfile");
    }
    let signed = match &raw[..8] {
        m if m == MAGIC_SIGNED => true,
        m if m == MAGIC_UNSIGNED => false,
        other => bail!("not an NX1 fastfile ({:?})", String::from_utf8_lossy(other)),
    };
    let version = be32(raw, 8);
    let count = be32(raw, 0x1a) as usize;
    let table_end = 0x1e + 12 * count;
    if table_end + 8 > raw.len() {
        bail!("chunk table runs past the file");
    }
    let main_end = be32(raw, table_end) as usize;
    let mut lanes: [Vec<(usize, usize)>; 4] = Default::default();
    for i in 0..count {
        let (s, e) = (be32(raw, 0x1e + 12 * i + 4) as usize, be32(raw, 0x1e + 12 * i + 8) as usize);
        if s != e {
            lanes[i % 4].push((s, e));
        }
    }
    let mut start = table_end + 8;
    if signed {
        if raw.len() < start + 8 || &raw[start..start + 8] != b"NXffs100" {
            bail!("signed fastfile without its signature block");
        }
        start += SIGNATURES;
    }
    if main_end < start + 2 || main_end > raw.len() {
        bail!("main stream bounds {start:#x}..{main_end:#x} are outside the file");
    }
    let data: Vec<u8> = if signed {
        let mut out = Vec::with_capacity(main_end - start);
        let (mut pos, mut rem) = (start, main_end - start);
        while rem > PIECE {
            out.extend_from_slice(&raw[pos..pos + PIECE]);
            pos += PIECE + TABLE;
            rem = rem.saturating_sub(PIECE + TABLE);
        }
        out.extend_from_slice(&raw[pos..pos + rem]);
        out
    } else {
        raw[start..main_end].to_vec()
    };
    let mut image = Vec::with_capacity(data.len() * 6);
    DeflateDecoder::new(&data[2..]).read_to_end(&mut image).map_err(|e| anyhow::anyhow!("inflating the zone: {e}"))?;
    if image.len() < 32 {
        bail!("zone image too small ({} bytes)", image.len());
    }
    Ok(NxFile { version, signed, image, main_end, lanes })
}

impl NxFile {
    /// The `XFile` header: stream size, external size and the sizes of the six blocks.
    pub fn xfile(&self) -> (u32, u32, [u32; BLOCKS]) {
        let mut b = [0u32; BLOCKS];
        for (i, v) in b.iter_mut().enumerate() {
            *v = be32(&self.image, 8 + 4 * i);
        }
        (be32(&self.image, 0), be32(&self.image, 4), b)
    }

    /// Uncompressed bytes per lane (inflates every chunk; slow for the large map zones).
    pub fn lane_sizes(&self, raw: &[u8]) -> [u64; 4] {
        let mut sizes = [0u64; 4];
        for (l, chunks) in self.lanes.iter().enumerate() {
            for &(s, e) in chunks {
                if e <= raw.len() && s + 2 < e {
                    let mut sink = std::io::sink();
                    if let Ok(n) = std::io::copy(&mut DeflateDecoder::new(&raw[s + 2..e]), &mut sink) {
                        sizes[l] += n;
                    }
                }
            }
        }
        sizes
    }
}

/// The asset list of a zone image, without any structure layouts: the script strings and the (type, pointer) entries.
pub struct AssetList {
    pub strings: Vec<String>,
    pub entries: Vec<(u32, u32)>,
    /// Image offset of the entry array.
    pub array_at: usize,
}

/// Reads the asset list at the start of the stream (the string pointers, the strings and the entries follow one another;
/// the stream has no padding).
pub fn asset_list(image: &[u8]) -> Result<AssetList> {
    let base = 32;
    if image.len() < base + 16 {
        bail!("zone too short for the asset list");
    }
    let (nstr, pstr, nass, pass) = (be32(image, base) as usize, be32(image, base + 4), be32(image, base + 8) as usize, be32(image, base + 12));
    let mut pos = base + 16;
    let mut strings = Vec::new();
    if pstr != 0 {
        let mut ptrs = Vec::new();
        for i in 0..nstr {
            if pos + 4 * (i + 1) > image.len() {
                bail!("string list runs past the image");
            }
            ptrs.push(be32(image, pos + 4 * i));
        }
        pos += 4 * nstr;
        for p in ptrs {
            if p == u32::MAX {
                let end = image[pos..].iter().position(|&c| c == 0).map(|e| pos + e).ok_or_else(|| anyhow::anyhow!("unterminated string"))?;
                strings.push(String::from_utf8_lossy(&image[pos..end]).into_owned());
                pos = end + 1;
            } else {
                strings.push(String::new());
            }
        }
    }
    let mut entries = Vec::new();
    if pass != 0 {
        let at = pos;
        if at + 8 * nass > image.len() {
            bail!("asset array runs past the image");
        }
        for i in 0..nass {
            entries.push((be32(image, at + 8 * i), be32(image, at + 8 * i + 4)));
        }
        return Ok(AssetList { strings, entries, array_at: at });
    }
    Ok(AssetList { strings, entries, array_at: pos })
}

/// Asset type names of the build (the `XAssetType` enumeration of its program database).
pub const ASSET_NAMES: [&str; 48] = [
    "physpreset", "physcollmap", "xanimparts", "xmodel_surfs", "xmodel", "material", "pixelshader", "technique_set", "image", "sound",
    "snd_volume_falloff_curve", "snd_lpf_curve", "loaded_sound", "clipmap_sp", "clipmap_mp", "comworld", "gameworld_sp",
    "gameworld_mp", "map_ents", "fxworld", "gfxworld", "light_def", "ui_map", "font", "menulist", "menu", "localize_entry",
    "weapon", "snddriver_globals", "fx", "impact_fx", "surface_fx", "aitype", "mptype", "character", "xmodelalias", "rawfile",
    "stringtable", "leaderboard", "structured_data_def", "tracer", "laser", "vehicle", "addon_map_ents", "shockfile",
    "volumesetting", "reverbpreset", "fog",
];

pub fn asset_type_name(ty: u32) -> &'static str {
    ASSET_NAMES.get(ty as usize).copied().unwrap_or("?")
}

/// `atlas --nx1info FILE...`: the container of NX1 fastfiles and the number of assets of each type.
pub fn command(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("usage: atlas --nx1info FILE.ff...");
    }
    for path in args {
        let raw = std::fs::read(path).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
        let f = open(&raw)?;
        let (size, external, blocks) = f.xfile();
        println!("{path}: {} version {}, image {} bytes (size field {size}, external {external}), blocks {:?}", if f.signed { "signed" } else { "unsigned" }, f.version, f.image.len(), blocks);
        let chunks: usize = f.lanes.iter().map(|l| l.len()).sum();
        println!("  texture chunks: {chunks} in lanes of {:?}", f.lanes.iter().map(|l| l.len()).collect::<Vec<_>>());
        let list = asset_list(&f.image)?;
        let mut by_type = std::collections::BTreeMap::new();
        for (t, _) in &list.entries {
            *by_type.entry(*t).or_insert(0usize) += 1;
        }
        println!("  {} script strings, {} assets", list.strings.len(), list.entries.len());
        for (t, n) in by_type {
            println!("    {n:6} {t:2} {}", asset_type_name(t));
        }
    }
    Ok(())
}
