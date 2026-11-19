// SPDX-License-Identifier: GPL-3.0-only (ported by its copyright holder from his own server code, codol-qa-server)
//! CoDOL fastfile (`.ff`) container: decryption and inflation to the zone image.
//!
//! Layout, as in the server project's `codol_fastfile.c` (checked against its output): a 0x115-byte header
//! (`IWff0100` signed or `IWffu100`, `u32` version, a 256-byte RSA blob at 0x15 holding the AES-192 key and
//! 16-byte IV), then an AES-CTR body (the counter is the IV as a little-endian 128-bit integer, incremented
//! little-endian per 16-byte block). Signed files start with an `IWffs100` block whose SHA-256 table covers every
//! 0x2000-byte block. The inflated body is the zone image.
//!
//! The RSA private key is found in the game's own executable (or in a `c1_keys.bin`), never stored here.

use aes::Aes192;
use aes::cipher::{Block, BlockCipherEncrypt, KeyInit};
use anyhow::{Context, Result, bail};
use flate2::read::ZlibDecoder;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::{Oaep, Pkcs1v15Encrypt, PrivateKey};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

const HEADER: usize = 0x115;
const BLOCK: usize = 0x2000;
const DER_SIZE: usize = 1191;
/// SHA-256 of the archive key's DER (same key in 1.1.1.13, QA 3.3.3.3 and 3.19).
const DER_SHA256: [u8; 32] = [
    0x3a, 0x2f, 0x78, 0x91, 0xac, 0x98, 0xf1, 0xfd, 0xf6, 0xa9, 0xa2, 0x21, 0xc5, 0x58, 0x67, 0x25, 0x15, 0x7d, 0xf2, 0x70, 0xd6, 0x83, 0x1e, 0x0f, 0xf8, 0x2c, 0x38, 0x3c, 0xd2,
    0x62, 0xcb, 0xb5,
];
pub const VERSION_C1: u32 = 334;
pub const VERSION_QA: u32 = 423;
pub const VERSION_319: u32 = 461;
/// Black Ops II (`TAff0100`).
pub const VERSION_BO2: u32 = 147;

/// Which header family a fastfile belongs to (from its magic).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    /// `IWff0100` / `IWffu100` / `IWffa100`: Infinity Ward's container, also used by Call of Duty Online. The
    /// version number tells the games apart.
    Iw,
    /// `TAff0100`: Treyarch (Black Ops II here).
    Treyarch,
}

/// What a fastfile version belongs to, and whether the zone replay (model index) can read it.
#[derive(Clone, Copy, Debug)]
pub struct VersionInfo {
    pub build: &'static str,
    pub replay: bool,
}

/// Names the build for a fastfile (family, version). Versions seen in the files on this machine; Xbox 360 files
/// are big-endian and are named for the swapped number.
pub fn version_info(family: Family, version: u32) -> VersionInfo {
    let (build, replay) = match (family, version) {
        (Family::Iw, 5) => ("Call of Duty 4", false),
        (Family::Iw, 25) => ("Call of Duty HQ (modern engine)", false),
        (Family::Iw, 276) => ("Modern Warfare 2", false),
        (Family::Iw, 316) => ("Call of Duty Online Alpha 2 (0.0.2.2)", false),
        (Family::Iw, VERSION_C1) => ("Call of Duty Online 1.1.1.13 (retail client)", true),
        (Family::Iw, VERSION_QA) => ("Call of Duty Online QA 3.3.3.3", true),
        (Family::Iw, VERSION_319) => ("Call of Duty Online 3.19", true),
        (Family::Iw, 253) => ("Modern Warfare 2 development build (Xbox 360)", false),
        (Family::Iw, _) => ("Infinity Ward engine, unknown build", false),
        (Family::Treyarch, VERSION_BO2) => ("Black Ops II", false),
        (Family::Treyarch, _) => ("Treyarch, unknown game", false),
    };
    VersionInfo { build, replay }
}

/// The fixed part of a fastfile header, readable without the archive key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peek {
    pub family: Family,
    pub signed: bool,
    /// Big-endian file (Xbox 360); `version` is already swapped.
    pub big_endian: bool,
    pub version: u32,
}

