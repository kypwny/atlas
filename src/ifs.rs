//! Call of Duty Online `.ifs` archives (3.19 and later), read through the game's own `IFS2.dll`.
//!
//! Since version 1.2.1.12 the game keeps its fastfiles and loose files (images, models, sounds) in `.ifs` archives under
//! `IIPS/IIPSDownload`. This program does not read that format itself and ships none of the game's code: it starts the
//! small 32-bit helper in `tools/ifs_helper`, which loads the installation's own `IIPS/IFS2.dll` and asks it for file
//! lists and file contents, the same calls Maurice Heumann's MIT-licensed `ifs-tool` makes. The helper is built with
//! `cargo build --release --target i686-pc-windows-msvc` inside `tools/ifs_helper` and found next to the program, in that
//! build folder, or through the `CODOL_IFS_HELPER` environment variable. Without the helper (or on a build without an
//! `IIPS` folder) nothing changes: the archives are simply not listed.
//!
//! What the DLL hands back is the archive's content as stored. Zones (`ff_*.ifs`) come back plain. The loose files
//! (`main\...` images, models, sounds) carry a second layer that the game removes itself, and so does this program: each
//! file is `body + u32 trailer`; the body is cut into 0x8000-byte chunks, each decrypted with AES-192 in counter mode
//! (big-endian counter, a fresh IV per chunk made of the file name's CRC, the trailer, the chunk offset and its length,
//! all little-endian u32), and the result is zlib-compressed. The AES key is the game's own, recovered at run time from the
//! executable in the installation (an RSA blob decrypted with the client key the fastfile reader already uses); nothing
//! key-related is stored in this repository. The layer was worked out from the client's own code and checked on every
//! image and model of the archives.

/// The AES-192 key of the loose-file layer.
pub struct LooseKey {
    cipher: Aes192,
}

fn crc_table() -> &'static [u32; 256] {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

/// CRC-32 table update over the base name seeded with the complement of its length (not zlib's CRC).
fn name_crc(base: &str) -> u32 {
    let t = crc_table();
    let mut v = !(base.len() as u32);
    for b in base.bytes() {
        v = t[((v ^ b as u32) & 0xFF) as usize] ^ (v >> 8);
    }
    !v
}

/// AES in counter mode with the counter incremented as a big-endian 128-bit integer.
fn ctr_be(cipher: &Aes192, iv: [u8; 16], data: &[u8]) -> Vec<u8> {
    let mut counter = u128::from_be_bytes(iv);
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(16) {
        let mut block = Block::<Aes192>::from(counter.to_be_bytes());
        cipher.encrypt_block(&mut block);
        out.extend(chunk.iter().zip(block.iter()).map(|(a, b)| a ^ b));
        counter = counter.wrapping_add(1);
    }
    out
}

impl LooseKey {
    /// A key given directly (24 bytes); used by tests.
    pub fn from_aes_key(key: &[u8]) -> Result<LooseKey> {
        Ok(LooseKey { cipher: Aes192::new_from_slice(key).map_err(|_| anyhow::anyhow!("an AES-192 key is 24 bytes"))? })
    }

    /// Recovers the key from an executable of the installation: the client RSA key is found by hash, the 256-byte blob
    /// sits a fixed distance before it in the builds seen (or is searched for near it), and decrypts to the AES key.
    pub fn from_exe(path: &Path) -> Result<LooseKey> {
        let rsa = ArchiveKey::load(path)?;
        let data = std::fs::read(path)?;
        let at = fastfile::der_offset(&data).context("no client key in the executable")?;
        let try_at = |o: usize| -> Option<Aes192> {
            let blob = data.get(o..o + 256)?;
            let key = rsa.key.decrypt(Oaep::<Sha256>::new(), blob).ok()?;
            (key.len() == 24).then(|| Aes192::new_from_slice(&key).ok()).flatten()
        };
        let guess = at.checked_sub(0x2DA8).and_then(try_at);
        let cipher = guess.or_else(|| (at.saturating_sub(0x8000)..at).step_by(4).find_map(try_at)).context("the loose-file key blob was not found near the client key")?;
        Ok(LooseKey { cipher })
    }

