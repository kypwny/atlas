//! Whole-zone conversion from a stock Modern Warfare 2 (IW4) zone to a Call of Duty Online QA 3.3.3.3 zone (version 423).
//!
//! How it works. The stock zone is walked with the schema walker (`schema_walk::walk_with`), which hands every asset to a hook
//! as soon as it is loaded, together with a read-only view of everything it points at. The hook rewrites the asset into the
//! QA stream layout (`Out`): the QA header, then the payload in the order the QA walker (`Replay`, not stock) consumes it.
//! Every asset becomes its own directory entry with the "insert" pointer (-2), exactly as in the stock zone, so a later
//! reference to it is the address of the 4-byte pointer slot the loader reserves in block 3. Those addresses depend on the
//! whole layout, so the fragments hold symbolic references (`Out::reference`); `assemble` lays the image out, walks it once
//! with tracing to learn the block-3 allocations, patches every reference with the real address, sets the block sizes from
//! that walk and walks the finished image again as the check (every asset must end exactly at its extent, the block sizes
//! are the walk's, and no reference may dangle).
//!
//! Assets the converter has no rule for are left out; the references to them become null.

use super::fastfile::{self, ArchiveKey, Container};
use super::replay::Replay;
use super::convert_walk::{walk_with, Game, View, ZoneImage};
use super::{u32_at, Zone};
use anyhow::{bail, Context, Result};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

thread_local! {
    /// Name of the stock structure being converted and the problems met so far (message -> count).
    static CURRENT: RefCell<String> = RefCell::new(String::new());
    static WARNINGS: RefCell<BTreeMap<String, usize>> = RefCell::new(BTreeMap::new());
}

/// Notes a conversion problem that does not stop the conversion (a reference that resolved to no asset, ...).
pub fn warn(msg: String) {
    WARNINGS.with(|w| *w.borrow_mut().entry(msg).or_default() += 1);
}

pub const INLINE: u32 = u32::MAX;
pub const INSERT: u32 = u32::MAX - 1;
/// What a reference word holds until the layout is known (any value that is neither 0, -1 nor -2).
const PLACEHOLDER: u32 = 0x3000_0001;

/// One converted asset: its QA stream bytes and the words that still need an address.
#[derive(Default, Clone)]
pub struct Frag {
    pub ty: u32,
    pub name: String,
    /// The stock walker's id of the asset (what references name).
    pub id: u32,
    pub bytes: Vec<u8>,
    /// (offset of the word, id of the asset it refers to)
    pub refs: Vec<(usize, u32)>,
    /// (offset of the word, offset in this asset's stream of the data it points at): a reference inside the asset.
    pub inner: Vec<(usize, usize)>,
    /// (id, offset of the data in this asset's stream): assets stored inside this one (with a pointer slot of their own) that
    /// later references may name by `id`.
    pub anchors: Vec<(u32, usize)>,
}

/// Builds the stream of one asset.
pub struct Out {
    pub f: Frag,
}