pub fn peek(path: &Path) -> Result<Peek> {
    let mut head = [0u8; 12];
    std::fs::File::open(path).with_context(|| path.display().to_string())?.read_exact(&mut head).context("file too short for a fastfile header")?;
    let (family, signed) = match &head[..8] {
        b"IWff0100" => (Family::Iw, true),
        b"IWffu100" | b"IWffa100" => (Family::Iw, false),
        // Treyarch's header has no signed/unsigned split in the magic
        b"TAff0100" => (Family::Treyarch, false),
        other => bail!("not a known fastfile ({:?})", String::from_utf8_lossy(other)),
    };
    let le = u32::from_le_bytes(head[8..12].try_into().unwrap());
    // No real version is above 0xFFFF; a number that large is a big-endian one read the wrong way round.
    let (version, big_endian) = if le > 0xFFFF { (le.swap_bytes(), true) } else { (le, false) };
    Ok(Peek { family, signed, big_endian, version })
}

pub struct ArchiveKey {
    pub(crate) key: PrivateKey,
}

impl ArchiveKey {
    /// Reads the key from `c1_keys.bin` (256-byte blob + DER) or finds its DER inside an executable.
    pub fn load(path: &Path) -> Result<ArchiveKey> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let der = if data.len() == 256 + DER_SIZE {
            data[256..].to_vec()
        } else {
            find_der(&data).with_context(|| format!("{}: no CoDOL archive key found", path.display()))?.to_vec()
        };
        if Sha256::digest(&der).as_slice() != DER_SHA256 {
            bail!("{}: key does not match the known archive key", path.display());
        }
        let key = PrivateKey::from_pkcs1_der(&der).map_err(|e| anyhow::anyhow!("archive key: {e}"))?;
        Ok(ArchiveKey { key })
    }
}

/// File offset of the archive key inside an executable (found by hash, nothing is hard-coded).
pub fn der_offset(data: &[u8]) -> Option<usize> {
    find_der(data).map(|d| d.as_ptr() as usize - data.as_ptr() as usize)
}

/// Scans for the PKCS#1 key header and checks the hash, so no offsets are hard-coded.
fn find_der(data: &[u8]) -> Option<&[u8]> {
    const PREFIX: [u8; 7] = [0x30, 0x82, 0x04, 0xa3, 0x02, 0x01, 0x00];
    let mut from = 0;
    while let Some(i) = data[from..].windows(PREFIX.len()).position(|w| w == PREFIX) {
        let at = from + i;
        if let Some(cand) = data.get(at..at + DER_SIZE) {
            if Sha256::digest(cand).as_slice() == DER_SHA256 {
                return Some(cand);
            }
        }
        from = at + 1;
    }
    None
}

/// AES-CTR, counter incremented as a little-endian 128-bit integer (not the usual big-endian).
fn ctr(cipher: &Aes192, iv: &[u8], data: &[u8]) -> Vec<u8> {
    let mut counter = u128::from_le_bytes(iv[..16].try_into().unwrap());
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let mut block = Block::<Aes192>::from(counter.to_le_bytes());
        cipher.encrypt_block(&mut block);
        out.extend(chunk.iter().zip(block.iter()).map(|(a, b)| a ^ b));
        counter = counter.wrapping_add(1);
    }
    out
}

/// What was needed to open a fastfile and is needed again to write one back: the key material and the header parts the
/// zone image itself does not carry.
#[derive(Clone)]
pub struct Container {
    pub signed: bool,
    /// Header bytes 0x0c..0x15 (between the version and the key blob).
    pub header_tail: [u8; 9],
    /// The RSA-wrapped key (256 bytes) and the AES-192 key + IV it holds (40 bytes).
    pub key_blob: Vec<u8>,
    pub key_iv: [u8; 40],
    /// For signed files, the decrypted first 0x2000-byte block: a template for the new file's header block.
    pub signed_header: Option<Vec<u8>>,
}

/// Result of opening a fastfile.
pub struct Opened {
    pub version: u32,
    pub image: Vec<u8>,
    pub container: Container,
}

