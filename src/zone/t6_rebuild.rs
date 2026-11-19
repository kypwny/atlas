//! Editing Black Ops II zones: replace the contents of an asset, or remove an asset, and rebuild the image so that every
//! pointer, count and block size is still right. Raw files, script parse trees, localize entries and string tables can be
//! replaced by their content; any asset can be removed (when nothing else points into it).
//!
//! How a zone stays consistent. The loader (`schema_walk`) is run with a recorder: it notes every operation on a memory block
//! (align, take bytes from the stream, insert a pointer slot) and every *offset pointer* of the stream, a pointer that refers back
//! to data loaded earlier (shared strings, references to earlier assets through their slot in the asset list or in the insert
//! block). The stream is then spliced: the asset list array loses the entries of removed assets, replaced assets bring their new
//! bytes, everything else is copied. The spliced stream is walked again, with room in every block, which gives the new offset of
//! each block operation: the operations of an untouched asset correspond one to one, in order, to the old ones, so an old
//! (block, offset) maps to the new offset of the operation that contains it (alignment may move things by different amounts
//! than the edit did, which is why the operations are replayed rather than shifted). Each offset pointer is rewritten through
//! that map; one that points into replaced or removed data cannot be mapped and stops the rebuild with a message that names the
//! assets. The block sizes of the header come from the new walk, the header's size word and asset count are updated, and the
//! result is walked a last time, which has to use the stream up and end every block at the size the header states; the final
//! walk must also repeat the operations of the second one exactly and every rewritten pointer has to land in the operation the
//! old pointer's target corresponds to.
//!
//! This checks the structure of the zone, not the game's behaviour: no rebuilt zone was loaded by the game.

use super::schema_walk::{Ev, EvKind, Game, Layout, WalkReport, ZoneImage, walk_recorded};
use super::t6::T6Zone;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;

const GAME: Game = Game::T6;
/// Pointer value of data that follows in the stream.
pub const FOLLOW: u32 = 0xFFFF_FFFF;
/// Pointer value of data that follows and registers a slot (assets in the temporary block).
pub const FOLLOW_SLOT: u32 = 0xFFFF_FFFE;
/// The temporary block (rewound after each asset: offsets in it carry no meaning outside the asset).
const TEMP_BLOCK: u8 = 0;
/// Asset types.
pub const LOCALIZE: u32 = 24;
pub const RAWFILE: u32 = 41;
pub const STRINGTABLE: u32 = 42;
pub const SCRIPT_PARSE_TREE: u32 = 48;
/// The asset count of the asset list sits at this image offset.
const ASSET_COUNT_AT: usize = 56;

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// A zone image that was walked exactly, with the layout of the walk.
pub struct T6Image {
    pub zone: ZoneImage,
    pub report: WalkReport,
    pub layout: Layout,
    index: EventIndex,
}

impl T6Image {
    /// Walks `image` (an `XFile` header followed by the stream); fails unless the walk is exact.
    pub fn new(name: &str, image: Vec<u8>) -> Result<T6Image> {
        if image.len() < 64 {
            bail!("zone image too small");
        }
        let mut block_sizes = Vec::new();
        for i in 0..8 {
            block_sizes.push(u32_at(&image, 8 + 4 * i));
        }
        let zone = ZoneImage { name: name.to_string(), image, block_sizes };
        let (report, layout) = walk_recorded(GAME, &zone);
        if let Some(f) = &report.failure {
            bail!("{name}: the walk fails at asset {} (type {}) at {:#x}: {}", f.asset_index, f.ty, f.offset, f.reason);
        }
        if !report.exact {
            bail!("{name}: the walk is not exact: {}", report.mismatches.join("; "));
        }
        let index = EventIndex::new(&layout);
        Ok(T6Image { zone, report, layout, index })
    }

    pub fn from_zone(z: &T6Zone) -> Result<T6Image> {
        T6Image::new(&z.name, z.image.clone())
    }

    pub fn image(&self) -> &[u8] {
        &self.zone.image
    }

    pub fn name(&self) -> &str {
        &self.zone.name
    }

    pub fn asset_count(&self) -> usize {
        self.layout.array_count
    }

    pub fn asset_type(&self, k: usize) -> u32 {
        u32_at(self.image(), self.layout.array_pos + 8 * k)
    }

    /// The pointer of the asset list entry: `FOLLOW`/`FOLLOW_SLOT` when the asset's data is in the stream.
    pub fn asset_pointer(&self, k: usize) -> u32 {
        u32_at(self.image(), self.layout.array_pos + 8 * k + 4)
    }