impl Out {
    pub fn new(ty: u32, id: u32) -> Out {
        Out { f: Frag { ty, id, ..Default::default() } }
    }
    /// Stream offset of the next byte.
    pub fn pos(&self) -> usize {
        self.f.bytes.len()
    }
    /// Appends bytes; returns where they start.
    pub fn put(&mut self, b: &[u8]) -> usize {
        let at = self.pos();
        self.f.bytes.extend_from_slice(b);
        at
    }
    /// Appends a NUL-terminated string.
    pub fn put_str(&mut self, s: &[u8]) -> usize {
        let at = self.put(s);
        self.f.bytes.push(0);
        at
    }
    pub fn set(&mut self, at: usize, v: u32) {
        self.f.bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    /// Makes the word at `at` refer to asset `id` (null when `id` is `None`).
    pub fn reference(&mut self, at: usize, id: Option<u32>) {
        match id {
            Some(id) => {
                self.set(at, PLACEHOLDER);
                self.f.refs.push((at, id));
            }
            None => self.set(at, 0),
        }
    }
    /// Notes that the asset `id`, stored inline in this one, starts at the current position (call before writing its data).
    pub fn anchor(&mut self, id: u32) {
        let at = self.pos();
        self.f.anchors.push((id, at));
    }
    /// Makes the word at `at` point at the data that starts at stream offset `target` of this asset.
    pub fn inner(&mut self, at: usize, target: usize) {
        self.set(at, PLACEHOLDER);
        self.f.inner.push((at, target));
    }
}

/// A QA header being filled: little-endian words and bytes at offsets.
pub struct Hdr(pub Vec<u8>);

impl Hdr {
    pub fn new(size: usize) -> Hdr {
        Hdr(vec![0; size])
    }
    pub fn u32(&mut self, at: usize, v: u32) {
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    pub fn u16(&mut self, at: usize, v: u16) {
        self.0[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    pub fn u8(&mut self, at: usize, v: u8) {
        self.0[at] = v;
    }
    pub fn raw(&mut self, at: usize, b: &[u8]) {
        self.0[at..at + b.len()].copy_from_slice(b);
    }
    pub fn get32(&self, at: usize) -> u32 {
        u32_at(&self.0, at)
    }
}

/// Reads stock structures through the walker's view.
pub struct Src<'a, 'b> {
    pub v: &'a View<'a, 'b>,
    pub hdr: u32,
}

impl<'a, 'b> Src<'a, 'b> {
    pub fn u8(&self, off: u32) -> u8 {
        self.v.u8(self.hdr + off).unwrap_or(0)
    }
    pub fn u16(&self, off: u32) -> u16 {
        self.v.u16(self.hdr + off).unwrap_or(0)
    }
    pub fn u32(&self, off: u32) -> u32 {
        self.v.u32(self.hdr + off).unwrap_or(0)
    }
    pub fn f32(&self, off: u32) -> f32 {
        self.v.f32(self.hdr + off).unwrap_or(0.0)
    }
    /// A pointer field is set (the walker has replaced the stream's marker with an address).
    pub fn has(&self, off: u32) -> bool {
        self.u32(off) != 0
    }
    pub fn bytes(&self, off: u32, n: usize) -> Vec<u8> {
        self.v.bytes(self.hdr + off, n).map(|b| b.to_vec()).unwrap_or_default()
    }
    /// The array an address field points at.
    pub fn array(&self, off: u32, count: usize, stride: usize) -> Vec<u8> {
        let p = self.u32(off);
        if p == 0 || count == 0 {
            return Vec::new();
        }
        self.v.bytes(p, count * stride).map(|b| b.to_vec()).unwrap_or_default()
    }
    /// The NUL-terminated string a pointer field points at (bytes, not decoded: some are not UTF-8).
    pub fn cstr(&self, off: u32) -> Option<Vec<u8>> {
        let p = self.u32(off);
        if p == 0 {
            return None;
        }
        Some(cstr_at(self.v, p))
    }
    /// The id of the asset the pointer field at `off` refers to.
    pub fn asset(&self, off: u32) -> Option<u32> {
        if !self.has(off) {
            return None;
        }
        let id = self.v.asset_id_at_slot(self.hdr + off);
        if id.is_none() {
            warn(format!("{}: a set reference at +{off} named no known asset (now null)", CURRENT.with(|c| c.borrow().clone())));
        }
        id
    }
    /// The structure `off` bytes into this one.
    pub fn sub(&self, off: u32) -> Src<'a, 'b> {
        Src { v: self.v, hdr: self.hdr + off }
    }
    /// The structure at a walker address.
    pub fn at(&self, addr: u32) -> Src<'a, 'b> {
        Src { v: self.v, hdr: addr }
    }
}

/// The string at a walker address.
pub fn cstr_at(v: &View, addr: u32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut a = addr;
    'outer: loop {
        for take in [64usize, 16, 4, 1] {
            if let Some(b) = v.bytes(a, take) {
                if let Some(n) = b.iter().position(|&c| c == 0) {
                    out.extend_from_slice(&b[..n]);
                    return out;
                }
                out.extend_from_slice(b);
                a += take as u32;
                continue 'outer;
            }
        }
        return out;
    }
}

/// Asset type number of a stock structure name.
fn type_of(name: &str) -> Option<u32> {
    Some(match name {
        "PhysPreset" => 0,
        "PhysCollmap" => 1,
        "XAnimParts" => 2,
        "XModel" => 4,
        "Material" => 5,
        "MaterialPixelShader" => 6,
        "MaterialVertexShader" => 7,
        "MaterialVertexDeclaration" => 8,
        "MaterialTechniqueSet" => 9,
        "GfxImage" => 10,
        "snd_alias_list_t" => 11,
        "SndCurve" => 12,
        "LoadedSound" => 13,
        "clipMap_t" => 15,
        "ComWorld" => 16,
        "GameWorldSp" => 17,
        "GameWorldMp" => 18,
        "MapEnts" => 19,
        "FxWorld" => 20,
        "GfxWorld" => 21,
        "GfxLightDef" => 22,
        "Font_s" => 24,
        "MenuList" => 25,
        "menuDef_t" => 26,
        "LocalizeEntry" => 27,
        "WeaponCompleteDef" => 28,
        "FxEffectDef" => 30,
        "FxImpactTable" => 31,
        "RawFile" => 36,
        "StringTable" => 37,
        "LeaderboardDef" => 38,
        "StructuredDataDefSet" => 39,
        "TracerDef" => 40,
        "VehicleDef" => 41,
        "AddonMapEnts" => 42,
        _ => return None,
    })
}

/// What happened to the assets of one type.
#[derive(Default, Debug, Clone)]
pub struct TypeStat {
    pub converted: usize,
    /// reason -> count
    pub left_out: BTreeMap<String, usize>,
}

/// The result of converting a zone.
pub struct Converted {
    pub image: Vec<u8>,
    pub block_sizes: [u32; 8],
    pub stats: BTreeMap<u32, TypeStat>,
    /// References to assets that were left out (now null).
    pub nulled: usize,
    pub assets: usize,
    /// Problems that did not stop the conversion, with how often each happened.
    pub warnings: BTreeMap<String, usize>,
}

/// Rewrites one stock asset; `Err` leaves the asset out.
pub type Rule = fn(&Src, &mut Out) -> std::result::Result<(), String>;

fn rule_for(name: &str) -> Option<Rule> {
    if let Some(r) = super::convert_basic::rule_for(name) {
        return Some(r);
    }
    if let Some(r) = super::convert_gfx::rule_for(name) {
        return Some(r);
    }
    if let Some(r) = super::convert_sound::rule_for(name) {
        return Some(r);
    }
    if let Some(r) = super::convert_model::rule_for(name) {
        return Some(r);
    }
    if let Some(r) = super::convert_fx::rule_for(name) {
        return Some(r);
    }
    if let Some(r) = super::convert_clip::rule_for(name) {
        return Some(r);
    }
    if let Some(r) = super::convert_world::rule_for(name) {
        return Some(r);
    }
    None
}

/// Converts the assets of a stock zone to QA fragments (not yet laid out).
pub fn convert_frags(stock: &ZoneImage) -> Result<(Vec<Frag>, BTreeMap<u32, TypeStat>)> {
    let mut frags: Vec<Frag> = Vec::new();
    let mut stats: BTreeMap<u32, TypeStat> = BTreeMap::new();
    super::convert_model::reset();
    super::convert_clip::reset();
    WARNINGS.with(|w| w.borrow_mut().clear());
    {
        let mut hook = |v: &View, name: &str, hdr: u32, id: u32| {
            if name == "XModelSurfs" {
                super::convert_model::surfs_event(&Src { v, hdr }, id);
                return;
            }
            let Some(ty) = type_of(name) else { return };
            CURRENT.with(|c| *c.borrow_mut() = name.to_string());
            let st = stats.entry(ty).or_default();
            let Some(rule) = rule_for(name) else {
                *st.left_out.entry("no conversion rule".to_string()).or_default() += 1;
                return;
            };
            let src = Src { v, hdr };
            let mut out = Out::new(ty, id);
            match rule(&src, &mut out) {
                Ok(()) => {
                    out.f.name = src.cstr(0).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                    st.converted += 1;
                    frags.push(out.f);
                }
                Err(e) => *st.left_out.entry(e).or_default() += 1,
            }
        };
        let report = walk_with(Game::Iw4, stock, Some(&mut hook));
        if !report.exact {
            bail!("the stock zone does not walk exactly: {:?} {:?}", report.failure, report.mismatches);
        }
    }
    // map entities that the collision map holds inline are not assets of their own
    frags.retain(|f| !(f.ty == 19 && super::convert_clip::consumed(f.id)));
    Ok((frags, stats))
}

/// Converts a stock zone image (already inflated) to a QA zone image.
pub fn convert_image(stock: &ZoneImage) -> Result<Converted> {
    let (frags, stats) = convert_frags(stock)?;
    assemble(stock, frags, stats)
}

/// Prefix of a zone image up to the asset list: header, script strings.
fn prefix_of(image: &[u8]) -> Result<usize> {
    let string_count = u32_at(image, 0x28) as usize;
    let mut pos = 0x38 + 4 * string_count;
    for i in 0..string_count {
        if u32_at(image, 0x38 + 4 * i) == INLINE {
            pos += image[pos..].iter().position(|&c| c == 0).context("unterminated script string")? + 1;
        }
    }
    Ok(pos)
}

fn lay_out(prefix: &[u8], sizes: &[u32; 8], frags: &[Frag]) -> Vec<u8> {
    let mut out = prefix.to_vec();
    for (i, s) in sizes.iter().enumerate() {
        out[8 + 4 * i..12 + 4 * i].copy_from_slice(&s.to_le_bytes());
    }
    out[0x30..0x34].copy_from_slice(&(frags.len() as u32).to_le_bytes());
    let mut offset = out.len() + 16 * frags.len() - 0x28;
    for f in frags {
        out.extend_from_slice(&f.ty.to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        out.extend_from_slice(&INSERT.to_le_bytes());
        out.extend_from_slice(&(f.bytes.len() as u32).to_le_bytes());
        offset += f.bytes.len();
    }
    for f in frags {
        out.extend_from_slice(&f.bytes);
    }
    let size = (out.len() - 0x28) as u32;
    out[0..4].copy_from_slice(&size.to_le_bytes());
    out
}

fn walk_all(zone: &Zone) -> Result<Replay<'_>> {
    let mut r = Replay::new(zone).map_err(|f| anyhow::anyhow!("zone prefix: {} at {}", f.what, f.at))?;
    r.enable_tracing();
    for i in 0..zone.assets.len() {
        r.replay_asset(zone, i).map_err(|f| anyhow::anyhow!("asset {i} (type {}): {} at {:#x}", zone.assets[i].asset_type, f.what, f.at))?;
    }
    Ok(r)
}

fn assemble(stock: &ZoneImage, mut frags: Vec<Frag>, stats: BTreeMap<u32, TypeStat>) -> Result<Converted> {
    let table = prefix_of(&stock.image)?;
    let prefix = stock.image[..table].to_vec();
    // references to assets that were left out become null
    let have: HashSet<u32> = frags.iter().flat_map(|f| std::iter::once(f.id).chain(f.anchors.iter().map(|a| a.0))).collect();
    let mut nulled = 0usize;
    for f in frags.iter_mut() {
        let mut keep = Vec::with_capacity(f.refs.len());
        for &(at, id) in &f.refs {
            if have.contains(&id) {
                keep.push((at, id));
            } else {
                f.bytes[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
                nulled += 1;
            }
        }
        f.refs = keep;
    }
    // pass 1: roomy blocks, placeholder addresses
    let roomy = [0x0f00_0000u32; 8];
    let image = lay_out(&prefix, &roomy, &frags);
    let mut zone = Zone::from_image(fastfile::VERSION_QA, image)?;
    let mut patches: Vec<(usize, u32)> = Vec::new();
    {
        let r = walk_all(&zone)?;
        let allocs = r.block3_allocations();
        let info = r.block3_allocation_info();
        // slot of each asset: its first block-3 allocation (4 bytes, no stream bytes)
        let mut first: HashMap<usize, u32> = HashMap::new();
        for (i, &(asset, address, length)) in allocs.iter().enumerate() {
            if length == 4 && !info[i].1 {
                first.entry(asset).or_insert(address);
            }
        }
        let mut slot_of: HashMap<u32, u32> = HashMap::new();
        for (i, f) in frags.iter().enumerate() {
            let Some(&s) = first.get(&i) else { bail!("asset {i} (type {}, {}) reserved no pointer slot", f.ty, f.name) };
            slot_of.insert(f.id, s);
        }
        // slots of the assets stored inside others: the 4-byte allocation made where their data starts
        {
            let mut nested: HashMap<(usize, usize), u32> = HashMap::new();
            for (i, &(asset, address, length)) in allocs.iter().enumerate() {
                if length == 4 && !info[i].1 {
                    nested.entry((asset, info[i].0)).or_insert(address);
                }
            }
            let mut start = zone.table_offset + 16 * zone.assets.len();
            for (i, f) in frags.iter().enumerate() {
                for &(id, off) in &f.anchors {
                    let Some(&a) = nested.get(&(i, start + off)) else { bail!("{}: the asset stored inline at {off:#x} reserved no pointer slot", f.name) };
                    slot_of.insert(id, a);
                }
                start += f.bytes.len();
            }
        }
        // data that starts at a stream offset, for references inside an asset: (start, length, address), by start
        let mut spans: Vec<(usize, usize, u32)> = allocs.iter().zip(info).filter(|((_, _, l), (_, read))| *read && *l > 0).map(|(&(_, a, l), &(at, _))| (at, l, a)).collect();
        spans.sort_by_key(|s| s.0);
        let mut start = zone.table_offset + 16 * zone.assets.len();
        for f in &frags {
            for &(at, id) in &f.refs {
                patches.push((start + at, slot_of[&id]));
            }
            for &(at, target) in &f.inner {
                let t = start + target;
                let k = spans.partition_point(|s| s.0 <= t);
                let Some(&(s, l, a)) = k.checked_sub(1).map(|k| &spans[k]) else { bail!("{}: inner reference to {t:#x} has no allocation", f.name) };
                if t - s >= l.max(1) {
                    bail!("{}: inner reference to {t:#x} is outside the allocation at {s:#x}", f.name);
                }
                patches.push((start + at, a + (t - s) as u32));
            }
            start += f.bytes.len();
        }
    }
    let mut data = std::mem::take(&mut zone.data);
    for (at, v) in patches {
        data[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    // pass 2: the block sizes are what the walk fills
    let zone = Zone::from_image(fastfile::VERSION_QA, data)?;
    let peaks = walk_all(&zone)?.block_positions();
    let mut sizes = [0u32; 8];
    for (b, s) in sizes.iter_mut().enumerate() {
        *s = peaks[b] as u32 + if b == 0 { 16 } else { 0 };
    }
    let mut data = zone.data;
    for (i, s) in sizes.iter().enumerate() {
        data[8 + 4 * i..12 + 4 * i].copy_from_slice(&s.to_le_bytes());
    }
    let done = Zone::from_image(fastfile::VERSION_QA, data)?;
    let r = walk_all(&done)?;
    if r.block_positions() != peaks {
        bail!("the block footprint changed after the block sizes were written");
    }
    if !r.dangling_references().is_empty() {
        bail!("{} references dangle, e.g. {:?}", r.dangling_references().len(), &r.dangling_references()[..r.dangling_references().len().min(3)]);
    }
    Ok(Converted { image: done.data, block_sizes: sizes, stats, nulled, assets: done.assets.len(), warnings: WARNINGS.with(|w| w.borrow().clone()) })
}

/// Opens a stock IW4 fastfile and converts it.
pub fn convert_file(path: &Path) -> Result<Converted> {
    let raw = std::fs::read(path).with_context(|| path.display().to_string())?;
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let stock = ZoneImage::open(Game::Iw4, &name, &raw)?;
    convert_image(&stock)
}

/// A container to seal with: the smallest QA zone of the game folder (any will do; the key and IV are reused).
pub fn template_container(root: &Path, key: &ArchiveKey) -> Result<Container> {
    let mut zones = super::export::zone_files(root);
    zones.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX));
    for p in zones {
        if let Ok(o) = fastfile::open(&p, key) {
            if o.version == fastfile::VERSION_QA {
                return Ok(o.container);
            }
        }
    }
    bail!("no QA 3.3.3.3 zone found in {} to take the container from", root.display())
}

/// Compares the plain content (raw files, string tables, localised strings) of the converted zone, as the QA walker reads it,
/// with what the stock walk found: returns (compared, mismatching).
pub fn compare_content(stock_image: &[u8], converted: &[u8]) -> Result<(usize, usize)> {
    use super::replay::DataRec;
    let stock = super::stock::probe_data(stock_image)?;
    let zone = Zone::from_image(fastfile::VERSION_QA, converted.to_vec())?;
    let mut r = walk_all(&zone)?;
    let conv = std::mem::take(&mut r.rec.data);
    let key = |d: &DataRec| -> Option<(String, Vec<u8>)> {
        Some(match d {
            DataRec::RawFile { name, compressed, length, bytes } => (format!("raw {} {compressed} {length}", name.clone().unwrap_or_default()), bytes.clone()),
            DataRec::StringTable { name, columns, rows, cells } => (format!("table {} {columns}x{rows}", name.clone().unwrap_or_default()), cells.iter().flat_map(|c| c.clone().unwrap_or_default().into_bytes().into_iter().chain([0])).collect()),
            DataRec::Localize { key, text } => (format!("loc {}", key.clone().unwrap_or_default()), text.clone().unwrap_or_default().into_bytes()),
            _ => return None,
        })
    };
    let a: Vec<_> = stock.iter().filter_map(key).collect();
    let b: Vec<_> = conv.iter().filter_map(key).collect();
    let bad = a.iter().zip(&b).filter(|(x, y)| x != y).count() + a.len().abs_diff(b.len());
    Ok((a.len(), bad))
}

pub fn print_report(c: &Converted) {
    println!("{} assets, blocks {:?}, {} references to left-out assets set to null", c.assets, c.block_sizes, c.nulled);
    for (w, n) in &c.warnings {
        println!("  warning: {w} (x{n})");
    }
    for (ty, st) in &c.stats {
        let out: usize = st.left_out.values().sum();
        print!("  type {ty:2}: {:5} converted, {out:5} left out", st.converted);
        for (why, n) in &st.left_out {
            print!("  [{n} x {why}]");
        }
        println!();
    }
}

/// `atlas --convertzone MW2_ZONE.ff GAME_FOLDER OUT.ff [--signed]`, and development aids (`check`, `dir`, `events`).
pub fn command(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("dir") => {
            let root = Path::new(args.get(1).context("game folder")?);
            let key = super::export::find_key(root)?;
            return qa_directory(Path::new(args.get(2).context("zone file")?), &key);
        }
        Some("qadump") => return super::convert_dev::qadump(&args[1..]),
        Some("stockmodel") => return super::convert_dev::stockmodel(&args[1..]),
        Some("qastats") => return super::convert_dev::qastats(&args[1..]),
        Some("qalodhash") => return super::convert_dev::qalodhash(&args[1..]),
        Some("qaraw") => return super::convert_dev::qaraw(&args[1..]),
        Some("cmp1") => return super::convert_dev::cmp1(&args[1..]),
        Some("qaclip") => return super::convert_dev::qaclip(&args[1..]),
        Some("impactmask") => return super::convert_dev::impactmask(&args[1..]),
        Some("fxsamples") => return super::convert_dev::fxsamples(&args[1..]),
        Some("cmp") => return super::convert_dev::cmp(&args[1..]),
        Some("stocksurfs") => return super::convert_dev::stocksurfs(&args[1..]),
        Some("qamodelrefs") => return super::convert_dev::qamodelrefs(&args[1..]),
        Some("qapreset") => return super::convert_dev::qapreset(&args[1..]),
        Some("events") => return stock_events(Path::new(args.get(1).context("zone")?)),
        Some("oracle") => return oracle(Path::new(args.get(1).context("stock zone")?), Path::new(args.get(2).context("qa headers tsv")?), args.get(3).context("kind")?),
        Some("hdr") => return stock_headers(Path::new(args.get(1).context("zone")?), args.get(2).context("structure name")?),
        Some("stockdir") => return stock_directory(Path::new(args.get(1).context("zone")?)),
        Some("check") => {
            // convert only: report what converts and whether the QA walker accepts the result
            let path = Path::new(args.get(1).context("stock zone")?);
            let c = convert_file(path)?;
            print_report(&c);
            let (_, stock_image) = super::stock::open(&std::fs::read(path)?)?;
            let sb: Vec<u32> = (0..8).map(|i| u32_at(&stock_image, 8 + 4 * i)).collect();
            println!("  stock blocks {sb:?}");
            let (n, bad) = compare_content(&stock_image, &c.image)?;
            println!("  content (raw files, string tables, localised strings): {n} compared, {bad} differ");
            // what the QA walker's recorder makes of the world
            let zone = Zone::from_image(fastfile::VERSION_QA, c.image.clone())?;
            let r = walk_all(&zone)?;
            if let Some(w) = &r.rec.world {
                let named = w.surfaces.iter().filter(|(_, n)| n.is_some()).count();
                let models = w.static_models.iter().filter(|(_, n)| n.is_some()).count();
                println!("  world {:?}: {} surfaces ({named} with a material name), {} static models ({models} named), {} vertices, {} indices", w.name, w.surfaces.len(), w.static_models.len(), w.vertices.1, w.indices.1);
            }
            println!("  {} models recorded, {} materials", r.rec.models.len(), r.rec.materials.len());
            return Ok(());
        }
        _ => {}
    }
    let (Some(input), Some(root), Some(out)) = (args.first(), args.get(1), args.get(2)) else {
        bail!("usage: atlas --convertzone MW2_ZONE.ff GAME_FOLDER OUT.ff [--signed]");
    };
    let root = Path::new(root);
    let key = super::export::find_key(root)?;
    let c = convert_file(Path::new(input))?;
    print_report(&c);
    let mut container = template_container(root, &key)?;
    container.signed = args.iter().any(|a| a == "--signed") && container.signed;
    let sealed = fastfile::seal(&c.image, fastfile::VERSION_QA, &container)?;
    std::fs::write(out, &sealed)?;
    println!("{out}: {} bytes", sealed.len());
    Ok(())
}

/// Development aid: the directory of a QA zone (type, reference word, extent), with a histogram of the reference words.
pub fn qa_directory(path: &Path, key: &ArchiveKey) -> Result<()> {
    let z = Zone::open(path, key)?;
    let mut hist: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for (i, a) in z.assets.iter().enumerate() {
        let r = if a.reference >= 0xffff_fffe { a.reference } else { 1 };
        *hist.entry((a.asset_type, r)).or_default() += 1;
        if i < 40 {
            println!("#{i} type {} ref {:#x} extent {}", a.asset_type, a.reference, a.extent);
        }
    }
    println!("blocks {:?}", z.block_sizes);
    for ((t, r), n) in hist {
        println!("type {t:2} ref {} x{n}", if r == 1 { "address".to_string() } else { format!("{r:#x}") });
    }
    Ok(())
}

/// Development aid: the asset list of a stock zone (type, header pointer) as a histogram.
pub fn stock_directory(path: &Path) -> Result<()> {
    let (_, image) = super::stock::open(&std::fs::read(path)?)?;
    let p = super::stock::probe(&image, 334)?;
    let mut hist: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for &(t, r) in &p.list {
        *hist.entry((t, if r >= 0xffff_fffe { r } else { 1 })).or_default() += 1;
    }
    for ((t, r), n) in hist {
        println!("type {t:2} ref {} x{n}", if r == 1 { "address".to_string() } else { format!("{r:#x}") });
    }
    Ok(())
}

/// Development aid: what the stock walker's hook reports for a zone (events per structure, nested ones counted apart).
pub fn stock_events(path: &Path) -> Result<()> {
    let raw = std::fs::read(path)?;
    let zone = ZoneImage::open(Game::Iw4, "x", &raw)?;
    let mut events: Vec<(String, u32, usize)> = Vec::new();
    {
        let mut hook = |v: &View, name: &str, _hdr: u32, id: u32| events.push((name.to_string(), id, v.top_index()));
        let r = walk_with(Game::Iw4, &zone, Some(&mut hook));
        println!("walk exact {} assets {}/{}", r.exact, r.assets_walked, r.assets_total);
    }
    let mut per: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (i, (n, _, top)) in events.iter().enumerate() {
        let last = events.get(i + 1).map(|e| e.2 != *top).unwrap_or(true);
        let e = per.entry(n.clone()).or_default();
        if last { e.0 += 1 } else { e.1 += 1 }
    }
    for (n, (top, nested)) in per {
        println!("{n:32} top {top:5} nested {nested:5}");
    }
    Ok(())
}

/// Development aid: `name<TAB>hex` of the header of every stock asset of structure `want` (for pairing with QA headers).
pub fn stock_headers(path: &Path, want: &str) -> Result<()> {
    let raw = std::fs::read(path)?;
    let zone = ZoneImage::open(Game::Iw4, "x", &raw)?;
    let mut hook = |v: &View, name: &str, hdr: u32, _id: u32| {
        if name != want {
            return;
        }
        let size = v.struct_size(name).unwrap_or(0) as usize;
        let s = Src { v, hdr };
        let n = s.cstr(if name == "GfxImage" { 28 } else { 0 }).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let hex: String = v.bytes(hdr, size).unwrap_or(&[]).iter().map(|b| format!("{b:02x}")).collect();
        let extra = if name == "snd_alias_list_t" {
            let n = s.u32(8) as usize;
            s.array(4, n, 100).iter().map(|b| format!("{b:02x}")).collect::<String>()
        } else if name == "GfxImage" && s.has(0) { v.bytes(s.u32(0), 16).unwrap_or(&[]).iter().map(|b| format!("{b:02x}")).collect::<String>() } else { String::new() };
        println!("{n}	{hex}	{extra}");
    };
    walk_with(Game::Iw4, &zone, Some(&mut hook));
    Ok(())
}

/// Development aid: converts a stock zone and compares the headers of the converted assets of `kind` (material, techset, image, ...)
/// with the headers of the assets of the same name in real QA zones (`atlas --dumpheaders` output): per header word how often it is equal.
pub fn oracle(stock_zone: &Path, qa_tsv: &Path, kind: &str) -> Result<()> {
    use super::replay::DataRec;
    let c = convert_file(stock_zone)?;
    let zone = Zone::from_image(fastfile::VERSION_QA, c.image)?;
    let mut r = walk_all(&zone)?;
    let mut qa: HashMap<String, Vec<u8>> = HashMap::new();
    for l in std::fs::read_to_string(qa_tsv)?.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() >= 4 && f[0] == kind && !f[1].is_empty() {
            let bytes: Vec<u8> = (0..f[3].len() / 2).map(|i| u8::from_str_radix(&f[3][2 * i..2 * i + 2], 16).unwrap_or(0)).collect();
            qa.entry(f[1].to_ascii_lowercase()).or_insert(bytes);
        }
    }
    let norm = |w: u32| if w == 0 { 0 } else if w >= 0x1000_0000 { 0xAAAA_AAAA } else { w };
    let mut per_word: Vec<(usize, usize, HashMap<(u32, u32), usize>)> = Vec::new();
    let mut pairs = 0usize;
    for d in std::mem::take(&mut r.rec.data) {
        let DataRec::Header { kind: k, name: Some(name), bytes } = d else { continue };
        if k != kind {
            continue;
        }
        let Some(q) = qa.get(&name.to_ascii_lowercase()) else { continue };
        if q.len() != bytes.len() {
            continue;
        }
        pairs += 1;
        if per_word.len() < bytes.len() / 4 {
            per_word.resize_with(bytes.len() / 4, || (0, 0, HashMap::new()));
        }
        for i in 0..bytes.len() / 4 {
            let (a, b) = (norm(u32_at(&bytes, 4 * i)), norm(u32_at(q, 4 * i)));
            let e = &mut per_word[i];
            if a == b {
                e.0 += 1;
            } else {
                e.1 += 1;
                *e.2.entry((a, b)).or_default() += 1;
            }
        }
    }
    println!("{pairs} {kind} headers paired by name with QA");
    for (i, (eq, ne, m)) in per_word.iter().enumerate() {
        if *ne == 0 {
            continue;
        }
        let mut top: Vec<_> = m.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1));
        let shown: Vec<String> = top.iter().take(3).map(|((a, b), n)| format!("{a:08x}->{b:08x} x{n}")).collect();
        println!("  +{:3}: {eq} equal, {ne} differ   (mine -> QA) {}", 4 * i, shown.join("  "));
    }
    Ok(())
}
