//! Writing Black Ops II PC fastfiles: the image (see `t6`) goes in, a fastfile comes out.
//!
//! The encoder mirrors what the loader in `t6::open_named` undoes, chunk by chunk and in the opposite order. The image is cut into
//! chunks, a chunk is raw-deflated, the SHA-1 of the deflated bytes is taken, the deflated bytes are Salsa20-encrypted with the IV
//! that the head of the stream's rolling hash table holds, then the table advances by one block and the new head is XORed with
//! the SHA-1 (the loader decrypts first and hashes the plaintext, which gives the same table). A chunk is written as
//! `[u32 size][data]`; the streams take the chunks in turn; a size word never straddles a 0x80000-byte page of the file (the few
//! bytes left of a page are zero). Chunk sizes follow OpenAssetTools' T6 writer (GPL-3.0; this is an independent implementation):
//! at most 0x8000 - 0x40 bytes of image per chunk, and at least 0x40 zero bytes at the end, aligned to 0x40.
//!
//! The RSA signature of Treyarch's zones cannot be made. Written zones are unsigned (`TAffu100`, no authentication header; the
//! zone name for the cipher comes from the file name) unless the image is unchanged and the original authentication header is
//! passed in, in which case a signed container comes out. The signature covers the final hash table, which depends on the exact
//! deflate output of every chunk; this encoder is not zlib, so its deflate bytes equal Treyarch's only for some chunks, and a
//! copied signature is only good where the written file equals the original (the tests count those): to keep a valid signature
//! copy the original file.
//!
//! Acceptance of a written zone is what `t6::open_named` reads back (identical image) and what the schema walker accepts; the
//! game itself has not been run on any of this.

use super::t6::{BLOCK_HASHES, CHUNK_MAX, PAGE, SALSA20_KEY, STREAMS, T6Zone, VERSION_PC, salsa20_xor, sha1};
use anyhow::{Result, bail};
use flate2::{Compression, write::DeflateEncoder};
use std::io::Write;

/// Image bytes per chunk that OpenAssetTools' writer takes (`XCHUNK_SIZE - 0x40`).
pub const CHUNK_IMAGE_SIZE: usize = CHUNK_MAX - 0x40;
const SUFFIX_MIN: usize = 0x40;
const SUFFIX_ALIGN: usize = 0x40;

/// How a fastfile is written.
#[derive(Clone, Debug)]
pub struct WriteOptions {
    /// The authentication header (`PHEEBs71`, flags, name, signature: 300 bytes) of the original zone, which makes a signed
    /// container. Only valid when the image is the original's and its chunking is repeated (`chunk_sizes`).
    pub auth_header: Option<Vec<u8>>,
    /// Inflated size of each chunk; `None` cuts the image the way OpenAssetTools does.
    pub chunk_sizes: Option<Vec<u32>>,
    /// Deflate level (OpenAssetTools uses the best compression, 9).
    pub level: u32,
    /// Zero bytes after the last chunk: `Some(n)` keeps `n` (what the original had), `None` follows OpenAssetTools.
    pub suffix: Option<usize>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions { auth_header: None, chunk_sizes: None, level: 9, suffix: None }
    }
}

impl WriteOptions {
    /// Options that repeat the container of an opened zone as far as possible (signed when the zone was).
    pub fn like(zone: &T6Zone) -> WriteOptions {
        WriteOptions {
            auth_header: (!zone.auth_header.is_empty()).then(|| zone.auth_header.clone()),
            chunk_sizes: Some(zone.chunk_sizes.clone()),
            ..WriteOptions::default()
        }
    }
}

/// The per-stream rolling table of SHA-1 blocks the cipher takes its IVs from.
pub struct HashTable {
    blocks: Vec<u8>,
    index: [usize; STREAMS],
}

impl HashTable {
    /// Seeded with the zone name's characters (at most 31), four bytes each.
    pub fn new(name: &str) -> Result<HashTable> {
        let seed = name.as_bytes();
        let seed = &seed[..seed.len().min(31)];
        if seed.is_empty() {
            bail!("the cipher needs a zone name");
        }
        let mut blocks = vec![0u8; BLOCK_HASHES * STREAMS * 20];
        let mut k = 0;
        for i in (0..blocks.len()).step_by(4) {
            blocks[i..i + 4].fill(seed[k % seed.len()]);
            k += 1;
        }
        Ok(HashTable { blocks, index: [0; STREAMS] })
    }

    fn at(&self, stream: usize) -> usize {
        self.index[stream] * STREAMS * 20 + stream * 20
    }

    /// The IV for the next chunk of `stream`.
    fn iv(&self, stream: usize) -> [u8; 8] {
        let at = self.at(stream);
        self.blocks[at..at + 8].try_into().unwrap()
    }

