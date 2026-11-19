//! Name lookup for the hashed names of the newest Call of Duty containers.
//!
//! Two things live here:
//!
//! - a reader for `.wni` files, the hash index format of ate47's HashIndex project (https://github.com/ate47/HashIndex, MIT; the
//!   format was read from its documentation, no code was used). Atlas neither bundles nor downloads an index: the user supplies
//!   the file.
//! - the FNV-1a variants that the same documentation lists for asset names, so that a string can be hashed and looked up.
//!
//! WNI layout (little endian): magic `"WNI "` (u32 `0x20494E57`), u16 version (1), u32 entry count, u32 compressed size, u32
//! decompressed size, then one LZ4 block of the compressed size. The decompressed data is a run of entries `{u64 hash, string,
//! 0}`. Hashes are masked with [`MASK60`] when loading and when looking up, so a lookup works with a hash of 60, 63 or 64 bits.
//!
//! The hashes of dvars and script names insert a secret string after the first character; they are not implemented.

use crate::oodle;
use anyhow::{Context, Result, bail};
use std::path::Path;

pub const MAGIC: u32 = 0x2049_4E57;
pub const VERSION: u16 = 1;
/// Hashes of the index are compared on their low 60 bits.
pub const MASK60: u64 = 0x0FFF_FFFF_FFFF_FFFF;
pub const MASK63: u64 = 0x7FFF_FFFF_FFFF_FFFF;

pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
/// Offset basis of the "IW Resources" hash (XAsset names of Modern Warfare II and III and Black Ops 6).
pub const IW_RESOURCE_OFFSET: u64 = 0x47F5_817A_5EF9_61BA;

/// FNV-1a over `bytes` with the given offset basis and prime (64-bit wrapping arithmetic).
pub fn fnv1a_with(bytes: &[u8], offset: u64, prime: u64) -> u64 {
    let mut h = offset;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(prime);
    }
    h
}

/// Base 64-bit FNV-1a.
pub fn fnv1a64(s: &str) -> u64 {
    fnv1a_with(s.as_bytes(), FNV_OFFSET, FNV_PRIME)
}

/// Base FNV-1a masked to 63 bits (the form listed for Black Ops 4 and Cold War asset names).
pub fn fnv1a63(s: &str) -> u64 {
    fnv1a64(s) & MASK63
}

/// "IW Resources" hash: FNV-1a with offset basis [`IW_RESOURCE_OFFSET`], masked to 63 bits. The string is hashed as given.
pub fn hash_iw_resource(s: &str) -> u64 {
    fnv1a_with(s.as_bytes(), IW_RESOURCE_OFFSET, FNV_PRIME) & MASK63
}

/// [`hash_iw_resource`] of the ASCII-lower-cased string (names are normally lower case).
pub fn hash_iw_resource_lower(s: &str) -> u64 {
    hash_iw_resource(&s.to_ascii_lowercase())
}