    /// The asset's name (empty for entries that only refer to an asset loaded earlier).
    pub fn asset_name(&self, k: usize) -> &str {
        &self.report.assets[k].name
    }

    /// Image offsets of the asset's stream bytes.
    pub fn range(&self, k: usize) -> (usize, usize) {
        (self.report.assets[k].start, self.report.assets[k].end)
    }

    pub fn asset_bytes(&self, k: usize) -> &[u8] {
        let (s, e) = self.range(k);
        &self.image()[s..e]
    }

    /// Index of the asset of type `ty` with this name.
    pub fn find(&self, ty: u32, name: &str) -> Option<usize> {
        (0..self.asset_count()).find(|&k| self.asset_type(k) == ty && self.asset_name(k) == name)
    }

    /// Index of the asset whose stream bytes contain image offset `pos`.
    pub fn asset_at(&self, pos: usize) -> Option<usize> {
        let a = &self.report.assets;
        let i = a.partition_point(|x| x.start <= pos);
        (i > 0 && pos < a[i - 1].end).then(|| i - 1)
    }

    /// The offset pointer stored at image offset `pos`, as (block, offset) of its target.
    pub fn offset_pointer_at(&self, pos: usize) -> Option<(u8, u32)> {
        self.index.offref_at(&self.layout, pos as u32)
    }

    /// The NUL-terminated string at (block, offset), when it is data from the stream.
    pub fn cstr_at(&self, block: u8, offset: u32) -> Option<String> {
        self.cstr_bytes_at(block, offset).map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// The same as bytes (the strings of the zones are not always UTF-8).
    pub fn cstr_bytes_at(&self, block: u8, offset: u32) -> Option<Vec<u8>> {
        let i = self.index.find(&self.layout, block, offset)?;
        let e = self.layout.events[i];
        if e.kind != EvKind::Bytes || e.stream == u32::MAX {
            return None;
        }
        let from = e.stream as usize + (offset - e.start) as usize;
        let end = self.image()[from..].iter().position(|&c| c == 0)?;
        Some(self.image()[from..from + end].to_vec())
    }
}

/// Block operations by block, for finding the one that contains an offset.
struct EventIndex {
    /// Per block: indices into `Layout::events`, in the order the loader performed them (the offsets rise; the temporary block
    /// is left out because it is rewound).
    blocks: Vec<Vec<u32>>,
    /// Offset pointers sorted by image position.
    refs: Vec<(u32, u8, u32)>,
}

impl EventIndex {
    fn new(l: &Layout) -> EventIndex {
        let mut blocks: Vec<Vec<u32>> = vec![Vec::new(); 8];
        for (i, e) in l.events.iter().enumerate() {
            if e.block != TEMP_BLOCK && (e.block as usize) < blocks.len() {
                blocks[e.block as usize].push(i as u32);
            }
        }
        let mut refs: Vec<(u32, u8, u32)> = l.offrefs.iter().map(|o| (o.stream, o.block, o.offset)).collect();
        refs.sort_unstable();
        EventIndex { blocks, refs }
    }

    /// The event (index) that holds `offset` of `block`: the bytes or slot that cover it, or the alignment point that sits there.
    fn find(&self, l: &Layout, block: u8, offset: u32) -> Option<usize> {
        let v = self.blocks.get(block as usize)?;
        let p = v.partition_point(|&i| l.events[i as usize].start <= offset);
        let mut zero = None;
        for &i in v[..p].iter().rev() {
            let e = &l.events[i as usize];
            if e.len == 0 {
                if e.start == offset && zero.is_none() {
                    zero = Some(i as usize);
                }
                continue;
            }
            if offset < e.start + e.len {
                return Some(i as usize);
            }
            break;
        }
        zero
    }

