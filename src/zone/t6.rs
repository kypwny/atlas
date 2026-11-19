//! Call of Duty: Black Ops II (T6) PC fastfiles.
//!
//! Container (the layout is documented by OpenAssetTools' T6 loader, GPL-3.0; this is an independent implementation):
//! `TAff0100`, version 147, then (signed zones) an authentication header `PHEEBs71`, 4 flag bytes, the zone name padded to 32
//! bytes and a 256-byte RSA signature. The rest is a series of chunks `[u32 size][data]` taken from four interleaved streams in
//! turn; a chunk-size word never straddles a 0x80000-byte page of the file (the few bytes left at a page end are skipped).
//! Every chunk is Salsa20-encrypted (a fixed key, an IV taken from a per-stream rolling table of SHA-1 blocks seeded with the
//! zone name) and then raw-deflated; the chunks of all streams, in order, make up the zone image: an `XFile` header (size,
//! external size, eight block sizes) followed by the asset list and the streamed assets.

use anyhow::{Context, Result, bail};
use flate2::read::DeflateDecoder;
use std::io::Read;

pub const VERSION_PC: u32 = 147;
/// The Salsa20 key of Black Ops II PC fastfiles (the same for every zone). The value is the one published in OpenAssetTools
/// (`ZoneConstants::SALSA20_KEY_TREYARCH_PC`, GPL-3.0, https://github.com/Laupetin/OpenAssetTools); see ATTRIBUTION.md.
pub(crate) const SALSA20_KEY: [u8; 32] = [
    0x64, 0x1D, 0x8A, 0x2F, 0xE3, 0x1D, 0x3A, 0xA6, 0x36, 0x22, 0xBB, 0xC9, 0xCE, 0x85, 0x87, 0x22, 0x9D, 0x42, 0xB0, 0xF8, 0xED, 0x9B, 0x92, 0x41, 0x30, 0xBF,
    0x88, 0xB6, 0x5E, 0xDC, 0x50, 0xBE,
];
pub(crate) const STREAMS: usize = 4;
pub(crate) const CHUNK_MAX: usize = 0x8000;
pub(crate) const PAGE: usize = 0x80000;
pub(crate) const BLOCK_HASHES: usize = 200;

/// Names of the T6 asset types by number.
pub const ASSET_TYPES: [&str; 60] = [
    "xmodelpieces", "physpreset", "physconstraints", "destructibledef", "xanimparts", "xmodel", "material", "technique_set", "image", "sound", "sound_patch", "clipmap",
    "clipmap_pvs", "comworld", "gameworld_sp", "gameworld_mp", "map_ents", "gfxworld", "light_def", "ui_map", "font", "fonticon", "menulist", "menu",
    "localize_entry", "weapon", "weapondef", "weapon_variant", "weapon_full", "attachment", "attachment_unique", "weapon_camo", "snddriver_globals", "fx",
    "impact_fx", "aitype", "mptype", "mpbody", "mphead", "character", "xmodelalias", "rawfile", "stringtable", "leaderboard", "xglobals", "ddl", "glasses",
    "emblemset", "scriptparsetree", "keyvaluepairs", "vehicledef", "memoryblock", "addon_map_ents", "tracer", "skinnedverts", "qdb", "slug",
    "footstep_table", "footstepfx_table", "zbarrier",
];

fn salsa20_block(key: &[u8; 32], iv: &[u8; 8], counter: u64) -> [u8; 64] {
    let le = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let c = b"expand 32-byte k";
    let mut s = [0u32; 16];
    s[0] = le(&c[0..]);
    s[5] = le(&c[4..]);
    s[10] = le(&c[8..]);
    s[15] = le(&c[12..]);
    for i in 0..4 {
        s[1 + i] = le(&key[4 * i..]);
        s[11 + i] = le(&key[16 + 4 * i..]);
    }
    s[6] = le(&iv[0..]);
    s[7] = le(&iv[4..]);
    s[8] = counter as u32;
    s[9] = (counter >> 32) as u32;
    let mut x = s;
    macro_rules! qr {
        ($a:expr, $b:expr, $c:expr, $d:expr) => {
            x[$b] ^= x[$a].wrapping_add(x[$d]).rotate_left(7);
            x[$c] ^= x[$b].wrapping_add(x[$a]).rotate_left(9);
            x[$d] ^= x[$c].wrapping_add(x[$b]).rotate_left(13);
            x[$a] ^= x[$d].wrapping_add(x[$c]).rotate_left(18);
        };
    }
    for _ in 0..10 {
        qr!(0, 4, 8, 12);
        qr!(5, 9, 13, 1);
        qr!(10, 14, 2, 6);
        qr!(15, 3, 7, 11);
        qr!(0, 1, 2, 3);
        qr!(5, 6, 7, 4);
        qr!(10, 11, 8, 9);
        qr!(15, 12, 13, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&x[i].wrapping_add(s[i]).to_le_bytes());
    }
    out
}

