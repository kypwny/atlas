//! Where images come from.
//!
//! A [`Provider`] lists and reads IWI files from one place (a loose folder, an
//! IWD archive, or - in a separate private add-on - something encrypted). The
//! catalogue merges every provider; the rest of the program never cares where
//! the bytes live.

use crate::iwi::{Header, MAX_HEADER_LEN};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct Listing {
    /// File name without directory or extension, lowercase.
    pub name: String,
    /// Provider-specific handle passed back to `read`.
    pub key: String,
    pub size: u64,
}

pub trait Provider: Send + Sync {
    fn label(&self) -> &str;
    fn list(&self) -> Result<Vec<Listing>>;
    fn read(&self, key: &str) -> Result<Vec<u8>>;
    /// First `n` bytes of an entry (headers). Override when cheaper than `read`.
    fn read_head(&self, key: &str, n: usize) -> Result<Vec<u8>> {
        let mut v = self.read(key)?;
        v.truncate(n);
        Ok(v)
    }
}

fn stem(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next()?;
    let lower = base.to_ascii_lowercase();
    lower.strip_suffix(".iwi").map(str::to_owned)
}

/// A folder of loose `.iwi` files.
pub struct LooseDir {
    label: String,
    dir: PathBuf,
}

impl LooseDir {
    pub fn new(label: impl Into<String>, dir: PathBuf) -> Self {
        Self { label: label.into(), dir }
    }
}

impl Provider for LooseDir {
    fn label(&self) -> &str {
        &self.label
    }

    fn list(&self) -> Result<Vec<Listing>> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.dir).with_context(|| format!("reading {}", self.dir.display()))? {
            let e = e?;
            let file_name = e.file_name().to_string_lossy().into_owned();
            if let Some(name) = stem(&file_name) {
                out.push(Listing { name, key: file_name, size: e.metadata().map(|m| m.len()).unwrap_or(0) });
            }
        }
        Ok(out)
    }

    fn read(&self, key: &str) -> Result<Vec<u8>> {
        std::fs::read(self.dir.join(key)).with_context(|| format!("reading {key}"))
    }

    fn read_head(&self, key: &str, n: usize) -> Result<Vec<u8>> {
        let mut f = File::open(self.dir.join(key)).with_context(|| format!("opening {key}"))?;
        let mut buf = vec![0u8; n];
        let mut got = 0;
        while got < n {
            let r = f.read(&mut buf[got..])?;
            if r == 0 {
                break;
            }
            got += r;
        }
        buf.truncate(got);
        Ok(buf)
    }
}

/// An IWD (zip) archive; images live under `images/`.
pub struct IwdArchive {
    label: String,
    zip: Mutex<zip::ZipArchive<File>>,
}

impl IwdArchive {
    pub fn open(path: &Path) -> Result<Self> {
        let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let zip = zip::ZipArchive::new(File::open(path)?).with_context(|| format!("opening {}", path.display()))?;
        Ok(Self { label, zip: Mutex::new(zip) })
    }
}

impl Provider for IwdArchive {
    fn label(&self) -> &str {
        &self.label
    }

    fn list(&self) -> Result<Vec<Listing>> {
        let mut zip = self.zip.lock().unwrap();
        let mut out = Vec::new();
        for i in 0..zip.len() {
            let f = zip.by_index_raw(i)?;
            let n = f.name()?.replace('\\', "/");
            if n.to_ascii_lowercase().starts_with("images/") {
                if let Some(name) = stem(&n) {
                    out.push(Listing { name, key: i.to_string(), size: f.size() });
                }
            }
        }
        Ok(out)
    }

    fn read(&self, key: &str) -> Result<Vec<u8>> {
        let mut zip = self.zip.lock().unwrap();
        let mut f = zip.by_index(key.parse()?)?;
        let mut v = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut v)?;
        Ok(v)
    }

    fn read_head(&self, key: &str, n: usize) -> Result<Vec<u8>> {
        let mut zip = self.zip.lock().unwrap();
        let f = zip.by_index(key.parse()?)?;
        let mut v = Vec::new();
        f.take(n as u64).read_to_end(&mut v)?;
        Ok(v)
    }
}

/// A Black Ops II IPAK. Images have no names there, only a hash of the name, so they are listed as `0x<hash>`;
/// several versions of one image share a hash.
pub struct IpakArchive {
    label: String,
    ipak: crate::ipak::Ipak,
}

impl IpakArchive {
    pub fn open(path: &Path) -> Result<Self> {
        let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Self { label, ipak: crate::ipak::Ipak::open(path)? })
    }
}

impl Provider for IpakArchive {
    fn label(&self) -> &str {
        &self.label
    }

    fn list(&self) -> Result<Vec<Listing>> {
        Ok(self.ipak.entries.iter().enumerate().map(|(i, e)| Listing { name: format!("0x{:08x}", e.name_hash), key: i.to_string(), size: e.size as u64 }).collect())
    }

    fn read(&self, key: &str) -> Result<Vec<u8>> {
        let e = self.ipak.entries.get(key.parse::<usize>()?).context("no such IPAK entry")?;
        self.ipak.read(e)
    }