    fn offref_at(&self, _l: &Layout, pos: u32) -> Option<(u8, u32)> {
        let i = self.refs.binary_search_by_key(&pos, |r| r.0).ok()?;
        Some((self.refs[i].1, self.refs[i].2))
    }
}

/// New stream bytes for an asset.
pub struct NewAsset {
    pub bytes: Vec<u8>,
    /// Offset pointers of the old asset that stay in the new bytes: (offset in `bytes`, image offset of the pointer in the old
    /// zone). They are rewritten like all other offset pointers.
    pub keep: Vec<(usize, usize)>,
}

pub enum Edit {
    /// Replace the asset with these stream bytes.
    Replace(usize, NewAsset),
    /// Remove the asset (its entry leaves the asset list).
    Remove(usize),
}

#[derive(Debug, Default, Clone)]
pub struct RebuildStats {
    pub replaced: usize,
    pub removed: usize,
    /// Offset pointers rewritten (a changed value), kept as they were, and dropped with the data they were in.
    pub pointers_moved: usize,
    pub pointers_unchanged: usize,
    pub pointers_dropped: usize,
    pub old_len: usize,
    pub new_len: usize,
}

pub struct Rebuilt {
    /// The new zone, walked exactly.
    pub zone: T6Image,
    pub stats: RebuildStats,
}

enum Act {
    Keep,
    Replace(NewAsset),
    Remove,
}

#[derive(Clone, Copy)]
struct Ent {
    old: u32,
    len: u32,
    new: u32,
    new_len: u32,
    new_ev: u32,
    array: bool,
}

struct Hit {
    offset: u32,
    new_ev: u32,
}

struct Map {
    blocks: Vec<Vec<Ent>>,
    kept_index: Vec<Option<usize>>,
}

impl Map {
    fn lookup(&self, block: u8, t: u32) -> Option<Hit> {
        let v = self.blocks.get(block as usize)?;
        let p = v.partition_point(|e| e.old <= t);
        let mut zero: Option<&Ent> = None;
        let mut found: Option<&Ent> = None;
        for e in v[..p].iter().rev() {
            if e.len == 0 {
                if e.old == t && zero.is_none() {
                    zero = Some(e);
                }
                continue;
            }
            if t < e.old + e.len {
                found = Some(e);
            }
            break;
        }
        let e = found.or(zero)?;
        let rel = t - e.old;
        if e.array {
            let (k, rem) = ((rel / 8) as usize, rel % 8);
            let k2 = (*self.kept_index.get(k)?)? as u32;
            return Some(Hit { offset: e.new + 8 * k2 + rem, new_ev: e.new_ev });
        }
        if rel >= e.new_len && e.len > 0 {
            return None;
        }
        Some(Hit { offset: e.new + rel, new_ev: e.new_ev })
    }
}

/// Splices the edits into the zone and relocates everything that moved; see the module documentation.
pub fn rebuild(base: &T6Image, edits: Vec<Edit>) -> Result<Rebuilt> {
    let lay = &base.layout;
    let n = lay.array_count;
    let old = base.image();
    let mut acts: Vec<Act> = (0..n).map(|_| Act::Keep).collect();
    for e in edits {
        let (k, act) = match e {
            Edit::Replace(k, na) => (k, Act::Replace(na)),
            Edit::Remove(k) => (k, Act::Remove),
        };
        if k >= n {
            bail!("no asset {k} (the zone has {n})");
        }
        if !matches!(acts[k], Act::Keep) {
            bail!("asset {k} is edited twice");
        }
        if matches!(act, Act::Replace(_)) && !matches!(base.asset_pointer(k), FOLLOW | FOLLOW_SLOT) {
            bail!("asset {k} only refers to an asset loaded earlier; it has no data to replace");
        }
        acts[k] = act;
    }
    // the assets are contiguous in the stream and end where the image does
    let array_end = lay.array_pos + 8 * n;
    let mut expect = array_end;
    for k in 0..n {
        let (s, e) = base.range(k);
        if s != expect {
            bail!("asset {k} does not start where the previous one ends");
        }
        expect = e;
    }
    if expect != old.len() {
        bail!("the assets end at {expect:#x}, the image at {:#x}", old.len());
    }

    // ---- splice
    let kept: Vec<usize> = (0..n).filter(|&k| !matches!(acts[k], Act::Remove)).collect();
    let mut kept_index = vec![None; n];
    for (i, &k) in kept.iter().enumerate() {
        kept_index[k] = Some(i);
    }
    let mut new = Vec::with_capacity(old.len() + 4096);
    new.extend_from_slice(&old[..lay.array_pos]);
    for &k in &kept {
        new.extend_from_slice(&old[lay.array_pos + 8 * k..lay.array_pos + 8 * k + 8]);
    }
    put_u32(&mut new, ASSET_COUNT_AT, kept.len() as u32);
    let new_array_pos = lay.array_pos;
    let mut new_start = vec![0usize; n];
    for &k in &kept {
        new_start[k] = new.len();
        match &acts[k] {
            Act::Keep => new.extend_from_slice(base.asset_bytes(k)),
            Act::Replace(na) => new.extend_from_slice(&na.bytes),
            Act::Remove => unreachable!(),
        }
    }
    let growth = new.len().saturating_sub(old.len());

    // ---- walk the spliced stream with room in every block
    let margin = (growth + 0x10000) as u32;
    let mut loose = vec![];
    for i in 0..8 {
        loose.push(base.zone.block_sizes[i].saturating_add(margin));
    }
    let name = base.name().to_string();
    let zi = ZoneImage { name: name.clone(), image: new, block_sizes: loose };
    let (r2, lay2) = walk_recorded(GAME, &zi);
    if let Some(f) = &r2.failure {
        bail!("the edited stream cannot be walked: asset {} (type {}) at {:#x}: {}", f.asset_index, f.ty, f.offset, f.reason);
    }
    let mut new = zi.image;
    if lay2.assets.len() != kept.len() {
        bail!("the edited stream holds {} assets, {} expected", lay2.assets.len(), kept.len());
    }

    // ---- map old block offsets to new ones
    let mut map = Map { blocks: vec![Vec::new(); 8], kept_index: kept_index.clone() };
    let describe = |k: usize| format!("asset {k} ({} {:?})", super::t6::ASSET_TYPES.get(base.asset_type(k) as usize).copied().unwrap_or("?"), base.asset_name(k));
    {
        let push = |i: usize, j: usize, array: bool, map: &mut Map| -> Result<()> {
            let (a, b) = (lay.events[i], lay2.events[j]);
            if a.block == TEMP_BLOCK {
                return Ok(());
            }
            let v = &mut map.blocks[a.block as usize];
            if v.last().is_some_and(|l| l.old > a.start) {
                bail!("block {} offsets do not rise in the walk", a.block);
            }
            v.push(Ent { old: a.start, len: a.len, new: b.start, new_len: b.len, new_ev: j as u32, array });
            Ok(())
        };
        let (po, pn) = (lay.preamble.clone(), lay2.preamble.clone());
        if po.len() != pn.len() {
            bail!("the script strings or the asset list were walked differently");
        }
        for (i, j) in po.zip(pn) {
            let (a, b) = (&lay.events[i], &lay2.events[j]);
            let is_array = a.kind == EvKind::Bytes && a.stream as usize == lay.array_pos && a.len as usize == 8 * n;
            if !is_array && (a.kind, a.block, a.len) != (b.kind, b.block, b.len) {
                bail!("the preamble of the zone was walked differently after the edit");
            }
            push(i, j, is_array, &mut map)?;
        }
        for k in 0..n {
            let Some(k2) = kept_index[k] else { continue };
            let (ro, rn) = (lay.assets[k].clone(), lay2.assets[k2].clone());
            let key = |e: &Ev| (e.kind, e.block);
            match &acts[k] {
                Act::Keep => {
                    if ro.len() != rn.len() {
                        bail!("{} was walked differently after the edit ({} operations, {} before)", describe(k), rn.len(), ro.len());
                    }
                    for (i, j) in ro.zip(rn) {
                        let (a, b) = (&lay.events[i], &lay2.events[j]);
                        if (a.kind, a.block, a.len) != (b.kind, b.block, b.len) {
                            bail!("{} was walked differently after the edit (operation {i})", describe(k));
                        }
                        push(i, j, false, &mut map)?;
                    }
                }
                Act::Replace(_) => {
                    let same_shape = ro.len() == rn.len() && ro.clone().zip(rn.clone()).all(|(i, j)| key(&lay.events[i]) == key(&lay2.events[j]));
                    if same_shape {
                        for (i, j) in ro.zip(rn) {
                            push(i, j, false, &mut map)?;
                        }
                    } else {
                        // only the slots (how later assets refer to this one) keep their meaning
                        let olds: Vec<usize> = ro.filter(|&i| lay.events[i].kind == EvKind::Slot).collect();
                        let news: Vec<usize> = rn.filter(|&j| lay2.events[j].kind == EvKind::Slot).collect();
                        if olds.len() == news.len() {
                            for (i, j) in olds.into_iter().zip(news) {
                                push(i, j, false, &mut map)?;
                            }
                        }
                    }
                }
                Act::Remove => unreachable!(),
            }
        }
    }

    // ---- positions in the new stream
    let old_starts: Vec<usize> = (0..n).map(|k| base.range(k).0).collect();
    let new_pos = |p: usize| -> Option<usize> {
        if p < lay.array_pos {
            return Some(p);
        }
        if p < array_end {
            let k = (p - lay.array_pos) / 8;
            return kept_index[k].map(|k2| new_array_pos + 8 * k2 + (p - lay.array_pos) % 8);
        }
        let k = old_starts.partition_point(|&s| s <= p) - 1;
        match acts[k] {
            Act::Keep => Some(new_start[k] + (p - old_starts[k])),
            _ => None,
        }
    };

    // ---- rewrite the offset pointers
    let mut stats = RebuildStats { old_len: old.len(), ..RebuildStats::default() };
    let mut expect_ev: Vec<(usize, u8, u32, u32)> = Vec::new(); // (new position, block, new offset, expected event)
    let mut patch = |new: &mut Vec<u8>, old_pos: usize, at: usize, stats: &mut RebuildStats| -> Result<()> {
        let (block, off) = base.offset_pointer_at(old_pos).context("an offset pointer to keep is not recorded")?;
        let Some(hit) = map.lookup(block, off) else {
            let target = base.index.find(lay, block, off).and_then(|i| (0..n).find(|&k| lay.assets[k].contains(&i)));
            let from = base.asset_at(old_pos);
            let what = |k: Option<usize>| k.map(&describe).unwrap_or_else(|| "the zone's asset list".to_string());
            bail!("{} points into {}, which was replaced or removed (block {block}, offset {off})", what(from), what(target));
        };
        let old_val = u32_at(old, old_pos);
        let new_val = old_val.wrapping_add(hit.offset.wrapping_sub(off));
        if new_val != old_val {
            stats.pointers_moved += 1;
        } else {
            stats.pointers_unchanged += 1;
        }
        put_u32(new, at, new_val);
        expect_ev.push((at, block, hit.offset, hit.new_ev));
        Ok(())
    };
    for o in &base.layout.offrefs {
        match new_pos(o.stream as usize) {
            Some(at) => patch(&mut new, o.stream as usize, at, &mut stats)?,
            None => stats.pointers_dropped += 1,
        }
    }
    for k in 0..n {
        if let Act::Replace(na) = &acts[k] {
            stats.replaced += 1;
            for &(rel, old_pos) in &na.keep {
                patch(&mut new, old_pos, new_start[k] + rel, &mut stats)?;
            }
        }
        if matches!(acts[k], Act::Remove) {
            stats.removed += 1;
        }
    }

    // ---- header
    let mut sizes = [0u32; 8];
    for i in 0..8 {
        sizes[i] = if i == TEMP_BLOCK as usize { base.zone.block_sizes[i].max(r2.temp_peak as u32) } else { r2.block_end[i] as u32 };
        put_u32(&mut new, 8 + 4 * i, sizes[i]);
    }
    let size_word = u32_at(old, 0);
    if size_word as usize + 40 != old.len() {
        bail!("the size word ({size_word}) is not the image length less 40; the header is not understood");
    }
    let size_word_new = (new.len() - 40) as u32;
    put_u32(&mut new, 0, size_word_new);
    // the external size is not derivable (the loader's own accounting); grow it with the blocks
    let grown: i64 = (1..8).map(|i| sizes[i] as i64 - base.zone.block_sizes[i] as i64).sum();
    let ext = u32_at(old, 4) as i64 + grown.max(0);
    put_u32(&mut new, 4, ext as u32);
    stats.new_len = new.len();

    // ---- the last walk
    let fin = T6Image::new(&name, new).context("the rebuilt zone does not walk exactly")?;
    let (a, b) = (&lay2.events, &fin.layout.events);
    if a.len() != b.len() || a.iter().zip(b).any(|(x, y)| (x.kind, x.block, x.start, x.len) != (y.kind, y.block, y.start, y.len)) {
        bail!("the rewritten pointers changed what the loader does");
    }
    let by_pos: HashMap<u32, (u8, u32)> = fin.layout.offrefs.iter().map(|o| (o.stream, (o.block, o.offset))).collect();
    for (at, block, off, ev) in expect_ev {
        match by_pos.get(&(at as u32)) {
            Some(&(b2, o2)) if b2 == block && o2 == off => {}
            other => bail!("pointer at {at:#x} reads back as {other:?}, expected block {block} offset {off}"),
        }
        if fin.index.find(&fin.layout, block, off) != Some(ev as usize) {
            bail!("pointer at {at:#x} lands in a different operation than its old target");
        }
    }
    Ok(Rebuilt { zone: fin, stats })
}

// ----------------------------------------------------------------------------------------------------- asset contents
fn cstr_end(b: &[u8], from: usize) -> Result<usize> {
    let e = b.get(from..).and_then(|s| s.iter().position(|&c| c == 0)).context("unterminated string in an asset")?;
    Ok(from + e + 1)
}

/// Offset pointers of the old asset `k` inside the first `len` bytes of its stream, as `keep` entries at the same offsets.
fn keep_header(base: &T6Image, k: usize, len: usize, skip: &[usize]) -> Vec<(usize, usize)> {
    let start = base.range(k).0;
    (0..len / 4).map(|w| 4 * w).filter(|rel| !skip.contains(rel)).filter(|rel| base.offset_pointer_at(start + rel).is_some()).map(|rel| (rel, start + rel)).collect()
}

impl T6Image {
    /// The asset, byte for byte, as a replacement (all its offset pointers kept): rebuilding with it changes nothing.
    pub fn unchanged(&self, k: usize) -> NewAsset {
        let (s, e) = self.range(k);
        let keep = self.layout.offrefs.iter().filter(|o| (o.stream as usize) >= s && (o.stream as usize) < e).map(|o| (o.stream as usize - s, o.stream as usize)).collect();
        NewAsset { bytes: self.asset_bytes(k).to_vec(), keep }
    }