pub(crate) fn salsa20_xor(key: &[u8; 32], iv: &[u8; 8], data: &mut [u8]) {
    for (n, chunk) in data.chunks_mut(64).enumerate() {
        let block = salsa20_block(key, iv, n as u64);
        for (b, k) in chunk.iter_mut().zip(block) {
            *b ^= k;
        }
    }
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

/// A T6 zone image and what its header says.
pub struct T6Zone {
    pub name: String,
    pub signed: bool,
    pub image: Vec<u8>,
    /// Size and external size words of the `XFile` header, and the eight block sizes.
    pub size: u32,
    pub external_size: u32,
    pub block_sizes: [u32; 8],
    /// Inflated size of every chunk, in order (the chunking the file was written with; `t6_write` can repeat it).
    pub chunk_sizes: Vec<u32>,
    /// The authentication header of a signed zone (`PHEEBs71`, flags, zone name, RSA signature); empty when unsigned.
    pub auth_header: Vec<u8>,
}

/// Opens a T6 PC fastfile.
pub fn open(bytes: &[u8]) -> Result<T6Zone> {
    open_named(bytes, None)
}

/// Opens a T6 PC fastfile. An unsigned zone carries no name, so the cipher takes `name` (the file name without its extension,
/// as the game's loader does); a signed zone's own name wins.
pub fn open_named(bytes: &[u8], file_name: Option<&str>) -> Result<T6Zone> {
    if bytes.len() < 12 {
        bail!("too short for a fastfile");
    }
    let magic = &bytes[0..8];
    let signed = match magic {
        b"TAff0100" | b"ABff0100" => true,
        b"TAffu100" => false,
        _ => bail!("not a T6 PC fastfile (magic {:?})", String::from_utf8_lossy(magic)),
    };
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if version != VERSION_PC {
        bail!("T6 PC zone version {VERSION_PC} expected, found {version}");
    }
    let mut pos = 12;
    let mut name = file_name.unwrap_or_default().to_string();
    let mut auth_header = Vec::new();
    if signed {
        if bytes.len() < 12 + 8 + 4 + 32 + 256 || &bytes[12..20] != b"PHEEBs71" {
            bail!("missing authentication header");
        }
        let n = &bytes[24..56];
        name = String::from_utf8_lossy(&n[..n.iter().position(|&c| c == 0).unwrap_or(32)]).into_owned();
        pos = 12 + 8 + 4 + 32 + 256;
        auth_header = bytes[12..pos].to_vec();
    }
    // the rolling hash blocks: BLOCK_HASHES x STREAMS x 20 bytes, seeded with the zone name's characters, four bytes each
    let seed = name.as_bytes();
    let seed = &seed[..seed.len().min(31)];
    let mut hashes = vec![0u8; BLOCK_HASHES * STREAMS * 20];
    if seed.is_empty() {
        bail!("an unsigned zone needs its name for the cipher (use open_named)");
    }
    let mut k = 0;
    for i in (0..hashes.len()).step_by(4) {
        hashes[i..i + 4].fill(seed[k % seed.len()]);
        k += 1;
    }
    let mut index = [0usize; STREAMS];
    let mut image = Vec::new();
    let mut chunk_sizes = Vec::new();
    let mut page_offset = pos % PAGE;
    let mut stream = 0usize;
    loop {
        if page_offset + 4 > PAGE {
            pos += PAGE - page_offset;
            page_offset = 0;
        }
        if pos + 4 > bytes.len() {
            break;
        }
        let size = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        page_offset = (page_offset + 4) % PAGE;
        if size == 0 {
            break;
        }
        if size > CHUNK_MAX || pos + size > bytes.len() {
            bail!("bad chunk size {size} at {pos:#x}");
        }
        let mut chunk = bytes[pos..pos + size].to_vec();
        pos += size;
        page_offset = (page_offset + size) % PAGE;
        let at = index[stream] * STREAMS * 20 + stream * 20;
        let iv: [u8; 8] = hashes[at..at + 8].try_into().unwrap();
        salsa20_xor(&SALSA20_KEY, &iv, &mut chunk);
        let digest = sha1(&chunk);
        index[stream] = (index[stream] + 1) % BLOCK_HASHES;
        let next = index[stream] * STREAMS * 20 + stream * 20;
        for (i, d) in digest.iter().enumerate() {
            hashes[next + i] ^= d;
        }
        let mut inflated = Vec::new();
        DeflateDecoder::new(&chunk[..]).read_to_end(&mut inflated).with_context(|| format!("inflating the chunk at {:#x}", pos - size))?;
        chunk_sizes.push(inflated.len() as u32);
        image.extend_from_slice(&inflated);
        stream = (stream + 1) % STREAMS;
    }
    if image.len() < 40 {
        bail!("zone image too small ({} bytes)", image.len());
    }
    let w = |o: usize| u32::from_le_bytes(image[o..o + 4].try_into().unwrap());
    let mut block_sizes = [0u32; 8];
    for (i, b) in block_sizes.iter_mut().enumerate() {
        *b = w(8 + 4 * i);
    }
    Ok(T6Zone { name, signed, size: w(0), external_size: w(4), block_sizes, chunk_sizes, auth_header, image })
}

/// The asset list of a T6 zone: the script strings, the zones it depends on and the (type, header pointer) of every asset.
pub struct AssetList {
    pub strings: Vec<String>,
    pub depends: Vec<String>,
    pub assets: Vec<(u32, u32)>,
    /// Offset in the image where the first asset's data starts.
    pub data_start: usize,
}

impl T6Zone {
    /// Reads the asset list that opens the zone.
    pub fn asset_list(&self) -> Result<AssetList> {
        let img = &self.image;
        let w = |o: usize| -> Result<u32> { img.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).context("asset list runs past the image") };
        let (string_count, strings_ptr, depend_count, depends_ptr, asset_count, assets_ptr) = (w(40)?, w(44)?, w(48)?, w(52)?, w(56)?, w(60)?);
        let mut pos = 64usize;
        let mut read_strings = |count: u32, ptr: u32, pos: &mut usize| -> Result<Vec<String>> {
            let mut out = Vec::new();
            if ptr == 0 {
                return Ok(out);
            }
            if count > 1_000_000 {
                bail!("string count {count}");
            }
            let table = *pos;
            *pos += 4 * count as usize;
            for i in 0..count as usize {
                if w(table + 4 * i)? == u32::MAX {
                    let end = img[*pos..].iter().position(|&c| c == 0).context("unterminated string")?;
                    out.push(String::from_utf8_lossy(&img[*pos..*pos + end]).into_owned());
                    *pos += end + 1;
                } else {
                    out.push(String::new());
                }
            }
            Ok(out)
        };
        let strings = read_strings(string_count, strings_ptr, &mut pos)?;
        let depends = read_strings(depend_count, depends_ptr, &mut pos)?;
        let mut assets = Vec::new();
        if assets_ptr != 0 {
            // the array follows the strings directly: this loader does not align it
            if asset_count > 1_000_000 {
                bail!("asset count {asset_count}");
            }
            for i in 0..asset_count as usize {
                assets.push((w(pos + 8 * i)?, w(pos + 8 * i + 4)?));
            }
            pos += 8 * asset_count as usize;
        }
        Ok(AssetList { strings, depends, assets, data_start: pos })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_of_abc() {
        let d = sha1(b"abc");
        let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn salsa20_matches_the_published_vector() {
        // eSTREAM Salsa20/20 256-bit, set 1 vector 0: key 80 00.. , IV 0; first keystream bytes
        let mut key = [0u8; 32];
        key[0] = 0x80;
        let block = salsa20_block(&key, &[0u8; 8], 0);
        assert_eq!(&block[..8], &[0xE3, 0xBE, 0x8F, 0xDD, 0x8B, 0xEC, 0xA2, 0xE3]);
    }
}