    /// Finds an executable in the installation folder that carries the key.
    pub fn find(root: &Path) -> Option<LooseKey> {
        let mut exes: Vec<PathBuf> = std::fs::read_dir(root).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe"))).collect();
        exes.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX));
        exes.iter().find_map(|p| LooseKey::from_exe(p).ok())
    }

    fn chunks(&self, base: &str, stored: &[u8]) -> Result<Vec<Vec<u8>>> {
        if stored.len() < 8 {
            bail!("{base}: too short for a protected file");
        }
        let (body, trailer) = stored.split_at(stored.len() - 4);
        let trailer = u32::from_le_bytes(trailer.try_into().unwrap());
        let crc = name_crc(base);
        Ok(body
            .chunks(0x8000)
            .enumerate()
            .map(|(i, c)| {
                let mut iv = [0u8; 16];
                for (k, v) in [crc, trailer, (i * 0x8000) as u32, c.len() as u32].into_iter().enumerate() {
                    iv[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
                }
                ctr_be(&self.cipher, iv, c)
            })
            .collect())
    }

    /// Removes the protection layer from a loose file as the archive hands it out. `base` is the file's name without folders.
    pub fn decode(&self, base: &str, stored: &[u8]) -> Result<Vec<u8>> {
        let plain: Vec<u8> = self.chunks(base, stored)?.concat();
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(&plain[..]).read_to_end(&mut out).with_context(|| format!("{base}: inflating"))?;
        Ok(out)
    }

    /// The first `n` bytes of a loose file (enough for an image header), without decoding the whole file.
    pub fn decode_head(&self, base: &str, stored: &[u8], n: usize) -> Result<Vec<u8>> {
        let first = self.chunks_first(base, stored)?;
        let mut out = vec![0u8; n];
        let mut got = 0;
        let mut dec = flate2::read::ZlibDecoder::new(&first[..]);
        while got < n {
            match dec.read(&mut out[got..]) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(_) if got > 0 => break,
                Err(e) => return Err(e).with_context(|| format!("{base}: inflating")),
            }
        }
        out.truncate(got);
        Ok(out)
    }

    fn chunks_first(&self, base: &str, stored: &[u8]) -> Result<Vec<u8>> {
        if stored.len() < 8 {
            bail!("{base}: too short for a protected file");
        }
        let (body, trailer) = stored.split_at(stored.len() - 4);
        let trailer = u32::from_le_bytes(trailer.try_into().unwrap());
        let c = &body[..body.len().min(0x8000)];
        let mut iv = [0u8; 16];
        for (k, v) in [name_crc(base), trailer, 0, c.len() as u32].into_iter().enumerate() {
            iv[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
        }
        Ok(ctr_be(&self.cipher, iv, c))
    }
}

use crate::modelsource::{ModelListing, ModelProvider};
use crate::zone::fastfile::{self, ArchiveKey};
use aes::Aes192;
use aes::cipher::{Block, BlockCipherEncrypt, KeyInit};
use rsa::Oaep;
use sha2::Sha256;
use crate::source::{Listing, Provider};
use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use crate::ifs_cache::{self, ArchiveRec, Cache, Fingerprint};

/// Archives opened by one helper process before it is replaced: the DLL keeps memory for every archive it opens, and the
/// 32-bit process runs out of address space (and the DLL throws) after a few hundred.
const OPENS_PER_PROCESS: usize = 40;

/// How long the helper may stay silent in the middle of a reply. `IFS2.dll` can block for good, for example while the running
/// game's updater (IIPS) rewrites the archives; without a limit the image scan would wait forever.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeouts after which the helper is not asked again in this session: what is not in the cache then stays unread.
const MAX_STRIKES: usize = 2;

struct Helper {
    exe: PathBuf,
    dll: PathBuf,
    child: Child,
    stdin: ChildStdin,
    /// The helper's output, read on a thread of its own so that a reply can time out; `pending` holds what is not consumed yet.
    output: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    open: Option<usize>,
    opens: usize,
    /// Replies that timed out (kept across respawns), and the archives that were open when they did.
    strikes: usize,
    stuck: std::collections::HashSet<usize>,
}