    /// A string pointer inside an asset: the text (inline, or through an offset pointer) and where it ends in the stream.
    fn string_at(&self, k: usize, rel: usize, cursor: &mut usize) -> Result<Option<String>> {
        let bytes = self.asset_bytes(k);
        let p = u32_at(bytes, rel);
        match p {
            0 => Ok(None),
            FOLLOW => {
                let end = cstr_end(bytes, *cursor)?;
                let s = String::from_utf8_lossy(&bytes[*cursor..end - 1]).into_owned();
                *cursor = end;
                Ok(Some(s))
            }
            _ => {
                let (b, o) = self.offset_pointer_at(self.range(k).0 + rel).context("string pointer that is neither inline nor an offset pointer")?;
                Ok(Some(self.cstr_at(b, o).context("string behind an offset pointer not found")?))
            }
        }
    }

    // ----- raw files and script parse trees: name, length, buffer
    /// The content of a raw file or script parse tree.
    pub fn rawfile(&self, k: usize) -> Result<Vec<u8>> {
        if !matches!(self.asset_type(k), RAWFILE | SCRIPT_PARSE_TREE) {
            bail!("asset {k} is not a raw file or a script parse tree");
        }
        let b = self.asset_bytes(k);
        let (len, buf) = (u32_at(b, 4) as usize, u32_at(b, 8));
        let mut c = 12;
        self.string_at(k, 0, &mut c)?;
        if buf == 0 {
            return Ok(Vec::new());
        }
        if buf != FOLLOW || c + len + 1 != b.len() {
            bail!("asset {k}: unexpected raw file layout");
        }
        Ok(b[c..c + len].to_vec())
    }