    /// Advances the stream by one chunk whose plaintext (the deflated bytes) hashed to `digest`.
    fn advance(&mut self, stream: usize, digest: &[u8; 20]) {
        self.index[stream] = (self.index[stream] + 1) % BLOCK_HASHES;
        let next = self.at(stream);
        for (i, d) in digest.iter().enumerate() {
            self.blocks[next + i] ^= d;
        }
    }

    /// The whole table (what a signature covers).
    pub fn bytes(&self) -> &[u8] {
        &self.blocks
    }

    /// Encrypts one deflated chunk in place and moves the stream on (the mirror of the loader's decrypt-then-hash).
    pub fn encrypt(&mut self, stream: usize, chunk: &mut [u8]) {
        let digest = sha1(chunk);
        let iv = self.iv(stream);
        salsa20_xor(&SALSA20_KEY, &iv, chunk);
        self.advance(stream, &digest);
    }
}

/// Raw deflate of `data`.
pub fn deflate(data: &[u8], level: u32) -> Vec<u8> {
    let mut enc = DeflateEncoder::new(Vec::with_capacity(data.len() / 2 + 64), Compression::new(level.min(9)));
    enc.write_all(data).expect("write to memory");
    enc.finish().expect("deflate to memory")
}

/// The fastfile for `image` (an `XFile` header, the asset list and the streamed assets). `name` is the zone name (the file name
/// without its extension), which seeds the cipher; it is also written into a signed zone's header when `auth_header` is given
/// (the header carries its own copy, which has to agree).
pub fn write_fastfile(name: &str, image: &[u8], opts: &WriteOptions) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(image.len() / 3 + 4096);
    out.extend_from_slice(if opts.auth_header.is_some() { b"TAff0100" } else { b"TAffu100" });
    out.extend_from_slice(&VERSION_PC.to_le_bytes());
    let mut cipher_name = name.to_string();
    if let Some(h) = &opts.auth_header {
        if h.len() != 8 + 4 + 32 + 256 || &h[..8] != b"PHEEBs71" {
            bail!("authentication header must be 300 bytes starting with PHEEBs71");
        }
        let inner = &h[12..44];
        let inner = String::from_utf8_lossy(&inner[..inner.iter().position(|&c| c == 0).unwrap_or(32)]).into_owned();
        if !name.is_empty() && inner != name {
            bail!("the authentication header names the zone {inner:?}, not {name:?}");
        }
        cipher_name = inner;
        out.extend_from_slice(h);
    }
    let mut table = HashTable::new(&cipher_name)?;
    let mut page_offset = out.len() % PAGE;
    let mut stream = 0usize;
    let mut pos = 0usize;
    let mut sizes = opts.chunk_sizes.as_deref().unwrap_or(&[]).iter().copied();
    while pos < image.len() {
        let take = if opts.chunk_sizes.is_some() {
            match sizes.next() {
                Some(n) => n as usize,
                None => bail!("the chunk list ends before the image does"),
            }
        } else {
            CHUNK_IMAGE_SIZE
        }
        .min(image.len() - pos);
        if take == 0 {
            bail!("empty chunk");
        }
        let mut chunk = deflate(&image[pos..pos + take], opts.level);
        pos += take;
        if chunk.len() > CHUNK_MAX {
            bail!("a chunk deflates to {} bytes, more than {CHUNK_MAX:#x}", chunk.len());
        }
        table.encrypt(stream, &mut chunk);
        if page_offset + 4 > PAGE {
            out.resize(out.len() + PAGE - page_offset, 0);
            page_offset = 0;
        }
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&chunk);
        page_offset = (page_offset + 4 + chunk.len()) % PAGE;
        stream = (stream + 1) % STREAMS;
    }
    if opts.chunk_sizes.is_some() && sizes.next().is_some() {
        bail!("the chunk list is longer than the image");
    }
    match opts.suffix {
        Some(n) => out.resize(out.len() + n, 0),
        None => {
            out.resize(out.len() + SUFFIX_MIN, 0);
            let rem = out.len() % SUFFIX_ALIGN;
            if rem != 0 {
                out.resize(out.len() + SUFFIX_ALIGN - rem, 0);
            }
        }
    }
    Ok(out)
}

/// Writes an opened zone back: same image, same chunking, same signature when `keep_signature` (and a signed original).
pub fn write_zone(zone: &T6Zone, keep_signature: bool) -> Result<Vec<u8>> {
    let mut opts = WriteOptions::like(zone);
    if !keep_signature {
        opts.auth_header = None;
    }
    write_fastfile(&zone.name, &zone.image, &opts)
}