impl Helper {
    fn spawn(helper: &Path, dll: &Path) -> Result<Helper> {
        let mut cmd = Command::new(helper);
        cmd.arg(dll).stdin(Stdio::piped()).stdout(Stdio::piped());
        match std::env::var_os("CODOL_IFS_DEBUG").and_then(|p| std::fs::File::create(p).ok()) {
            Some(f) => cmd.stderr(Stdio::from(f)),
            None => cmd.stderr(Stdio::null()),
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // no console window
        }
        let mut child = cmd.spawn().with_context(|| format!("starting {}", helper.display()))?;
        let stdin = child.stdin.take().context("helper has no stdin")?;
        let mut stdout = child.stdout.take().context("helper has no stdout")?;
        let (tx, output) = channel();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 16];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut h = Helper { exe: helper.to_path_buf(), dll: dll.to_path_buf(), child, stdin, output, pending: Vec::new(), open: None, opens: 0, strikes: 0, stuck: Default::default() };
        let hello = h.line()?;
        if !hello.starts_with("OK") {
            bail!("IFS helper: {}", hello.trim());
        }
        Ok(h)
    }

    /// Replaces the process with a fresh one (nothing is open afterwards); the strikes and stuck archives carry over.
    fn respawn(&mut self) -> Result<()> {
        let (strikes, stuck) = (self.strikes, std::mem::take(&mut self.stuck));
        *self = Helper::spawn(&self.exe.clone(), &self.dll.clone())?;
        (self.strikes, self.stuck) = (strikes, stuck);
        Ok(())
    }

    /// False once the helper timed out too often: it is not asked again.
    fn usable(&self) -> bool {
        self.strikes < MAX_STRIKES
    }

    /// Waits for more output, at most [`REPLY_TIMEOUT`]; on a timeout the process is killed and the open archive marked stuck.
    fn fill(&mut self) -> Result<()> {
        match self.output.recv_timeout(REPLY_TIMEOUT) {
            Ok(chunk) => {
                self.pending.extend_from_slice(&chunk);
                Ok(())
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                self.strikes += 1;
                if let Some(a) = self.open.take() {
                    self.stuck.insert(a);
                }
                let note = if self.usable() { "" } else { "; it is not asked again until atlas restarts (is the game or its updater using the archives?)" };
                crate::log::log(format!("IFS: the helper did not answer for {} s{note}", REPLY_TIMEOUT.as_secs()));
                bail!("the IFS helper did not answer")
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.open = None;
                bail!("the IFS helper stopped")
            }
        }
    }

    fn line(&mut self) -> Result<String> {
        loop {
            if let Some(i) = self.pending.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.pending.drain(..=i).collect();
                return Ok(String::from_utf8_lossy(&line).trim_end().to_string());
            }
            self.fill()?;
        }
    }

    fn bytes(&mut self, n: usize) -> Result<Vec<u8>> {
        while self.pending.len() < n {
            self.fill()?;
        }
        Ok(self.pending.drain(..n).collect())
    }

    fn ensure_open(&mut self, index: usize, path: &Path) -> Result<()> {
        if !self.usable() {
            bail!("the IFS helper stopped answering earlier");
        }
        if self.stuck.contains(&index) {
            bail!("{}: the IFS helper hung on this archive earlier", path.display());
        }
        if self.open == Some(index) && self.child.try_wait().ok().flatten().is_none() {
            return Ok(());
        }
        if self.opens >= OPENS_PER_PROCESS || self.child.try_wait().ok().flatten().is_some() {
            self.respawn()?;
        }
        writeln!(self.stdin, "OPEN {}", path.display())?;
        self.stdin.flush()?;
        // marked open while waiting, so that a timeout knows which archive hung
        self.open = Some(index);
        let r = match self.line() {
            Ok(r) => r,
            Err(e) => {
                self.open = None;
                return Err(e);
            }
        };
        if r != "OK" {
            self.open = None;
            bail!("{}: {r}", path.display());
        }
        self.opens += 1;
        Ok(())
    }

    fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        if writeln!(self.stdin, "READ {name}").and_then(|_| self.stdin.flush()).is_err() {
            self.open = None;
            bail!("the IFS helper stopped");
        }
        let r = self.line()?;
        let Some(n) = r.strip_prefix("OK ").and_then(|n| n.parse::<usize>().ok()) else { bail!("{name}: {r}") };
        self.bytes(n)
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "QUIT");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
    }
}