    /// The asset with another content (its name stays; the stored length is the new length).
    pub fn set_rawfile(&self, k: usize, content: &[u8]) -> Result<NewAsset> {
        self.rawfile(k)?;
        let b = self.asset_bytes(k);
        let mut c = 12;
        self.string_at(k, 0, &mut c)?;
        let mut out = b[..c].to_vec();
        put_u32(&mut out, 4, content.len() as u32);
        put_u32(&mut out, 8, FOLLOW);
        out.extend_from_slice(content);
        out.push(0);
        Ok(NewAsset { bytes: out, keep: keep_header(self, k, 12, &[4, 8]) })
    }

    // ----- localize entries: value, name
    /// (name, value) of a localize entry.
    pub fn localize(&self, k: usize) -> Result<(String, String)> {
        if self.asset_type(k) != LOCALIZE {
            bail!("asset {k} is not a localize entry");
        }
        let mut c = 8;
        let value = self.string_at(k, 0, &mut c)?.unwrap_or_default();
        let name = self.string_at(k, 4, &mut c)?.unwrap_or_default();
        if c != self.asset_bytes(k).len() {
            bail!("asset {k}: unexpected localize layout");
        }
        Ok((name, value))
    }

    pub fn set_localize(&self, k: usize, value: &str) -> Result<NewAsset> {
        self.localize(k)?;
        let b = self.asset_bytes(k);
        let mut out = b[..8].to_vec();
        put_u32(&mut out, 0, FOLLOW);
        out.extend_from_slice(value.as_bytes());
        out.push(0);
        let mut keep = Vec::new();
        match u32_at(b, 4) {
            FOLLOW => {
                // the name is inline: copy it (it comes after the old value if that was inline)
                let mut c = 8;
                if u32_at(b, 0) == FOLLOW {
                    c = cstr_end(b, 8)?;
                }
                let end = cstr_end(b, c)?;
                out.extend_from_slice(&b[c..end]);
            }
            0 => {}
            _ => keep.push((4, self.range(k).0 + 4)),
        }
        Ok(NewAsset { bytes: out, keep })
    }

