//! On-disk cache for the `.ifs` scan. Listing the 974 archives of the 3.19 client through the helper and reading the first
//! bytes of its 87,000 images takes about a minute and a half; none of it changes until an archive does. The cache keeps,
//! per archive, its size and modification time, the names inside it and the image headers read from it; an archive whose
//! size and time still match is not touched again, a new or changed one is scanned and the cache rewritten.
//!
//! The file lives in the user's cache folder (`ATLAS_CACHE` overrides it), one per installation. It holds file names
//! and the first 64 bytes of image files (their headers), nothing else; it can be deleted at any time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAGIC: &[u8; 8] = b"CIFSCAC1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fingerprint {
    pub size: u64,
    pub secs: i64,
    pub nanos: u32,
}

/// Size and modification time of a file, as the cache compares them.
pub fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let m = std::fs::metadata(path).ok()?;
    let d = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(Fingerprint { size: m.len(), secs: d.as_secs() as i64, nanos: d.subsec_nanos() })
}

#[derive(Clone, Debug, Default)]
pub struct ArchiveRec {
    pub fingerprint: Fingerprint,
    /// File names inside the archive, as listed (backslashes).
    pub entries: Vec<String>,
    /// Entry name -> first bytes (image headers).
    pub heads: Vec<(String, Vec<u8>)>,
}

#[derive(Clone, Debug, Default)]
pub struct Cache {
    /// Keyed by the archive's file name.
    pub archives: HashMap<String, ArchiveRec>,
}

/// Where the cache of the installation at `root` lives.
pub fn cache_file(root: &Path) -> Option<PathBuf> {
    use sha2::{Digest, Sha256};
    let dir = crate::config::env_var_os("CACHE").map(PathBuf::from).or_else(crate::config::cache_dir)?;
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let hash = Sha256::digest(canon.to_string_lossy().to_ascii_lowercase().as_bytes());
    let name: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Some(dir.join(format!("ifs-{name}.bin")))
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn string(&mut self) -> Option<String> {
        String::from_utf8(self.bytes()?.to_vec()).ok()
    }
}

fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(b);
}

impl Cache {
    /// Reads a cache file; anything unexpected (missing, other version, cut off) gives an empty cache.
    pub fn load(path: &Path) -> Cache {
        Cache::parse(&std::fs::read(path).unwrap_or_default()).unwrap_or_default()
    }

    fn parse(data: &[u8]) -> Option<Cache> {
        let mut r = Reader(data);
        if r.take(8)? != MAGIC {
            return None;
        }
        let count = r.u32()?;
        let mut archives = HashMap::new();
        for _ in 0..count {
            let name = r.string()?;
            let fingerprint = Fingerprint { size: r.u64()?, secs: r.u64()? as i64, nanos: r.u32()? };
            let n = r.u32()?;
            let mut entries = Vec::with_capacity(n as usize);
            for _ in 0..n {
                entries.push(r.string()?);
            }
            let n = r.u32()?;
            let mut heads = Vec::with_capacity(n as usize);
            for _ in 0..n {
                heads.push((r.string()?, r.bytes()?.to_vec()));
            }
            archives.insert(name, ArchiveRec { fingerprint, entries, heads });
        }
        r.0.is_empty().then_some(Cache { archives })
    }

    /// Writes the cache (to a temporary file first, so a crash never leaves a half-written one behind).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        let mut names: Vec<&String> = self.archives.keys().collect();
        names.sort();
        out.extend_from_slice(&(names.len() as u32).to_le_bytes());
        for name in names {
            let a = &self.archives[name];
            put_bytes(&mut out, name.as_bytes());
            out.extend_from_slice(&a.fingerprint.size.to_le_bytes());
            out.extend_from_slice(&(a.fingerprint.secs as u64).to_le_bytes());
            out.extend_from_slice(&a.fingerprint.nanos.to_le_bytes());
            out.extend_from_slice(&(a.entries.len() as u32).to_le_bytes());
            for e in &a.entries {
                put_bytes(&mut out, e.as_bytes());
            }
            out.extend_from_slice(&(a.heads.len() as u32).to_le_bytes());
            for (n, h) in &a.heads {
                put_bytes(&mut out, n.as_bytes());
                put_bytes(&mut out, h);
            }
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &out)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cache_survives_a_round_trip_and_rejects_damage() {
        let mut c = Cache::default();
        c.archives.insert("a.ifs".into(), ArchiveRec { fingerprint: Fingerprint { size: 7, secs: 1_700_000_000, nanos: 5 }, entries: vec!["main\\images\\x.iwi".into(), "main\\models\\y".into()], heads: vec![("main\\images\\x.iwi".into(), vec![1, 2, 3])] });
        c.archives.insert("b.ifs".into(), ArchiveRec::default());
        let dir = std::env::temp_dir().join("atlas_ifs_cache_test");
        let path = dir.join("c.bin");
        c.save(&path).unwrap();
        let back = Cache::load(&path);
        assert_eq!(back.archives.len(), 2);
        let a = &back.archives["a.ifs"];
        assert_eq!((a.fingerprint, a.entries.len(), a.heads[0].1.clone()), (c.archives["a.ifs"].fingerprint, 2, vec![1, 2, 3]));
        // a cut-off file and a file of another version give an empty cache, not a partial one
        let bytes = std::fs::read(&path).unwrap();
        assert!(Cache::parse(&bytes[..bytes.len() - 1]).is_none());
        let mut other = bytes.clone();
        other[7] = b'2';
        assert!(Cache::parse(&other).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