    fn read_head(&self, key: &str, n: usize) -> Result<Vec<u8>> {
        let e = self.ipak.entries.get(key.parse::<usize>()?).context("no such IPAK entry")?;
        self.ipak.read_prefix(e, n)
    }
}

/// Finds image sources under a game folder (or a folder inside one).
///
/// Looks at the folder itself and at `main`, `hires` and `raw` below it for an
/// `images` folder of loose files and for `*.iwd` archives.
pub fn discover(root: &Path) -> Vec<Arc<dyn Provider>> {
    let mut found: Vec<Arc<dyn Provider>> = Vec::new();
    let mut seen = HashSet::new();
    let mut dirs = vec![root.to_path_buf()];
    for sub in ["main", "hires", "raw", "zone", "zone/all"] {
        dirs.push(root.join(sub));
    }
    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        let tag = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let images = dir.join("images");
        let loose = if images.is_dir() {
            Some(images)
        } else if dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("images")) {
            Some(dir.clone())
        } else {
            None
        };
        if let Some(images) = loose {
            let canon = images.canonicalize().unwrap_or_else(|_| images.clone());
            if seen.insert(canon) {
                found.push(Arc::new(LooseDir::new(format!("{tag}/images"), images)));
            }
        }
        let mut iwds: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("iwd")))
            .collect();
        iwds.sort();
        for p in iwds {
            let canon = p.canonicalize().unwrap_or_else(|_| p.clone());
            if !seen.insert(canon) {
                continue;
            }
            match IwdArchive::open(&p) {
                Ok(a) => found.push(Arc::new(a)),
                Err(e) => eprintln!("skipping {}: {e:#}", p.display()),
            }
        }
        let mut ipaks: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ipak")))
            .collect();
        ipaks.sort();
        if dir == root {
            found.extend(ifs_providers(root));
        }
        for p in ipaks {
            let canon = p.canonicalize().unwrap_or_else(|_| p.clone());
            if !seen.insert(canon) {
                continue;
            }
            match IpakArchive::open(&p) {
                Ok(a) => found.push(Arc::new(a)),
                Err(e) => eprintln!("skipping {}: {e:#}", p.display()),
            }
        }
    }
    found
}

/// Image providers for an installation's `.ifs` archives (empty when it has none or the helper is not available).
fn ifs_providers(root: &Path) -> Vec<Arc<dyn Provider>> {
    crate::ifs::IfsStore::shared(root).map(|s| s.image_providers()).unwrap_or_default()
}

pub struct Variant {
    pub provider: usize,
    pub key: String,
    pub size: u64,
    pub header: Option<Header>,
    /// IWI version byte, known even when the header could not be parsed (an unsupported game, say).
    pub version: Option<u8>,
    pub error: Option<String>,
}

pub struct ImageEntry {
    pub name: String,
    pub variants: Vec<Variant>,
}

impl ImageEntry {
    pub fn header(&self) -> Option<&Header> {
        self.variants.iter().find_map(|v| v.header.as_ref())
    }
}

pub struct Catalog {
    pub providers: Vec<Arc<dyn Provider>>,
    pub entries: Vec<ImageEntry>,
}

impl Catalog {
    /// How many files carry each IWI version, most common first.
    pub fn versions(&self) -> Vec<(u8, usize)> {
        let mut m: BTreeMap<u8, usize> = BTreeMap::new();
        for v in self.entries.iter().flat_map(|e| &e.variants).filter_map(|v| v.version) {
            *m.entry(v).or_default() += 1;
        }
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    }
}

/// Progress shared with the UI while scanning.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
}

/// Lists every provider and reads all headers (in parallel for loose files).
pub fn scan(providers: Vec<Arc<dyn Provider>>, progress: &Progress) -> Catalog {
    let mut by_name: BTreeMap<String, Vec<Variant>> = BTreeMap::new();
    for (pi, p) in providers.iter().enumerate() {
        let listing = match p.list() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("{}: {e:#}", p.label());
                continue;
            }
        };
        progress.total.fetch_add(listing.len(), Ordering::Relaxed);
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(16);
        let chunk = listing.len().div_ceil(threads).max(1);
        let results: Vec<Vec<(String, Variant)>> = std::thread::scope(|s| {
            let handles: Vec<_> = listing
                .chunks(chunk)
                .map(|part| {
                    let p = Arc::clone(p);
                    s.spawn(move || {
                        part.iter()
                            .map(|l| {
                                let head = p.read_head(&l.key, MAX_HEADER_LEN);
                                let version = head.as_ref().ok().filter(|b| b.len() > 3 && &b[..3] == b"IWi").map(|b| b[3]);
                                let (header, error) = match head.and_then(|b| Header::parse(&b)) {
                                    Ok(h) => (Some(h), None),
                                    Err(e) => (None, Some(format!("{e:#}"))),
                                };
                                progress.done.fetch_add(1, Ordering::Relaxed);
                                (l.name.clone(), Variant { provider: pi, key: l.key.clone(), size: l.size, header, version, error })
                            })
                            .collect()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (name, v) in results.into_iter().flatten() {
            by_name.entry(name).or_default().push(v);
        }
    }
    let entries = by_name.into_iter().map(|(name, variants)| ImageEntry { name, variants }).collect();
    Catalog { providers, entries }
}