    // ----- string tables
    pub fn stringtable(&self, k: usize) -> Result<StringTable> {
        if self.asset_type(k) != STRINGTABLE {
            bail!("asset {k} is not a string table");
        }
        let b = self.asset_bytes(k);
        let start = self.range(k).0;
        let mut c = 20;
        let name = self.string_at(k, 0, &mut c)?.unwrap_or_default();
        let (cols, rows) = (u32_at(b, 4) as usize, u32_at(b, 8) as usize);
        let n = cols.checked_mul(rows).filter(|&n| n < 0x400_0000).context("string table size")?;
        let (values, index_ptr) = (u32_at(b, 12), u32_at(b, 16));
        let mut cells = Vec::new();
        let mut hashes = Vec::new();
        let mut index = Vec::new();
        if values == FOLLOW && n > 0 {
            let table = c;
            c += 8 * n;
            if c > b.len() {
                bail!("asset {k}: the cell table runs past the asset");
            }
            for i in 0..n {
                hashes.push(u32_at(b, table + 8 * i + 4));
                let p = u32_at(b, table + 8 * i);
                let text = match p {
                    0 => None,
                    FOLLOW => {
                        let end = cstr_end(b, c)?;
                        let s = b[c..end - 1].to_vec();
                        c = end;
                        Some(s)
                    }
                    _ => {
                        let (bl, o) = self.offset_pointer_at(start + table + 8 * i).context("cell pointer")?;
                        Some(self.cstr_bytes_at(bl, o).context("shared cell string not found")?)
                    }
                };
                cells.push(text);
            }
        } else if values != 0 && !(values == FOLLOW && n == 0) {
            bail!("asset {k}: unexpected string table layout (cell pointer {values:#x}, {n} cells)");
        }
        if index_ptr == FOLLOW && n > 0 {
            if c + 2 * n != b.len() {
                bail!("asset {k}: unexpected cell index size");
            }
            for i in 0..n {
                index.push(u16::from_le_bytes([b[c + 2 * i], b[c + 2 * i + 1]]));
            }
        } else if c != b.len() || !(index_ptr == 0 || n == 0) {
            bail!("asset {k}: unexpected string table layout (cell index pointer {index_ptr:#x}, {n} cells, {} of {} bytes read)", c, b.len());
        }
        Ok(StringTable { name, columns: cols, rows, cells, hashes, index })
    }

