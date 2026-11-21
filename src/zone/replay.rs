// SPDX-License-Identifier: GPL-3.0-only (ported by its copyright holder from his own server code, codol-qa-server)
//! Walks a zone's asset stream the way the game's loader does, tracking its eight memory blocks.
//!
//! A port of the server project's `codol_load_blocks.c` (QA 3.3.3.3 layouts, fastfile version 423). The stream
//! carries pointers that mean "inline asset follows" (`-1`), "inline asset follows and register a pointer slot"
//! (`-2`) or "an asset loaded earlier" (a block-3 address). To resolve the last kind the walk must reproduce the
//! loader's block-3 allocation exactly, so every asset is traversed in full even when only models matter.
//!
//! Block memory itself is not copied: positions are tracked, and the only memory ever read back (shared names)
//! is kept as a map from block-3 address to the string's place in the zone image.

use std::collections::HashMap;

pub const INLINE: u32 = u32::MAX;
pub const INSERT: u32 = u32::MAX - 1;

/// A stream word holding an address into earlier memory (a name or an asset reference), found while walking with tracing on.
#[derive(Debug, Clone, Copy)]
pub struct AliasWord {
    pub asset: usize,
    /// Offset of the word in the zone image.
    pub at: usize,
    pub value: u32,
}

#[derive(Debug, Clone)]
pub struct Fail {
    pub what: &'static str,
    pub at: usize,
}

pub type Res<T = ()> = Result<T, Fail>;

/// Asset families whose pointer slots can be referenced again later (the alias types of the C code).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Preset,
    PhysMap,
    Surfs,
    Model,
    Material,
    Shader,
    Declaration,
    TechSet,
    Image,
    SoundAlias,
    SoundCurve,
    LoadedSound,
    Effect,
    Tracer,
    Weapon,
    Menu,
    AddonMapEnts,
}

impl Kind {
    /// The zone asset type number used for alias bookkeeping.
    fn alias_type(self) -> Option<u32> {
        Some(match self {
            Kind::Preset => 0,
            Kind::PhysMap => 1,
            Kind::Surfs => 3,
            Kind::Model => 4,
            Kind::Material => 5,
            Kind::Image => 10,
            Kind::TechSet => 9,
            Kind::SoundAlias => 11,
            Kind::SoundCurve => 12,
            Kind::LoadedSound => 13,
            Kind::Effect => 30,
            Kind::Tracer => 40,
            Kind::Weapon => 28,
            _ => return None,
        })
    }
}

#[derive(Clone)]
pub struct TexRec {
    pub semantic: u8,
    pub sampler: u8,
    pub hash: u32,
    pub image_start: i64,
    pub image_ref: u32,
}

#[derive(Clone)]
pub struct MatRec {
    pub name: Option<String>,
    pub textures: Vec<TexRec>,
    /// The 104-byte QA material header as stored.
    pub header: Vec<u8>,
    pub techset_start: i64,
    pub techset_ref: u32,
    /// GfxStateBits entries (8 bytes each) of the material's state-bits table.
    pub state_bits: Vec<u8>,
    /// Material constants: 32 bytes each (name hash, 12-byte name, four floats).
    pub constants: Vec<u8>,
}

/// A model's skeleton as stored: bone names (script string indices), parents (as offsets back from the bone's own
/// index), local rotations (i16 x4) and positions (f32 x3) of the non-root bones, and every bone's model-space
/// `DObjAnimMat` (rotation x, y, z, w, position x, y, z, weight; 32 bytes).
#[derive(Clone, Default)]
pub struct SkelRec {
    pub names: Vec<u8>,
    pub parents: Vec<u8>,
    pub quats: Vec<u8>,
    pub trans: Vec<u8>,
    pub base: Vec<u8>,
}

/// One sub-track of an animation's delta (root motion) part, as stored: the 4-byte header (key count minus one, and for
/// translations the small-values flag), a constant value when it has one key, or otherwise the frames struct (mins and
/// size for translations), the frame numbers and the quantised values.
#[derive(Clone, Default)]
pub struct DeltaRec {
    pub header: Vec<u8>,
    pub constant: Vec<u8>,
    pub frames: Vec<u8>,
    pub indices: Vec<u8>,
    pub data: Vec<u8>,
}

/// An animation (`XAnimParts`, IW4 layout) as stored: the 88-byte header, bone names (script string indices), notifies,
/// and the flat data arrays its tracks are decoded from (see `anim.rs`).
#[derive(Clone, Default)]
pub struct AnimRec {
    pub name: Option<String>,
    pub header: Vec<u8>,
    pub names: Vec<u8>,
    pub notify: Vec<u8>,
    pub data_byte: Vec<u8>,
    pub data_short: Vec<u8>,
    pub data_int: Vec<u8>,
    pub random_short: Vec<u8>,
    pub random_byte: Vec<u8>,
    pub indices: Vec<u8>,
    /// The delta part's translation, 2D rotation and 3D rotation tracks.
    pub delta: [Option<DeltaRec>; 3],
}

#[derive(Clone)]
pub struct ModelRec {
    pub skel: Option<SkelRec>,
    pub name: Option<String>,
    pub bones: u8,
    pub roots: u8,
    pub surfs: u8,
    pub lod: [[u8; 56]; 4],
    pub lod_start: [i64; 4],
    pub lod_ref: [u32; 4],
    pub mat_start: Vec<i64>,
    pub mat_ref: Vec<u32>,
}

/// A map's drawable geometry as found in its world asset: where the vertices, indices and surface records sit in the zone
/// image, and the material each surface uses.
#[derive(Debug, Clone, Default)]
pub struct WorldRec {
    pub name: Option<String>,
    /// (offset in the image, count): 44-byte vertices and 16-bit indices.
    pub vertices: (usize, usize),
    pub indices: (usize, usize),
    /// Offset of each 24-byte surface record and its material's name.
    pub surfaces: Vec<(usize, Option<String>)>,
    /// The static models placed in the map: (offset of the 76-byte record in the image, model name).
    pub static_models: Vec<(usize, Option<String>)>,
    /// Offset of the 36-byte per-model records that follow the draw records (bounds first: mins, maxs), one per static model.
    pub static_insts: usize,
}

/// Where the arrays of a QA / 3.19 collision map's first `ClipInfo` sit in the zone image, as the walk met them. Offsets are stream
/// offsets. Pointer words that refer to earlier data (a brush side's plane) are followed with `Recorder::addrs`.
#[derive(Debug, Clone, Default)]
pub struct ClipRec {
    /// Whose collision info this is: 0 the map's own, 1 the second one behind a pointer, 100 + i the one of sub model i.
    pub tag: u32,
    /// Offset of the 256-byte clipMap_t header in the image.
    pub header: usize,
    /// (offset, count) of the 20-byte planes, 12-byte materials, 8-byte brush sides, 36-byte brushes.
    pub planes: (usize, usize),
    pub materials: (usize, usize),
    pub sides: (usize, usize),
    pub brushes: (usize, usize),
    /// (offset, count) of the 20-byte leaf-brush nodes and the 2-byte leaf brush indices.
    pub lb_nodes: (usize, usize),
    pub leaf_brushes: (usize, usize),
    /// Stream offset of the inline brush index array of each leaf-brush node (0 when the node has none or points elsewhere).
    pub lb_inline: Vec<usize>,
    /// Offsets of the 24-byte brush bounds and the 4-byte brush contents (one per brush); 0 when absent.
    pub bounds: usize,
    pub contents: usize,
    /// Material names, in order (`None` when the name is a reference).
    pub material_names: Vec<Option<String>>,
}

/// Everything the walk observes about models, materials and images.
/// Game data stored in zones that is plain content rather than geometry: what the data tables are made from.
#[derive(Debug, Clone)]
pub enum DataRec {
    /// A string table: columns x rows of cells, row-major.
    StringTable { name: Option<String>, columns: usize, rows: usize, cells: Vec<Option<String>> },
    /// A localised string: its key and text.
    Localize { key: Option<String>, text: Option<String> },
    /// A weapon definition: its internal name and the fixed-size record (CoDOL: one flat record; stock IW4: the 116-byte complete
    /// definition followed by the 1,668-byte weapon definition).
    Weapon { name: Option<String>, display: Option<String>, record: Vec<u8> },
    /// The fixed-size header of a model, material, technique set, image, surfaces or physics preset (for comparing layouts
    /// between games).
    Header { kind: &'static str, name: Option<String>, bytes: Vec<u8> },
    /// A raw file (scripts, configs, menus). `compressed` says the bytes are a zlib stream of `length` bytes.
    RawFile { name: Option<String>, compressed: bool, length: usize, bytes: Vec<u8> },
}

#[derive(Default)]
pub struct Recorder {
    /// The map's geometry, when the zone has a world.
    pub world: Option<WorldRec>,
    /// The collision map's brushes, planes and materials (QA / 3.19 layout only): one record per `ClipInfo` the walk met.
    pub clips: Vec<ClipRec>,
    /// (stream offset of the header, of the text, length including the NUL) of every entity string met: the map's own is nested in
    /// its collision map.
    pub map_ents: Vec<(usize, usize, usize)>,
    /// (offset, count) of the 72-byte `cmodel_t` records of the collision map (model 0 is the world, `*N` entities use model N).
    pub cmodels: (usize, usize),
    /// (offset, count) of the collision map's 76-byte static model records (xmodel, origin, inverse scaled axis, absolute bounds).
    pub clip_statics: (usize, usize),
    /// (offset, count) of the collision map's 40-byte `cLeaf_t` records; `leafBrushNode` (at +36) roots a leaf-brush tree.
    pub clip_leafs: (usize, usize),
    /// With `Replay::record_addresses(true)`: (block address, stream offset, length) of every array the walk read, in order.
    pub addrs: Vec<(u32, usize, usize)>,
    /// The map's lights: the Com world's primary lights and the GfxWorld's hero lights.
    pub lights: super::world::MapLights,
    /// String tables, localised strings and raw files, in zone order.
    pub data: Vec<DataRec>,
    pub images: HashMap<usize, Option<String>>,
    pub surfs: HashMap<usize, Option<String>>,
    pub techsets: HashMap<usize, Option<String>>,
    /// Shader programs seen inline: (name, bytecode), vertex and pixel shaders alike.
    pub shaders: Vec<(Option<String>, Vec<u8>)>,
    pub materials: Vec<MatRec>,
    pub material_by_start: HashMap<usize, usize>,
    /// Image offset of a model's header -> its name (stock models are not recorded in `models`).
    pub model_names: HashMap<usize, Option<String>>,
    pub models: Vec<ModelRec>,
    pub anims: Vec<AnimRec>,
    /// (alias type, block-3 address) -> start of the inline asset in the zone image.
    pub aliases: HashMap<(u32, u32), usize>,
    g_image_start: i64,
    pub(super) g_last_material_start: i64,
    g_last_surfs_start: i64,
    g_last_techset_start: i64,
    cur_img_start: Vec<i64>,
    cur_img_ref: Vec<u32>,
    cur_mat_start: Vec<i64>,
    cur_mat_ref: Vec<u32>,
    cur_lod_start: [i64; 4],
    cur_lod_ref: [u32; 4],
}

pub struct Replay<'a> {
    src: &'a [u8],
    pub(super) pos: usize,
    end: usize,
    version: u32,
    cap: [usize; 8],
    position: [usize; 8],
    /// The highest each block has been filled (block 0 rewinds, so its peak is what the header records).
    peak: [usize; 8],
    active: usize,
    stack: Vec<(usize, usize)>,
    /// block-3 address of a string -> its offset in the zone image
    strings: HashMap<u32, usize>,
    pub rec: Recorder,
    /// false during the dry run that validates an asset before it is applied
    pub(super) apply: bool,
    /// block-3 address of the asset directory (entries are pointer slots too)
    pub directory_reference: u32,
    /// Bytes per entry of the asset list: 16 for the CoDOL directory (type, offset, reference, extent), 8 for the
    /// stock IW4 one (type, header pointer).
    directory_stride: usize,
    /// Walking a stock IW4 zone (a few layouts follow IW4 rather than CoDOL).
    pub(super) stock: bool,
    /// 3.19 world header: (start, end) of the QA-sized header; reads at or after +0x34 land 4 bytes later (a sort key was added there).
    pub(super) world_header: Option<(usize, usize)>,
    /// Stream offsets of -2 ("reusable") pointer words read in a stock zone.
    pub insert_words: std::cell::RefCell<std::collections::HashSet<usize>>,
    pending_slots: std::cell::Cell<u32>,
    /// References to earlier memory (names, assets) that did not resolve: (stream offset, address). A zone that was
    /// assembled correctly has none; the writer relies on this to catch addresses it did not relocate.
    pub(super) dangling: Vec<(usize, u32)>,
    /// Tracing (for the zone writer): the last words read, every address-bearing word found, and the block-3 allocations
    /// in order as (asset index, address, length).
    tracing: bool,
    trace: std::cell::RefCell<Vec<(usize, u32)>>,
    alias_words: std::cell::RefCell<Vec<AliasWord>>,
    allocs3: Vec<(usize, u32, usize)>,
    /// For each entry of `allocs3`: the stream offset the allocation started at and whether it consumed stream bytes.
    allocs3_info: Vec<(usize, bool)>,
    cur_asset: usize,
    /// While true, `clip_note` records into the collision-map record.
    pub(super) clip_on: bool,
    record_addrs: bool,
    pub(super) clip_tag: u32,
    pub(super) clip_header: usize,
}