/// Every `.ifs` archive of an installation and what is in it.
pub struct IfsStore {
    helper: Mutex<Helper>,
    /// None when no executable of the installation carries the key (the loose files then stay unreadable).
    key: Option<LooseKey>,
    archives: Vec<PathBuf>,
    /// (archive index, entry path with backslashes as stored)
    entries: Vec<(usize, String)>,
    /// Where the scan is cached, each archive's fingerprint (None when it could not be listed, so it is never cached), and
    /// the image headers read so far ("archive|entry" -> first bytes).
    cache_path: Option<PathBuf>,
    fingerprints: Vec<Option<Fingerprint>>,
    head_cache: Mutex<std::collections::HashMap<String, Vec<u8>>>,
}

fn is_loose(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    l.starts_with("main\\") || l.starts_with("hires\\")
}

fn base_name(name: &str) -> &str {
    name.rsplit('\\').next().unwrap_or(name)
}

/// SHA-256 of the `IFS2.dll` builds the helper knows (the same list it checks itself).
const KNOWN_IFS2: [&str; 1] = ["5b0324e0b2ba3545a6c02626287d4fbc6a3116594744f4647333982d85508421"];

/// Whether `dll` is a build the helper accepts; None when it cannot be read.
pub fn dll_is_known(dll: &Path) -> Option<bool> {
    let bytes = std::fs::read(dll).ok()?;
    let hash: String = <Sha256 as sha2::Digest>::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    Some(KNOWN_IFS2.contains(&hash.as_str()))
}

/// Where the IFS helper executable is, when it has been built.
pub fn helper_path() -> Option<PathBuf> {
    find_helper()
}