pub fn open(path: &Path, key: &ArchiveKey) -> Result<Opened> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    open_bytes(&raw, key).with_context(|| path.display().to_string())
}

pub fn open_bytes(raw: &[u8], key: &ArchiveKey) -> Result<Opened> {
    if raw.len() < HEADER {
        bail!("too short for a fastfile");
    }
    let magic = &raw[..8];
    let unsigned = magic == b"IWffu100";
    if !unsigned && magic != b"IWff0100" {
        bail!("not a CoDOL fastfile ({:?})", String::from_utf8_lossy(magic));
    }
    let version = u32::from_le_bytes(raw[8..12].try_into().unwrap());
    if !matches!(version, VERSION_C1 | VERSION_QA | VERSION_319) {
        bail!("unsupported fastfile version {version}");
    }
    // The key blob is OAEP in 1.1.1.13 and PKCS #1 v1.5 from QA on.
    let blob = &raw[0x15..0x15 + 256];
    let key_iv = if version == VERSION_C1 { key.key.decrypt(Oaep::<Sha256>::new(), blob) } else { key.key.decrypt(Pkcs1v15Encrypt, blob) }
        .map_err(|e| anyhow::anyhow!("cannot unwrap the archive key: {e}"))?;
    if key_iv.len() != 40 {
        bail!("unexpected key blob size {}", key_iv.len());
    }
    let cipher = Aes192::new_from_slice(&key_iv[..24]).map_err(|e| anyhow::anyhow!("{e}"))?;
    let iv = &key_iv[24..40];
    let body = &raw[HEADER..];
    let mut signed_header = None;

    let compressed = if unsigned {
        ctr(&cipher, iv, body)
    } else {
        if body.len() % BLOCK != 0 || body.len() < 3 * BLOCK {
            bail!("signed body is not whole blocks");
        }
        let header = ctr(&cipher, iv, &body[..BLOCK]);
        if &header[..8] != b"IWffs100" || Sha256::digest(&header[0x12c..0x1fd0]).as_slice() != &header[0xc..0x2c] {
            bail!("signed header check failed");
        }
        signed_header = Some(header.clone());
        let mut out = Vec::with_capacity(body.len());
        let (mut section, mut table, mut index) = (0usize, Vec::new(), 0usize);
        for i in 1..body.len() / BLOCK {
            let block = &body[i * BLOCK..(i + 1) * BLOCK];
            if index == 0 {
                let entry = &header[0x150 + section * 32..0x150 + section * 32 + 32];
                table = ctr(&cipher, &entry[..16], block);
                if Sha256::digest(&table).as_slice() != entry {
                    bail!("hash table {section} mismatch");
                }
                index = 1;
            } else {
                let entry = &table[(index - 1) * 32..index * 32];
                let plain = ctr(&cipher, &entry[..16], block);
                if Sha256::digest(&plain).as_slice() != entry {
                    bail!("block {i} hash mismatch");
                }
                out.extend_from_slice(&plain);
                index += 1;
                if index == 257 {
                    section += 1;
                    index = 0;
                }
            }
        }
        out
    };
    let mut image = Vec::with_capacity(compressed.len() * 4);
    ZlibDecoder::new(&compressed[..]).read_to_end(&mut image).context("inflating the zone")?;
    let container = Container { signed: !unsigned, header_tail: raw[12..0x15].try_into().unwrap(), key_blob: blob.to_vec(), key_iv: key_iv.as_slice().try_into().unwrap(), signed_header };
    Ok(Opened { version, image, container })
}

