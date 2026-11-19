//! A private copy of `schema_walk` for the stock-to-QA converter (`convert_zone`), with one addition: the hook is also told about
//! the model-surface records (`XModelSurfs`) of stock models, which the original walker keeps in the temporary block, where the
//! header is overwritten by the next one before the model's hook runs. Everything else is identical to `schema_walk`.
//!
//! Walks the asset stream of a zone (Black Ops II, Call of Duty 4, Modern Warfare 2 or 3) the way the game's loader does.
//!
//! The loading rules are data: `schema/<game>.json` (made by `tools/gen_zone_schema.py`) lists every structure of the game with its
//! x86 layout and the rules attached to its members (strings, element counts, conditions, memory blocks, reusable pointers,
//! alignment, member order).  This file interprets them.  The way the rules are applied (block stack, alignment of every
//! allocation, `-1` "data follows" pointers, `-2` pointers that also register a slot, offset pointers to earlier data, runtime
//! blocks that take no stream bytes, the temporary block that is rewound when it is popped) follows the loaders that the
//! OpenAssetTools zone code generator writes (GPL-3.0, <https://github.com/Laupetin/OpenAssetTools>); this is an independent
//! implementation.
//!
//! The eight blocks are kept as real memory, so counts that refer to members of earlier structures (also through pointers)
//! are read from what was loaded, and a walk is complete when the stream is used up exactly and every block ends where the
//! `XFile` header says it does.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const MAX_DEPTH: usize = 400;

/// The games whose zones can be walked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Game {
    /// Call of Duty 4.
    Iw3,
    /// Modern Warfare 2 (stock).
    Iw4,
    /// Modern Warfare 3.
    Iw5,
    /// Black Ops II.
    T6,
}

impl Game {
    pub fn parse(s: &str) -> Option<Game> {
        match s.to_ascii_lowercase().as_str() {
            "iw3" | "cod4" => Some(Game::Iw3),
            "iw4" | "mw2" => Some(Game::Iw4),
            "iw5" | "mw3" => Some(Game::Iw5),
            "t6" | "bo2" => Some(Game::T6),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Game::Iw3 => "iw3",
            Game::Iw4 => "iw4",
            Game::Iw5 => "iw5",
            Game::T6 => "t6",
        }
    }

    /// Asset type names by number.
    pub fn asset_type_name(self, ty: u32) -> String {
        let names = &Schema::get(self).asset_names;
        match names.get(ty as usize) {
            Some(n) => n.trim_start_matches("ASSET_TYPE_").to_ascii_lowercase(),
            None => "?".to_string(),
        }
    }
}

/// An opened zone: the inflated image and the block sizes of its `XFile` header.
pub struct ZoneImage {
    pub name: String,
    pub image: Vec<u8>,
    pub block_sizes: Vec<u32>,
}

impl ZoneImage {
    /// Opens a fastfile of the game.
    pub fn open(game: Game, name: &str, raw: &[u8]) -> Result<ZoneImage> {
        let schema = Schema::get(game);
        let n = schema.blocks.len();
        let image = match game {
            Game::T6 => {
                let z = super::t6::open_named(raw, Some(name))?;
                return Ok(ZoneImage { name: z.name.clone(), block_sizes: z.block_sizes.to_vec(), image: z.image });
            }
            Game::Iw3 => open_iw(raw, 5, 12)?,
            Game::Iw4 => open_iw(raw, 276, 0x15)?,
            Game::Iw5 => open_iw(raw, 1, 0x15)?,
        };
        if image.len() < 8 + 4 * n {
            anyhow::bail!("zone image too small ({} bytes)", image.len());
        }
        let block_sizes = (0..n).map(|i| u32::from_le_bytes(image[8 + 4 * i..12 + 4 * i].try_into().unwrap())).collect();
        Ok(ZoneImage { name: name.to_string(), image, block_sizes })
    }
}

/// Infinity Ward containers: `IWffu100` (unsigned) or `IWff0100` (signed: SHA-256 hash tables between 0x2000-byte blocks), the
/// version, a fixed header, then one zlib stream.
fn open_iw(raw: &[u8], version: u32, header: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    if raw.len() < header + 16 {
        anyhow::bail!("too short for a fastfile");
    }
    let signed = match &raw[..8] {
        b"IWff0100" => true,
        b"IWffu100" => false,
        other => anyhow::bail!("not an Infinity Ward fastfile ({:?})", String::from_utf8_lossy(other)),
    };
    let v = u32::from_le_bytes(raw[8..12].try_into().unwrap());
    if v != version {
        anyhow::bail!("fastfile version {v} (expected {version})");
    }
    let body = &raw[header..];
    let compressed: Vec<u8> = if signed {
        if &body[..8] != b"IWffs100" {
            anyhow::bail!("signed fastfile without its authentication block");
        }
        let mut out = Vec::with_capacity(body.len());
        let mut index = 0usize;
        for chunk in body.chunks(0x2000).skip(1) {
            if index == 0 {
                index = 1;
            } else {
                out.extend_from_slice(chunk);
                index += 1;
                if index == 257 {
                    index = 0;
                }
            }
        }
        out
    } else {
        body.to_vec()
    };
    let mut image = Vec::with_capacity(compressed.len() * 4);
    flate2::read::ZlibDecoder::new(&compressed[..]).read_to_end(&mut image).context("inflating the zone")?;
    Ok(image)
}

// ---------------------------------------------------------------------------------------------------------------- schema
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
}

impl Kind {
    fn parse(s: &str) -> Kind {
        match s {
            "i8" => Kind::I8,
            "u8" => Kind::U8,
            "i16" => Kind::I16,
            "u16" => Kind::U16,
            "i32" => Kind::I32,
            "i64" => Kind::I64,
            "u64" => Kind::U64,
            "f32" => Kind::F32,
            "f64" => Kind::F64,
            _ => Kind::U32,
        }
    }
    fn size(self) -> usize {
        match self {
            Kind::I8 | Kind::U8 => 1,
            Kind::I16 | Kind::U16 => 2,
            Kind::I32 | Kind::U32 | Kind::F32 => 4,
            Kind::I64 | Kind::U64 | Kind::F64 => 8,
        }
    }
}

#[derive(Debug)]
enum Expr {
    Num(i64),
    Op(String, Box<Expr>, Box<Expr>),
    Dyn(Box<DynRef>),
    /// The loader's current pointer to a structure.
    Var(usize),
}

#[derive(Debug)]
struct DynRef {
    root: usize,
    refs: Vec<(u32, bool)>,
    kind: Kind,
    dims: Vec<u32>,
    esz: u32,
    idx: Vec<Expr>,
}

#[derive(Debug)]
enum Mod {
    Array { n: u32, dsize: Option<Expr>, dcount: Option<Expr> },
    Ptr { count: Option<Expr>, by_idx: Vec<Option<Expr>> },
}

#[derive(Debug)]
struct Member {
    name: String,
    off: u32,
    ty: Option<usize>,
    mods: Vec<Mod>,
    is_string: bool,
    reusable: bool,
    cond: Option<Expr>,
    blk: Option<u8>,
    aa: Option<Expr>,
    leaf: bool,
    ign: bool,
    after_partial: bool,
    need: bool,
    es: u32,
    ea: u32,
    ma: u32,
    typedef_base: bool,
    tk: String,
}

#[derive(Debug)]
struct SStruct {
    name: String,
    union: bool,
    size: u32,
    align: u32,
    leaf: bool,
    asset: bool,
    blk: Option<u8>,
    aa: Option<Expr>,
    dynoff: i64,
    members: Vec<Member>,
}

pub struct Schema {
    structs: Vec<SStruct>,
    assets: HashMap<u32, usize>,
    skip: HashSet<u32>,
    by_name: HashMap<String, usize>,
    asset_names: Vec<String>,
    /// Per block: name, kind (0 temp, 1 runtime, 2 delay, 3 normal).
    blocks: Vec<(String, u8)>,
    shift: u32,
    insert: usize,
    default_normal: usize,
    default_temp: usize,
    /// Whether the asset list names the zones it depends on (Black Ops II).
    has_depends: bool,
    /// Pointer size in the zone (4, or 8 for the 64-bit builds).
    ptr: usize,
}

fn expr_from(v: &Value) -> Result<Expr> {
    let a = v.as_array().context("expression")?;
    Ok(match a[0].as_str().unwrap() {
        "n" => Expr::Num(a[1].as_i64().unwrap()),
        "o" => Expr::Op(a[1].as_str().unwrap().to_string(), Box::new(expr_from(&a[2])?), Box::new(expr_from(&a[3])?)),
        "v" => Expr::Var(a[1].as_u64().unwrap() as usize),
        "d" => {
            let refs = a[2].as_array().unwrap().iter().map(|r| (r[0].as_u64().unwrap() as u32, r[1].as_u64().unwrap() != 0)).collect();
            let dims = a[4].as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as u32).collect();
            let idx = a[6].as_array().unwrap().iter().map(expr_from).collect::<Result<Vec<_>>>()?;
            Expr::Dyn(Box::new(DynRef { root: a[1].as_u64().unwrap() as usize, refs, kind: Kind::parse(a[3].as_str().unwrap()), dims, esz: a[5].as_u64().unwrap() as u32, idx }))
        }
        other => anyhow::bail!("bad expression tag {other}"),
    })
}

fn opt_expr(v: &Value) -> Result<Option<Expr>> {
    if v.is_null() { Ok(None) } else { Ok(Some(expr_from(v)?)) }
}