pub(super) fn align4(v: usize) -> usize {
    align_up(v, 4)
}

fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

impl<'a> Replay<'a> {
    pub fn new(zone: &'a super::Zone) -> Res<Replay<'a>> {
        Replay::with_stride(zone, 16)
    }

    /// For a stock IW4 zone (asset list of 8-byte entries, no extents; see `walk_stream_asset`).
    pub fn new_stock(zone: &'a super::Zone) -> Res<Replay<'a>> {
        let mut r = Replay::with_stride(zone, 8)?;
        r.stock = true;
        Ok(r)
    }

    fn with_stride(zone: &'a super::Zone, stride: usize) -> Res<Replay<'a>> {
        let mut cap = [0usize; 8];
        for (i, c) in cap.iter_mut().enumerate() {
            let b = zone.block_sizes[i] as usize;
            if b > 0x1000_0000 {
                return Err(Fail { what: "block size too large", at: 0 });
            }
            *c = (b + 4095) & !4095;
        }
        let mut r = Replay {
            src: &zone.data,
            pos: 56,
            end: zone.data.len(),
            version: zone.version,
            cap,
            position: [0; 8],
            peak: [0; 8],
            active: 0,
            stack: Vec::new(),
            strings: HashMap::new(),
            apply: true,
            rec: Recorder { g_image_start: -1, g_last_material_start: -1, g_last_surfs_start: -1, g_last_techset_start: -1, ..Default::default() },
            directory_reference: 0,
            directory_stride: 16,
            stock: false,
            world_header: None,
            insert_words: Default::default(),
            pending_slots: Default::default(),
            dangling: Vec::new(),
            tracing: false,
            trace: Default::default(),
            alias_words: Default::default(),
            allocs3: Vec::new(),
            allocs3_info: Vec::new(),
            cur_asset: usize::MAX,
            clip_on: false,
            record_addrs: false,
            clip_tag: 0,
            clip_header: 0,
        };
        r.rec.cur_img_start = vec![-1; 256];
        r.rec.cur_img_ref = vec![0; 256];
        r.rec.cur_mat_start = vec![-1; 256];
        r.rec.cur_mat_ref = vec![0; 256];
        r.rec.cur_lod_start = [-1; 4];
        // BeginZoneReplay: block 3 is active; the script-string pool then the directory are read into it.
        r.push(3)?;
        if super::u32_at(&zone.data, 44) != 0 {
            let strings = r.bytes(zone.string_count as u32, 4, 4)?;
            for i in 0..zone.string_count {
                if r.ptr(strings + 4 * i) == INLINE {
                    r.string()?;
                }
            }
        } else if zone.string_count != 0 {
            return Err(r.fail("script strings without a pool"));
        }
        if r.pos != zone.table_offset {
            return Err(r.fail("string pool does not end at the directory"));
        }
        if super::u32_at(&zone.data, 52) != 0 {
            r.directory_reference = 0x3000_0001 + align_up(r.position[3], 4) as u32;
            r.directory_stride = stride;
            r.bytes(zone.assets.len() as u32, stride, 4)?;
        } else if !zone.assets.is_empty() {
            return Err(r.fail("assets without a directory"));
        }
        Ok(r)
    }

    /// How far each of the eight memory blocks has been filled by the walk so far.
    pub fn block_positions(&self) -> [usize; 8] {
        self.peak
    }

    // ---- low-level helpers -------------------------------------------------------------------------------

    pub(super) fn fail(&self, what: &'static str) -> Fail {
        Fail { what, at: self.pos }
    }

    /// Where a QA-layout header offset really is (the 3.19 world header has one more word at +0x34).
    pub(super) fn adjust(&self, o: usize) -> usize {
        match self.world_header {
            Some((s, e)) if o >= s + 0x34 && o < e => o + 4,
            _ => o,
        }
    }

    pub(super) fn rd32(&self, o: usize) -> u32 {
        let o = self.adjust(o);
        let mut v = super::u32_at(self.src, o);
        // stock IW4 zones mark data that later fields may point back at ("reusable" pointers) with -2 where CoDOL zones
        // use -1; for the walk both just mean the data follows
        if self.stock && v == INSERT {
            v = INLINE;
            // the loader reserves a 4-byte slot in block 3 for every reusable pointer it meets; it is allocated just before the
            // next allocation (the data the pointer announces)
            if self.apply && self.insert_words.borrow_mut().insert(o) {
                self.pending_slots.set(self.pending_slots.get() + 1);
            }
        }
        if self.tracing {
            let mut t = self.trace.borrow_mut();
            t.push((o, v));
            if t.len() > 64 {
                t.remove(0);
            }
        }
        v
    }

    /// Reads a pointer word. A value that is neither null nor a "data follows" marker is the block-3 address of something read
    /// earlier (a shared name, technique, plane array ...): with tracing on it is recorded, so the writer moves it with its target.
    pub(super) fn ptr(&self, o: usize) -> u32 {
        let v = self.rd32(o);
        if self.tracing && self.apply && self.is_block_three_address(v) {
            self.alias_words.borrow_mut().push(AliasWord { asset: self.cur_asset, at: self.adjust(o), value: v });
        }
        v
    }

    /// Whether a word can be the address of something already placed in block 3 (addresses are offset + 1). Floats between
    /// 4.7e-10 and 2 share the top nibble, but their "offset" lies far beyond anything a zone has placed.
    fn is_block_three_address(&self, v: u32) -> bool {
        v >> 28 == 3 && (v & 0x0fff_ffff) >= 1 && ((v & 0x0fff_ffff) as usize - 1) < self.position[3]
    }

    /// Records a pointer value that was read with `rd32` just before (see `ptr`).
    pub(super) fn note_pointer(&self, value: u32) {
        if self.is_block_three_address(value) {
            self.note_alias(value);
        }
    }

    /// Starts recording address-bearing words and block-3 allocations (call before replaying assets).
    pub fn enable_tracing(&mut self) {
        self.tracing = true;
    }

    pub fn alias_words(&self) -> Vec<AliasWord> {
        let mut seen = std::collections::HashSet::new();
        self.alias_words.borrow().iter().filter(|w| seen.insert(w.at)).cloned().collect()
    }

    /// Block-3 allocations in order: (asset index, address as the stream stores references to it, length).
    pub fn block3_allocations(&self) -> &[(usize, u32, usize)] {
        &self.allocs3
    }

    /// For each block-3 allocation (same order as `block3_allocations`): the stream offset it started at and whether it read
    /// stream bytes (a 4-byte pointer slot reserved for an inserted asset does not).
    pub fn block3_allocation_info(&self) -> &[(usize, bool)] {
        &self.allocs3_info
    }

    /// Notes that the word just read with this value is an address into earlier memory.
    fn note_alias(&self, value: u32) {
        if !self.tracing || !self.apply {
            return;
        }
        let at = self.trace.borrow().iter().rev().find(|(_, v)| *v == value).map(|(o, _)| *o);
        if let Some(at) = at {
            self.alias_words.borrow_mut().push(AliasWord { asset: self.cur_asset, at, value });
        }
    }

    /// Bytes of the current asset not yet consumed.
    pub(super) fn remaining(&self) -> usize {
        self.end.saturating_sub(self.pos)
    }

    pub(super) fn rd16(&self, o: usize) -> u32 {
        super::u16_at(self.src, self.adjust(o))
    }

    pub(super) fn rd8(&self, o: usize) -> u32 {
        self.src[self.adjust(o)] as u32
    }

    /// The QA layouts (as opposed to the older 1.1.1.13 ones). Version 461 shares them except where `v319` says otherwise.
    pub(super) fn qa(&self) -> bool {
        matches!(self.version, super::fastfile::VERSION_QA | super::fastfile::VERSION_319)
    }

    /// Version 461 (the 3.19 client): the QA layouts with one more technique slot in techsets (212 bytes) and one more
    /// state-bits entry in materials (the stored header keeps its 104 bytes: the counts after the entries sit one byte later).
    /// Layouts that are IW4's own: stock zones, and the 1.1.1.13 client (version 334), whose structures were still IW4-sized.
    pub(super) fn iw4_sizes(&self) -> bool {
        self.stock || self.version == super::fastfile::VERSION_C1
    }

    pub(super) fn v319(&self) -> bool {
        self.version == super::fastfile::VERSION_319
    }

    pub(super) fn push(&mut self, block: usize) -> Res {
        if block >= 8 || self.stack.len() >= 64 {
            return Err(self.fail("block stack"));
        }
        self.stack.push((self.active, self.position[block]));
        self.active = block;
        Ok(())
    }

    pub(super) fn pop(&mut self) -> Res {
        let Some((prev, entered)) = self.stack.pop() else { return Err(self.fail("block stack underflow")) };
        if self.active == 0 {
            self.position[0] = entered;
        }
        self.active = prev;
        Ok(())
    }

    /// Reserves `length` bytes (aligned) in `block`; with `read` the bytes also come out of the stream.
    pub(super) fn reserve(&mut self, block: usize, length: usize, alignment: usize, read: bool) -> Res<u32> {
        // slots for the reusable pointers read since the last allocation (stock zones)
        for _ in 0..self.pending_slots.replace(0) {
            let p = align_up(self.position[3], 4);
            self.position[3] = p + 4;
            self.peak[3] = self.peak[3].max(p + 4);
        }
        if block >= 8 || self.position[block] > self.cap[block] {
            return Err(self.fail("block position"));
        }
        let p = align_up(self.position[block], alignment);
        if p >= 0x1000_0000 || p > self.cap[block] || length > self.cap[block] - p {
            return Err(self.fail("block capacity exceeded"));
        }
        if read && block != 2 && (self.pos > self.end || length > self.end - self.pos) {
            return Err(self.fail("read past the asset"));
        }
        let started_at = self.pos;
        if read && block != 2 {
            self.pos += length;
        }
        self.position[block] = p + length;
        self.peak[block] = self.peak[block].max(p + length);
        let address = ((block as u32) << 28) + p as u32 + 1;
        if block == 3 && self.tracing && self.apply {
            self.allocs3.push((self.cur_asset, address, length));
            self.allocs3_info.push((started_at, read));
        }
        Ok(address)
    }

    /// Consumes `count * stride` bytes into the active block; returns where they start in the image.
    /// Makes the walk note the block address and stream offset of every array it reads (`rec.addrs`), so a pointer word can be
    /// turned into an image offset afterwards. Off by default: it costs memory.
    pub fn record_addresses(&mut self, on: bool) {
        self.record_addrs = on;
    }

    pub(super) fn bytes(&mut self, count: u32, stride: usize, alignment: usize) -> Res<usize> {
        if self.pos > self.end || count as usize > (self.end - self.pos) / stride {
            return Err(self.fail("array runs past the asset"));
        }
        let start = self.pos;
        let a = self.active;
        let address = self.reserve(a, count as usize * stride, alignment, true)?;
        if self.record_addrs && self.apply {
            self.rec.addrs.push((address, start, count as usize * stride));
        }
        Ok(start)
    }

    pub(super) fn string(&mut self) -> Res {
        let Some(n) = self.src[self.pos..self.end].iter().position(|&c| c == 0) else { return Err(self.fail("unterminated string")) };
        let start = self.pos;
        let a = self.active;
        let r = self.reserve(a, n + 1, 1, true)?;
        if a == 3 && self.apply {
            self.strings.insert(r, start);
        }
        Ok(())
    }

    pub(super) fn cstr(&self, off: usize) -> Option<String> {
        let n = self.src[off..].iter().position(|&c| c == 0)?;
        let bytes = &self.src[off..off + n];
        // the Chinese builds store their text in GBK
        Some(match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => encoding_rs::GBK.decode(bytes).0.into_owned(),
        })
    }

    /// A name that is inline (`-1`, at `inline_at`) or a reference to a string written earlier.
    pub(super) fn str_of(&self, reference: u32, inline_at: usize) -> Option<String> {
        if reference == INLINE {
            self.cstr(inline_at)
        } else if reference != 0 {
            self.note_alias(reference);
            self.strings.get(&reference).and_then(|&o| self.cstr(o))
        } else {
            None
        }
    }

    /// The script string at a block-3 address (the pool is read when the replay is created).
    pub fn pool_string(&self, address: u32) -> Option<String> {
        self.strings.get(&address).and_then(|&o| self.cstr(o))
    }

    /// References to earlier memory that did not resolve while walking (see the field).
    pub fn dangling_references(&self) -> &[(usize, u32)] {
        &self.dangling
    }

    fn block_three_reference(&self) -> u32 {
        0x3000_0001 + align_up(self.position[3], 4) as u32
    }

    // ---- alias handling ----------------------------------------------------------------------------------

    pub(super) fn record_alias(&mut self, ty: u32, reference: u32, start: usize) {
        self.record_asset(ty, reference, start);
    }

    fn record_asset(&mut self, ty: u32, reference: u32, start: usize) {
        if reference != 0 && self.apply {
            self.rec.aliases.insert((ty, reference), start);
        }
    }

    /// `field` is the block-3 address of the word holding the reference (0 for temporary holders).
    fn field_asset_reference(&mut self, reference: u32, field: u32, kind: Kind) -> Res {
        // a reference to an asset loaded earlier must be one that was registered
        if reference != 0 && reference < INSERT {
            self.note_alias(reference);
        }
        if reference != 0 && reference < INSERT && self.apply {
            if let Some(t) = kind.alias_type() {
                if !self.rec.aliases.contains_key(&(t, reference)) {
                    self.dangling.push((self.pos, reference));
                }
            }
        }
        let mut alias = if reference == INLINE { field } else { 0 };
        let start = self.pos;
        self.push(0)?;
        if reference >= INSERT {
            if reference == INSERT {
                alias = self.reserve(3, 4, 4, false)?;
            }
            self.load(kind)?;
            if let Some(t) = kind.alias_type() {
                self.record_asset(t, alias, start);
            }
        }
        self.pop()
    }

    pub(super) fn asset_reference(&mut self, reference: u32, kind: Kind) -> Res {
        self.field_asset_reference(reference, 0, kind)
    }

    fn load(&mut self, kind: Kind) -> Res {
        match kind {
            Kind::Preset => self.physics(false),
            Kind::PhysMap => self.physics(true),
            Kind::Surfs => self.surfaces(),
            Kind::Model => self.model(),
            Kind::Material => self.material(),
            Kind::Shader => self.shader(false),
            Kind::Declaration => self.shader(true),
            Kind::TechSet => self.technique_set(),
            Kind::Image => self.image(),
            Kind::SoundAlias => self.sound_alias(),
            Kind::SoundCurve => self.sound_curve(),
            Kind::LoadedSound => self.loaded_sound(),
            Kind::Effect => self.effect(),
            Kind::Tracer => self.tracer(),
            Kind::Weapon => self.weapon(),
            Kind::Menu => self.menu(),
            Kind::AddonMapEnts => self.addon_map_entities(),
        }
    }

    // ---- walkers -----------------------------------------------------------------------------------------

    fn physics(&mut self, map: bool) -> Res {
        let h = self.pos;
        self.reserve(0, if map { 72 } else if self.stock { 44 } else { 68 }, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        if !map {
            if self.ptr(h + 28) == INLINE {
                self.string()?;
            }
        } else if self.rd32(h + 8) != 0 {
            let count = self.rd32(h + 4);
            let geoms = self.bytes(count, 68, 4)?;
            for i in 0..count as usize {
                if self.rd32(geoms + 68 * i) != 0 {
                    let brush = self.bytes(1, 68, 4)?;
                    let n = self.rd16(brush + 24);
                    if self.rd32(brush + 28) != 0 {
                        let sides = self.bytes(n, 8, 4)?;
                        for j in 0..n as usize {
                            let plane = self.rd32(sides + 8 * j);
                            if plane == INLINE {
                                self.bytes(1, 20, 4)?;
                            } else if plane != 0 && plane != INSERT {
                                // the address of a plane defined earlier: it moves with its target
                                self.note_alias(plane);
                            }
                        }
                    }
                    if self.rd32(brush + 32) != 0 {
                        let len = self.rd32(brush + 60);
                        self.bytes(len, 1, 1)?;
                    }
                    let planes = self.rd32(brush + 64);
                    if planes == INLINE {
                        self.bytes(n, 20, 4)?;
                    } else if planes != 0 && planes != INSERT {
                        // the brush's plane array given as the address of planes read before (the sides' own)
                        self.note_alias(planes);
                    }
                }
            }
        }
        self.pop()
    }

    fn surfaces(&mut self) -> Res {
        let h = self.pos;
        // stock IW4: 36-byte header and 64-byte surfaces (CoDOL widens the bone part bits and adds a few fields)
        let (hsize, stride, vc, tc, tri, bc, bp, vp, rc, rp) = if self.stock { (36usize, 64usize, 2usize, 4usize, 12usize, 16usize, 24usize, 28usize, 32usize, 36usize) } else { (48, 84, 4, 6, 16, 20, 28, 32, 40, 44) };
        self.reserve(0, hsize, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        // observation: the surfaces' own name
        if self.apply {
            let name = self.str_of(self.rd32(h), h + hsize);
            self.rec.g_last_surfs_start = h as i64;
            self.rec.data.push(DataRec::Header { kind: "surfaces", name: name.clone(), bytes: self.src[h..h + hsize].to_vec() });
            self.rec.surfs.insert(h, name);
        }
        if self.rd32(h + 4) != 0 {
            let count = self.rd16(h + 8);
            let surfaces = self.bytes(count, stride, 4)?;
            for i in 0..count as usize {
                let s = surfaces + stride * i;
                if self.ptr(s + bp) == INLINE {
                    let mut blend = 0u32;
                    for j in 0..4u32 {
                        let n = self.rd16(s + bc + 2 * j as usize);
                        if n > 32767 {
                            return Err(self.fail("blend count"));
                        }
                        blend += n * (2 * j + 1);
                    }
                    self.bytes(blend, 2, 2)?;
                }
                self.push(6)?;
                if self.ptr(s + vp) == INLINE {
                    let n = self.rd16(s + vc);
                    self.bytes(n, 32, 16)?;
                }
                self.pop()?;
                if self.ptr(s + rp) == INLINE {
                    let n = self.rd32(s + rc);
                    let rigid = self.bytes(n, 12, 4)?;
                    for j in 0..n as usize {
                        if self.ptr(rigid + 12 * j + 8) == INLINE {
                            let tree = self.bytes(1, 40, 4)?;
                            if self.rd32(tree + 28) != 0 {
                                let n = self.rd32(tree + 24);
                                self.bytes(n, 16, 16)?;
                            }
                            if self.rd32(tree + 36) != 0 {
                                let n = self.rd32(tree + 32);
                                self.bytes(n, 2, 2)?;
                            }
                        }
                    }
                }
                self.push(7)?;
                if self.ptr(s + tri) == INLINE {
                    let n = self.rd16(s + tc);
                    self.bytes(n, 6, 16)?;
                }
                self.pop()?;
            }
        }
        self.pop()
    }

    fn image(&mut self) -> Res {
        let h = self.pos;
        // stock IW4: 32 bytes, name last; 1.1.1.13: 36; QA: 52
        let (size, name_at) = if self.stock { (32, 28) } else if self.qa() { (52, 48) } else { (36, 32) };
        self.reserve(0, size, 4, true)?;
        self.push(3)?;
        if self.ptr(h + name_at) == INLINE {
            self.string()?;
        }
        // observation
        if self.apply {
            let name = self.str_of(self.rd32(h + name_at), h + size);
            self.rec.g_image_start = h as i64;
            self.rec.data.push(DataRec::Header { kind: "image", name: name.clone(), bytes: self.src[h..h + size].to_vec() });
            self.rec.images.insert(h, name);
        }
        if self.ptr(h) == INLINE {
            // stock IW4: the texture's load definition and data are temporary (block 0)
            if self.stock {
                self.push(0)?;
            }
            let definition = self.bytes(1, 16, 4)?;
            let n = self.rd32(definition + 12);
            self.bytes(n, 1, 1)?;
            if self.stock {
                self.pop()?;
            }
        }
        self.pop()
    }

    fn water(&mut self) -> Res {
        let h = self.pos;
        self.reserve(3, 68, 4, true)?;
        let (m, n) = (self.rd32(h + 12), self.rd32(h + 16));
        let cells = m as u64 * n as u64;
        if m > i32::MAX as u32 || n > i32::MAX as u32 || cells > u32::MAX as u64 {
            return Err(self.fail("water size"));
        }
        if self.rd32(h + 4) != 0 {
            self.bytes(cells as u32, 8, 4)?;
        }
        if self.rd32(h + 8) != 0 {
            self.bytes(cells as u32, 4, 4)?;
        }
        let image = self.rd32(h + 64);
        self.push(0)?;
        if image >= INSERT {
            if image == INSERT {
                self.reserve(3, 4, 4, false)?;
            }
            self.image()?;
        }
        self.pop()
    }

    fn shader(&mut self, declaration: bool) -> Res {
        let h = self.pos;
        self.reserve(0, if declaration { 100 } else { 16 }, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        if !declaration && self.rd32(h + 8) != 0 {
            let n = self.rd16(h + 12);
            let at = self.bytes(n, 4, 4)?;
            if self.apply {
                let name = self.str_of(self.rd32(h), h + 16);
                self.rec.shaders.push((name, self.src[at..at + 4 * n as usize].to_vec()));
            }
        }
        self.pop()
    }

    fn shader_reference(&mut self, reference: u32, declaration: bool) -> Res {
        self.note_pointer(reference);
        self.push(0)?;
        if reference >= INSERT {
            if reference == INSERT {
                self.reserve(3, 4, 4, false)?;
            }
            self.shader(declaration)?;
        }
        self.pop()
    }

    fn technique_set(&mut self) -> Res {
        let h = self.pos;
        let techniques = if self.qa() || self.v319() { 16 } else { 12 };
        let slots = if self.v319() { 49 } else { 48 };
        let size = if self.v319() { 212 } else if self.qa() { 208 } else { 204 };
        self.reserve(0, size, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        if self.apply {
            let name = self.str_of(self.rd32(h), h + size);
            self.rec.data.push(DataRec::Header { kind: "techset", name: name.clone(), bytes: self.src[h..h + size].to_vec() });
            self.rec.techsets.insert(h, name);
            self.rec.g_last_techset_start = h as i64;
        }
        for i in 0..slots {
            if self.ptr(h + techniques + 4 * i) == INLINE {
                let technique = self.bytes(1, 8, 4)?;
                let count = self.rd16(technique + 6);
                let passes = self.bytes(count, 20, 1)?;
                for j in 0..count as usize {
                    let pass = passes + 20 * j;
                    self.shader_reference(self.rd32(pass), true)?;
                    self.shader_reference(self.rd32(pass + 4), false)?;
                    self.shader_reference(self.rd32(pass + 8), false)?;
                    if self.rd32(pass + 16) != 0 {
                        let n = self.rd8(pass + 12) + self.rd8(pass + 13) + self.rd8(pass + 14);
                        let args = self.bytes(n, 8, 4)?;
                        for k in 0..n as usize {
                            let t = self.rd16(args + 8 * k);
                            if (t == 1 || t == 7) && self.ptr(args + 8 * k + 4) == INLINE {
                                self.bytes(1, 16, 4)?;
                            }
                        }
                    }
                }
                if self.ptr(technique) == INLINE {
                    self.string()?;
                }
            }
        }
        self.pop()
    }

    fn material(&mut self) -> Res {
        if !self.qa() && !self.v319() && !self.iw4_sizes() {
            return Err(self.fail("material layout of this fastfile version is not supported"));
        }
        let h = self.pos;
        // 3.19 has 49 state-bits entries before the counts, so the counts start one byte later; the stock IW4 material is
        // 96 bytes with the name first
        let (name, techset, arrays, counts, size) = if self.iw4_sizes() { (0usize, 80usize, 84usize, 72usize, 96usize) } else { (16usize, 88usize, 92usize, if self.v319() { 81usize } else { 80usize }, 104usize) };
        self.reserve(0, size, 4, true)?;
        self.push(3)?;
        if self.ptr(h + name) == INLINE {
            self.string()?;
        }
        let techset_ref = self.rd32(h + techset);
        self.rec.g_last_techset_start = -1;
        self.asset_reference(techset_ref, Kind::TechSet)?;
        let techset_start = if techset_ref >= INSERT { self.rec.g_last_techset_start } else { -1 };
        let mut textures = None;
        if self.ptr(h + arrays) == INLINE {
            let tex_base = align_up(self.position[3], 4);
            let n = self.rd8(h + counts);
            let t = self.bytes(n, 12, 4)?;
            textures = Some(t);
            for i in 0..n as usize {
                let texture = t + 12 * i;
                let r = self.ptr(texture + 8);
                if self.src[texture + 7] == 11 {
                    if r == INLINE {
                        self.water()?;
                    }
                } else {
                    self.rec.g_image_start = -1;
                    self.field_asset_reference(r, 0x3000_0001 + (tex_base + 12 * i + 8) as u32, Kind::Image)?;
                    let inline = r >= INSERT;
                    self.rec.cur_img_start[i] = if inline { self.rec.g_image_start } else { -1 };
                    self.rec.cur_img_ref[i] = r;
                }
            }
        }
        let mut constants = Vec::new();
        if self.ptr(h + arrays + 4) == INLINE {
            let n = self.rd8(h + counts + 1);
            let at = self.bytes(n, 32, 16)?;
            constants = self.src[at..at + 32 * n as usize].to_vec();
        }
        let mut state_bits = Vec::new();
        if self.ptr(h + arrays + 8) == INLINE {
            let n = self.rd8(h + counts + 2);
            let at = self.bytes(n, 8, 4)?;
            state_bits = self.src[at..at + 8 * n as usize].to_vec();
        }
        if !self.apply {
            return self.pop();
        }
        // observation
        let m_name = self.str_of(self.rd32(h + name), h + size);
        self.rec.data.push(DataRec::Header { kind: "material", name: m_name.clone(), bytes: self.src[h..h + size].to_vec() });
        let ntex = if textures.is_some() { self.rd8(h + counts) as usize } else { 0 };
        let mut tex = Vec::with_capacity(ntex);
        if let Some(t) = textures {
            for i in 0..ntex {
                let b = t + 12 * i;
                let water = self.src[b + 7] == 11;
                tex.push(TexRec {
                    hash: self.rd32(b),
                    sampler: self.src[b + 6],
                    semantic: self.src[b + 7],
                    image_start: if water { -1 } else { self.rec.cur_img_start[i] },
                    image_ref: if water { 0 } else { self.rec.cur_img_ref[i] },
                });
            }
        }
        let idx = self.rec.materials.len();
        let mut header = self.src[h..h + size].to_vec();
        if self.v319() {
            // the way the client's own converter does it: drop the new technique's entry (index 46, at +0x4E) so the
            // header has QA's layout, which the index reader knows
            header.copy_within(0x4F..0x58, 0x4E);
            header[0x57] = 0;
        }
        self.rec.materials.push(MatRec { name: m_name, textures: tex, header, techset_start, techset_ref, state_bits, constants });
        self.rec.g_last_material_start = h as i64;
        self.rec.material_by_start.insert(h, idx);
        self.pop()
    }

    fn model(&mut self) -> Res {
        const STRIDES: [usize; 6] = [2, 1, 8, 12, 1, 32];
        const ALIGNS: [usize; 6] = [2, 1, 2, 4, 1, 4];
        let h = self.pos;
        // the stock IW4 model is 56 bytes smaller: 8 fewer in front of the skeleton pointers (bone part bits) and 12 fewer
        // in each of the four LOD records (no name pointer, narrower part bits)
        let (size, shift, lod_stride) = if self.stock { (304usize, 0usize, 44usize) } else { (360, 8, 56) };
        self.reserve(0, size, 4, true)?;
        self.push(3)?;
        if self.apply {
            let name = self.str_of(self.rd32(h), h + size);
            self.rec.model_names.insert(h, name.clone());
            self.rec.data.push(DataRec::Header { kind: "model", name, bytes: self.src[h..h + size].to_vec() });
        }
        if self.src[h + 5] > self.src[h + 4] {
            return Err(self.fail("model root bones"));
        }
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        let mut skel_at = [None::<usize>; 6];
        for i in 0..6 {
            if self.ptr(h + 36 + shift + 4 * i) == INLINE {
                let (bones, roots) = (self.rd8(h + 4), self.rd8(h + 5));
                let count = if (1..=3).contains(&i) { bones - roots } else { bones };
                skel_at[i] = Some(self.bytes(count, STRIDES[i], ALIGNS[i])?);
            }
        }
        let n_surfs = self.rd8(h + 6) as usize;
        let mut mat_start = vec![-1i64; n_surfs];
        let mut mat_ref = vec![0u32; n_surfs];
        let has_materials = self.rd32(h + 60 + shift) != 0;
        if has_materials {
            let slots = 0x3000_0001 + align_up(self.position[3], 4) as u32;
            let materials = self.bytes(n_surfs as u32, 4, 4)?;
            for i in 0..n_surfs {
                let start = self.pos;
                let r = self.rd32(materials + 4 * i);
                self.asset_reference(r, Kind::Material)?;
                mat_start[i] = if r >= INSERT { self.rec.g_last_material_start } else { -1 };
                mat_ref[i] = r;
                if r >= INSERT {
                    self.record_asset(5, slots + 4 * i as u32, start);
                }
            }
        }
        for i in 0..4 {
            let lod = h + 64 + shift + lod_stride * i;
            let r = self.rd32(lod + 8);
            self.asset_reference(r, Kind::Surfs)?;
            self.rec.cur_lod_start[i] = if r >= INSERT { self.rec.g_last_surfs_start } else { -1 };
            self.rec.cur_lod_ref[i] = r;
            if !self.stock && self.ptr(lod + 12) == INLINE {
                self.string()?;
            }
        }
        let tail = if self.stock { 0 } else { 56 };
        if self.rd32(h + 0x12c - 56 + tail) != 0 {
            let count = self.rd32(h + 0x130 - 56 + tail);
            let surfaces = self.bytes(count, 44, 4)?;
            for i in 0..count as usize {
                let entries = self.ptr(surfaces + 44 * i);
                let inline = if self.qa() { entries == INLINE } else { entries != 0 };
                if inline {
                    let n = self.rd32(surfaces + 44 * i + 4);
                    self.bytes(n, 48, 4)?;
                }
            }
        }
        if self.rd32(h + 0x138 - 56 + tail) != 0 {
            let n = self.rd8(h + 4);
            self.bytes(n, 28, 4)?;
        }
        self.asset_reference(self.rd32(h + 0x160 - 56 + tail), Kind::Preset)?;
        self.asset_reference(self.rd32(h + 0x164 - 56 + tail), Kind::PhysMap)?;
        if !self.apply || self.stock {
            return self.pop();
        }
        // observation
        let name = self.str_of(self.rd32(h), h + 360);
        let mut lod = [[0u8; 56]; 4];
        for (i, l) in lod.iter_mut().enumerate() {
            l.copy_from_slice(&self.src[h + 72 + 56 * i..h + 72 + 56 * (i + 1)]);
        }
        let (bones, roots) = (self.src[h + 4] as usize, self.src[h + 5] as usize);
        let skel = match skel_at {
            [Some(n), Some(p), Some(q), Some(t), _, Some(b)] if bones > 0 => Some(SkelRec {
                names: self.src[n..n + 2 * bones].to_vec(),
                parents: self.src[p..p + bones - roots].to_vec(),
                quats: self.src[q..q + 8 * (bones - roots)].to_vec(),
                trans: self.src[t..t + 12 * (bones - roots)].to_vec(),
                base: self.src[b..b + 32 * bones].to_vec(),
            }),
            _ => None,
        };
        self.rec.models.push(ModelRec {
            skel,
            name,
            bones: self.src[h + 4],
            roots: self.src[h + 5],
            surfs: self.src[h + 6],
            lod,
            lod_start: self.rec.cur_lod_start,
            lod_ref: self.rec.cur_lod_ref,
            mat_start: if has_materials { mat_start } else { Vec::new() },
            mat_ref: if has_materials { mat_ref } else { Vec::new() },
        });
        self.pop()
    }

    fn animation(&mut self) -> Res {
        const POINTERS: [usize; 6] = [52, 56, 60, 64, 68, 72];
        const COUNTS: [usize; 6] = [4, 6, 8, 32, 10, 12];
        const WIDTHS: [usize; 6] = [1, 2, 4, 2, 1, 4];
        let h = self.pos;
        self.reserve(0, 88, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        let (mut names_at, mut notify_at) = (None, None);
        if self.rd32(h + 48) != 0 {
            let n = self.rd8(h + 26);
            names_at = Some(self.bytes(n, 2, 2)?);
        }
        if self.rd32(h + 80) != 0 {
            let n = self.rd8(h + 27);
            notify_at = Some(self.bytes(n, 8, 4)?);
        }
        let index_width = if self.rd16(h + 14) < 256 { 1 } else { 2 };
        let mut delta_rec: [Option<DeltaRec>; 3] = [None, None, None];
        if self.rd32(h + 84) != 0 {
            let delta = self.bytes(1, 12, 4)?;
            for i in 0..3usize {
                if self.rd32(delta + 4 * i) != 0 {
                    let track = self.bytes(1, 4, 4)?;
                    let count = self.rd16(track);
                    let mut rec = DeltaRec { header: self.src[track..track + 4].to_vec(), ..Default::default() };
                    if count == 0 {
                        let len = [12, 4, 8][i];
                        let at = self.bytes(1, len, 1)?;
                        rec.constant = self.src[at..at + len].to_vec();
                    } else {
                        let flen = if i == 0 { 28 } else { 4 };
                        let frames = self.bytes(1, flen, 1)?;
                        rec.frames = self.src[frames..frames + flen].to_vec();
                        let iat = self.bytes(count + 1, index_width, 1)?;
                        rec.indices = self.src[iat..iat + (count as usize + 1) * index_width as usize].to_vec();
                        if self.rd32(frames + if i == 0 { 24 } else { 0 }) != 0 {
                            let t2 = self.rd8(track + 2) != 0;
                            let stride = match i {
                                0 => {
                                    if t2 {
                                        3
                                    } else {
                                        6
                                    }
                                }
                                1 => 4,
                                _ => 8,
                            };
                            let alignment = if i == 0 && t2 { 1 } else { 4 };
                            let dat = self.bytes(count + 1, stride, alignment)?;
                            rec.data = self.src[dat..dat + (count as usize + 1) * stride].to_vec();
                        }
                    }
                    delta_rec[i] = Some(rec);
                }
            }
        }
        let mut arrays: [Option<(usize, usize)>; 6] = [None; 6];
        for i in 0..6 {
            if self.rd32(h + POINTERS[i]) != 0 {
                let count = if i == 3 { self.rd32(h + COUNTS[i]) } else { self.rd16(h + COUNTS[i]) };
                arrays[i] = Some((self.bytes(count, WIDTHS[i], WIDTHS[i])?, count as usize * WIDTHS[i]));
            }
        }
        let mut indices_at = None;
        if self.rd32(h + 76) != 0 {
            let n = self.rd32(h + 36);
            indices_at = Some((self.bytes(n, index_width, index_width)?, n as usize * index_width as usize));
        }
        if self.apply {
            let take = |a: Option<(usize, usize)>| a.map(|(at, len)| self.src[at..at + len].to_vec()).unwrap_or_default();
            let rec = AnimRec {
                name: self.str_of(self.rd32(h), h + 88),
                header: self.src[h..h + 88].to_vec(),
                names: names_at.map(|at| self.src[at..at + 2 * self.rd8(h + 26) as usize].to_vec()).unwrap_or_default(),
                notify: notify_at.map(|at| self.src[at..at + 8 * self.rd8(h + 27) as usize].to_vec()).unwrap_or_default(),
                data_byte: take(arrays[0]),
                data_short: take(arrays[1]),
                data_int: take(arrays[2]),
                random_short: take(arrays[3]),
                random_byte: take(arrays[4]),
                indices: take(indices_at),
                delta: delta_rec,
            };
            self.rec.anims.push(rec);
        }
        self.pop()
    }

    fn effect_visual(&mut self, ty: u32, reference: u32, slot: u32) -> Res {
        self.note_pointer(reference);
        let start = self.pos;
        if ty == 12 || ty == 14 {
            return if reference != INLINE { Ok(()) } else { self.string() };
        }
        if ty == 10 || ty == 11 {
            return Ok(());
        }
        self.asset_reference(reference, if ty == 9 { Kind::Model } else { Kind::Material })?;
        if reference >= INSERT {
            self.record_asset(if ty == 9 { 4 } else { 5 }, slot, start);
        }
        Ok(())
    }

    fn effect(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 32, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        let mut count = 0u64;
        for i in [16usize, 20, 24] {
            let part = self.rd32(h + i);
            if part > i32::MAX as u32 {
                return Err(self.fail("effect element count"));
            }
            count += part as u64;
        }
        if count > i32::MAX as u64 {
            return Err(self.fail("effect element count"));
        }
        if self.rd32(h + 28) != 0 {
            let element_slots = 0x3000_0001 + align_up(self.position[3], 4) as u32;
            // IW4: 252-byte elements and element types numbered two lower from the trail on (CoDOL inserted two)
            let stride = if self.stock { 252 } else { 260 };
            let elements = self.bytes(count as u32, stride, 4)?;
            for i in 0..count as usize {
                let e = elements + stride * i;
                let (ty, n) = (self.rd8(e + 176), self.rd8(e + 177));
                let ty = if self.stock && ty >= 3 { ty + 2 } else { ty };
                if self.rd32(e + 180) != 0 {
                    let c = self.rd8(e + 178) + 1;
                    self.bytes(c, 96, 4)?;
                }
                if self.rd32(e + 184) != 0 {
                    let c = self.rd8(e + 179) + 1;
                    self.bytes(c, if self.qa() { 64 } else { 48 }, 4)?;
                }
                if ty == 13 {
                    if self.rd32(e + 188) != 0 {
                        let visual_slots = self.block_three_reference();
                        let visuals = self.bytes(n, 8, 4)?;
                        for j in 0..2 * n as usize {
                            self.effect_visual(0, self.rd32(visuals + 4 * j), visual_slots + 4 * j as u32)?;
                        }
                    }
                } else if n <= 1 {
                    self.effect_visual(ty, self.rd32(e + 188), element_slots + stride as u32 * i as u32 + 188)?;
                } else if self.rd32(e + 188) != 0 {
                    let visual_slots = self.block_three_reference();
                    let visuals = self.bytes(n, 4, 4)?;
                    for j in 0..n as usize {
                        self.effect_visual(ty, self.rd32(visuals + 4 * j), visual_slots + 4 * j as u32)?;
                    }
                }
                for j in [216usize, 220, 224] {
                    if self.ptr(e + j) == INLINE {
                        self.string()?;
                    }
                }
                if self.rd32(e + 244) != 0 {
                    if ty == 5 {
                        let trail = self.bytes(1, 36, 4)?;
                        if self.rd32(trail + 24) != 0 {
                            let n = self.rd32(trail + 20);
                            self.bytes(n, 20, 4)?;
                        }
                        if self.rd32(trail + 32) != 0 {
                            let n = self.rd32(trail + 28);
                            self.bytes(n, 2, 2)?;
                        }
                    } else if ty == 8 {
                        self.bytes(1, 52, 4)?;
                    } else {
                        self.bytes(1, 1, 1)?;
                    }
                }
                if !self.stock && self.ptr(e + 256) == INLINE {
                    self.string()?;
                }
            }
        }
        self.pop()
    }

    fn loaded_sound(&mut self) -> Res {
        let h = self.pos;
        // IW4 stores 44 bytes (the sound data pointer at +40), CoDOL 48 (+44)
        let (size, data) = if self.stock { (44usize, 40usize) } else { (48, 44) };
        self.reserve(0, size, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        self.push(0)?;
        let r = self.rd32(h + data);
        if r >= INSERT {
            if r == INSERT {
                self.reserve(3, 4, 4, false)?;
            }
            let n = self.rd32(h + 12);
            self.bytes(n, 1, 1)?;
        }
        self.pop()?;
        self.pop()
    }

    fn sound_curve(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 136, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        self.pop()
    }

    fn sound_alias(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 12, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        let count = self.rd32(h + 8);
        if count > i32::MAX as u32 {
            return Err(self.fail("sound alias count"));
        }
        if self.ptr(h + 4) == INLINE {
            // the alias record is 100 bytes in IW4 and 108 in CoDOL (8 more before the falloff curve)
            let (stride, curve, speaker) = if self.stock { (100usize, 80usize, 96usize) } else { (108, 88, 104) };
            let array = self.block_three_reference();
            let aliases = self.bytes(count, stride, 4)?;
            for i in 0..count as usize {
                let a = aliases + i * stride;
                for j in (0..=16).step_by(4) {
                    if self.ptr(a + j) == INLINE {
                        self.string()?;
                    }
                }
                if self.ptr(a + 20) == INLINE {
                    let record = self.block_three_reference();
                    let file = self.bytes(1, 12, 4)?;
                    if self.src[file] == 1 {
                        let r = self.rd32(file + 4);
                        self.field_asset_reference(r, record + 4, Kind::LoadedSound)?;
                    } else {
                        for j in [4usize, 8] {
                            if self.ptr(file + j) == INLINE {
                                self.string()?;
                            }
                        }
                    }
                }
                let r = self.rd32(a + curve);
                self.field_asset_reference(r, array + (i * stride) as u32 + curve as u32, Kind::SoundCurve)?;
                if self.ptr(a + speaker) == INLINE {
                    let speakers = self.bytes(1, 408, 4)?;
                    if self.ptr(speakers + 4) == INLINE {
                        self.string()?;
                    }
                }
            }
        }
        self.pop()
    }

    fn tracer(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, if self.qa() { 116 } else { 112 }, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        self.asset_reference(self.rd32(h + 4), Kind::Material)?;
        if self.qa() {
            self.asset_reference(self.rd32(h + 8), Kind::Effect)?;
        }
        self.pop()
    }

    fn sound_name(&mut self, reference: u32) -> Res {
        self.note_pointer(reference);
        if reference != INLINE {
            return Ok(());
        }
        let name = self.bytes(1, 4, 4)?;
        if self.ptr(name) != INLINE { Ok(()) } else { self.string() }
    }

    fn weapon(&mut self) -> Res {
        #[derive(Clone, Copy, PartialEq)]
        enum F {
            Str,
            Model,
            Effect,
            Material,
            Sound,
            SoundArray,
            Points,
            Physics,
            Preset,
            Tracer,
        }
        use F::*;
        if self.stock {
            return self.stock_weapon();
        }
        if self.v319() {
            return self.weapon_461();
        }
        if !self.qa() {
            return Err(self.fail("weapon layout of this fastfile version is not supported"));
        }
        const FIELDS: &[(usize, usize, F)] = &[
            (0x000, 4, Str),
            (0x010, 10, Model),
            (0x078, 56, Str),
            (0x158, 56, Str),
            (0x238, 56, Str),
            (0x3b8, 16, Effect),
            (0x418, 16, Effect),
            (0x498, 3, Effect),
            (0x4a4, 53, Sound),
            (0x578, 2, SoundArray),
            (0x580, 4, Effect),
            (0x590, 2, Material),
            (0x66c, 6, Material),
            (0x688, 1, Material),
            (0x690, 1, Material),
            (0x69c, 4, Material),
            (0x6b4, 1, Str),
            (0x6bc, 1, Str),
            (0x6cc, 1, Str),
            (0x7dc, 4, Material),
            (0x8b0, 1, Physics),
            (0x8b4, 1, Preset),
            (0x910, 1, Model),
            (0x918, 2, Effect),
            (0x920, 2, Sound),
            (0xa38, 2, Effect),
            (0xa58, 1, Effect),
            (0xa5c, 1, Sound),
            (0xa60, 3, Effect),
            (0xb24, 1, Str),
            (0xb2c, 1, Points),
            (0xb28, 1, Str),
            (0xb30, 1, Points),
            (0xb80, 2, Str),
            (0xba4, 2, Str),
            (0xc44, 2, Str),
            (0xc4c, 2, Tracer),
            (0xc6c, 1, Sound),
            (0xc70, 1, Effect),
            (0xc74, 1, Str),
            (0xc84, 9, Sound),
            (0xca8, 2, Sound),
            (0xcf0, 6, Sound),
            (0xd20, 1, Str),
            (0xd30, 2, Str),
            (0xd3c, 3, Material),
            (0xd60, 2, Points),
            (0xe08, 1, Str),
            (0xe18, 1, Str),
        ];
        let h = self.pos;
        self.reserve(0, 0xe28, 4, true)?;
        self.push(3)?;
        let (mut record_name, mut display) = (None, None);
        for &(base, n, kind) in FIELDS {
            for j in 0..n {
                let offset = base + 4 * j;
                let r = self.ptr(h + offset);
                match kind {
                    Str => {
                        let at = self.pos;
                        if r == INLINE {
                            self.string()?;
                        }
                        if self.apply && offset == 0 {
                            record_name = self.str_of(r, at);
                        }
                        if self.apply && offset == 4 {
                            display = self.str_of(r, at);
                        }
                    }
                    Model => self.asset_reference(r, Kind::Model)?,
                    Effect => self.asset_reference(r, Kind::Effect)?,
                    Material => self.asset_reference(r, Kind::Material)?,
                    Physics => self.asset_reference(r, Kind::PhysMap)?,
                    Preset => self.asset_reference(r, Kind::Preset)?,
                    Tracer => self.asset_reference(r, Kind::Tracer)?,
                    Sound => self.sound_name(r)?,
                    SoundArray => {
                        if r == INLINE {
                            let names = self.bytes(31, 4, 4)?;
                            for k in 0..31 {
                                self.sound_name(self.rd32(names + 4 * k))?;
                            }
                        }
                    }
                    Points => {
                        if r == INLINE {
                            let at = if offset == 0xb2c || offset == 0xd60 { 0xd5c } else { 0xd5e };
                            let n = self.rd16(h + at);
                            self.bytes(n, 8, 4)?;
                        }
                    }
                }
            }
        }
        if self.apply {
            let record = self.src[h..h + 0xe28].to_vec();
            self.rec.data.push(DataRec::Weapon { name: record_name, display, record });
        }
        self.pop()
    }

    /// The 3.19 (461) weapon: one 4124-byte record, then its dependencies in the order the 3.19 loader reads them (the same
    /// table as the server's `replay_weapon_461`). Script-string ids stream nothing; the two point graphs take their counts
    /// from the 16-bit words at +0xEC4 and +0xEC6; five entries at +1688 are sound alias assets rather than names.
    fn weapon_461(&mut self) -> Res {
        #[derive(Clone, Copy, PartialEq)]
        enum F {
            Str,
            Model,
            Effect,
            Material,
            Sound,
            SoundArray,
            SoundList,
            Points4,
            Points6,
            Physics,
            Preset,
            Tracer,
        }
        use F::*;
        const SIZE: usize = 4124;
        const FIELDS: &[(usize, usize, F)] = &[
            (0, 4, Str),
            (16, 10, Model),
            (124, 62, Str),
            (372, 62, Str),
            (620, 62, Str),
            (1028, 16, Effect),
            (1124, 16, Effect),
            (1188, 16, Effect),
            (1316, 16, Effect),
            (1444, 5, Effect),
            (1464, 1, Str),
            (1472, 1, Str),
            (1476, 53, Sound),
            (1688, 5, SoundList),
            (1708, 2, SoundArray),
            (1716, 4, Effect),
            (1732, 2, Material),
            (1952, 6, Material),
            (1980, 1, Material),
            (1988, 1, Material),
            (2000, 4, Material),
            (2024, 1, Str),
            (2032, 1, Str),
            (2048, 1, Str),
            (2332, 4, Material),
            (2544, 1, Physics),
            (2548, 1, Preset),
            (2656, 1, Model),
            (2664, 2, Effect),
            (2672, 2, Sound),
            (2952, 2, Effect),
            (2984, 1, Effect),
            (2988, 1, Sound),
            (2992, 5, Effect),
            (3196, 1, Str),
            (3204, 1, Points4),
            (3200, 1, Str),
            (3208, 1, Points6),
            (3292, 2, Str),
            (3328, 2, Str),
            (3488, 2, Str),
            (3496, 3, Tracer),
            (3532, 1, Sound),
            (3536, 1, Effect),
            (3540, 1, Str),
            (3556, 1, Sound),
            (3560, 4, Sound),
            (3576, 4, Sound),
            (3592, 2, Sound),
            (3664, 6, Sound),
            (3716, 1, Str),
            (3732, 2, Str),
            (3744, 4, Material),
            (3784, 1, Points4),
            (3788, 1, Points6),
            (3880, 3, Str),
            (4000, 1, Str),
            (4016, 1, Str),
        ];
        let h = self.pos;
        self.reserve(0, SIZE, 4, true)?;
        self.push(3)?;
        for &(base, n, kind) in FIELDS {
            for j in 0..n {
                let offset = base + 4 * j;
                let r = self.ptr(h + offset);
                match kind {
                    Str => {
                        if r == INLINE {
                            self.string()?;
                        }
                    }
                    Model => self.asset_reference(r, Kind::Model)?,
                    Effect => self.asset_reference(r, Kind::Effect)?,
                    Material => self.asset_reference(r, Kind::Material)?,
                    Physics => self.asset_reference(r, Kind::PhysMap)?,
                    Preset => self.asset_reference(r, Kind::Preset)?,
                    Tracer => self.asset_reference(r, Kind::Tracer)?,
                    Sound => self.sound_name(r)?,
                    SoundList => self.asset_reference(r, Kind::SoundAlias)?,
                    SoundArray => {
                        if r == INLINE {
                            let names = self.bytes(31, 4, 4)?;
                            for k in 0..31 {
                                self.sound_name(self.rd32(names + 4 * k))?;
                            }
                        }
                    }
                    Points4 | Points6 => {
                        if r == INLINE {
                            let n = self.rd16(h + if kind == Points4 { 0xec4 } else { 0xec6 });
                            self.bytes(n, 8, 4)?;
                        }
                    }
                }
            }
        }
        self.pop()
    }

    /// A string pointer: the text follows when the pointer is -1.
    fn field_string(&mut self, at: usize) -> Res {
        if self.ptr(at) == INLINE {
            self.string()?;
        }
        Ok(())
    }

    /// `count` string pointers behind an array pointer.
    fn field_strings(&mut self, at: usize, count: u32) -> Res {
        if self.ptr(at) == INLINE {
            let list = self.bytes(count, 4, 4)?;
            for i in 0..count as usize {
                self.field_string(list + 4 * i)?;
            }
        }
        Ok(())
    }

    /// `count` asset pointers behind an array pointer.
    fn field_assets(&mut self, at: usize, count: u32, kind: Kind) -> Res {
        if self.ptr(at) == INLINE {
            let list = self.bytes(count, 4, 4)?;
            for i in 0..count as usize {
                self.asset_reference(self.rd32(list + 4 * i), kind)?;
            }
        }
        Ok(())
    }

    /// `count` elements of `stride` bytes behind a pointer (no pointers inside).
    fn field_array(&mut self, at: usize, count: u32, stride: usize, align: usize) -> Res {
        if self.ptr(at) == INLINE {
            self.bytes(count, stride, align)?;
        }
        Ok(())
    }

    /// The stock IW4 weapon: a 116-byte complete definition and, through a pointer, the 1,668-byte weapon definition (the field
    /// order is the struct order, with the accuracy graphs moved to the end of the definition, as OpenAssetTools' zone code
    /// describes). CoDOL's weapon is one flat 3,624-byte record instead.
    fn stock_weapon(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 116, 4, true)?;
        self.push(3)?;
        let name_at = self.pos;
        let mut stock_record = None;
        self.field_string(h)?;
        if self.ptr(h + 4) == INLINE {
            let d = self.bytes(1, 1668, 4)?;
            if self.apply {
                let mut record = self.src[h..h + 116].to_vec();
                record.extend_from_slice(&self.src[d..d + 1668]);
                let name = self.str_of(self.rd32(h), name_at);
                stock_record = Some((name, record));
            }
            self.field_string(d)?;
            self.field_assets(d + 4, 16, Kind::Model)?;
            self.asset_reference(self.rd32(d + 8), Kind::Model)?;
            self.field_strings(d + 12, 37)?;
            self.field_strings(d + 16, 37)?;
            self.field_string(d + 20)?;
            for at in [24usize, 28, 32, 36] {
                self.field_array(d + at, 16, 2, 2)?;
            }
            for at in [72usize, 76] {
                self.asset_reference(self.rd32(d + at), Kind::Effect)?;
            }
            // 47 sound aliases (pickup ... scan), each a pointer to a name holder
            for k in 0..47usize {
                self.sound_name(self.rd32(d + 80 + 4 * k))?;
            }
            if self.ptr(d + 268) == INLINE {
                let names = self.bytes(31, 4, 4)?;
                for k in 0..31 {
                    self.sound_name(self.rd32(names + 4 * k))?;
                }
            }
            for at in [272usize, 276, 280, 284] {
                self.asset_reference(self.rd32(d + at), Kind::Effect)?;
            }
            for at in [288usize, 292] {
                self.asset_reference(self.rd32(d + at), Kind::Material)?;
            }
            self.field_assets(d + 472, 16, Kind::Model)?;
            for at in [476usize, 480, 484, 488] {
                self.asset_reference(self.rd32(d + at), Kind::Model)?;
            }
            for at in [492usize, 500, 508] {
                self.asset_reference(self.rd32(d + at), Kind::Material)?;
            }
            for at in [524usize, 532, 548] {
                self.field_string(d + at)?;
            }
            for at in [776usize, 780, 784, 788] {
                self.asset_reference(self.rd32(d + at), Kind::Material)?;
            }
            self.asset_reference(self.rd32(d + 968), Kind::PhysMap)?;
            self.asset_reference(self.rd32(d + 1056), Kind::Model)?;
            for at in [1064usize, 1068] {
                self.asset_reference(self.rd32(d + at), Kind::Effect)?;
            }
            for at in [1072usize, 1076] {
                self.sound_name(self.rd32(d + at))?;
            }
            self.field_array(d + 1092, 31, 4, 4)?;
            self.field_array(d + 1096, 31, 4, 4)?;
            for at in [1100usize, 1104, 1132] {
                self.asset_reference(self.rd32(d + at), Kind::Effect)?;
            }
            self.sound_name(self.rd32(d + 1136))?;
            // the accuracy graphs sit between the ignition sound and the hint strings: name and original knots, ai-vs-ai then ai-vs-player
            for k in 0..2usize {
                self.field_string(d + 1292 + 4 * k)?;
                let n = self.rd16(d + 1308 + 2 * k);
                self.field_array(d + 1300 + 4 * k, n, 8, 4)?;
            }
            for at in [1384usize, 1388, 1420] {
                self.field_string(d + at)?;
            }
            self.field_array(d + 1460, 20, 4, 4)?;
            for at in [1464usize, 1468] {
                self.field_string(d + at)?;
            }
            self.asset_reference(self.rd32(d + 1472), Kind::Tracer)?;
            self.sound_name(self.rd32(d + 1500))?;
            self.asset_reference(self.rd32(d + 1504), Kind::Effect)?;
            self.field_string(d + 1508)?;
            // turret barrel spin sounds: max, 4 up, 4 down, then the missile cone pair
            for k in 0..11usize {
                self.sound_name(self.rd32(d + 1524 + 4 * k))?;
            }
        }
        let display_at = self.pos;
        self.field_string(h + 8)?;
        if self.apply {
            if let Some((name, record)) = stock_record.take() {
                let display = self.str_of(self.rd32(h + 8), display_at);
                self.rec.data.push(DataRec::Weapon { name, display, record });
            }
        }
        self.field_array(h + 12, 32, 2, 2)?;
        self.field_strings(h + 16, 37)?;
        self.field_string(h + 60)?;
        for at in [72usize, 76] {
            self.asset_reference(self.rd32(h + at), Kind::Material)?;
        }
        for k in 0..2usize {
            let n = self.rd16(h + 100 + 2 * k);
            self.field_array(h + 104 + 4 * k, n, 8, 4)?;
        }
        self.pop()
    }

    /// The stock IW4 vehicle (720 bytes; CoDOL's is 788).
    fn stock_vehicle(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 720, 4, true)?;
        self.push(3)?;
        for o in [0usize, 8, 172, 180, 408] {
            self.field_string(h + o)?;
        }
        self.asset_reference(self.rd32(h + 412), Kind::Weapon)?;
        for o in [436usize, 440] {
            self.sound_name(self.rd32(h + o))?;
        }
        for o in [472usize, 476] {
            self.asset_reference(self.rd32(h + o), Kind::Material)?;
        }
        for o in [488usize, 492, 496, 500, 508, 516, 520, 524, 528, 536, 544, 552, 560, 568] {
            self.sound_name(self.rd32(h + o))?;
        }
        self.field_string(h + 576)?;
        for k in 0..31usize {
            self.sound_name(self.rd32(h + 580 + 4 * k))?;
        }
        self.pop()
    }

    fn vehicle(&mut self) -> Res {
        if self.stock {
            return self.stock_vehicle();
        }
        const NAMES: [usize; 3] = [0, 8, 0xac];
        const SOUNDS: [usize; 14] = [0x1e0, 0x1e4, 0x1e8, 0x1ec, 0x1f4, 0x1fc, 0x200, 0x204, 0x208, 0x210, 0x218, 0x220, 0x228, 0x230];
        let h = self.pos;
        self.reserve(0, 0x314, 4, true)?;
        self.push(3)?;
        for o in NAMES {
            if self.ptr(h + o) == INLINE {
                self.string()?;
            }
        }
        self.asset_reference(self.rd32(h + 0xb0), Kind::Preset)?;
        if self.ptr(h + 0xb4) == INLINE {
            self.string()?;
        }
        if self.ptr(h + 0x190) == INLINE {
            self.string()?;
        }
        self.asset_reference(self.rd32(h + 0x194), Kind::Weapon)?;
        for i in 0..2 {
            self.sound_name(self.rd32(h + 0x1ac + 4 * i))?;
        }
        for i in 0..2 {
            self.asset_reference(self.rd32(h + 0x1d0 + 4 * i), Kind::Material)?;
        }
        for o in SOUNDS {
            self.sound_name(self.rd32(h + o))?;
        }
        if self.ptr(h + 0x238) == INLINE {
            self.string()?;
        }
        for i in 0..31 {
            self.sound_name(self.rd32(h + 0x23c + 4 * i))?;
        }
        self.pop()
    }

    fn weapon_attachment(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 12, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        if self.rd32(h + 8) != 0 {
            let count = self.rd32(h + 4);
            if count > i32::MAX as u32 {
                return Err(self.fail("attachment count"));
            }
            let fields = self.bytes(count, 12, 4)?;
            for i in 0..count as usize {
                let ty = self.rd8(fields + 12 * i);
                let string = (ty < 16 && !(4..=9).contains(&ty)) || ty == 39 || ty == 42 || ty == 43;
                if string && self.ptr(fields + 12 * i + 8) == INLINE {
                    self.string()?;
                }
            }
        }
        self.pop()
    }

    fn impact_table(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 8, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        if self.rd32(h + 4) != 0 {
            // 35 surface types per impact kind (IW4, QA); the 3.19 table has 39 (2964 bytes of effect pointers against QA's 2660)
            let (records, per) = (if self.stock { 15 } else if self.qa() { 19 } else { 16 }, if self.v319() { 39usize } else { 35usize });
            let effects = self.bytes(records, 4 * per, 4)?;
            for i in 0..per * records as usize {
                self.asset_reference(self.rd32(effects + 4 * i), Kind::Effect)?;
            }
        }
        self.pop()
    }

    fn localize(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 8, 4, true)?;
        self.push(3)?;
        let value_at = self.pos;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        let key_at = self.pos;
        if self.ptr(h + 4) == INLINE {
            self.string()?;
        }
        if self.apply {
            let (text, key) = (self.str_of(self.rd32(h), value_at), self.str_of(self.rd32(h + 4), key_at));
            self.rec.data.push(DataRec::Localize { key, text });
        }
        self.pop()
    }

    fn net_const_strings(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 20, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        if self.ptr(h + 16) == INLINE {
            let count = self.rd32(h + 12);
            if count > i32::MAX as u32 {
                return Err(self.fail("netconststrings count"));
            }
            let names = self.bytes(count, 4, 4)?;
            for i in 0..count as usize {
                if self.ptr(names + 4 * i) == INLINE {
                    self.string()?;
                }
            }
        }
        self.pop()
    }

    fn rawfile(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 16, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        let (compressed, length) = (self.rd32(h + 4), self.rd32(h + 8));
        if compressed > i32::MAX as u32 || length > i32::MAX as u32 {
            return Err(self.fail("rawfile size"));
        }
        let name = if self.apply { self.str_of(self.rd32(h), h + 16) } else { None };
        if self.rd32(h + 12) != 0 {
            let n = if compressed != 0 { compressed } else { length + 1 };
            let at = self.bytes(n, 1, 1)?;
            if self.apply {
                let bytes = self.src[at..at + n as usize].to_vec();
                self.rec.data.push(DataRec::RawFile { name, compressed: compressed != 0, length: length as usize, bytes });
            }
        }
        self.pop()
    }

    fn string_table(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 16, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        let (columns, rows) = (self.rd32(h + 4), self.rd32(h + 8));
        let count = columns as u64 * rows as u64;
        if columns > i32::MAX as u32 || rows > i32::MAX as u32 || count > i32::MAX as u64 {
            return Err(self.fail("string table size"));
        }
        let name = if self.apply { self.str_of(self.rd32(h), h + 16) } else { None };
        let mut texts = Vec::new();
        if self.rd32(h + 12) != 0 {
            let cells = self.bytes(count as u32, 8, 4)?;
            for i in 0..count as usize {
                let at = self.pos;
                let reference = self.ptr(cells + 8 * i);
                if reference == INLINE {
                    self.string()?;
                }
                if self.apply {
                    texts.push(self.str_of(reference, at));
                }
            }
        }
        if self.apply {
            self.rec.data.push(DataRec::StringTable { name, columns: columns as usize, rows: rows as usize, cells: texts });
        }
        self.pop()
    }

    fn leaderboard(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 24, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        if self.rd32(h + 20) != 0 {
            let count = self.rd32(h + 8);
            if count > i32::MAX as u32 {
                return Err(self.fail("leaderboard columns"));
            }
            let columns = self.bytes(count, 32, 4)?;
            for i in 0..count as usize {
                if self.ptr(columns + 32 * i) == INLINE {
                    self.string()?;
                }
                if self.ptr(columns + 32 * i + 16) == INLINE {
                    self.string()?;
                }
            }
        }
        self.pop()
    }

    fn structured_data(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 12, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        if self.rd32(h + 8) != 0 {
            let count = self.rd32(h + 4);
            if count > i32::MAX as u32 {
                return Err(self.fail("structured data count"));
            }
            let defs = self.bytes(count, 60, 4)?;
            for i in 0..count as usize {
                let d = defs + 60 * i;
                for kind in 0..2usize {
                    if self.rd32(d + 12 + 8 * kind) != 0 {
                        let n = self.rd32(d + 8 + 8 * kind);
                        if n > i32::MAX as u32 {
                            return Err(self.fail("structured data records"));
                        }
                        let rsize = if kind == 1 { 20 } else { 12 };
                        let records = self.bytes(n, rsize, 4)?;
                        for j in 0..n as usize {
                            let r = records + j * rsize;
                            if self.rd32(r + if kind == 1 { 4 } else { 8 }) == 0 {
                                continue;
                            }
                            let m = self.rd32(r);
                            if m > i32::MAX as u32 {
                                return Err(self.fail("structured data members"));
                            }
                            let msize = if kind == 1 { 24 } else { 8 };
                            let members = self.bytes(m, msize, 4)?;
                            for k in 0..m as usize {
                                if self.ptr(members + k * msize) == INLINE {
                                    self.string()?;
                                }
                            }
                        }
                    }
                }
                for j in [24usize, 32] {
                    if self.rd32(d + j + 4) != 0 {
                        let n = self.rd32(d + j);
                        self.bytes(n, 16, 4)?;
                    }
                }
                if self.rd32(d + 56) != 0 {
                    let n = self.rd32(d + 52);
                    self.bytes(n, 4, 4)?;
                }
            }
        }
        self.pop()
    }

    fn font(&mut self) -> Res {
        let h = self.pos;
        self.reserve(0, 24, 4, true)?;
        self.push(3)?;
        if self.ptr(h) == INLINE {
            self.string()?;
        }
        self.asset_reference(self.rd32(h + 12), Kind::Material)?;
        self.asset_reference(self.rd32(h + 16), Kind::Material)?;
        if self.ptr(h + 20) == INLINE {
            let n = self.rd32(h + 8);
            self.bytes(n, 24, 4)?;
        }
        self.pop()
    }

    // ---- the zone directory ------------------------------------------------------------------------------

    /// Replays directory entry `index` of `zone` (assets must be replayed in order).
    pub fn replay_asset(&mut self, zone: &super::Zone, index: usize) -> Res {
        self.cur_asset = index;
        let e = zone.assets[index];
        self.pos = e.start;
        let ty = e.asset_type;
        // QA types without a load operation carry no payload.
        if self.qa() && matches!(ty, 14 | 17 | 18) {
            return if e.extent == 0 { Ok(()) } else { Err(self.fail("payload on a type without a loader")) };
        }
        if matches!(ty, 23 | 29 | 32 | 33 | 34 | 35) {
            return if e.extent == 0 { Ok(()) } else { Err(self.fail("payload on a type without a loader")) };
        }
        let kind = match ty {
            0 => Some(Kind::Preset),
            1 => Some(Kind::PhysMap),
            3 => Some(Kind::Surfs),
            4 => Some(Kind::Model),
            5 => Some(Kind::Material),
            6 | 7 => Some(Kind::Shader),
            8 => Some(Kind::Declaration),
            9 => Some(Kind::TechSet),
            10 => Some(Kind::Image),
            11 => Some(Kind::SoundAlias),
            12 => Some(Kind::SoundCurve),
            13 => Some(Kind::LoadedSound),
            30 => Some(Kind::Effect),
            40 => Some(Kind::Tracer),
            28 => Some(Kind::Weapon),
            26 => Some(Kind::Menu),
            _ => None,
        };
        // A directory entry may name an asset loaded earlier: a back-reference with no payload.
        if e.reference < INSERT {
            return if self.qa() && e.extent == 0 { Ok(()) } else { Err(self.fail("directory back-reference with a payload")) };
        }
        // Like the loader: a dry run validates the asset without recording anything, then the real pass applies it.
        // A failure in either leaves the block state exactly as it was.
        let saved = (self.position, self.active, self.stack.clone());
        let mut outcome = Ok(());
        for pass in 0..2 {
            self.apply = pass == 1;
            self.pos = e.start;
            self.end = e.start + e.extent;
            let result = self.replay_typed(ty, kind, e.reference);
            let done = self.pos == e.start + e.extent;
            self.end = self.src.len();
            outcome = match result {
                Err(f) => Err(f),
                Ok(()) if !done => Err(Fail { what: "asset did not end at its extent", at: self.pos }),
                Ok(()) => Ok(()),
            };
            if outcome.is_err() {
                break;
            }
            if pass == 0 {
                (self.position, self.active, self.stack) = (saved.0, saved.1, saved.2.clone());
            }
        }
        self.apply = true;
        if let Err(f) = outcome {
            (self.position, self.active, self.stack) = saved;
            self.pos = e.start;
            return Err(f);
        }
        if matches!(ty, 0..=5 | 11 | 12 | 13 | 16 | 18 | 20 | 28 | 30 | 39 | 40 | 41 | 43) {
            let slot = self.directory_reference + 16 * index as u32 + 8;
            self.record_asset(ty, slot, e.start);
        }
        Ok(())
    }

    /// Walks the next asset of a stock IW4 zone, whose asset list has no extents: the asset starts where the previous
    /// one ended (`self.pos`) and the walk says where it ends. Returns the end, or the failure.
    pub fn walk_stream_asset(&mut self, index: usize, ty: u32, reference: u32) -> Res<usize> {
        self.cur_asset = index;
        let start = self.pos;
        if reference < INSERT {
            return Ok(start);
        }
        let kind = Self::kind_of(ty);
        let saved = (self.position, self.active, self.stack.clone());
        self.end = self.src.len();
        let mut end = start;
        for pass in 0..2 {
            self.apply = pass == 1;
            self.pos = start;
            if let Err(f) = self.replay_typed(ty, kind, reference) {
                self.apply = true;
                (self.position, self.active, self.stack) = saved;
                self.pos = start;
                return Err(f);
            }
            if pass == 0 {
                end = self.pos;
                (self.position, self.active, self.stack) = (saved.0, saved.1, saved.2.clone());
            } else if self.pos != end {
                self.apply = true;
                (self.position, self.active, self.stack) = saved;
                return Err(Fail { what: "the two passes ended differently", at: self.pos });
            }
        }
        self.apply = true;
        if matches!(ty, 0..=5 | 11 | 12 | 13 | 16 | 18 | 20 | 28 | 30 | 39 | 40 | 41 | 43) {
            let slot = self.directory_reference + self.directory_stride as u32 * index as u32 + 4;
            self.record_asset(ty, slot, start);
        }
        Ok(end)
    }

    fn kind_of(ty: u32) -> Option<Kind> {
        match ty {
            0 => Some(Kind::Preset),
            1 => Some(Kind::PhysMap),
            3 => Some(Kind::Surfs),
            4 => Some(Kind::Model),
            5 => Some(Kind::Material),
            6 | 7 => Some(Kind::Shader),
            8 => Some(Kind::Declaration),
            9 => Some(Kind::TechSet),
            10 => Some(Kind::Image),
            11 => Some(Kind::SoundAlias),
            12 => Some(Kind::SoundCurve),
            13 => Some(Kind::LoadedSound),
            30 => Some(Kind::Effect),
            40 => Some(Kind::Tracer),
            28 => Some(Kind::Weapon),
            26 => Some(Kind::Menu),
            _ => None,
        }
    }

    fn replay_typed(&mut self, ty: u32, kind: Option<Kind>, reference: u32) -> Res {
        if let Some(k) = kind {
            return self.asset_reference(reference, k);
        }
        // Types handled by walkers that are not referenceable families still go through the same prologue.
        let start = self.pos;
        let _ = start;
        self.push(0)?;
        if reference == INSERT {
            self.reserve(3, 4, 4, false)?;
        }
        match ty {
            2 => self.animation()?,
            27 => self.localize()?,
            24 => self.font()?,
            31 => self.impact_table()?,
            36 => self.rawfile()?,
            37 => self.string_table()?,
            38 => self.leaderboard()?,
            39 => self.structured_data()?,
            41 => self.vehicle()?,
            43 => self.weapon_attachment()?,
            44 => self.net_const_strings()?,
            17 if self.stock => self.stock_sp_game_world()?,
            18 if self.stock => self.stock_game_world()?,
            18 if self.version == super::fastfile::VERSION_C1 => self.c1_game_world()?,
            15 if self.iw4_sizes() => self.stock_clip_map()?,
            15 => self.collision_qa()?,
            16 => self.common_world()?,
            21 => self.world()?,
            42 if self.rd32(self.pos + 4) == 0 => self.asym_addon_data()?,
            19 | 42 => self.addon_map_entities()?,
            20 => self.effect_world()?,
            22 => self.light_definition()?,
            25 => self.menu_list()?,
            _ => return Err(Fail { what: "asset type not supported yet", at: self.pos }),
        }
        self.pop()
    }

    pub fn positions(&self) -> [usize; 8] {
        self.position
    }

    pub fn capacities(&self) -> [usize; 8] {
        self.cap
    }
}
