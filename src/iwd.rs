//! Building and editing IWD archives (zip files with the game's content).
//!
//! An archive is edited as a list of [`Entry`]s: where each file goes inside the archive and where its bytes
//! come from. [`write`] produces a new archive from such a list. Entries taken from an existing IWD are
//! copied without being decompressed, so editing an archive does not touch the files you leave alone.
//! Output is deterministic (fixed timestamp), so identical input gives identical archives.
//!
//! Notes on the format, checked against the game's own installed archives: IWDs are ordinary zip files,
//! deflate-compressed, with paths in forward-slash form such as `images/name.iwi`. The retail Call of Duty
//! Online client ships IWDs with scrambled file names; those open (and can be copied) but their names are
//! meaningless. Mods and the QA build use plain names.

use crate::convert;
use crate::iwi;
use crate::source::Provider;
use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Where an entry's bytes come from.
#[derive(Clone)]
pub enum Source {
    /// Held in memory (for example a converted DDS).
    Bytes(Arc<Vec<u8>>),
    /// A file on disk, read when the archive is written.
    File(PathBuf),
    /// An entry of an existing archive; `name` is the name it has there.
    Archive { path: PathBuf, index: usize, name: String },
    /// A file of an image provider (loose folder or archive in the browser's catalogue).
    Provider { provider: Arc<dyn Provider>, key: String },
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Bytes(_) => "converted".into(),
            Source::File(p) => p.display().to_string(),
            Source::Archive { path, .. } => format!("{} (copied as is)", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            Source::Provider { provider, .. } => provider.label().to_string(),
        }
    }
}

#[derive(Clone)]
pub struct Entry {
    /// Path inside the archive.
    pub path: String,
    pub source: Source,
    /// Uncompressed size.
    pub size: u64,
}

/// Normalises a path for use inside an archive: forward slashes, no leading slash, lowercase (the game's file
/// system compares names case-insensitively, and lowercase is what its own archives use).
pub fn archive_path(s: &str) -> String {
    let mut p = s.trim().replace('\\', "/");
    while let Some(rest) = p.strip_prefix("./").or_else(|| p.strip_prefix('/')) {
        p = rest.to_string();
    }
    p.to_ascii_lowercase()
}

/// Why `path` cannot be an archive entry, if it cannot.
pub fn check_path(path: &str) -> Option<&'static str> {
    if path.is_empty() || path.ends_with('/') {
        Some("empty name")
    } else if path.split('/').any(|c| c == ".." || c.is_empty()) {
        Some("path must not contain empty parts or '..'")
    } else if path.contains(':') {
        Some("path must not contain ':'")
    } else {
        None
    }
}

/// How loose files are turned into entries.
#[derive(Clone, Default)]
pub struct AddOptions {
    /// When set, `.dds` files become `images/<name>.iwi` converted with these options; when `None` they are
    /// added unchanged.
    pub dds: Option<convert::Options>,
}

fn from_loose_file(path: &Path, rel: Option<&str>, opts: &AddOptions) -> Result<Entry> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).context("file has no name")?;
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    if let (Some(copts), "dds") = (&opts.dds, ext) {
        let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
        let (iwi_bytes, _) = convert::dds_to_iwi(&bytes, copts).with_context(|| format!("converting {}", path.display()))?;
        let stem = lower.strip_suffix(".dds").unwrap_or(&lower);
        return Ok(Entry { path: format!("images/{stem}.iwi"), size: iwi_bytes.len() as u64, source: Source::Bytes(Arc::new(iwi_bytes)) });
    }
    let archive_name = match rel {
        Some(r) => archive_path(r),
        // A loose .iwi has an obvious home; anything else lands at the root for the user to place.
        None if ext == "iwi" => format!("images/{lower}"),
        None => lower,
    };
    let size = std::fs::metadata(path).with_context(|| path.display().to_string())?.len();
    Ok(Entry { path: archive_name, source: Source::File(path.to_path_buf()), size })
}

/// Entries for loose files chosen by the user.
pub fn add_files(paths: &[PathBuf], opts: &AddOptions) -> Result<Vec<Entry>> {
    paths.iter().map(|p| from_loose_file(p, None, opts)).collect()
}