    /// The table with new cells (`rows` x `columns`, row by row). Hashes and the sorted cell index are made again. Every cell
    /// string is stored in the table itself, so shared strings the old table had are no longer shared.
    pub fn set_stringtable(&self, k: usize, columns: usize, rows: usize, cells: &[Vec<u8>]) -> Result<NewAsset> {
        let old = self.stringtable(k)?;
        let n = columns * rows;
        if cells.len() != n {
            bail!("{} cells given for {rows} rows of {columns}", cells.len());
        }
        if n > 0xFFFF {
            bail!("a string table has at most 65535 cells");
        }
        let b = self.asset_bytes(k);
        let mut c = 20;
        self.string_at(k, 0, &mut c)?;
        let mut out = b[..c].to_vec();
        put_u32(&mut out, 4, columns as u32);
        put_u32(&mut out, 8, rows as u32);
        put_u32(&mut out, 12, if n > 0 { FOLLOW } else { u32_at(b, 12) });
        put_u32(&mut out, 16, if n > 0 { FOLLOW } else { u32_at(b, 16) });
        let same_shape = old.columns == columns && old.rows == rows;
        let mut table = Vec::new();
        let mut strings = Vec::new();
        let mut hashes = Vec::with_capacity(n);
        for (i, text) in cells.iter().enumerate() {
            // a cell that was empty and stays empty stays null
            let was_null = same_shape && old.cells[i].is_none() && text.is_empty();
            let h = hash_bytes(text);
            hashes.push(h);
            table.extend_from_slice(&(if was_null { 0 } else { FOLLOW }).to_le_bytes());
            table.extend_from_slice(&h.to_le_bytes());
            if !was_null {
                strings.extend_from_slice(text);
                strings.push(0);
            }
        }
        out.extend_from_slice(&table);
        out.extend_from_slice(&strings);
        let index = if same_shape && !old.index.is_empty() {
            let changed: Vec<bool> = (0..n).map(|i| hashes[i] != old.hashes[i] || cells[i] != old.cells[i].clone().unwrap_or_default()).collect();
            cell_index_update(&old.index, &hashes, &changed, columns)
        } else {
            cell_index(&hashes, columns)
        };
        for i in index {
            out.extend_from_slice(&i.to_le_bytes());
        }
        Ok(NewAsset { bytes: out, keep: keep_header(self, k, 20, &[4, 8, 12, 16]) })
    }
}

/// A string table of the zone: cells row by row, as the bytes stored (not always UTF-8); `None` is a null string pointer.
#[derive(Debug, Clone)]
pub struct StringTable {
    pub name: String,
    pub columns: usize,
    pub rows: usize,
    pub cells: Vec<Option<Vec<u8>>>,
    pub hashes: Vec<u32>,
    pub index: Vec<u16>,
}

impl StringTable {
    /// The cells as text (null as empty; bytes that are not UTF-8 are replaced).
    pub fn texts(&self) -> Vec<String> {
        self.cells.iter().map(|c| String::from_utf8_lossy(c.as_deref().unwrap_or_default()).into_owned()).collect()
    }