/// 32-bit FNV-1a (offset basis `0x811c9dc5`, prime `0x01000193`). The asset type of a zone's asset record is this hash of the
/// lower-case type name.
pub fn fnv1a32(s: &str) -> u32 {
    let mut h = 0x811c_9dc5u32;
    for &b in s.as_bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Type names whose [`fnv1a32`] occurs among the 53 record types of the installed `cod26` zones (20 of the 53 are named this
/// way; the others are not known). The names are guesses from the usual asset-type vocabulary, kept only when the hash occurs
/// in the install: with a few thousand guesses against 53 types, a chance match at 32 bits is below one in ten thousand.
pub const ASSET_TYPE_NAMES: &[&str] = &[
    "xmodelsurfs",
    "localize",
    "physicsasset",
    "image",
    "scriptbundle",
    "techset",
    "luafile",
    "streaminginfo",
    "movie",
    "material",
    "xmodel",
    "font",
    "netconststrings",
    "rawfile",
    "soundbank",
    "soundsubmix",
    "physicsfxshape",
    "stringtable",
    "physicsfxpipeline",
    "gscobj",
];

/// The type name for an asset record's type hash, if it is one of [`ASSET_TYPE_NAMES`].
pub fn asset_type_name(hash: u32) -> Option<&'static str> {
    ASSET_TYPE_NAMES.iter().copied().find(|n| fnv1a32(n) == hash)
}

/// A loaded index: sorted masked hashes and the strings they stand for.
pub struct HashIndex {
    hashes: Vec<u64>,
    offsets: Vec<u32>,
    data: Vec<u8>,
    /// Entry count the header declares.
    pub declared: u32,
    /// Entries that were dropped: a second string for a hash that was already present, or a string that is not UTF-8.
    pub skipped: usize,
}

impl HashIndex {
    pub fn open(path: &Path) -> Result<HashIndex> {
        let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
        Self::from_bytes(&bytes).with_context(|| path.display().to_string())
    }

    pub fn from_bytes(b: &[u8]) -> Result<HashIndex> {
        if b.len() < 18 {
            bail!("too short for a WNI file ({} bytes)", b.len());
        }
        let magic = u32::from_le_bytes(b[0..4].try_into().unwrap());
        if magic != MAGIC {
            bail!("not a WNI file (magic {:#010x}, {:#010x} expected)", magic, MAGIC);
        }
        let version = u16::from_le_bytes(b[4..6].try_into().unwrap());
        if version != VERSION {
            bail!("WNI version {version} (only {VERSION} is known)");
        }
        let count = u32::from_le_bytes(b[6..10].try_into().unwrap());
        let comp = u32::from_le_bytes(b[10..14].try_into().unwrap()) as usize;
        let raw = u32::from_le_bytes(b[14..18].try_into().unwrap()) as usize;
        let block = b.get(18..18 + comp).with_context(|| format!("the LZ4 block of {comp} bytes runs past the file ({} bytes)", b.len()))?;
        let mut data = Vec::new();
        oodle::lz4_block_append(block, raw, &mut data).context("decoding the index")?;
        Self::from_entries_data(data, count)
    }

    /// Parses the decompressed entries `{u64 hash, string, 0}`.
    fn from_entries_data(data: Vec<u8>, declared: u32) -> Result<HashIndex> {
        let mut pairs: Vec<(u64, u32)> = Vec::with_capacity(declared as usize);
        let mut skipped = 0usize;
        let mut i = 0usize;
        while i < data.len() {
            let Some(h) = data.get(i..i + 8) else { bail!("entry {} is cut off at offset {i}", pairs.len() + skipped) };
            let hash = u64::from_le_bytes(h.try_into().unwrap()) & MASK60;
            let start = i + 8;
            let Some(len) = data[start..].iter().position(|&c| c == 0) else { bail!("entry {} has no string terminator", pairs.len() + skipped) };
            if std::str::from_utf8(&data[start..start + len]).is_err() {
                skipped += 1;
            } else {
                pairs.push((hash, start as u32));
            }
            i = start + len + 1;
        }
        if data.len() > u32::MAX as usize {
            bail!("index data of {} bytes does not fit 32-bit offsets", data.len());
        }
        // stable sort, then keep the first string of each hash
        pairs.sort_by_key(|p| p.0);
        let before = pairs.len();
        pairs.dedup_by_key(|p| p.0);
        skipped += before - pairs.len();
        let (hashes, offsets) = pairs.into_iter().unzip();
        Ok(HashIndex { hashes, offsets, data, declared, skipped })
    }

    /// Number of distinct hashes in the index.
    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hashes.is_empty()
    }

    /// The string for `hash`, comparing the low 60 bits only (so a 60-, 63- or 64-bit form of the same hash finds it).
    pub fn lookup(&self, hash: u64) -> Option<&str> {
        let h = hash & MASK60;
        let i = self.hashes.binary_search(&h).ok()?;
        let start = self.offsets[i] as usize;
        let len = self.data[start..].iter().position(|&c| c == 0)?;
        std::str::from_utf8(&self.data[start..start + len]).ok()
    }
}

/// Builds a WNI file from `(hash, string)` pairs. The block is stored as literals only (a valid LZ4 block without matches).
pub fn build_wni(entries: &[(u64, &str)]) -> Vec<u8> {
    let mut data = Vec::new();
    for (h, s) in entries {
        data.extend_from_slice(&h.to_le_bytes());
        data.extend_from_slice(s.as_bytes());
        data.push(0);
    }
    // one sequence: token with the literal length, extra length bytes, the literals, no match part
    let mut block = Vec::new();
    let n = data.len();
    block.push((n.min(15) as u8) << 4);
    if n >= 15 {
        let mut rest = n - 15;
        while rest >= 255 {
            block.push(255);
            rest -= 255;
        }
        block.push(rest as u8);
    }
    block.extend_from_slice(&data);
    let mut out = Vec::new();
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    out.extend_from_slice(&(block.len() as u32).to_le_bytes());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&block);
    out
}

// ---------------------------------------------------------------------------------------------------------------------------
// the `--hash` command

const USAGE: &str = "usage: atlas --hash STRING... [--hashes FILE.wni] [--names LIST.txt] [--keys FILE|FOLDER]\n\
  prints the hashes of each string (FNV-1a 64, 63-bit, IW Resources exact and lower-case; hex, with the 63- and 60-bit masked forms);\n\
  --hashes: also looks each hash up in a WNI index (or set ATLAS_HASHES); --names: more strings, one per line;\n\
  --keys: also tests every form against the entry keys of the KAPI packages (.xpak/.xsub) there";