/// Entries for every file below `dir`; paths inside the archive are relative to `dir`
/// (so a folder holding `images/x.iwi` gives `images/x.iwi`). A folder that is itself called `images`
/// (the loose image folder of a game install) is prefixed, so its files land under `images/` as the game expects.
pub fn add_dir(dir: &Path, opts: &AddOptions) -> Result<Vec<Entry>> {
    let prefix = if dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("images")) { "images/" } else { "" };
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut items: Vec<_> = std::fs::read_dir(&d).with_context(|| d.display().to_string())?.flatten().map(|e| e.path()).collect();
        items.sort();
        for p in items {
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(dir).unwrap_or(&p).to_string_lossy().into_owned();
            let rel = format!("{prefix}{}", archive_path(&rel));
            let mut e = from_loose_file(&p, Some(&rel), opts)?;
            // a converted DDS keeps its folder: images/foo.dds under the root stays images/foo.iwi
            if let (Some(_), Some((parent, _))) = (&opts.dds, rel.rsplit_once('/')) {
                if rel.ends_with(".dds") {
                    let stem = e.path.rsplit('/').next().unwrap_or(&e.path).to_string();
                    e.path = format!("{parent}/{stem}");
                }
            }
            out.push(e);
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// The entries of an existing archive (directories skipped).
pub fn load(path: &Path) -> Result<Vec<Entry>> {
    let mut zip = ZipArchive::new(File::open(path).with_context(|| path.display().to_string())?).with_context(|| format!("opening {}", path.display()))?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i)?;
        if f.is_dir() {
            continue;
        }
        let name = f.name()?.to_string();
        out.push(Entry { path: name.clone(), size: f.size(), source: Source::Archive { path: path.to_path_buf(), index: i, name } });
    }
    Ok(out)
}

/// Entry for an image the browser already lists (stored as `images/<name>.iwi`).
pub fn from_provider(provider: Arc<dyn Provider>, key: String, name: &str, size: u64) -> Entry {
    Entry { path: format!("images/{}.iwi", name.to_ascii_lowercase()), source: Source::Provider { provider, key }, size }
}

/// Entries sharing a (case-insensitive) path; the later one wins when written.
pub fn duplicates(entries: &[Entry]) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut dup = HashSet::new();
    for e in entries {
        let k = e.path.to_ascii_lowercase();
        if !seen.insert(k.clone()) {
            dup.insert(k);
        }
    }
    dup
}

#[derive(Clone, Copy)]
pub struct WriteOptions {
    /// Deflate level 1-9; `None` stores files uncompressed.
    pub level: Option<i64>,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self { level: Some(6) }
    }
}

#[derive(Debug, Default)]
pub struct Summary {
    pub files: usize,
    pub replaced_duplicates: usize,
    pub bytes: u64,
}

fn read_source(src: &Source) -> Result<Vec<u8>> {
    match src {
        Source::Bytes(b) => Ok(b.as_ref().clone()),
        Source::File(p) => std::fs::read(p).with_context(|| p.display().to_string()),
        Source::Provider { provider, key } => provider.read(key),
        Source::Archive { .. } => unreachable!("archive entries are raw-copied"),
    }
}

/// Writes `entries` to `out`. When two entries share a path the later one is kept. `out` may be one of the
/// archives the entries come from: the new file is written next to it and moved into place at the end.
pub fn write(entries: &[Entry], out: &Path, opts: WriteOptions) -> Result<Summary> {
    for e in entries {
        if let Some(why) = check_path(&e.path) {
            bail!("bad path {:?}: {why}", e.path);
        }
    }
    // later entries win
    let mut last: HashMap<String, usize> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        last.insert(e.path.to_ascii_lowercase(), i);
    }
    let keep: Vec<&Entry> = entries.iter().enumerate().filter(|(i, e)| last[&e.path.to_ascii_lowercase()] == *i).map(|(_, e)| e).collect();
    let mut summary = Summary { files: keep.len(), replaced_duplicates: entries.len() - keep.len(), bytes: 0 };

    let tmp = out.with_extension("iwd.tmp");
    let result = (|| -> Result<()> {
        let mut zw = ZipWriter::new(File::create(&tmp).with_context(|| tmp.display().to_string())?);
        let file_opts = match opts.level {
            Some(l) => SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).compression_level(Some(l.clamp(1, 9))),
            None => SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        };
        let mut open: HashMap<PathBuf, ZipArchive<File>> = HashMap::new();
        for e in keep {
            summary.bytes += e.size;
            match &e.source {
                Source::Archive { path, index, name } => {
                    if !open.contains_key(path) {
                        open.insert(path.clone(), ZipArchive::new(File::open(path)?).with_context(|| format!("opening {}", path.display()))?);
                    }
                    let f = open.get_mut(path).unwrap().by_index_raw(*index)?;
                    if &e.path == name {
                        zw.raw_copy_file(f)?;
                    } else {
                        zw.raw_copy_file_rename(f, &e.path)?;
                    }
                }
                other => {
                    let bytes = read_source(other).with_context(|| e.path.clone())?;
                    zw.start_file(&e.path, file_opts)?;
                    zw.write_all(&bytes)?;
                }
            }
        }
        zw.finish()?.sync_all()?;
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // all source archives are closed by now, so replacing one of them is fine
    std::fs::rename(&tmp, out).with_context(|| format!("moving the new archive to {}", out.display()))?;
    Ok(summary)
}