impl Schema {
    fn load(game: Game) -> Result<Schema> {
        let text = match game {
            Game::T6 => include_str!("schema/t6.json"),
            Game::Iw3 => include_str!("schema/iw3.json"),
            Game::Iw4 => include_str!("schema/iw4.json"),
            Game::Iw5 => include_str!("schema/iw5.json"),
        };
        let root: Value = serde_json::from_str(text)?;
        let mut structs = Vec::new();
        for s in root["structs"].as_array().unwrap() {
            let mut members = Vec::new();
            for m in s["m"].as_array().unwrap() {
                let mut mods = Vec::new();
                for md in m["mods"].as_array().unwrap() {
                    if md[0] == "a" {
                        mods.push(Mod::Array { n: md[1].as_u64().unwrap() as u32, dsize: opt_expr(&md[2])?, dcount: opt_expr(&md[3])? });
                    } else {
                        let by_idx = md[2].as_array().unwrap().iter().map(opt_expr).collect::<Result<Vec<_>>>()?;
                        mods.push(Mod::Ptr { count: opt_expr(&md[1])?, by_idx });
                    }
                }
                members.push(Member {
                    name: m["n"].as_str().unwrap().to_string(),
                    off: m["o"].as_u64().unwrap() as u32,
                    ty: m["ty"].as_i64().filter(|&t| t >= 0).map(|t| t as usize),
                    mods,
                    is_string: m["str"] == 1,
                    reusable: m["re"] == 1,
                    cond: opt_expr(&m["c"])?,
                    blk: m["blk"].as_u64().map(|b| b as u8),
                    aa: opt_expr(&m["aa"])?,
                    leaf: m["leaf"] == 1,
                    ign: m["ign"] == 1,
                    after_partial: m["ap"] == 1,
                    need: m["need"] == 1,
                    es: m["es"].as_u64().unwrap() as u32,
                    ea: m["ea"].as_u64().unwrap() as u32,
                    ma: m["ma"].as_u64().unwrap() as u32,
                    typedef_base: m["td"] == 1,
                    tk: m["tk"].as_str().unwrap().to_string(),
                });
            }
            structs.push(SStruct {
                name: s["n"].as_str().unwrap().to_string(),
                union: s["u"] == 1,
                size: s["s"].as_u64().unwrap() as u32,
                align: s["a"].as_u64().unwrap() as u32,
                leaf: s["leaf"] == 1,
                asset: !s["asset"].as_str().unwrap().is_empty(),
                blk: s["blk"].as_u64().map(|b| b as u8),
                aa: opt_expr(&s["aa"])?,
                dynoff: s["dynoff"].as_i64().unwrap(),
                members,
            });
        }
        let assets = root["assets"].as_object().unwrap().iter().map(|(k, v)| (k.parse().unwrap(), v.as_u64().unwrap() as usize)).collect();
        let by_name = structs.iter().enumerate().map(|(i, s)| (s.name.clone(), i)).collect();
        let blocks: Vec<(String, u8)> = root["blocks"].as_array().unwrap().iter().map(|b| (b["name"].as_str().unwrap().trim_start_matches("XFILE_BLOCK_").to_ascii_lowercase(), b["kind"].as_u64().unwrap() as u8)).collect();
        let first_default = |kind: u64| root["blocks"].as_array().unwrap().iter().position(|b| b["kind"] == kind && b["default"] == 1).unwrap_or(0);
        Ok(Schema {
            structs,
            assets,
            skip: root["skip"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u32).collect(),
            by_name,
            asset_names: root["asset_names"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect(),
            blocks,
            shift: 32 - root["bits"].as_u64().unwrap() as u32,
            insert: root["insert"].as_u64().unwrap() as usize,
            default_normal: first_default(3),
            default_temp: first_default(0),
            has_depends: game == Game::T6,
            ptr: root["ptr"].as_u64().unwrap_or(4) as usize,
        })
    }

    pub fn get(game: Game) -> &'static Schema {
        static T6: OnceLock<Schema> = OnceLock::new();
        static IW3: OnceLock<Schema> = OnceLock::new();
        static IW4: OnceLock<Schema> = OnceLock::new();
        static IW5: OnceLock<Schema> = OnceLock::new();
        let cell = match game {
            Game::T6 => &T6,
            Game::Iw3 => &IW3,
            Game::Iw4 => &IW4,
            Game::Iw5 => &IW5,
        };
        cell.get_or_init(|| Schema::load(game).expect("zone schema"))
    }
}