/// Every hash form of a string: label, value.
pub fn forms(s: &str) -> Vec<(&'static str, u64)> {
    let lower = s.to_ascii_lowercase();
    let mut v = vec![("fnv1a64", fnv1a64(s)), ("fnv1a63", fnv1a63(s)), ("iw-resource", hash_iw_resource(s))];
    if lower != s {
        v.push(("fnv1a64 lower", fnv1a64(&lower)));
        v.push(("fnv1a63 lower", fnv1a63(&lower)));
        v.push(("iw-resource lower", hash_iw_resource(&lower)));
    }
    v
}

/// Path of the index: the option value, else `ATLAS_HASHES`.
pub fn index_path(option: Option<&str>) -> Option<std::path::PathBuf> {
    option.map(std::path::PathBuf::from).or_else(|| std::env::var_os("ATLAS_HASHES").filter(|v| !v.is_empty()).map(std::path::PathBuf::from))
}

fn collect_kapi(path: &Path, out: &mut Vec<std::path::PathBuf>) {
    if path.is_dir() {
        let Ok(rd) = std::fs::read_dir(path) else { return };
        let mut v: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        v.sort();
        for p in v {
            collect_kapi(&p, out);
        }
    } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "xpak" | "xsub")) {
        out.push(path.to_path_buf());
    }
}