fn find_helper() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CODOL_IFS_HELPER").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let tail = Path::new("tools").join("ifs_helper").join("target").join("i686-pc-windows-msvc").join("release").join("ifs-helper.exe");
    let mut candidates = vec![dir.join("ifs-helper.exe")];
    // a development build: target/release/<exe> -> the repository root is two levels up
    for up in [dir.parent().and_then(Path::parent), std::env::current_dir().ok().as_deref()] {
        if let Some(root) = up {
            candidates.push(root.join(&tail));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

impl IfsStore {
    /// Like [`open`](Self::open) but once per installation: the catalogues of images and models share one helper.
    pub fn shared(root: &Path) -> Option<Arc<IfsStore>> {
        static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<PathBuf, Option<Arc<IfsStore>>>>> = std::sync::OnceLock::new();
        let key = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
        cache.entry(key).or_insert_with(|| IfsStore::open(root)).clone()
    }

    /// Opens the archives of the installation at `root`, or None when it has none or the helper is not available.
    pub fn open(root: &Path) -> Option<Arc<IfsStore>> {
        let dll = root.join("IIPS").join("IFS2.dll");
        let dir = root.join("IIPS").join("IIPSDownload");
        if !dll.is_file() || !dir.is_dir() {
            return None;
        }
        let mut archives: Vec<PathBuf> = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ifs"))).collect();
        if archives.is_empty() {
            return None;
        }
        archives.sort();
        let Some(helper_exe) = find_helper() else {
            crate::log::log(format!("{} has .ifs archives; build tools/ifs_helper (cargo build --release --target i686-pc-windows-msvc) to read them", root.display()));
            return None;
        };
        let mut helper = match Helper::spawn(&helper_exe, &dll) {
            Ok(h) => h,
            Err(e) => {
                crate::log::log(format!("IFS archives not readable: {e:#}"));
                return None;
            }
        };
        let mut entries = Vec::new();
        let cache_path = ifs_cache::cache_file(root);
        let cache = cache_path.as_deref().map(Cache::load).unwrap_or_default();
        let mut fingerprints = Vec::with_capacity(archives.len());
        let mut head_cache = std::collections::HashMap::new();
        let (mut cached, mut scanned) = (0usize, 0usize);
        for (i, a) in archives.iter().enumerate() {
            let fp = ifs_cache::fingerprint(a);
            let name = a.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // an archive that has not changed since the last scan is not opened again
            if let (Some(fp), Some(rec)) = (fp, cache.archives.get(&name)) {
                if rec.fingerprint == fp {
                    entries.extend(rec.entries.iter().map(|e| (i, e.clone())));
                    head_cache.extend(rec.heads.iter().map(|(n, h)| (format!("{i}|{n}"), h.clone())));
                    fingerprints.push(Some(fp));
                    cached += 1;
                    continue;
                }
            }
            scanned += 1;
            fingerprints.push(None);
            if helper.ensure_open(i, a).is_err() {
                continue;
            }
            let Ok(list) = helper.read("(listfile)") else { continue };
            fingerprints[i] = fp;
            for line in String::from_utf8_lossy(&list).lines() {
                let line = line.trim();
                // only files: directories and the archive's own .lst have no use here
                if !line.is_empty() && !line.ends_with(".lst") {
                    entries.push((i, line.to_string()));
                }
            }
        }
        let key = LooseKey::find(root);
        if key.is_none() {
            crate::log::log(format!("{}: no executable with the client key was found, so the loose files in the .ifs archives cannot be read", root.display()));
        }
        crate::log::log(format!("IFS: {} archives, {} entries ({cached} archives from the cache, {scanned} listed through the game's own IFS2.dll)", archives.len(), entries.len()));
        let store = Arc::new(IfsStore { helper: Mutex::new(helper), key, archives, entries, cache_path, fingerprints, head_cache: Mutex::new(head_cache) });
        if scanned > 0 {
            store.save_cache();
        }
        Some(store)
    }

    /// Writes what the scan found (listings and image headers) to the cache file.
    pub fn save_cache(&self) {
        let Some(path) = &self.cache_path else { return };
        let mut cache = Cache::default();
        let name = |i: usize| self.archives[i].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        for (i, fp) in self.fingerprints.iter().enumerate() {
            if let Some(fp) = fp {
                cache.archives.insert(name(i), ArchiveRec { fingerprint: *fp, ..Default::default() });
            }
        }
        for (i, e) in &self.entries {
            if let Some(rec) = cache.archives.get_mut(&name(*i)) {
                rec.entries.push(e.clone());
            }
        }
        for (key, head) in self.head_cache.lock().unwrap().iter() {
            let Ok((i, entry)) = split_key(key) else { continue };
            if let Some(rec) = cache.archives.get_mut(&name(i)) {
                rec.heads.push((entry.to_string(), head.clone()));
            }
        }
        match cache.save(path) {
            Ok(()) => crate::log::log(format!("IFS: scan cached in {}", path.display())),
            Err(e) => crate::log::log(format!("IFS: the scan could not be cached ({e})")),
        }
    }

    pub fn archive_count(&self) -> usize {
        self.archives.len()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// The stored bytes of an entry (loose files still protected).
    fn read_stored(&self, archive: usize, name: &str) -> Result<Vec<u8>> {
        let mut h = self.helper.lock().unwrap();
        h.ensure_open(archive, &self.archives[archive])?;
        match h.read(name) {
            Ok(v) => Ok(v),
            // the process may have died on the way: start a fresh one and try once more (after a timeout ensure_open refuses
            // the archive, or the helper altogether)
            Err(_) => {
                if h.usable() && !h.stuck.contains(&archive) {
                    h.respawn()?;
                }
                h.ensure_open(archive, &self.archives[archive])?;
                h.read(name)
            }
        }
    }

    /// An entry's content; loose files (`main\\...`, `hires\\...`) have the protection layer removed.
    pub fn read(&self, archive: usize, name: &str) -> Result<Vec<u8>> {
        let stored = self.read_stored(archive, name)?;
        match (&self.key, is_loose(name)) {
            (Some(k), true) => k.decode(base_name(name), &stored),
            _ => Ok(stored),
        }
    }

    /// The first bytes of an entry (headers), cheaper than a full read for loose files.
    pub fn read_head(&self, archive: usize, name: &str, n: usize) -> Result<Vec<u8>> {
        let stored = self.read_stored(archive, name)?;
        match (&self.key, is_loose(name)) {
            (Some(k), true) => k.decode_head(base_name(name), &stored, n),
            _ => Ok(stored[..n.min(stored.len())].to_vec()),
        }
    }

    fn under(&self, prefix: &str) -> Vec<(usize, String)> {
        self.entries.iter().filter(|(_, n)| n.to_ascii_lowercase().starts_with(prefix)).cloned().collect()
    }

    /// Image providers (`main\images`, `hires\images`) for the browser's catalogue.
    pub fn image_providers(self: &Arc<Self>) -> Vec<Arc<dyn Provider>> {
        if self.key.is_none() {
            return Vec::new();
        }
        ["main\\images\\", "hires\\images\\"]
            .iter()
            .filter_map(|p| {
                let entries: Vec<_> = self.under(p).into_iter().filter(|(_, n)| n.to_ascii_lowercase().ends_with(".iwi")).collect();
                (!entries.is_empty()).then(|| Arc::new(IfsImages { store: Arc::clone(self), label: format!("ifs {}", p.trim_end_matches('\\').replace('\\', "/")), entries, heads: Mutex::new(std::collections::HashMap::new()) }) as Arc<dyn Provider>)
            })
            .collect()
    }

    /// Model provider (`main\models`).
    pub fn model_providers(self: &Arc<Self>) -> Vec<Arc<dyn ModelProvider>> {
        if self.key.is_none() {
            return Vec::new();
        }
        let entries = self.under("main\\models\\");
        if entries.is_empty() {
            return Vec::new();
        }
        vec![Arc::new(IfsModels { store: Arc::clone(self), entries }) as Arc<dyn ModelProvider>]
    }
}

struct IfsImages {
    store: Arc<IfsStore>,
    label: String,
    entries: Vec<(usize, String)>,
    /// First bytes of every image, read in archive order when the catalogue lists the provider. Header reads come
    /// from many threads in no useful order, and every switch of archive costs the helper dearly.
    heads: Mutex<std::collections::HashMap<String, Vec<u8>>>,
}

const HEAD_BYTES: usize = 64;

fn split_key(key: &str) -> Result<(usize, &str)> {
    let (a, n) = key.split_once('|').context("bad IFS key")?;
    Ok((a.parse()?, n))
}

impl Provider for IfsImages {
    fn label(&self) -> &str {
        &self.label
    }

    fn list(&self) -> Result<Vec<Listing>> {
        {
            let mut heads = self.heads.lock().unwrap();
            if heads.is_empty() {
                // entries are in archive order, so each archive is opened once; headers read on an earlier run come from the cache
                let mut fresh = 0usize;
                for (a, n) in &self.entries {
                    let key = format!("{a}|{n}");
                    let known = self.store.head_cache.lock().unwrap().get(&key).cloned();
                    if let Some(h) = known {
                        heads.insert(key, h);
                    } else if let Ok(h) = self.store.read_head(*a, n, HEAD_BYTES) {
                        self.store.head_cache.lock().unwrap().insert(key.clone(), h.clone());
                        heads.insert(key, h);
                        fresh += 1;
                    }
                }
                if fresh > 0 {
                    self.store.save_cache();
                }
            }
        }
        Ok(self
            .entries
            .iter()
            .map(|(a, n)| {
                let base = n.rsplit('\\').next().unwrap_or(n).to_ascii_lowercase();
                Listing { name: base.strip_suffix(".iwi").unwrap_or(&base).to_string(), key: format!("{a}|{n}"), size: 0 }
            })
            .collect())
    }

    fn read(&self, key: &str) -> Result<Vec<u8>> {
        let (a, n) = split_key(key)?;
        self.store.read(a, n)
    }

    fn read_head(&self, key: &str, n: usize) -> Result<Vec<u8>> {
        if n <= HEAD_BYTES {
            if let Some(h) = self.heads.lock().unwrap().get(key) {
                return Ok(h[..n.min(h.len())].to_vec());
            }
        }
        let (a, name) = split_key(key)?;
        self.store.read_head(a, name, n)
    }
}

struct IfsModels {
    store: Arc<IfsStore>,
    entries: Vec<(usize, String)>,
}

impl ModelProvider for IfsModels {
    fn label(&self) -> &str {
        "ifs main/models"
    }

    fn list(&self) -> Result<Vec<ModelListing>> {
        Ok(self.entries.iter().map(|(a, n)| ModelListing { name: n.rsplit('\\').next().unwrap_or(n).to_string(), key: format!("{a}|{n}"), size: 0 }).collect())
    }

    fn read(&self, key: &str) -> Result<Vec<u8>> {
        let (a, n) = split_key(key)?;
        self.store.read(a, n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn name_crc_is_the_length_seeded_variant_not_zlibs() {
        // values computed with an independent implementation of the same definition
        assert_eq!(name_crc("chopper_icon_bar.iwi"), 0xf6b4f0fb);
        assert_eq!(name_crc("a"), 0x9fb08ed5);
        assert_ne!(name_crc("a"), crc32_zlib(b"a"));
    }

    fn crc32_zlib(b: &[u8]) -> u32 {
        let t = crc_table();
        !b.iter().fold(!0u32, |v, &x| t[((v ^ x as u32) & 0xFF) as usize] ^ (v >> 8))
    }

    /// Encrypts like the archives do (the cipher is its own inverse) and checks `decode` and `decode_head` undo it.
    #[test]
    fn decode_undoes_chunked_counter_mode_and_inflates() {
        let key = LooseKey::from_aes_key(&[7u8; 24]).unwrap();
        // 100,000 bytes that compress to more than one 0x8000 chunk
        let mut x = 12345u32;
        let original: Vec<u8> = (0..100_000).map(|_| { x = x.wrapping_mul(1664525).wrapping_add(1013904223); (x >> 24) as u8 }).collect();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(&original).unwrap();
        let compressed = z.finish().unwrap();
        assert!(compressed.len() > 0x8000, "the test needs several chunks");
        let trailer = 0x1234_5678u32;
        let mut stored = Vec::new();
        for (i, c) in compressed.chunks(0x8000).enumerate() {
            let mut iv = [0u8; 16];
            for (k, v) in [name_crc("x.bin"), trailer, (i * 0x8000) as u32, c.len() as u32].into_iter().enumerate() {
                iv[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
            }
            stored.extend(ctr_be(&key.cipher, iv, c));
        }
        stored.extend(trailer.to_le_bytes());
        assert_eq!(key.decode("x.bin", &stored).unwrap(), original);
        assert_eq!(key.decode_head("x.bin", &stored, 64).unwrap(), original[..64]);
        assert!(key.decode("other.bin", &stored).is_err(), "the name is part of the IV");
    }

    #[test]
    fn counter_increments_as_a_big_endian_integer() {
        let key = LooseKey::from_aes_key(&[1u8; 24]).unwrap();
        let mut iv = [0u8; 16];
        iv[15] = 0xFF; // the carry must run into byte 14, not wrap within the last byte
        let two_blocks = ctr_be(&key.cipher, iv, &[0u8; 32]);
        let mut second = [0u8; 16];
        second[14] = 1;
        assert_eq!(&two_blocks[16..], &ctr_be(&key.cipher, second, &[0u8; 16])[..]);
    }

    /// A helper that greets and then never answers: the reply times out instead of blocking, and after the second timeout the
    /// helper is not asked again.
    #[cfg(windows)]
    #[test]
    fn a_silent_helper_times_out() {
        let dir = std::env::temp_dir().join(format!("atlas_ifs_silent_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("silent.cmd");
        std::fs::write(&fake, "@echo OK\r\n@ping -n 60 127.0.0.1 >nul\r\n").unwrap();
        let mut h = Helper::spawn(&fake, Path::new("IFS2.dll")).unwrap();
        let t = std::time::Instant::now();
        assert!(h.ensure_open(3, Path::new("a.ifs")).is_err());
        assert!(t.elapsed() >= REPLY_TIMEOUT && t.elapsed() < REPLY_TIMEOUT * 3, "{:?}", t.elapsed());
        assert_eq!(h.strikes, 1);
        assert!(h.stuck.contains(&3));
        // the archive that hung is refused at once
        let t = std::time::Instant::now();
        assert!(h.ensure_open(3, Path::new("a.ifs")).is_err());
        assert!(t.elapsed() < Duration::from_secs(1));
        // another archive gets a fresh helper, which hangs too: then the helper is given up
        h.respawn().unwrap();
        assert!(h.ensure_open(4, Path::new("b.ifs")).is_err());
        assert!(!h.usable());
        let t = std::time::Instant::now();
        assert!(h.ensure_open(5, Path::new("c.ifs")).is_err());
        assert!(t.elapsed() < Duration::from_secs(1));
        drop(h);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