#[derive(Debug, Default)]
pub struct Verified {
    pub files: usize,
    pub images: usize,
    pub bad_images: Vec<String>,
}

/// Reads every entry back (which checks each one's CRC) and parses the header of every `.iwi`.
pub fn verify(path: &Path) -> Result<Verified> {
    let mut zip = ZipArchive::new(File::open(path)?).with_context(|| format!("opening {}", path.display()))?;
    let mut v = Verified::default();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        if f.is_dir() {
            continue;
        }
        let name = f.name()?.to_string();
        let mut buf = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut buf).with_context(|| format!("reading {name}"))?;
        v.files += 1;
        if name.to_ascii_lowercase().ends_with(".iwi") {
            v.images += 1;
            if let Err(e) = iwi::Header::parse(&buf) {
                v.bad_images.push(format!("{name}: {e:#}"));
            }
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes_entry(path: &str, data: &[u8]) -> Entry {
        Entry { path: path.into(), size: data.len() as u64, source: Source::Bytes(Arc::new(data.to_vec())) }
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("codol-iwd-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn paths_are_normalised() {
        assert_eq!(archive_path("\\Images\\Foo.IWI"), "images/foo.iwi");
        assert_eq!(archive_path("./a/b"), "a/b");
        assert!(check_path("a/../b").is_some());
        assert!(check_path("c:/x").is_some());
        assert!(check_path("images/x.iwi").is_none());
    }

    #[test]
    fn write_is_deterministic_and_later_duplicate_wins() {
        let out1 = temp("a.iwd");
        let out2 = temp("b.iwd");
        let list = vec![bytes_entry("images/a.iwi", b"one"), bytes_entry("x/b.txt", b"bbb"), bytes_entry("IMAGES/A.iwi", b"two")];
        let s = write(&list, &out1, WriteOptions::default()).unwrap();
        assert_eq!((s.files, s.replaced_duplicates), (2, 1));
        write(&list, &out2, WriteOptions::default()).unwrap();
        assert_eq!(std::fs::read(&out1).unwrap(), std::fs::read(&out2).unwrap());
        let loaded = load(&out1).unwrap();
        assert_eq!(loaded.len(), 2);
        let mut zip = ZipArchive::new(File::open(&out1).unwrap()).unwrap();
        let mut s = String::new();
        zip.by_name("IMAGES/A.iwi").unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "two");
    }

    #[test]
    fn editing_in_place_keeps_untouched_entries_byte_for_byte() {
        let path = temp("edit.iwd");
        let big = vec![7u8; 5000];
        write(&[bytes_entry("images/a.iwi", &big), bytes_entry("images/b.iwi", b"bee")], &path, WriteOptions::default()).unwrap();
        let before = {
            let mut z = ZipArchive::new(File::open(&path).unwrap()).unwrap();
            let f = z.by_name("images/a.iwi").unwrap();
            (f.compressed_size(), f.crc32())
        };
        let mut entries = load(&path).unwrap();
        entries.retain(|e| e.path != "images/b.iwi");
        entries.push(bytes_entry("images/c.iwi", b"sea"));
        entries[0].path = "images/renamed.iwi".into();
        // write over the archive the entries come from
        write(&entries, &path, WriteOptions::default()).unwrap();
        let mut z = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        assert_eq!(z.len(), 2);
        let after = {
            let f = z.by_name("images/renamed.iwi").unwrap();
            (f.compressed_size(), f.crc32())
        };
        assert_eq!(after, before);
        assert!(z.by_name("images/c.iwi").is_ok());
        let v = verify(&path).unwrap();
        assert_eq!((v.files, v.images), (2, 2));
        assert_eq!(v.bad_images.len(), 2); // test payloads are not real IWIs
    }

    #[test]
    fn bad_paths_are_refused_and_nothing_is_left_behind() {
        let out = temp("bad.iwd");
        assert!(write(&[bytes_entry("../evil", b"x")], &out, WriteOptions::default()).is_err());
        assert!(!out.exists());
    }
}