// ---------------------------------------------------------------------------------------------------------------- results
#[derive(Debug, Clone)]
pub struct WalkedAsset {
    pub index: usize,
    pub ty: u32,
    /// The name stored in the asset header (empty when the entry only refers to an asset loaded earlier).
    pub name: String,
    /// The entry only pointed at data loaded earlier (inside another asset).
    pub reference: bool,
    /// Image offsets where the asset's data starts and ends.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default)]
pub struct StringTableRec {
    pub name: String,
    pub columns: usize,
    pub rows: usize,
    pub cells: Vec<String>,
}

/// Content that Atlas' data tables are made from.
#[derive(Debug, Clone, Default)]
pub struct T6Data {
    /// (name, value) of every localize entry.
    pub localize: Vec<(String, String)>,
    pub rawfiles: Vec<(String, Vec<u8>)>,
    pub stringtables: Vec<StringTableRec>,
}

#[derive(Debug, Clone)]
pub struct WalkFailure {
    pub asset_index: usize,
    pub ty: u32,
    pub offset: usize,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct WalkReport {
    pub zone: String,
    pub assets_total: usize,
    pub assets_walked: usize,
    pub failure: Option<WalkFailure>,
    /// Stream bytes used (offset in the image) and the image length.
    pub consumed: usize,
    pub image_len: usize,
    /// Where each block ended, the sizes in the header, and the temporary block's high-water mark.
    pub block_end: Vec<usize>,
    pub block_sizes: Vec<u32>,
    pub block_names: Vec<String>,
    pub temp_peak: usize,
    /// Everything matched: all assets walked, the whole stream used, every block at its header size.
    pub exact: bool,
    pub mismatches: Vec<String>,
    pub assets: Vec<WalkedAsset>,
    pub data: T6Data,
}

struct Fail(String);
type Res<T = ()> = std::result::Result<T, Fail>;

fn fail<T>(msg: impl Into<String>) -> Res<T> {
    Err(Fail(msg.into()))
}

// ---------------------------------------------------------------------------------------------------------------- walker
#[derive(Clone, Copy, PartialEq, Eq)]
enum Load {
    ArrayPointer,
    DynamicArray,
    Embedded,
    EmbeddedArray,
    PointerArray,
    SinglePointer,
}

#[derive(Clone)]
struct Mc {
    m: &'static Member,
    base: u32,
    idx: Vec<u32>,
    combined: i64,
}

struct Scope {
    vars: Vec<u32>,
    root: usize,
}

struct Walker<'a> {
    s: &'static Schema,
    img: &'a [u8],
    pos: usize,
    sizes: Vec<usize>,
    shift: u32,
    mask: u32,
    psz: usize,
    /// Shift of the block number in a pointer as it is stored in the stream.
    sshift: u32,
    following: u64,
    mem: Vec<Vec<u8>>,
    off: Vec<usize>,
    game: Game,
    stack: Vec<usize>,
    temp_stack: Vec<usize>,
    temp_peak: usize,
    scopes: Vec<Scope>,
    depth: usize,
    reuse_cache: HashMap<usize, HashSet<String>>,
    data: T6Data,
    /// Pointer slots (and their asset header) of the asset that is being loaded, for recording its name.
    last_header: Option<(usize, u32)>,
    assets: Vec<WalkedAsset>,
    /// Names of the members being loaded, and the path at the first failure.
    path: Vec<(usize, &'static str)>,
    fail_path: String,
    /// Called whenever an asset has been loaded (see [`walk_with`]); the id bookkeeping below only runs when it is set.
    hook: Option<&'a mut AssetHook<'a>>,
    next_id: u32,
    /// Asset ids by the address that identifies the asset to later references (its slot in the insert block, or its header).
    id_at: HashMap<u32, u32>,
    /// The asset id each pointer slot ended up designating.
    ref_id: HashMap<u32, u32>,
    /// The zone's script strings: address of the pointer array and the count.
    script_strings: (u32, usize),
    /// Index in the zone's asset list of the entry being loaded.
    top_index: usize,
    /// Stream offset where the entry being loaded starts (converter addition).
    top_start: usize,
}

/// Receives each asset as soon as it is loaded: a read-only view of the memory, the structure's name, the header address and
/// the asset's id (unique in the zone).
pub type AssetHook<'h> = dyn FnMut(&View, &str, u32, u32) + 'h;

/// Read access to the loader's memory for an [`AssetHook`]. Addresses are the walker's own encoding (block and offset); the
/// pointers inside loaded structures are already converted to it, and `0` is null.
pub struct View<'w, 'a>(&'w Walker<'a>);

impl View<'_, '_> {
    pub fn u8(&self, addr: u32) -> Option<u8> {
        self.0.rd(addr, 1).ok().map(|v| v as u8)
    }
    pub fn u16(&self, addr: u32) -> Option<u16> {
        self.0.rd(addr, 2).ok().map(|v| v as u16)
    }
    pub fn u32(&self, addr: u32) -> Option<u32> {
        self.0.rd(addr, 4).ok().map(|v| v as u32)
    }
    pub fn f32(&self, addr: u32) -> Option<f32> {
        self.u32(addr).map(f32::from_bits)
    }
    /// A pointer-sized slot.
    pub fn ptr(&self, addr: u32) -> Option<u32> {
        self.0.rdp(addr).ok().map(|v| v as u32)
    }
    /// `n` bytes at `addr` (`None` when they are outside the loaded memory or in a runtime block).
    pub fn bytes(&self, addr: u32, n: usize) -> Option<&[u8]> {
        let (b, o) = self.0.decode(addr, n).ok()?;
        self.0.mem[b].get(o..o + n)
    }
    pub fn cstr(&self, addr: u32) -> Option<String> {
        if addr == 0 {
            return None;
        }
        self.0.rd_cstr(addr).ok().map(|v| String::from_utf8_lossy(&v).into_owned())
    }
    /// Offset of a structure's member, from the schema.
    pub fn member(&self, structure: &str, member: &str) -> Option<u32> {
        let s = &self.0.s.structs[*self.0.s.by_name.get(structure)?];
        s.members.iter().find(|m| m.name == member).map(|m| m.off)
    }
    pub fn struct_size(&self, structure: &str) -> Option<u32> {
        Some(self.0.s.structs[*self.0.s.by_name.get(structure)?].size)
    }
    /// The id of the asset a pointer slot designates, once that asset was loaded (the slot of an asset reference).
    pub fn asset_id_at_slot(&self, slot: u32) -> Option<u32> {
        self.0.ref_id.get(&slot).copied()
    }
    /// Index in the zone's asset list of the entry being loaded (nested assets report their holder's).
    pub fn top_index(&self) -> usize {
        self.0.top_index
    }
    /// The stream bytes of the entry being loaded, from its start to the current position (converter addition: for assets whose
    /// stream layout is the same in both games and that hold no reusable pointers, the stream is the QA stream).
    pub fn entry_stream(&self) -> &[u8] {
        &self.0.img[self.0.top_start..self.0.pos]
    }
    /// The zone's script strings (bone names and the like are indices into this list).
    pub fn script_strings(&self) -> Vec<String> {
        let (arr, n) = self.0.script_strings;
        (0..n).map(|i| self.ptr(arr + (i * self.0.psz) as u32).and_then(|p| self.cstr(p)).unwrap_or_default()).collect()
    }
}



fn align_up(v: usize, a: usize) -> usize {
    if a > 1 { v.div_ceil(a) * a } else { v }
}

fn is_array_eval(e: &Expr) -> bool {
    match e {
        Expr::Num(n) => *n > 1,
        _ => true,
    }
}

impl<'a> Walker<'a> {
    fn new(game: Game, zone: &'a ZoneImage) -> Walker<'a> {
        let s = Schema::get(game);
        let n = s.blocks.len();
        let mut sizes = vec![0usize; n];
        let mut mem = Vec::new();
        for i in 0..n {
            sizes[i] = zone.block_sizes[i] as usize;
            mem.push(if s.blocks[i].1 == 0 || s.blocks[i].1 == 3 { vec![0u8; sizes[i]] } else { Vec::new() });
        }
        Walker {
            s,
            game,
            shift: s.shift,
            psz: s.ptr,
            sshift: s.shift,
            following: if s.ptr == 8 { u64::MAX } else { 0xFFFF_FFFF },
            mask: (1u32 << s.shift) - 1,
            img: &zone.image,
            pos: 8 + 4 * n,
            sizes,
            mem,
            off: vec![0; n],
            stack: Vec::new(),
            temp_stack: Vec::new(),
            temp_peak: 0,
            scopes: Vec::new(),
            depth: 0,
            reuse_cache: HashMap::new(),
            data: T6Data::default(),
            last_header: None,
            assets: Vec::new(),
            path: Vec::new(),
            fail_path: String::new(),
            hook: None,
            next_id: 1,
            id_at: HashMap::new(),
            ref_id: HashMap::new(),
            script_strings: (0, 0),
            top_index: 0,
            top_start: 0,
        }
    }

    // ----- stream and blocks
    fn kind(&self, b: usize) -> u8 {
        self.s.blocks[b].1
    }

    fn bname(&self, b: usize) -> &str {
        self.s.blocks.get(b).map(|x| x.0.as_str()).unwrap_or("?")
    }

    fn enc(&self, block: usize, off: usize) -> u32 {
        (((block as u32) << self.shift) | off as u32).wrapping_add(1)
    }

    fn is_temp(&self, blk: Option<u8>) -> bool {
        blk.is_some_and(|b| self.kind(b as usize) == 0)
    }

    fn is_runtime(&self, blk: Option<u8>) -> bool {
        blk.is_some_and(|b| self.kind(b as usize) == 1)
    }

    fn push_block(&mut self, b: usize) {
        self.stack.push(b);
        if self.kind(b) == 0 {
            self.temp_stack.push(self.off[b]);
        }
    }

    fn pop_block(&mut self) {
        if let Some(b) = self.stack.pop() {
            if self.kind(b) == 0 {
                if let Some(o) = self.temp_stack.pop() {
                    self.off[b] = o;
                }
            }
        }
    }

    fn top(&self) -> Res<usize> {
        match self.stack.last() {
            Some(&b) => Ok(b),
            None => fail("no block pushed"),
        }
    }

    /// `Alloc`: aligns the top block and returns the address of the next byte.
    fn alloc(&mut self, align: i64) -> Res<u32> {
        let b = self.top()?;
        if align < 0 {
            return fail(format!("negative alignment {align}"));
        }
        self.off[b] = align_up(self.off[b], align as usize);
        if self.off[b] > self.sizes[b] {
            return fail(format!("block {} overflows its size {} (alignment {align})", self.bname(b), self.sizes[b]));
        }
        Ok(self.enc(b, self.off[b]))
    }

    /// Takes `n` bytes from the stream into the top block (runtime blocks take none and read as zero).
    fn load_n(&mut self, n: usize) -> Res<u32> {
        let b = self.top()?;
        let start = self.off[b];
        if start + n > self.sizes[b] {
            return fail(format!("block {} overflows: {} bytes at {} of {}", self.bname(b), n, start, self.sizes[b]));
        }
        match self.kind(b) {
            0 | 3 => {
                if self.pos + n > self.img.len() {
                    return fail(format!("stream ends: {} bytes wanted at {:#x} of {:#x}", n, self.pos, self.img.len()));
                }
                self.mem[b][start..start + n].copy_from_slice(&self.img[self.pos..self.pos + n]);
                self.pos += n;
            }
            1 => {}
            _ => return fail(format!("load into delay block {}", self.bname(b))),
        }
        self.off[b] = start + n;
        if self.kind(b) == 0 && self.off[b] > self.temp_peak {
            self.temp_peak = self.off[b];
        }
        Ok(self.enc(b, start))
    }

    fn load_cstr(&mut self) -> Res<()> {
        let b = self.top()?;
        if self.kind(b) != 0 && self.kind(b) != 3 {
            return fail("string into a runtime block");
        }
        loop {
            if self.pos >= self.img.len() {
                return fail("stream ends inside a string");
            }
            if self.off[b] >= self.sizes[b] {
                return fail(format!("block {} overflows inside a string", self.bname(b)));
            }
            let c = self.img[self.pos];
            self.pos += 1;
            self.mem[b][self.off[b]] = c;
            self.off[b] += 1;
            if c == 0 {
                break;
            }
        }
        if self.kind(b) == 0 && self.off[b] > self.temp_peak {
            self.temp_peak = self.off[b];
        }
        Ok(())
    }

    /// `InsertPointerNative`: a pointer-sized slot in the insert block.
    fn insert_slot(&mut self) -> Res<u32> {
        let b = self.s.insert;
        self.off[b] = align_up(self.off[b], self.psz);
        if self.off[b] + self.psz > self.sizes[b] {
            return fail("insert block overflows");
        }
        let a = self.enc(b, self.off[b]);
        self.off[b] += self.psz;
        Ok(a)
    }

    // ----- memory
    fn decode(&self, addr: u32, n: usize) -> Res<(usize, usize)> {
        if addr == 0 {
            return fail("null pointer dereferenced");
        }
        let p = addr - 1;
        let b = (p >> self.shift) as usize;
        let o = (p & self.mask) as usize;
        if b >= self.sizes.len() || o + n > self.sizes[b] {
            return fail(format!("pointer {addr:#x} (block {b}, offset {o}) is outside its block"));
        }
        Ok((b, o))
    }

    fn rd(&self, addr: u32, size: usize) -> Res<u64> {
        let (b, o) = self.decode(addr, size)?;
        if self.mem[b].is_empty() {
            return Ok(0);
        }
        let mut v = 0u64;
        for i in 0..size {
            v |= (self.mem[b][o + i] as u64) << (8 * i);
        }
        Ok(v)
    }

    fn rd32(&self, addr: u32) -> Res<u32> {
        Ok(self.rd(addr, 4)? as u32)
    }

    /// A pointer slot (4 or 8 bytes): the value as the stream or an earlier conversion left it.
    fn rdp(&self, addr: u32) -> Res<u64> {
        self.rd(addr, self.psz)
    }

    fn zpt(&self, p: u64) -> u8 {
        if p == self.following {
            1
        } else if p == self.following - 1 {
            2
        } else {
            0
        }
    }

    fn wrp(&mut self, addr: u32, v: u32) -> Res {
        let (b, o) = self.decode(addr, self.psz)?;
        if self.mem[b].is_empty() {
            return Ok(());
        }
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&v.to_le_bytes());
        self.mem[b][o..o + self.psz].copy_from_slice(&bytes[..self.psz]);
        Ok(())
    }