/// Writes a fastfile from a zone image with the container information of the file it came from (the same AES key and IV
/// are reused, so the key blob stays valid). The image is deflated, then either encrypted as one stream (unsigned) or
/// cut into 0x2000-byte blocks with their hash tables (signed): each block is encrypted with an IV taken from its
/// SHA-256, each table from its entry in the header block, whose own hash covers the entries.
pub fn seal(image: &[u8], version: u32, c: &Container) -> Result<Vec<u8>> {
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    let mut z = ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(image)?;
    let compressed = z.finish()?;
    let cipher = Aes192::new_from_slice(&c.key_iv[..24]).map_err(|e| anyhow::anyhow!("{e}"))?;
    let iv = &c.key_iv[24..40];
    let mut out = Vec::with_capacity(compressed.len() + HEADER + 2 * BLOCK);
    out.extend_from_slice(if c.signed { b"IWff0100" } else { b"IWffu100" });
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&c.header_tail);
    out.extend_from_slice(&c.key_blob);
    debug_assert_eq!(out.len(), HEADER);
    if !c.signed {
        out.extend(ctr(&cipher, iv, &compressed));
        return Ok(out);
    }
    let Some(template) = &c.signed_header else { bail!("a signed file needs the header block of the file it replaces") };
    // 256 data blocks per hash table; the data blocks are the compressed stream padded to whole blocks
    let mut data = compressed;
    data.resize(data.len().div_ceil(BLOCK).max(1) * BLOCK, 0);
    let blocks: Vec<&[u8]> = data.chunks(BLOCK).collect();
    let sections = blocks.len().div_ceil(256);
    if 0x150 + sections * 32 > 0x1fd0 {
        bail!("{} blocks do not fit the header's hash table list", blocks.len());
    }
    let mut header = template.clone();
    let mut body: Vec<u8> = Vec::new();
    for (s, group) in blocks.chunks(256).enumerate() {
        let mut table = Vec::with_capacity(BLOCK);
        for b in group {
            table.extend_from_slice(Sha256::digest(b).as_slice());
        }
        table.resize(BLOCK, 0);
        let entry = Sha256::digest(&table);
        header[0x150 + s * 32..0x150 + s * 32 + 32].copy_from_slice(entry.as_slice());
        body.extend(ctr(&cipher, &entry[..16], &table));
        for b in group {
            let h = Sha256::digest(b);
            body.extend(ctr(&cipher, &h[..16], b));
        }
    }
    // the header's own hash covers the table entries (and the rest of 0x12c..0x1fd0)
    let digest = Sha256::digest(&header[0x12c..0x1fd0]);
    header[0xc..0x2c].copy_from_slice(digest.as_slice());
    out.extend(ctr(&cipher, iv, &header));
    out.extend(body);
    Ok(out)
}

#[cfg(test)]
mod peek_tests {
    use super::*;

    fn temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("codol-ff-{}-{name}", std::process::id()));
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn reads_version_and_signing_without_a_key() {
        let mut signed = b"IWff0100".to_vec();
        signed.extend_from_slice(&461u32.to_le_bytes());
        let p = temp("a.ff", &signed);
        assert_eq!(peek(&p).unwrap(), Peek { family: Family::Iw, signed: true, big_endian: false, version: VERSION_319 });
        let mut unsigned = b"IWffu100".to_vec();
        unsigned.extend_from_slice(&423u32.to_le_bytes());
        let q = temp("b.ff", &unsigned);
        assert_eq!(peek(&q).unwrap(), Peek { family: Family::Iw, signed: false, big_endian: false, version: VERSION_QA });
        assert!(version_info(Family::Iw, VERSION_QA).replay && version_info(Family::Iw, VERSION_319).replay && version_info(Family::Iw, VERSION_C1).replay);
        let mut ta = b"TAff0100".to_vec();
        ta.extend_from_slice(&147u32.to_le_bytes());
        let t = peek(&temp("e.ff", &ta)).unwrap();
        assert_eq!((t.family, t.version), (Family::Treyarch, VERSION_BO2));
        assert_eq!(version_info(t.family, t.version).build, "Black Ops II");
        let mut be = b"IWffu100".to_vec();
        be.extend_from_slice(&253u32.to_be_bytes());
        let x = peek(&temp("f.ff", &be)).unwrap();
        assert_eq!((x.version, x.big_endian), (253, true));
        assert!(peek(&temp("c.ff", b"MZ\x90\0\x03\0\0\0\x04\0\0\0")).is_err());
        assert!(peek(&temp("d.ff", b"IWff")).is_err());
    }
}