pub fn command(args: &[String]) -> Result<()> {
    let (mut strings, mut index_arg, mut keys_arg): (Vec<String>, Option<String>, Option<String>) = (Vec::new(), None, None);
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hashes" | "--names" | "--keys" => {
                let name = args[i].clone();
                i += 1;
                let v = args.get(i).with_context(|| format!("{name} needs a value"))?.clone();
                match name.as_str() {
                    "--hashes" => index_arg = Some(v),
                    "--keys" => keys_arg = Some(v),
                    _ => {
                        let text = std::fs::read_to_string(&v).with_context(|| v.clone())?;
                        strings.extend(text.lines().map(|l| l.trim_end_matches('\r')).filter(|l| !l.is_empty()).map(str::to_owned));
                    }
                }
            }
            a if a.starts_with("--") => bail!("unknown option {a}\n{USAGE}"),
            _ => strings.push(args[i].clone()),
        }
        i += 1;
    }
    if strings.is_empty() {
        bail!("{USAGE}");
    }
    let index = match index_path(index_arg.as_deref()) {
        Some(p) => {
            let t = std::time::Instant::now();
            let ix = HashIndex::open(&p)?;
            eprintln!("index: {} ({} hashes, header says {}, {} skipped, {:.1} s)", p.display(), ix.len(), ix.declared, ix.skipped, t.elapsed().as_secs_f64());
            Some(ix)
        }
        None => None,
    };
    // entry keys of KAPI packages: key -> package file name (first one)
    let mut keys: Option<std::collections::HashMap<u64, String>> = None;
    if let Some(k) = &keys_arg {
        let mut files = Vec::new();
        collect_kapi(Path::new(k), &mut files);
        let mut m = std::collections::HashMap::new();
        for f in &files {
            if let Ok(p) = crate::kapi::Kapi::open(f) {
                let name = f.file_name().unwrap().to_string_lossy().into_owned();
                for e in &p.entries {
                    m.entry(e.key).or_insert_with(|| name.clone());
                }
            }
        }
        eprintln!("keys: {} distinct entry keys from {} packages", m.len(), files.len());
        keys = Some(m);
    }
    for s in &strings {
        println!("{s}");
        for (label, h) in forms(s) {
            let mut line = format!("  {label:<18} {h:#018x}  63-bit {:#018x}  60-bit {:#018x}  48-bit {:#014x}", h & MASK63, h & MASK60, h & 0xffff_ffff_ffff);
            if let Some(ix) = &index {
                match ix.lookup(h) {
                    Some(n) if n.eq_ignore_ascii_case(s) => line += "  index: known",
                    Some(n) => line += &format!("  index: {n:?} (a different string)"),
                    None => line += "  index: not found",
                }
            }
            if let Some(k) = &keys {
                for (what, v) in [("64", h), ("63", h & MASK63), ("60", h & MASK60), ("48", h & 0xffff_ffff_ffff)] {
                    if let Some(p) = k.get(&v) {
                        line += &format!("  KEY MATCH ({what}-bit form) in {p}");
                    }
                }
            }
            println!("{line}");
        }
    }
    if index.is_none() {
        eprintln!("no index given (--hashes FILE.wni or ATLAS_HASHES): names are not looked up");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_known_vectors() {
        // published FNV-1a 64-bit test vectors
        assert_eq!(fnv1a64(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64("foobar"), 0x8594_4171_f739_67e8);
        assert_eq!(fnv1a63("foobar"), 0x8594_4171_f739_67e8 & MASK63);
    }

    #[test]
    fn type_names_hash_to_the_types_of_the_install() {
        // record types measured in the cod26 zones
        assert_eq!(fnv1a32("localize"), 0x2f64_0f0a);
        assert_eq!(fnv1a32("image"), 0xb351_35fa);
        assert_eq!(fnv1a32("techset"), 0x6bba_e577);
        assert_eq!(asset_type_name(0xc69e_b639), Some("xmodelsurfs"));
        assert_eq!(asset_type_name(0x0121_073c), Some("stringtable"));
        assert_eq!(asset_type_name(0x1234_5678), None);
        // every name is distinct and hashes to a distinct type
        let mut h: Vec<u32> = ASSET_TYPE_NAMES.iter().map(|n| fnv1a32(n)).collect();
        h.sort();
        h.dedup();
        assert_eq!(h.len(), ASSET_TYPE_NAMES.len());
    }

    #[test]
    fn iw_resource_is_masked_and_case_sensitive() {
        let h = hash_iw_resource("wpc/rex/priority_psos_frontend.csv");
        assert_eq!(h & !MASK63, 0);
        assert_ne!(h, hash_iw_resource("WPC/REX/priority_psos_frontend.csv"));
        assert_eq!(hash_iw_resource_lower("WPC/REX/Priority_psos_frontend.csv"), h);
        // the empty string hashes to the offset basis (masked)
        assert_eq!(hash_iw_resource(""), IW_RESOURCE_OFFSET & MASK63);
    }

    fn sample() -> Vec<(u64, String)> {
        ["wpc/rex/priority_psos_frontend.csv", "codhq26_shared", "cs_water.hlsl/cs_water_tile_generate_underwater_mesh_9x9"].iter().map(|s| (hash_iw_resource(s), s.to_string())).collect()
    }

    #[test]
    fn synthetic_index_round_trip() {
        let owned = sample();
        let pairs: Vec<(u64, &str)> = owned.iter().map(|(h, s)| (*h, s.as_str())).collect();
        let ix = HashIndex::from_bytes(&build_wni(&pairs)).unwrap();
        assert_eq!((ix.len(), ix.declared, ix.skipped), (3, 3, 0));
        for (h, s) in &owned {
            assert_eq!(ix.lookup(*h), Some(s.as_str()));
            // mask-insensitive: the 64-bit form with the top bits set and the 60-bit form find the same string
            assert_eq!(ix.lookup(*h | 0xF000_0000_0000_0000), Some(s.as_str()));
            assert_eq!(ix.lookup(*h & MASK60), Some(s.as_str()));
        }
        assert_eq!(ix.lookup(hash_iw_resource("unknown")), None);
    }

    #[test]
    fn long_data_uses_extended_literal_lengths() {
        // more than 15 + 255 bytes of data exercises the extra length bytes of the literal-only block
        let names: Vec<String> = (0..200).map(|i| format!("maps/mp/some_long_name_{i}.csv")).collect();
        let pairs: Vec<(u64, &str)> = names.iter().map(|s| (hash_iw_resource(s), s.as_str())).collect();
        let ix = HashIndex::from_bytes(&build_wni(&pairs)).unwrap();
        assert_eq!(ix.len(), 200);
        for n in &names {
            assert_eq!(ix.lookup(hash_iw_resource(n)), Some(n.as_str()));
        }
    }

    #[test]
    fn first_string_of_a_masked_collision_wins() {
        let ix = HashIndex::from_bytes(&build_wni(&[(0x1234, "first"), (0xF000_0000_0000_1234, "second")])).unwrap();
        assert_eq!((ix.len(), ix.skipped), (1, 1));
        assert_eq!(ix.lookup(0x1234), Some("first"));
    }

    #[test]
    fn bad_files_are_rejected() {
        let good = build_wni(&[(1, "a")]);
        assert!(HashIndex::from_bytes(&good[..10]).is_err());
        let mut m = good.clone();
        m[0] = b'X';
        assert!(HashIndex::from_bytes(&m).is_err());
        let mut v = good.clone();
        v[4] = 2;
        assert!(HashIndex::from_bytes(&v).is_err());
        // the block is cut off
        assert!(HashIndex::from_bytes(&good[..good.len() - 1]).is_err());
        // a string without its terminator
        let mut t = build_wni(&[(1, "abc")]);
        let n = t.len();
        t[n - 1] = b'x';
        assert!(HashIndex::from_bytes(&t).is_err());
    }
}