    fn rd_cstr(&self, addr: u32) -> Res<Vec<u8>> {
        let (b, o) = self.decode(addr, 1)?;
        let m = &self.mem[b];
        if m.is_empty() {
            return Ok(Vec::new());
        }
        let end = m[o..].iter().position(|&c| c == 0).map(|e| o + e).unwrap_or(m.len());
        Ok(m[o..end].to_vec())
    }

    /// An offset pointer into earlier data must point inside a block; returns the address in this walker's own encoding.
    fn check_offset(&self, p: u64, need: usize) -> Res<u32> {
        let q = p.wrapping_sub(1);
        let b = (q >> self.sshift) as usize;
        let o = (q & ((1u64 << self.sshift) - 1)) as usize;
        if b >= self.sizes.len() {
            return fail(format!("offset pointer {p:#x}: no block {b}"));
        }
        if o + need > self.sizes[b] {
            return fail(format!("offset pointer {p:#x}: offset {o} outside {} ({})", self.bname(b), self.sizes[b]));
        }
        Ok(self.enc(b, o))
    }

    // ----- expressions
    fn eval(&self, e: &Expr) -> Res<i64> {
        match e {
            Expr::Num(n) => Ok(*n),
            Expr::Op(op, a, b) => {
                let x = self.eval(a)?;
                // the generated code short-circuits && and ||
                if op == "&&" {
                    return Ok((x != 0 && self.eval(b)? != 0) as i64);
                }
                if op == "||" {
                    return Ok((x != 0 || self.eval(b)? != 0) as i64);
                }
                let y = self.eval(b)?;
                Ok(match op.as_str() {
                    "+" => x.wrapping_add(y),
                    "-" => {
                        // pointer difference: in elements of the structure the left pointer designates
                        if let Expr::Var(sid) = &**a {
                            let sz = self.s.structs[*sid].size.max(1) as i64;
                            return Ok((x - y) / sz);
                        }
                        x.wrapping_sub(y)
                    }
                    "*" => x.wrapping_mul(y),
                    "/" => {
                        if y == 0 {
                            return fail("division by zero in a count");
                        }
                        x / y
                    }
                    "%" => {
                        if y == 0 {
                            return fail("remainder by zero in a count");
                        }
                        x % y
                    }
                    "<<" => x.wrapping_shl(y as u32),
                    ">>" => x.wrapping_shr(y as u32),
                    "<" => (x < y) as i64,
                    "<=" => (x <= y) as i64,
                    ">" => (x > y) as i64,
                    ">=" => (x >= y) as i64,
                    "==" => (x == y) as i64,
                    "!=" => (x != y) as i64,
                    "&" => x & y,
                    "^" => x ^ y,
                    "|" => x | y,
                    _ => return fail(format!("operator {op}")),
                })
            }
            Expr::Var(sid) => Ok(self.scopes.last().map(|s| s.vars[*sid]).unwrap_or(0) as i64),
            Expr::Dyn(d) => {
                let var = self.scopes.last().map(|s| s.vars[d.root]).unwrap_or(0);
                if var == 0 {
                    return fail(format!("variable of struct {} is not set", self.s.structs[d.root].name));
                }
                let mut addr = var;
                for &(off, deref) in &d.refs {
                    addr = addr.wrapping_add(off);
                    if deref {
                        addr = self.rdp(addr)? as u32;
                    }
                }
                let mut extra = 0i64;
                for (k, ix) in d.idx.iter().enumerate() {
                    let v = self.eval(ix)?;
                    let stride: i64 = d.dims.iter().skip(k + 1).map(|&x| x as i64).product::<i64>() * d.esz as i64;
                    extra += v * stride;
                }
                let addr = (addr as i64 + extra) as u32;
                let raw = self.rd(addr, d.kind.size())?;
                Ok(match d.kind {
                    Kind::I8 => raw as u8 as i8 as i64,
                    Kind::U8 => raw as u8 as i64,
                    Kind::I16 => raw as u16 as i16 as i64,
                    Kind::U16 => raw as u16 as i64,
                    Kind::I32 => raw as u32 as i32 as i64,
                    Kind::U32 | Kind::F32 => raw as u32 as i64,
                    Kind::I64 | Kind::U64 | Kind::F64 => raw as i64,
                })
            }
        }
    }

    fn count(&self, e: &Expr) -> Res<usize> {
        let v = self.eval(e)?;
        if v < 0 || v > 0x4000_0000 {
            return fail(format!("count {v} out of range"));
        }
        Ok(v as usize)
    }

    // ----- declaration modifier computations
    fn mc_new(&self, m: &'static Member, base: u32) -> Mc {
        Mc { m, base, idx: Vec::new(), combined: 0 }
    }

    fn mc_child(&self, mc: &Mc, i: u32) -> Mc {
        let mut idx = mc.idx.clone();
        idx.push(i);
        let sizes: Vec<u32> = mc.m.mods.iter().filter_map(|m| if let Mod::Array { n, .. } = m { Some(*n) } else { None }).collect();
        let mut per_depth = vec![0i64; sizes.len()];
        let mut cur = 1i64;
        for i in (1..=sizes.len()).rev() {
            per_depth[i - 1] = cur;
            cur *= sizes[i - 1] as i64;
        }
        let mut combined = 0i64;
        for (d, &ix) in idx.iter().enumerate() {
            if d < per_depth.len() {
                combined += per_depth[d] * ix as i64;
            }
        }
        Mc { m: mc.m, base: mc.base, idx, combined }
    }

    fn decl_mod(&self, mc: &Mc) -> Option<&'static Mod> {
        mc.m.mods.get(mc.idx.len())
    }

    fn next_mod(&self, mc: &Mc) -> Option<&'static Mod> {
        mc.m.mods.get(mc.idx.len() + 1)
    }