    /// The cells as stored (null as empty).
    pub fn raw(&self) -> Vec<Vec<u8>> {
        self.cells.iter().map(|c| c.clone().unwrap_or_default()).collect()
    }

    /// Whether the stored hashes are the cells' hashes and the cell index is a permutation that is ordered by the game's rule
    /// (see [`index_follows`]).
    pub fn consistent(&self) -> bool {
        self.raw().iter().zip(&self.hashes).all(|(s, &h)| hash_bytes(s) == h) && index_follows(&self.hashes, &self.index, self.columns)
    }
}

/// The hash of a string table cell (`Com_HashString`: djb2 over the lower-cased characters, starting at 5381; a character is a
/// signed byte, so bytes of UTF-8 sequences add as negative numbers).
pub fn hash_string(s: &str) -> u32 {
    hash_bytes(s.as_bytes())
}

/// [`hash_string`] of raw bytes.
pub fn hash_bytes(s: &[u8]) -> u32 {
    let mut h = 5381u32;
    for &c in s {
        h = h.wrapping_shl(5).wrapping_add(h).wrapping_add(c.to_ascii_lowercase() as i8 as i32 as u32);
    }
    h
}

/// `a` comes before `b` in a cell index: the hashes compare as the signed difference `hash(a) - hash(b)` (the way the tool that
/// made the tables, and OpenAssetTools, compare them), cells with equal hashes by column.
fn index_less(hashes: &[u32], columns: usize, a: u16, b: u16) -> bool {
    let col = |i: u16| if columns == 0 { 0 } else { i as usize % columns };
    let d = hashes[a as usize].wrapping_sub(hashes[b as usize]) as i32;
    d < 0 || (d == 0 && col(a) < col(b))
}

/// Whether `index` holds every cell once and no entry is out of order against its predecessor under [`index_less`]. The tables
/// of the game are ordered this way but not by a plain sort of the hashes (their hashes cover the whole 32-bit range, where
/// the signed difference is not a total order: the stored order starts at some hash and runs round), so this is what can be
/// checked of them.
pub fn index_follows(hashes: &[u32], index: &[u16], columns: usize) -> bool {
    if index.len() != hashes.len() {
        return false;
    }
    let mut seen = vec![false; hashes.len()];
    for &i in index {
        match seen.get_mut(i as usize) {
            Some(s) if !*s => *s = true,
            _ => return false,
        }
    }
    index.windows(2).all(|w| !index_less(hashes, columns, w[1], w[0]))
}

/// A new cell index for a table with new cells: the entries of the cells that did not change keep their places, each changed
/// cell goes where it fits between its neighbours. (Sorting everything again would reorder entries that the game's tool
/// placed by a rule that is not a plain sort.)
pub fn cell_index_update(old_index: &[u16], new_hashes: &[u32], changed: &[bool], columns: usize) -> Vec<u16> {
    let mut v: Vec<u16> = old_index.iter().copied().filter(|&i| !changed[i as usize]).collect();
    for c in (0..new_hashes.len()).filter(|&c| changed[c]) {
        let c = c as u16;
        let at = (0..=v.len())
            .find(|&i| (i == 0 || !index_less(new_hashes, columns, c, v[i - 1])) && (i == v.len() || !index_less(new_hashes, columns, v[i], c)))
            // no place fits the neighbours (the chain is not an order there): the first entry that comes after it
            .or_else(|| v.iter().position(|&x| index_less(new_hashes, columns, c, x)))
            .unwrap_or(v.len());
        v.insert(at, c);
    }
    v
}

/// A cell index made from nothing (a table of another shape): the cells by hash, starting from the smallest.
pub fn cell_index(hashes: &[u32], columns: usize) -> Vec<u16> {
    let mut v: Vec<u16> = (0..hashes.len() as u16).collect();
    let col = |i: u16| if columns == 0 { 0 } else { i as usize % columns };
    v.sort_by_key(|&i| (hashes[i as usize], col(i), i));
    v
}
