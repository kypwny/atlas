//! Block codecs of the newest Call of Duty containers: Oodle through the game's own `oo2core_*_win64.dll` (loaded at run time with
//! `LoadLibraryW`; nothing of it is bundled or redistributed) and a small LZ4 block decoder.
//!
//! The DLL is a plain compression library. `Oodle::find` looks for it next to the game: the folder of the file that is opened and
//! up to four folders above it (the install root of `Call of Duty HQ` has `oo2core_8_win64.dll`), or at the path in the
//! environment variable `ATLAS_OODLE_DLL`.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

type DecompressFn = unsafe extern "system" fn(
    comp: *const u8,
    comp_len: isize,
    raw: *mut u8,
    raw_len: isize,
    fuzz_safe: i32,
    check_crc: i32,
    verbosity: i32,
    dec_buf_base: *mut u8,
    dec_buf_size: isize,
    callback: *mut u8,
    callback_user: *mut u8,
    decoder_memory: *mut u8,
    decoder_memory_size: isize,
    thread_phase: i32,
) -> isize;

/// A loaded `oo2core` library. Decompression is thread safe (`OodleLZ_Decompress` keeps no state between calls).
pub struct Oodle {
    decompress: DecompressFn,
    pub path: PathBuf,
}

unsafe impl Send for Oodle {}
unsafe impl Sync for Oodle {}

#[cfg(windows)]
mod sys {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn LoadLibraryW(name: *const u16) -> *mut c_void;
        pub fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }
}

/// Slack the decoder may write past the end of the output (the fast paths copy in 8..64 byte steps).
const SLACK: usize = 256;

impl Oodle {
    /// Loads the library at `path`.
    #[cfg(windows)]
    pub fn load(path: &Path) -> Result<Oodle> {
        use std::os::windows::ffi::OsStrExt;
        let abs = std::fs::canonicalize(path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        let mut wide: Vec<u16> = abs.as_os_str().encode_wide().collect();
        // canonicalize gives a \\?\ path; LoadLibraryW takes it as is
        wide.push(0);
        let module = unsafe { sys::LoadLibraryW(wide.as_ptr()) };
        if module.is_null() {
            bail!("cannot load {}", path.display());
        }
        let f = unsafe { sys::GetProcAddress(module, b"OodleLZ_Decompress\0".as_ptr()) };
        if f.is_null() {
            bail!("{} has no OodleLZ_Decompress", path.display());
        }
        let decompress: DecompressFn = unsafe { std::mem::transmute(f) };
        Ok(Oodle { decompress, path: path.to_path_buf() })
    }

    #[cfg(not(windows))]
    pub fn load(path: &Path) -> Result<Oodle> {
        bail!("Oodle is only available on Windows ({})", path.display())
    }

    /// Looks for `oo2core_*_win64.dll`: `ATLAS_OODLE_DLL`, then the folder of `start` and four folders above it.
    pub fn find(start: &Path) -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("ATLAS_OODLE_DLL") {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
        let start = &std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
        let mut dir = if start.is_dir() { Some(start.to_path_buf()) } else { start.parent().map(Path::to_path_buf) };
        for _ in 0..5 {
            let d = dir?;
            if let Ok(rd) = std::fs::read_dir(&d) {
                let mut names: Vec<PathBuf> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                            let n = n.to_ascii_lowercase();
                            n.starts_with("oo2core_") && n.ends_with("_win64.dll")
                        })
                    })
                    .collect();
                names.sort();
                if let Some(p) = names.pop() {
                    return Some(p);
                }
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        None
    }

    /// Decompresses one Oodle block of known raw size and appends the result to `out`.
    pub fn decompress_append(&self, comp: &[u8], raw_len: usize, out: &mut Vec<u8>) -> Result<()> {
        if raw_len == 0 {
            return Ok(());
        }
        out.reserve(raw_len + SLACK);
        let start = out.len();
        // fuzzSafe is off: the blocks are not marked as fuzz safe and Oodle then refuses them
        let r = unsafe {
            (self.decompress)(
                comp.as_ptr(),
                comp.len() as isize,
                out.as_mut_ptr().add(start),
                raw_len as isize,
                0,
                0,
                0,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                3,
            )
        };
        if r != raw_len as isize {
            bail!("Oodle returned {r} for a block of {} compressed and {raw_len} raw bytes", comp.len());
        }
        unsafe { out.set_len(start + raw_len) };
        Ok(())
    }

    pub fn decompress(&self, comp: &[u8], raw_len: usize) -> Result<Vec<u8>> {
        let mut v = Vec::new();
        self.decompress_append(comp, raw_len, &mut v)?;
        Ok(v)
    }
}

/// Decodes one LZ4 block (no frame) of known raw size and appends the result to `out`.
pub fn lz4_block_append(src: &[u8], raw_len: usize, out: &mut Vec<u8>) -> Result<()> {
    let base = out.len();
    out.reserve(raw_len);
    let mut i = 0usize;
    let len = |src: &[u8], i: &mut usize, mut n: usize| -> Result<usize> {
        if n == 15 {
            loop {
                let b = *src.get(*i).ok_or_else(|| anyhow::anyhow!("LZ4: length runs past the input"))?;
                *i += 1;
                n += b as usize;
                if b != 255 {
                    break;
                }
            }
        }
        Ok(n)
    };
    while i < src.len() {
        let token = src[i];
        i += 1;
        let lit = len(src, &mut i, (token >> 4) as usize)?;
        let Some(chunk) = src.get(i..i + lit) else { bail!("LZ4: literals run past the input") };
        out.extend_from_slice(chunk);
        i += lit;
        if i >= src.len() {
            break;
        }
        let Some(o) = src.get(i..i + 2) else { bail!("LZ4: offset runs past the input") };
        let offset = u16::from_le_bytes([o[0], o[1]]) as usize;
        i += 2;
        let m = len(src, &mut i, (token & 15) as usize)? + 4;
        let produced = out.len() - base;
        if offset == 0 || offset > produced {
            bail!("LZ4: match offset {offset} with {produced} bytes written");
        }
        if produced + m > raw_len {
            bail!("LZ4: block overruns its raw size {raw_len}");
        }
        let from = out.len() - offset;
        for k in 0..m {
            let b = out[from + k];
            out.push(b);
        }
    }
    if out.len() - base != raw_len {
        bail!("LZ4: block gave {} bytes, {raw_len} expected", out.len() - base);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lz4_literals_and_overlapping_match() {
        // "abc" then a match of 6 bytes at distance 3 -> "abcabcabc"; the final sequence carries literals only
        let src = [0x32, b'a', b'b', b'c', 0x03, 0x00, 0x10, b'z'];
        let mut out = Vec::new();
        lz4_block_append(&src, 10, &mut out).unwrap();
        assert_eq!(out, b"abcabcabcz");
    }

    #[test]
    fn lz4_rejects_a_bad_offset() {
        let src = [0x10, b'a', 0x05, 0x00, 0x00];
        assert!(lz4_block_append(&src, 9, &mut Vec::new()).is_err());
    }
}