    fn following(&self, mc: &Mc) -> &'static [Mod] {
        let s = mc.idx.len() + 1;
        if s < mc.m.mods.len() { &mc.m.mods[s..] } else { &[] }
    }

    fn ptr_count_for(&self, p: &'static Mod, combined: i64) -> Option<&'static Expr> {
        if let Mod::Ptr { count, by_idx } = p {
            if combined >= 0 && (combined as usize) < by_idx.len() {
                if let Some(e) = &by_idx[combined as usize] {
                    return Some(e);
                }
            }
            return count.as_ref();
        }
        None
    }

    fn ptr_count_is_array(&self, p: &'static Mod, combined: i64) -> bool {
        match self.ptr_count_for(p, combined) {
            Some(e) => is_array_eval(e),
            None => false,
        }
    }

    fn any_count_is_array(&self, p: &'static Mod) -> bool {
        if let Mod::Ptr { count, by_idx } = p {
            if count.as_ref().is_some_and(is_array_eval) {
                return true;
            }
            return by_idx.iter().flatten().any(is_array_eval);
        }
        false
    }

    fn is_array(&self, mc: &Mc) -> bool {
        matches!(self.decl_mod(mc), Some(Mod::Array { .. }))
    }

    fn is_dynamic_array(&self, mc: &Mc) -> bool {
        matches!(self.decl_mod(mc), Some(Mod::Array { dsize: Some(_), .. }))
    }

    fn is_single_pointer(&self, mc: &Mc) -> bool {
        match self.decl_mod(mc) {
            Some(p @ Mod::Ptr { .. }) => !self.ptr_count_is_array(p, mc.combined) && !self.following(mc).iter().any(|m| matches!(m, Mod::Ptr { .. })),
            _ => false,
        }
    }

    fn is_array_pointer(&self, mc: &Mc) -> bool {
        match self.decl_mod(mc) {
            Some(p @ Mod::Ptr { .. }) => self.ptr_count_is_array(p, mc.combined) && !self.following(mc).iter().any(|m| matches!(m, Mod::Ptr { .. })),
            _ => false,
        }
    }

    fn is_pointer_array(&self, mc: &Mc) -> bool {
        let (Some(d), Some(n)) = (self.decl_mod(mc), self.next_mod(mc)) else { return false };
        let this_array = match d {
            Mod::Ptr { .. } => self.ptr_count_is_array(d, mc.combined),
            Mod::Array { .. } => true,
        };
        if !this_array {
            return false;
        }
        matches!(n, Mod::Ptr { .. }) && !self.any_count_is_array(n)
    }

    fn mod_alignment(&self, mc: &Mc) -> i64 {
        if self.following(mc).iter().any(|m| matches!(m, Mod::Ptr { .. })) { mc.m.ma as i64 } else { mc.m.ea as i64 }
    }

    /// Size of one element after the current modifier (the `Following references` of the generated code).
    fn element_size(&self, mc: &Mc) -> usize {
        let mut cur = mc.m.es as usize;
        let f = self.following(mc);
        if f.is_empty() {
            return cur;
        }
        let ends_in_ptr = matches!(f.last(), Some(Mod::Ptr { .. }));
        if ends_in_ptr {
            cur = self.psz;
        }
        let n = if ends_in_ptr { f.len() - 1 } else { f.len() };
        for m in f[..n].iter().rev() {
            match m {
                Mod::Array { n, .. } => cur *= *n as usize,
                Mod::Ptr { .. } => cur = self.psz,
            }
        }
        cur
    }

    fn size_for_level(&self, m: &'static Member, level: usize) -> usize {
        if level == 0 {
            return self.full_size(m);
        }
        let mut cur = m.es as usize;
        let take = m.mods.len().saturating_sub(level);
        for md in m.mods.iter().rev().take(take) {
            match md {
                Mod::Ptr { .. } => cur = self.psz,
                Mod::Array { n, .. } => cur *= *n as usize,
            }
        }
        cur
    }

    fn full_size(&self, m: &'static Member) -> usize {
        let mut cur = m.es as usize;
        if matches!(m.mods.last(), Some(Mod::Ptr { .. })) {
            cur = 0;
        }
        for md in m.mods.iter().rev() {
            match md {
                Mod::Ptr { .. } => cur = self.psz,
                Mod::Array { n, .. } => cur *= *n as usize,
            }
        }
        cur
    }

    /// Address of the pointer (or element) the modifier state designates.
    fn slot(&self, mc: &Mc) -> u32 {
        let mut off = mc.m.off as usize;
        for (level, &ix) in mc.idx.iter().enumerate() {
            if ix > 0 {
                off += ix as usize * self.size_for_level(mc.m, level + 1);
            }
        }
        mc.base.wrapping_add(off as u32)
    }

    // ----- loading
    fn var_set(&mut self, sid: usize, addr: u32) {
        if let Some(s) = self.scopes.last_mut() {
            s.vars[sid] = addr;
        }
    }

    fn struct_align(&self, sid: usize, typedef: bool, decl_align: i64) -> Res<i64> {
        let st = &self.s.structs[sid];
        if !typedef {
            if let Some(e) = &st.aa {
                return self.eval(e);
            }
            return Ok(st.align as i64);
        }
        Ok(decl_align)
    }

    fn load_struct(&mut self, sid: usize, at_start: bool, addr: u32) -> Res {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return fail("structure nesting too deep");
        }
        let r = self.load_struct_inner(sid, at_start, addr);
        self.depth -= 1;
        r
    }

    fn load_struct_inner(&mut self, sid: usize, at_start: bool, addr: u32) -> Res {
        let st = &self.s.structs[sid];
        self.var_set(sid, addr);
        let has_dyn = st.dynoff >= 0;
        if has_dyn && st.union {
            // the members load themselves
        } else if at_start {
            let n = if has_dyn { st.dynoff as usize } else { st.size as usize };
            self.load_n(n)?;
        }
        let pushed = if st.asset {
            self.push_block(self.s.default_normal);
            true
        } else if let Some(b) = st.blk {
            self.push_block(b as usize);
            true
        } else {
            false
        };
        let r = self.load_members(sid, addr);
        if pushed {
            self.pop_block();
        }
        r
    }

    fn load_members(&mut self, sid: usize, addr: u32) -> Res {
        let st = &self.s.structs[sid];
        if st.union {
            let mut matched = false;
            for m in &st.members {
                if m.ign || !m.need {
                    continue;
                }
                if matched {
                    break;
                }
                let ok = match &m.cond {
                    Some(c) => self.eval(c)? != 0,
                    None => true,
                };
                if ok {
                    matched = true;
                    let mc = self.mc_new(m, addr);
                    self.load_member_reference(&mc)?;
                }
            }
        } else {
            for m in &st.members {
                if m.ign || !m.need {
                    continue;
                }
                if let Some(c) = &m.cond {
                    if self.eval(c)? == 0 {
                        continue;
                    }
                }
                let mc = self.mc_new(m, addr);
                self.load_member_reference(&mc)?;
            }
        }
        Ok(())
    }

    fn load_member_reference(&mut self, mc: &Mc) -> Res {
        if self.is_dynamic_array(mc) {
            self.load_member_block(mc, Load::DynamicArray)
        } else if self.is_single_pointer(mc) {
            self.load_member_block(mc, Load::SinglePointer)
        } else if self.is_array_pointer(mc) {
            self.load_member_block(mc, Load::ArrayPointer)
        } else if self.is_pointer_array(mc) {
            self.load_member_block(mc, Load::PointerArray)
        } else if self.is_array(mc) && self.next_mod(mc).is_none() {
            self.load_member_block(mc, Load::EmbeddedArray)
        } else if self.decl_mod(mc).is_none() {
            self.load_member_block(mc, Load::Embedded)
        } else if let Some(Mod::Array { n, .. }) = self.decl_mod(mc) {
            for i in 0..*n {
                let child = self.mc_child(mc, i);
                self.load_member_reference(&child)?;
            }
            Ok(())
        } else {
            fail(format!("cannot load member {}", mc.m.name))
        }
    }

    fn load_member_block(&mut self, mc: &Mc, lt: Load) -> Res {
        let pushed = match mc.m.blk {
            Some(b) if b as usize != self.s.default_normal => {
                self.push_block(b as usize);
                true
            }
            _ => false,
        };
        self.path.push((self.path.len(), mc.m.name.as_str()));
        let r = self.load_member_pointer_check(mc, lt);
        if r.is_err() && self.fail_path.is_empty() {
            self.fail_path = self.path.iter().map(|p| p.1).collect::<Vec<_>>().join(".");
        }
        self.path.pop();
        if pushed {
            self.pop_block();
        }
        r
    }

    fn should_make_pointer_check(&self, mc: &Mc, lt: Load) -> bool {
        match lt {
            Load::PointerArray => !self.is_array(mc),
            Load::ArrayPointer | Load::SinglePointer => !mc.m.is_string,
            _ => false,
        }
    }

    fn load_member_pointer_check(&mut self, mc: &Mc, lt: Load) -> Res {
        if self.should_make_pointer_check(mc, lt) {
            let p = self.rdp(self.slot(mc))?;
            if p == 0 {
                return Ok(());
            }
        }
        self.load_member_reuse(mc, lt)
    }

    fn should_make_reuse(&self, mc: &Mc, lt: Load) -> bool {
        match lt {
            Load::ArrayPointer | Load::SinglePointer => true,
            Load::PointerArray => !self.is_array(mc),
            _ => false,
        }
    }

    fn load_member_reuse(&mut self, mc: &Mc, lt: Load) -> Res {
        if !self.should_make_reuse(mc, lt) || !mc.m.reusable {
            return self.load_member_alloc(mc, lt);
        }
        let slot = self.slot(mc);
        let p = self.rdp(slot)?;
        let zt = self.zpt(p);
        let in_temp = self.is_temp(mc.m.blk);
        if in_temp {
            if zt == 1 || zt == 2 {
                self.load_member_alloc(mc, lt)
            } else {
                // ConvertOffsetToAliasNative: the slot at that offset holds the real pointer
                let a = self.check_offset(p, self.psz)?;
                let v = self.rdp(a)? as u32;
                if let Some(&id) = self.id_at.get(&a) {
                    self.ref_id.insert(slot, id);
                }
                self.wrp(slot, v)
            }
        } else if zt == 1 {
            self.load_member_alloc(mc, lt)
        } else {
            let a = self.check_offset(p, 1)?;
            self.wrp(slot, a)
        }
    }

    fn should_make_alloc(&self, mc: &Mc, lt: Load) -> bool {
        match lt {
            Load::PointerArray => !self.is_array(mc),
            Load::ArrayPointer | Load::SinglePointer => !(mc.m.is_string || mc.m.ty.is_some_and(|t| self.s.structs[t].asset)),
            _ => false,
        }
    }

    fn load_member_alloc(&mut self, mc: &Mc, lt: Load) -> Res {
        if !self.should_make_alloc(mc, lt) {
            return self.load_member_type_check(mc, lt);
        }
        let slot = self.slot(mc);
        let p = self.rdp(slot)?;
        let zt = self.zpt(p);
        let align = match &mc.m.aa {
            Some(e) => self.eval(e)?,
            None => self.mod_alignment(mc),
        };
        let a = self.alloc(align)?;
        self.wrp(slot, a)?;
        let in_temp = self.is_temp(mc.m.blk);
        let mut to_insert = 0u32;
        if in_temp && zt == 2 {
            to_insert = self.insert_slot()?;
        }
        self.load_member_type_check(mc, lt)?;
        if in_temp && zt == 2 {
            let v = self.rdp(slot)? as u32;
            self.wrp(to_insert, v)?;
        }
        // converter addition: model surfaces are announced like assets
        if self.hook.is_some() && in_temp {
            if let Some(t) = mc.m.ty.filter(|&t| self.s.structs[t].name == "XModelSurfs") {
                self.notify_asset(t, a, slot, to_insert);
            }
        }
        Ok(())
    }

    fn load_member_type_check(&mut self, mc: &Mc, lt: Load) -> Res {
        let m = mc.m;
        if m.is_string {
            let slot = self.slot(mc);
            match lt {
                Load::SinglePointer => self.load_xstring(slot),
                Load::PointerArray => {
                    if self.is_array(mc) {
                        let n = match self.decl_mod(mc) {
                            Some(Mod::Array { n, .. }) => *n as usize,
                            _ => 0,
                        };
                        self.load_xstring_array(false, slot, n)
                    } else {
                        let n = match self.decl_mod(mc).and_then(|d| self.ptr_count_for(d, mc.combined)) {
                            Some(e) => self.count(e)?,
                            None => 1,
                        };
                        let arr = self.rdp(slot)? as u32;
                        self.load_xstring_array(true, arr, n)
                    }
                }
                _ => fail(format!("unsupported load type for string {}", m.name)),
            }
        } else if let Some(t) = m.ty.filter(|&t| self.s.structs[t].asset) {
            match lt {
                Load::SinglePointer => {
                    let slot = self.slot(mc);
                    self.load_asset_ptr(t, slot)
                }
                Load::PointerArray => self.load_member_pointer_array(mc),
                _ => fail("asset member load type"),
            }
        } else {
            match lt {
                Load::ArrayPointer => self.load_member_array_pointer(mc),
                Load::SinglePointer => self.load_member_single_pointer(mc),
                Load::Embedded => self.load_member_embedded(mc),
                Load::PointerArray => self.load_member_pointer_array(mc),
                Load::DynamicArray => self.load_member_dynamic_array(mc),
                Load::EmbeddedArray => self.load_member_embedded_array(mc),
            }
        }
    }

    fn in_runtime_block(&self, m: &Member) -> bool {
        self.is_runtime(m.blk)
    }

    fn load_member_array_pointer(&mut self, mc: &Mc) -> Res {
        let m = mc.m;
        let cnt = match self.decl_mod(mc).and_then(|d| self.ptr_count_for(d, mc.combined)) {
            Some(e) => self.count(e)?,
            None => 1,
        };
        let slot = self.slot(mc);
        let p = self.rdp(slot)? as u32;
        if let Some(t) = m.ty.filter(|&t| !self.s.structs[t].leaf) {
            if !self.in_runtime_block(m) {
                return self.load_array(t, true, p, cnt);
            }
        }
        let es = self.element_size(mc);
        self.load_n(es * cnt)?;
        Ok(())
    }

    fn load_member_single_pointer(&mut self, mc: &Mc) -> Res {
        let m = mc.m;
        let slot = self.slot(mc);
        let p = self.rdp(slot)? as u32;
        if let Some(t) = m.ty.filter(|&t| !self.s.structs[t].leaf) {
            if !self.in_runtime_block(m) {
                return self.load_struct(t, true, p);
            }
        }
        let es = self.element_size(mc);
        self.load_n(es)?;
        Ok(())
    }

    fn load_member_embedded(&mut self, mc: &Mc) -> Res {
        let m = mc.m;
        let slot = self.slot(mc);
        if !m.leaf {
            if let Some(t) = m.ty {
                return self.load_struct(t, m.after_partial, slot);
            }
        }
        if m.after_partial {
            let n = self.full_size(m);
            self.load_n(n)?;
        }
        Ok(())
    }

    fn load_member_embedded_array(&mut self, mc: &Mc) -> Res {
        let m = mc.m;
        let slot = self.slot(mc);
        let (n_static, dcount) = match self.decl_mod(mc) {
            Some(Mod::Array { n, dcount, .. }) => (*n as usize, dcount.as_ref()),
            _ => (0, None),
        };
        let n = match dcount {
            Some(e) => self.count(e)?,
            None => n_static,
        };
        if !m.leaf {
            if let Some(t) = m.ty {
                return self.load_array(t, m.after_partial, slot, n);
            }
        }
        if m.after_partial {
            let es = self.element_size(mc);
            self.load_n(es * n)?;
        }
        Ok(())
    }

    fn load_member_dynamic_array(&mut self, mc: &Mc) -> Res {
        let m = mc.m;
        let slot = self.slot(mc);
        let n = match self.decl_mod(mc) {
            Some(Mod::Array { dsize: Some(e), .. }) => self.count(e)?,
            _ => 0,
        };
        if let Some(t) = m.ty.filter(|&t| !self.s.structs[t].leaf) {
            return self.load_array(t, true, slot, n);
        }
        let es = self.element_size(mc);
        self.load_n(es * n)?;
        Ok(())
    }

    fn load_member_pointer_array(&mut self, mc: &Mc) -> Res {
        let m = mc.m;
        let slot = self.slot(mc);
        let (arr, at_start, n) = if self.is_array(mc) {
            let (ns, dc) = match self.decl_mod(mc) {
                Some(Mod::Array { n, dcount, .. }) => (*n as usize, dcount.as_ref()),
                _ => (0, None),
            };
            let n = match dc {
                Some(e) => self.count(e)?,
                None => ns,
            };
            (slot, false, n)
        } else {
            let n = match self.decl_mod(mc).and_then(|d| self.ptr_count_for(d, mc.combined)) {
                Some(e) => self.count(e)?,
                None => 1,
            };
            (self.rdp(slot)? as u32, true, n)
        };
        self.load_ptr_array(m, arr, at_start, n)
    }

    /// `LoadArray_T`: the elements are taken in one piece, then each one's members are loaded.
    fn load_array(&mut self, sid: usize, at_start: bool, addr: u32, count: usize) -> Res {
        let size = self.s.structs[sid].size;
        if at_start {
            self.load_n(size as usize * count)?;
        }
        for i in 0..count {
            self.load_struct(sid, false, addr.wrapping_add(size * i as u32))?;
        }
        Ok(())
    }

    /// The pointer-array element loaders of an asset are reusable when any member of that asset says so.
    fn reuse_set(&mut self, root: usize) -> &HashSet<String> {
        if !self.reuse_cache.contains_key(&root) {
            let mut set = HashSet::new();
            let mut seen = HashSet::new();
            let mut work = vec![root];
            seen.insert(root);
            let s = self.s;
            while let Some(sid) = work.pop() {
                for m in &s.structs[sid].members {
                    if m.ign {
                        continue;
                    }
                    let ptrs = m.mods.iter().filter(|x| matches!(x, Mod::Ptr { .. })).count();
                    let last_single = match m.mods.last() {
                        Some(p @ Mod::Ptr { .. }) => !self.any_count_is_array(p),
                        _ => false,
                    };
                    let first_array = matches!(m.mods.first(), Some(Mod::Array { .. }));
                    let pa = last_single && ((first_array && ptrs == 1) || (!first_array && ptrs == 2));
                    if pa && !m.is_string && m.reusable {
                        set.insert(m.tk.clone());
                    }
                    if let Some(t) = m.ty {
                        let runtime = m.blk.is_some_and(|b| s.blocks[b as usize].1 == 1);
                        if !s.structs[t].asset && !runtime && seen.insert(t) {
                            work.push(t);
                        }
                    }
                }
            }
            self.reuse_cache.insert(root, set);
        }
        &self.reuse_cache[&root]
    }

    /// `LoadPtrArray_T`.
    fn load_ptr_array(&mut self, m: &'static Member, arr: u32, at_start: bool, count: usize) -> Res {
        if at_start {
            self.load_n(self.psz * count)?;
        }
        let asset = m.ty.filter(|&t| self.s.structs[t].asset);
        let root = self.scopes.last().map(|s| s.root).unwrap_or(0);
        let reusable = !asset.is_some() && self.reuse_set(root).contains(&m.tk);
        for i in 0..count {
            let slot = arr.wrapping_add((self.psz * i) as u32);
            let p = self.rdp(slot)?;
            if p == 0 {
                continue;
            }
            if let Some(t) = asset {
                self.load_asset_ptr(t, slot)?;
                continue;
            }
            if reusable && self.zpt(p) != 1 {
                let a = self.check_offset(p, 1)?;
                self.wrp(slot, a)?;
                continue;
            }
            // Alloc<T>(alignment of the type), then the element
            let align = match m.ty {
                Some(t) if !m.typedef_base => self.struct_align(t, false, 0)?,
                _ => m.ea as i64,
            };
            let a = self.alloc(align)?;
            self.wrp(slot, a)?;
            match m.ty.filter(|&t| !self.s.structs[t].leaf) {
                Some(t) => self.load_struct(t, true, a)?,
                None => {
                    self.load_n(m.es as usize)?;
                }
            }
        }
        Ok(())
    }

    fn load_xstring(&mut self, slot: u32) -> Res {
        let p = self.rdp(slot)?;
        if p == 0 {
            return Ok(());
        }
        if p == self.following {
            let a = self.alloc(1)?;
            self.wrp(slot, a)?;
            self.load_cstr()
        } else {
            let a = self.check_offset(p, 1)?;
            self.wrp(slot, a)
        }
    }

    fn load_xstring_array(&mut self, at_start: bool, arr: u32, count: usize) -> Res {
        if at_start {
            self.load_n(self.psz * count)?;
        }
        for i in 0..count {
            self.load_xstring(arr.wrapping_add((self.psz * i) as u32))?;
        }
        Ok(())
    }

    /// Gives a finished asset an id, remembers what later references to it look like and tells the hook.
    fn notify_asset(&mut self, sid: usize, hdr: u32, slot: u32, key: u32) {
        let id = self.next_id;
        self.next_id += 1;
        self.ref_id.insert(slot, id);
        // later references name the asset by its slot in the asset list or by the insert slot, which hold the real pointer
        self.id_at.insert(slot, id);
        if key != 0 {
            self.id_at.insert(key, id);
        }
        if let Some(h) = self.hook.take() {
            h(&View(self), self.s.structs[sid].name.as_str(), hdr, id);
            self.hook = Some(h);
        }
    }

    /// `Loader_T::Load`: an asset pointer at `slot`. The loader instance has its own member variables.
    fn load_asset_ptr(&mut self, sid: usize, slot: u32) -> Res {
        let p = self.rdp(slot)?;
        if p == 0 {
            return Ok(());
        }
        let st = &self.s.structs[sid];
        let in_temp = self.is_temp(st.blk);
        if in_temp {
            self.push_block(self.s.default_temp);
        }
        let zt = self.zpt(p);
        let r = (|| -> Res {
            if zt == 1 || (in_temp && zt == 2) {
                let align = self.struct_align(sid, false, 0)?;
                let hdr = self.alloc(align)?;
                self.wrp(slot, hdr)?;
                let mut to_insert = 0;
                if in_temp && zt == 2 {
                    to_insert = self.insert_slot()?;
                }
                self.scopes.push(Scope { vars: vec![0; self.s.structs.len()], root: sid });
                let r = self.load_struct(sid, true, hdr);
                if r.is_ok() {
                    self.last_header = Some((sid, hdr));
                    self.record_asset_content(sid, hdr)?;
                    if self.hook.is_some() {
                        self.notify_asset(sid, hdr, slot, if in_temp { to_insert } else { hdr });
                    }
                }
                self.scopes.pop();
                r?;
                if in_temp && zt == 2 {
                    // the real loader stores the pointer of the finished asset
                    self.wrp(to_insert, hdr)?;
                }
                Ok(())
            } else if in_temp {
                let a = self.check_offset(p, self.psz)?;
                let v = self.rdp(a)? as u32;
                self.wrp(slot, v)?;
                if let Some(&id) = self.id_at.get(&a) {
                    self.ref_id.insert(slot, id);
                }
                Ok(())
            } else {
                let a = self.check_offset(p, 1)?;
                self.wrp(slot, a)?;
                if let Some(&id) = self.id_at.get(&a) {
                    self.ref_id.insert(slot, id);
                }
                Ok(())
            }
        })();
        if in_temp {
            self.pop_block();
        }
        r
    }

    // ----- recording
    fn member_named(&self, sid: usize, name: &str) -> Option<&'static Member> {
        self.s.structs[sid].members.iter().find(|m| m.name == name)
    }

    fn read_str_field(&self, sid: usize, hdr: u32, name: &str) -> Res<Option<String>> {
        let Some(m) = self.member_named(sid, name) else { return Ok(None) };
        let p = self.rdp(hdr + m.off)? as u32;
        if p == 0 || self.zpt(p as u64) != 0 {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&self.rd_cstr(p)?).into_owned()))
    }

    /// The name an asset header carries: its member called `name`, otherwise its first member (a string, or a structure
    /// that starts with one, as in `Material`).
    fn asset_name(&self, sid: usize, hdr: u32, depth: usize) -> Option<String> {
        let st = &self.s.structs[sid];
        let m = st.members.iter().find(|m| m.name == "name" && m.is_string).or_else(|| st.members.first())?;
        if m.is_string {
            let p = self.rdp(hdr + m.off).ok()? as u32;
            if p == 0 || self.zpt(p as u64) != 0 {
                return None;
            }
            return Some(String::from_utf8_lossy(&self.rd_cstr(p).ok()?).into_owned());
        }
        match m.ty {
            Some(t) if depth < 3 && !m.mods.iter().any(|x| matches!(x, Mod::Ptr { .. })) => self.asset_name(t, hdr + m.off, depth + 1),
            _ => None,
        }
    }

    fn read_u32_field(&self, sid: usize, hdr: u32, name: &str) -> Res<u32> {
        let Some(m) = self.member_named(sid, name) else { return fail(format!("no member {name}")) };
        self.rd32(hdr + m.off)
    }

    fn read_ptr_field(&self, sid: usize, hdr: u32, name: &str) -> Res<u32> {
        let Some(m) = self.member_named(sid, name) else { return fail(format!("no member {name}")) };
        Ok(self.rdp(hdr + m.off)? as u32)
    }

    fn record_asset_content(&mut self, sid: usize, hdr: u32) -> Res {
        let name = self.s.structs[sid].name.as_str();
        match name {
            "LocalizeEntry" => {
                let v = self.read_str_field(sid, hdr, "value")?.unwrap_or_default();
                let n = self.read_str_field(sid, hdr, "name")?.unwrap_or_default();
                self.data.localize.push((n, v));
            }
            "RawFile" | "ScriptFile" => {
                let n = self.read_str_field(sid, hdr, "name")?.unwrap_or_default();
                // the data is a `buffer` pointer, or a union (`data`) whose first word is that pointer
                let buf = match self.member_named(sid, "buffer") {
                    Some(m) => self.rdp(hdr + m.off)? as u32,
                    None => match self.member_named(sid, "data") {
                        Some(m) => self.rdp(hdr + m.off)? as u32,
                        None => 0,
                    },
                };
                let len = self.read_u32_field(sid, hdr, "len")? as usize;
                let clen = if self.member_named(sid, "compressedLen").is_some() { self.read_u32_field(sid, hdr, "compressedLen")? as usize } else { 0 };
                let mut bytes = Vec::new();
                if buf != 0 {
                    let take = if clen > 0 { clen } else { len };
                    if take > 0 {
                        let (b, o) = self.decode(buf, take)?;
                        let stored = &self.mem[b][o..o + take];
                        bytes = stored.to_vec();
                        if clen > 0 && clen != len {
                            use std::io::Read;
                            let mut out = Vec::with_capacity(len);
                            if flate2::read::ZlibDecoder::new(stored).read_to_end(&mut out).is_ok() {
                                bytes = out;
                            }
                        }
                    }
                }
                self.data.rawfiles.push((n, bytes));
            }
            "StringTable" => {
                let n = self.read_str_field(sid, hdr, "name")?.unwrap_or_default();
                let cols = self.read_u32_field(sid, hdr, "columnCount")? as usize;
                let rows = self.read_u32_field(sid, hdr, "rowCount")? as usize;
                let vals = self.read_ptr_field(sid, hdr, "values")?;
                // `values` is an array of cells (string pointer, hash) or of plain string pointers
                let stride = self.member_named(sid, "values").and_then(|m| m.ty).map(|t| self.s.structs[t].size).unwrap_or(self.psz as u32);
                let mut cells = Vec::new();
                if vals != 0 {
                    for i in 0..cols * rows {
                        let sp = self.rdp(vals + stride * i as u32)? as u32;
                        cells.push(if sp == 0 { String::new() } else { String::from_utf8_lossy(&self.rd_cstr(sp)?).into_owned() });
                    }
                }
                self.data.stringtables.push(StringTableRec { name: n, columns: cols, rows, cells });
            }
            _ => {}
        }
        Ok(())
    }

    // ----- the zone
    fn run(&mut self, zone_asset_types: &mut Vec<(usize, u32)>, report: &mut WalkReport) -> Res {
        // XAssetList: loaded without a block
        let base = 8 + 4 * self.s.blocks.len();
        let psz = self.psz;
        // (count, pointer) pairs: the strings, [the zones depended on,] the assets
        let pairs = if self.s.has_depends { 3 } else { 2 };
        let list_len = 2 * psz * pairs;
        if base + list_len > self.img.len() {
            return fail("zone too short for the asset list");
        }
        let count_at = |i: usize| u32::from_le_bytes(self.img[base + 2 * psz * i..base + 2 * psz * i + 4].try_into().unwrap()) as usize;
        let ptr_at = |i: usize| {
            let o = base + 2 * psz * i + psz;
            let mut b = [0u8; 8];
            b[..psz].copy_from_slice(&self.img[o..o + psz]);
            u64::from_le_bytes(b)
        };
        let (string_count, strings) = (count_at(0), ptr_at(0));
        let (depend_count, depends) = if self.s.has_depends { (count_at(1), ptr_at(1)) } else { (0, 0) };
        let (asset_count, assets) = (count_at(pairs - 1), ptr_at(pairs - 1));
        self.pos = base + list_len;
        self.push_block(self.s.default_normal);
        if strings != 0 {
            if strings != self.following {
                return fail("string list pointer is not -1");
            }
            let arr = self.alloc(psz as i64)?;
            self.load_xstring_array(true, arr, string_count)?;
            self.script_strings = (arr, string_count);
        }
        if depends != 0 {
            let arr = self.alloc(psz as i64)?;
            self.load_xstring_array(true, arr, depend_count)?;
        }
        report.assets_total = asset_count;
        if assets != 0 {
            let arr = self.alloc(psz as i64)?;
            self.load_n(2 * psz * asset_count)?;
            for i in 0..asset_count {
                let entry = arr + (2 * psz * i) as u32;
                let ty = self.rd32(entry)?;
                let slot = entry + psz as u32;
                let start = self.pos;
                zone_asset_types.push((i, ty));
                self.top_index = i;
                self.top_start = start;
                if self.s.skip.contains(&ty) {
                    self.assets.push(WalkedAsset { index: i, ty, name: String::new(), reference: true, start, end: self.pos });
                    report.assets_walked = i + 1;
                    continue;
                }
                let Some(&sid) = self.s.assets.get(&ty) else {
                    return fail(format!("unsupported asset type {ty}"));
                };
                let p = self.rdp(slot)?;
                self.last_header = None;
                self.scopes.push(Scope { vars: vec![0; self.s.structs.len()], root: sid });
                let r = self.load_asset_ptr(sid, slot);
                self.scopes.pop();
                r?;
                let mut name = String::new();
                let reference = self.zpt(p) == 0;
                if let Some((hs, hdr)) = self.last_header {
                    if hs == sid {
                        name = self.asset_name(sid, hdr, 0).unwrap_or_default();
                    }
                }
                self.assets.push(WalkedAsset { index: i, ty, name, reference, start, end: self.pos });
                report.assets_walked = i + 1;
            }
        }
        self.pop_block();
        Ok(())
    }
}


/// Walks all assets of a zone.
pub fn walk(game: Game, zone: &ZoneImage) -> WalkReport {
    walk_with(game, zone, None)
}

/// Walks a zone and hands every asset to `hook` as soon as it is loaded (nested assets, such as the materials of a model,
/// come before the asset that holds them), while its structures and everything they point at can still be read.
pub fn walk_with<'a>(game: Game, zone: &'a ZoneImage, hook: Option<&'a mut AssetHook<'a>>) -> WalkReport {
    let schema = Schema::get(game);
    let mut report = WalkReport {
        zone: zone.name.clone(),
        assets_total: 0,
        assets_walked: 0,
        failure: None,
        consumed: 0,
        image_len: zone.image.len(),
        block_end: Vec::new(),
        block_sizes: zone.block_sizes.clone(),
        block_names: schema.blocks.iter().map(|b| b.0.clone()).collect(),
        temp_peak: 0,
        exact: false,
        mismatches: Vec::new(),
        assets: Vec::new(),
        data: T6Data::default(),
    };
    let mut w = Walker::new(game, zone);
    w.hook = hook;
    let mut types = Vec::new();
    let r = w.run(&mut types, &mut report);
    report.consumed = w.pos;
    report.block_end = w.off.clone();
    report.temp_peak = w.temp_peak;
    if let Err(Fail(reason)) = r {
        let (index, ty) = types.last().copied().unwrap_or((0, 0));
        let reason = if w.fail_path.is_empty() { reason } else { format!("{reason} [in {}]", w.fail_path) };
        report.failure = Some(WalkFailure { asset_index: index, ty, offset: w.pos, reason });
    } else {
        if report.consumed != report.image_len {
            report.mismatches.push(format!("stream used up to {:#x} of {:#x} bytes", report.consumed, report.image_len));
        }
        for b in 0..schema.blocks.len() {
            let want = report.block_sizes[b] as usize;
            if schema.blocks[b].1 == 0 {
                if report.temp_peak > want {
                    report.mismatches.push(format!("{} peaks at {} (header says {})", schema.blocks[b].0, report.temp_peak, want));
                }
            } else if report.block_end[b] != want {
                report.mismatches.push(format!("{} ends at {} (header says {})", schema.blocks[b].0, report.block_end[b], want));
            }
        }
        report.exact = report.mismatches.is_empty();
    }
    report.assets = std::mem::take(&mut w.assets);
    report.data = std::mem::take(&mut w.data);
    report
}

// ---------------------------------------------------------------------------------------------------------------- command
fn collect_zones(path: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    if path.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(path)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            collect_zones(&e, out)?;
        }
    } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("ff")) {
        out.push(path.to_path_buf());
    }
    Ok(())
}

/// Opens and walks one zone file.
pub fn walk_file(game: Game, path: &std::path::Path) -> Result<WalkReport> {
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let zone = ZoneImage::open(game, &name, &bytes).with_context(|| format!("opening {}", path.display()))?;
    Ok(walk(game, &zone))
}

/// `atlas --zonewalk GAME FILE|FOLDER [-v] [--dump DIR]` (and `--t6walk`): walks every asset of the zones and reports whether the
/// stream and all blocks come out exactly.
pub fn command(game: Option<Game>, args: &[String]) -> Result<()> {
    let mut args = args.to_vec();
    let game = match game {
        Some(g) => g,
        None => {
            if args.is_empty() {
                anyhow::bail!("usage: atlas --zonewalk iw3|iw4|iw5|t6 FILE|FOLDER [-v] [--dump DIR]");
            }
            let g = args.remove(0);
            Game::parse(&g).with_context(|| format!("unknown game {g:?} (iw3, iw4, iw5 or t6)"))?
        }
    };
    let args = &args[..];
    let verbose = args.iter().any(|a| a == "-v");
    let dump = args.iter().position(|a| a == "--dump").and_then(|i| args.get(i + 1)).map(std::path::PathBuf::from);
    let path = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with('-') && !(*i > 0 && args[*i - 1] == "--dump"))
        .map(|(_, a)| a)
        .context("usage: atlas --zonewalk GAME FILE|FOLDER [-v] [--dump DIR]")?;
    let mut files = Vec::new();
    collect_zones(std::path::Path::new(path), &mut files)?;
    if files.is_empty() {
        anyhow::bail!("no .ff zones under {path}");
    }
    let (mut exact, mut partial, mut failed, mut unopened) = (0, 0, 0, 0);
    let (mut assets_ok, mut assets_all) = (0usize, 0usize);
    let t = std::time::Instant::now();
    for f in &files {
        let root = std::path::Path::new(path);
        let label = if root.is_dir() { f.strip_prefix(root).unwrap_or(f).to_string_lossy().replace('\\', "/") } else { f.file_name().unwrap().to_string_lossy().into_owned() };
        let rep = match walk_file(game, f) {
            Ok(r) => r,
            Err(e) => {
                println!("{label}: cannot open: {e:#}");
                unopened += 1;
                continue;
            }
        };
        assets_ok += rep.assets_walked;
        assets_all += rep.assets_total;
        match (&rep.failure, rep.exact) {
            (None, true) => {
                exact += 1;
                println!("{label}: {}/{} assets, exact ({} stream bytes, blocks {:?})", rep.assets_walked, rep.assets_total, rep.consumed, rep.block_end);
            }
            (None, false) => {
                partial += 1;
                println!("{label}: {}/{} assets walked, but {}", rep.assets_walked, rep.assets_total, rep.mismatches.join("; "));
            }
            (Some(fl), _) => {
                failed += 1;
                let tn = game.asset_type_name(fl.ty);
                println!(
                    "{label}: {}/{} assets walked; FAILED at asset {} (type {} {tn}) at offset {:#x}: {}",
                    rep.assets_walked, rep.assets_total, fl.asset_index, fl.ty, fl.offset, fl.reason
                );
            }
        }
        if let Some(dir) = &dump {
            dump_data(dir, &f.file_name().unwrap().to_string_lossy(), &rep.data)?;
        }
        if verbose {
            println!(
                "    content: {} localize entries, {} raw files ({} bytes), {} string tables ({} cells)",
                rep.data.localize.len(),
                rep.data.rawfiles.len(),
                rep.data.rawfiles.iter().map(|r| r.1.len()).sum::<usize>(),
                rep.data.stringtables.len(),
                rep.data.stringtables.iter().map(|t| t.cells.len()).sum::<usize>()
            );
            for a in &rep.assets {
                let tn = game.asset_type_name(a.ty);
                println!("    {:5} {:2} {:16} {}{} {:#x}..{:#x}", a.index, a.ty, tn, a.name, if a.reference { " (reference)" } else { "" }, a.start, a.end);
            }
        }
    }
    println!(
        "{} zones: {exact} exact, {partial} walked with block/stream mismatches, {failed} failed, {unopened} not opened; {assets_ok}/{assets_all} assets walked ({:.1}s)",
        files.len(),
        t.elapsed().as_secs_f32()
    );
    Ok(())
}

/// Writes the localize entries (`<zone>.localize.tsv`), raw files (`rawfile/<name>`) and string tables (`stringtable/<name>`, CSV).
fn dump_data(dir: &std::path::Path, zone: &str, data: &T6Data) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let stem = zone.trim_end_matches(".ff");
    if !data.localize.is_empty() {
        let mut out = String::new();
        for (name, value) in &data.localize {
            out.push_str(&format!("{name}\t{}\n", value.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t")));
        }
        std::fs::write(dir.join(format!("{stem}.localize.tsv")), out)?;
    }
    let safe = |n: &str| n.replace(['\\', ':', '*', '?', '"', '<', '>', '|'], "_").trim_start_matches('/').to_string();
    for (name, bytes) in &data.rawfiles {
        let p = dir.join("rawfile").join(safe(name));
        std::fs::create_dir_all(p.parent().unwrap())?;
        std::fs::write(p, bytes)?;
    }
    for t in &data.stringtables {
        let p = dir.join("stringtable").join(safe(&t.name));
        std::fs::create_dir_all(p.parent().unwrap())?;
        let mut out = String::new();
        for row in 0..t.rows {
            let cells: Vec<String> = (0..t.columns)
                .map(|c| {
                    let v = &t.cells[row * t.columns + c];
                    if v.contains([',', '"', '\n']) { format!("\"{}\"", v.replace('"', "\"\"")) } else { v.clone() }
                })
                .collect();
            out.push_str(&cells.join(","));
            out.push('\n');
        }
        std::fs::write(p, out)?;
    }
    Ok(())
}

/// Size and alignment of a structure in the schema (the x86 layout the zones use).
pub fn struct_layout(game: Game, name: &str) -> Option<(u32, u32)> {
    let s = Schema::get(game);
    s.by_name.get(name).map(|&i| (s.structs[i].size, s.structs[i].align))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_layouts_match_the_sizes_the_header_asserts() {
        // static_asserts of the structure definitions
        assert_eq!(struct_layout(Game::T6, "MaterialTextureDefSamplerState").map(|l| l.0), Some(1));
        assert_eq!(struct_layout(Game::T6, "GfxStateBitsLoadBits").map(|l| l.0), Some(8));
        assert_eq!(struct_layout(Game::T6, "SndAliasFlags").map(|l| l.0), Some(8));
        // every asset type in the zones maps to a structure
        for t in [1u32, 4, 5, 6, 7, 8, 9, 11, 12, 17, 23, 24, 25, 41, 42, 45] {
            assert!(Schema::get(Game::T6).assets.contains_key(&t), "asset type {t}");
        }
    }
}
