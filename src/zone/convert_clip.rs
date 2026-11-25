//! Phase 5 of the stock-to-QA zone conversion (`convert_zone`): the collision map.
//!
//! The QA collision map is laid out like the IW5 one (ZoneTool `IW5/Structs.hpp`: `clipMap_t`, `ClipInfo`, `cmodel_t`; OpenAssetTools
//! `IW5_Assets.h` and its `clipMap_t.txt` zone-code description), the stock one like the IW4 one (`IW4/Structs.hpp`: `clipMap_t`). The
//! members are the same; the differences are where they sit:
//!
//! * IW5 gathers the arrays a brush model needs (planes, materials, brush sides, brush edges, leaf-brush nodes, leaf brushes, brushes,
//!   their bounds and contents) in a `ClipInfo` (64 bytes) at +8, and keeps a pointer to a second one (`pInfo`, loaded in the
//!   temporary block and then copied) at +72. Every `cmodel_t` (72 bytes against IW4's 68) has a `ClipInfo*` at +28.
//! * The map entities' triggers (`MapTriggers`) and stages are members of the collision map, not of the map-entities record.
//! * IW4's leaf surfaces are gone.
//!
//! Here the `ClipInfo` at +8 holds all the data (inline); the second one is a header that aliases it, and the cmodels point at that
//! second one. The shared planes, brush sides, edges and leaf brushes that the stock records point into the global arrays become
//! aliases into the arrays (as in the stock zone); records that carried their own data keep it inline.

use super::convert_zone::{Hdr, Out, Rule, Src, INLINE, INSERT};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

pub fn rule_for(name: &str) -> Option<Rule> {
    Some(match name {
        "clipMap_t" => clip_map,
        _ => return None,
    })
}

/// What the collision map takes over from the map entities: the QA record (without trigger data), the trigger arrays (counts and
/// bytes) and the stage records with their names.
#[derive(Clone, Default)]
pub struct MapEntsStash {
    pub record: Vec<u8>,
    pub triggers: [(u32, Vec<u8>); 3],
    pub stages: Vec<u8>,
    pub stage_names: Vec<Option<Vec<u8>>>,
}

thread_local! {
    /// The GfxWorld's plane array of the zone being converted: (asset id, stream offset, bytes).
    static WORLD_PLANES: RefCell<Option<(u32, usize, Vec<u8>)>> = RefCell::new(None);
    static MAP_ENTS: RefCell<HashMap<u32, MapEntsStash>> = RefCell::new(HashMap::new());
    static CONSUMED: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
}

pub fn reset() {
    WORLD_PLANES.with(|w| *w.borrow_mut() = None);
    MAP_ENTS.with(|m| m.borrow_mut().clear());
    CONSUMED.with(|m| m.borrow_mut().clear());
}

pub fn world_planes(id: u32, at: usize, bytes: Vec<u8>) {
    WORLD_PLANES.with(|w| *w.borrow_mut() = Some((id, at, bytes)));
}

pub fn stash_map_ents(id: u32, st: MapEntsStash) {
    MAP_ENTS.with(|m| m.borrow_mut().insert(id, st));
}

/// Whether the collision map stored the map-entities asset `id` inline (so that it is not an asset of its own).
pub fn consumed(id: u32) -> bool {
    CONSUMED.with(|c| c.borrow().contains(&id))
}

/// An array of the stock collision map: where it starts in the stock memory, its length in bytes and where it starts in the QA
/// stream (once written).
#[derive(Clone, Copy, Default)]
struct Arr {
    base: u32,
    len: u32,
    stream: Option<usize>,
}

impl Arr {
    fn index_of(&self, addr: u32, stride: u32) -> Option<u32> {
        if self.base != 0 && addr >= self.base && addr - self.base < self.len && (addr - self.base) % stride == 0 {
            Some((addr - self.base) / stride)
        } else {
            None
        }
    }
}

struct Ctx<'a, 'b, 'c> {
    s: &'a Src<'b, 'c>,
    /// words to point at array elements once the arrays' positions are known: (word offset, array, byte offset in the array)
    pending: Vec<(usize, usize, u32)>,
    arrays: Vec<Arr>,
    /// (asset id, stream offset) of the world's plane array when the collision map's planes are the same bytes: the map then aliases it
    shared_planes: Option<(u32, usize)>,
}

const PLANES: usize = 0;
const SIDES: usize = 1;
const EDGES: usize = 2;
const LEAF_BRUSHES: usize = 3;
const BORDERS: usize = 4;

fn read(s: &Src, ptr: u32, count: usize, stride: usize, what: &str) -> Result<Vec<u8>, String> {
    if count == 0 || !s.has(ptr) {
        return Ok(Vec::new());
    }
    let a = s.array(ptr, count, stride);
    if a.len() != count * stride {
        return Err(format!("{what} outside the loaded memory"));
    }
    Ok(a)
}

/// IW3's last leaf-brush node can name the box-hull brush, index `numBrushes`, that the engine adds at run time; the QA loader's reader
/// refuses an index past the brushes, and nothing in the file refers to that node, so trailing nodes like that are left out.
fn trim_lbn(s: &Src, lbn: usize, brushes: usize) -> usize {
    let mut n = lbn;
    if lbn == 0 || !s.has(68) {
        return lbn;
    }
    let base = s.u32(68);
    while n > 0 {
        let r = s.at(base + 20 * (n as u32 - 1));
        let leaves = r.u16(2) as i16;
        let p = r.u32(8);
        let out_of_range = leaves > 0 && p != 0 && s.v.bytes(p, 2 * leaves as usize).is_some_and(|b| b.chunks(2).any(|c| u16::from_le_bytes([c[0], c[1]]) as usize >= brushes));
        if !out_of_range {
            break;
        }
        n -= 1;
    }
    n
}

fn clip_map(s: &Src, o: &mut Out) -> Result<(), String> {
    let n = |off: u32| s.u32(off) as usize;
    let (planes, materials, sides, edges, lbn, lb) = (n(8), n(24), n(32), n(40), n(64), n(72));
    let brushes = s.u16(140) as usize;
    let lbn = trim_lbn(s, lbn, brushes);
    let (statics, nodes, leafs, verts, tris, borders, parts, trees, cmodels) = (n(16), n(48), n(56), n(88), n(96), n(108), n(116), n(124), n(132));
    let mut cx = Ctx { s, pending: Vec::new(), arrays: vec![Arr::default(); 5], shared_planes: None };
    if planes > 0 && s.has(12) {
        if let Some((id, at, bytes)) = WORLD_PLANES.with(|w| w.borrow().clone()) {
            if read(s, 12, planes, 20, "collision planes").map(|p| p == bytes).unwrap_or(false) {
                cx.shared_planes = Some((id, at));
            }
        }
    }
    cx.arrays[PLANES] = Arr { base: s.u32(12), len: (planes * 20) as u32, stream: None };
    cx.arrays[SIDES] = Arr { base: s.u32(36), len: (sides * 8) as u32, stream: None };
    cx.arrays[EDGES] = Arr { base: s.u32(44), len: edges as u32, stream: None };
    cx.arrays[LEAF_BRUSHES] = Arr { base: s.u32(76), len: (lb * 2) as u32, stream: None };
    cx.arrays[BORDERS] = Arr { base: s.u32(112), len: (borders * 28) as u32, stream: None };
    // ATLAS_CLIP_DUMP=FILE: index, box (mid, half) and contents of every brush, one per line (debug)
    if let Some(path) = std::env::var_os("ATLAS_CLIP_DUMP") {
        let f = |a: u32| s.v.bytes(a, 4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).unwrap_or(f32::NAN);
        let (bb, bc) = (s.u32(148), s.u32(152));
        let mut txt = String::new();
        for i in 0..brushes as u32 {
            let c = s.v.bytes(bc + 4 * i, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
            let ns = s.v.bytes(s.u32(144) + 36 * i, 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0);
            txt += &format!("{i} {} {} {} {} {} {} {c:#x} {ns}\n", f(bb + 24 * i), f(bb + 24 * i + 4), f(bb + 24 * i + 8), f(bb + 24 * i + 12), f(bb + 24 * i + 16), f(bb + 24 * i + 20));
        }
        let _ = std::fs::write(path, txt);
    }
    // ATLAS_CLIP_PROBE="x y": the brushes whose box covers that column and the terrain triangles under it (debug)
    if let Some(p) = std::env::var("ATLAS_CLIP_PROBE").ok() {
        let xy: Vec<f32> = p.split_whitespace().filter_map(|v| v.parse().ok()).collect();
        if xy.len() >= 2 {
            let f = |a: u32| s.v.bytes(a, 4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).unwrap_or(f32::NAN);
            let (bb, bc) = (s.u32(148), s.u32(152));
            let mut hits = Vec::new();
            for i in 0..brushes as u32 {
                let (mx, my, mz, hx, hy, hz) = (f(bb + 24 * i), f(bb + 24 * i + 4), f(bb + 24 * i + 8), f(bb + 24 * i + 12), f(bb + 24 * i + 16), f(bb + 24 * i + 20));
                if (xy[0] - mx).abs() <= hx && (xy[1] - my).abs() <= hy && mz - hz < 200.0 && mz + hz > -400.0 {
                    let c = s.v.bytes(bc + 4 * i, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
                    hits.push(format!("brush {i}: z {:.0}..{:.0} contents {c:#x}", mz - hz, mz + hz));
                }
            }
            // every brush a leaf-brush node lists (IW4 cLeafBrushNode_s: axis u8, leafBrushCount s16 +2, contents +4, leaf brushes +8)
            let (np, nn) = (s.u32(68), lbn as u32);
            let mut reach = vec![false; brushes];
            let mut leaf_nodes = 0;
            for k in 0..nn {
                let cnt = s.v.bytes(np + 20 * k + 2, 2).map(|b| i16::from_le_bytes([b[0], b[1]])).unwrap_or(0);
                if cnt > 0 {
                    leaf_nodes += 1;
                    let lp = s.v.bytes(np + 20 * k + 8, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
                    for q in 0..cnt as u32 {
                        if let Some(b) = s.v.bytes(lp + 2 * q, 2) {
                            let v = u16::from_le_bytes([b[0], b[1]]) as usize;
                            if v < brushes {
                                reach[v] = true;
                            }
                        }
                    }
                }
            }
            eprintln!("clip probe: {} of {} brushes are in a leaf-brush node list ({} leaf nodes of {})", reach.iter().filter(|r| **r).count(), brushes, leaf_nodes, nn);
            for h in hits.iter_mut() {
                let i: usize = h.split(':').next().unwrap().trim_start_matches("brush ").parse().unwrap_or(0);
                h.push_str(if reach.get(i).copied().unwrap_or(false) { " (in a leaf list)" } else { " (IN NO LEAF LIST)" });
            }
            // the leaf of the point (cNode_t: plane +0, children s16 +4/+6; cplane_s: normal, dist +12) and its leaf-brush subtree
            if xy.len() >= 3 {
                let u = |a: u32| s.v.bytes(a, 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
                let i16at = |a: u32| s.v.bytes(a, 2).map(|b| i16::from_le_bytes([b[0], b[1]])).unwrap_or(0);
                let (nodes_p, leafs_p) = (s.u32(52), s.u32(60));
                let mut num: i32 = 0;
                let mut guard = 0;
                while num >= 0 && guard < 10000 {
                    guard += 1;
                    let node = nodes_p + 8 * num as u32;
                    let pl = u(node);
                    let d = f(pl) * xy[0] + f(pl + 4) * xy[1] + f(pl + 8) * xy[2] - f(pl + 12);
                    num = i16at(node + if d >= 0.0 { 4 } else { 6 }) as i32;
                }
                let leaf = (-1 - num) as u32;
                let lr = leafs_p + 40 * leaf;
                let (bc, first_lbn) = (u(lr + 4), u(lr + 36));
                let mut set = std::collections::BTreeSet::new();
                let mut stack = vec![first_lbn];
                let mut visited = 0;
                while let Some(k) = stack.pop() {
                    visited += 1;
                    if visited > 100000 || k >= nn {
                        break;
                    }
                    let cnt = i16at(np + 20 * k + 2);
                    if cnt > 0 {
                        let lp = u(np + 20 * k + 8);
                        for q in 0..cnt as u32 {
                            set.insert(s.v.bytes(lp + 2 * q, 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0));
                        }
                    } else {
                        for c in 0..2u32 {
                            let off = s.v.bytes(np + 20 * k + 16 + 2 * c, 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0) as u32;
                            if off > 0 {
                                stack.push(k + off);
                            }
                        }
                    }
                }
                eprintln!("clip probe point {:?}: leaf {leaf}, brushContents {bc:#x}, leafBrushNode {first_lbn}, {} brushes reachable, 3508 in it: {}, first: {:?}",
                    &xy[..3], set.len(), set.contains(&3508), set.iter().take(10).collect::<Vec<_>>());
                // the engine's pruned walk for the point (colltree::Tree::visit), with childOffset[0] above dist, and swapped
                for swap in [false, true] {
                    let mut got = std::collections::BTreeSet::new();
                    let mut st = vec![first_lbn];
                    let mut n_vis = 0;
                    while let Some(mut k) = st.pop() {
                        loop {
                            n_vis += 1;
                            if n_vis > 200000 || k >= nn {
                                break;
                            }
                            let cnt = i16at(np + 20 * k + 2);
                            if cnt > 0 {
                                let lp = u(np + 20 * k + 8);
                                for q in 0..cnt as u32 {
                                    got.insert(s.v.bytes(lp + 2 * q, 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0));
                                }
                                break;
                            }
                            if cnt < 0 {
                                st.push(k + 1);
                            }
                            let axis = s.v.bytes(np + 20 * k, 1).map(|b| b[0]).unwrap_or(0) as usize % 3;
                            let dist = f(np + 20 * k + 8);
                            let co = |c: u32| s.v.bytes(np + 20 * k + 16 + 2 * c, 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0) as u32;
                            let (above, below) = if swap { (co(1), co(0)) } else { (co(0), co(1)) };
                            if xy[axis] > dist {
                                k += above;
                            } else {
                                k += below;
                            }
                        }
                    }
                    eprintln!("  pruned point walk ({}): {} brushes, 3508 in it: {}", if swap { "children swapped" } else { "engine order" }, got.len(), got.contains(&3508));
                }
                let lb: Vec<f32> = (0..6u32).map(|k| f(lr + 12 + 4 * k)).collect();
                eprintln!("  leaf {leaf} bounds (mid/half) {lb:?}");
                // which leaf-brush nodes list brush 3508, and which leaves start a subtree holding one of them
                let holders: Vec<u32> = (0..nn).filter(|&k| {
                    let cnt = i16at(np + 20 * k + 2);
                    cnt > 0 && (0..cnt as u32).any(|q| s.v.bytes(u(np + 20 * k + 8) + 2 * q, 2).map(|b| u16::from_le_bytes([b[0], b[1]])) == Some(3508))
                }).collect();
                eprintln!("  lb nodes listing 3508: {holders:?}");
                for k in first_lbn..(first_lbn + 56).min(nn) {
                    let b = s.v.bytes(np + 20 * k, 20).unwrap_or(&[0; 20]).to_vec();
                    let cnt = i16::from_le_bytes([b[2], b[3]]);
                    let contents = u32::from_le_bytes(b[4..8].try_into().unwrap());
                    let dist = f32::from_le_bytes(b[8..12].try_into().unwrap());
                    let co = (u16::from_le_bytes([b[16], b[17]]), u16::from_le_bytes([b[18], b[19]]));
                    eprintln!("  lbn {k}: axis {} count {cnt} contents {contents:#x} dist {dist} childOffset {co:?}", b[0]);
                }
                let nl = leafs as u32;
                for l in 0..nl {
                    let r = leafs_p + 40 * l;
                    let st = u(r + 36);
                    if holders.iter().any(|&h| h >= st && h < st + 64) {
                        let b: Vec<f32> = (0..6u32).map(|k| f(r + 12 + 4 * k)).collect();
                        eprintln!("  leaf {l}: leafBrushNode {st} bounds {b:?}");
                    }
                }
            }
            eprintln!("clip probe {xy:?}: {} brushes ({} total)", hits.len(), brushes);
            for h in hits.iter().take(40) {
                eprintln!("  {h}");
            }
            let (vp, tp) = (s.u32(92), s.u32(100));
            let mut tri_hits = 0;
            for t in 0..tris as u32 {
                let idx: Vec<u32> = (0..3).map(|k| s.v.bytes(tp + 6 * t + 2 * k, 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u32).unwrap_or(0)).collect();
                let v: Vec<[f32; 3]> = idx.iter().map(|&i| [f(vp + 12 * i), f(vp + 12 * i + 4), f(vp + 12 * i + 8)]).collect();
                let s2 = |a: [f32; 3], b: [f32; 3]| (b[0] - a[0]) * (xy[1] - a[1]) - (b[1] - a[1]) * (xy[0] - a[0]);
                let (d0, d1, d2) = (s2(v[0], v[1]), s2(v[1], v[2]), s2(v[2], v[0]));
                if (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0) {
                    tri_hits += 1;
                    if tri_hits <= 10 {
                        eprintln!("  terrain tri {t}: z {:.0} {:.0} {:.0}", v[0][2], v[1][2], v[2][2]);
                    }
                }
            }
            eprintln!("clip probe: {tri_hits} terrain triangles under it ({tris} total, {verts} vertices)");
        }
    }

    let mut h = Hdr::new(256);
    h.u32(0, if s.has(0) { INLINE } else { 0 });
    h.u32(4, s.u32(4)); // isInUse
    // the ClipInfo at +8: planes, materials, brush sides, brush edges, leaf-brush nodes, leaf brushes, brushes, bounds, contents
    let counts = [planes, materials, sides, edges, lbn, lb];
    let ptrs = [12u32, 28, 36, 44, 68, 76];
    for (k, (&c, &p)) in counts.iter().zip(ptrs.iter()).enumerate() {
        h.u32(8 + 8 * k, c as u32);
        h.u32(8 + 8 * k + 4, if c > 0 && s.has(p) { INLINE } else { 0 });
    }
    h.u16(56, brushes as u16);
    for (k, p) in [144u32, 148, 152].into_iter().enumerate() {
        h.u32(60 + 4 * k, if brushes > 0 && s.has(p) { INLINE } else { 0 });
    }
    h.u32(72, INSERT);
    // static models, nodes, leafs, vertices, triangles, borders, partitions, trees, collision models
    h.u32(76, statics as u32);
    h.u32(80, if statics > 0 && s.has(20) { INLINE } else { 0 });
    h.u32(84, nodes as u32);
    h.u32(88, if nodes > 0 && s.has(52) { INLINE } else { 0 });
    h.u32(92, leafs as u32);
    h.u32(96, if leafs > 0 && s.has(60) { INLINE } else { 0 });
    h.u32(100, verts as u32);
    h.u32(104, if verts > 0 && s.has(92) { INLINE } else { 0 });
    h.u32(108, tris as u32);
    h.u32(112, if tris > 0 && s.has(100) { INLINE } else { 0 });
    h.u32(116, if tris > 0 && s.has(104) { INLINE } else { 0 });
    h.u32(120, borders as u32);
    h.u32(124, if borders > 0 && s.has(112) { INLINE } else { 0 });
    h.u32(128, parts as u32);
    h.u32(132, if parts > 0 && s.has(120) { INLINE } else { 0 });
    h.u32(136, trees as u32);
    h.u32(140, if trees > 0 && s.has(128) { INLINE } else { 0 });
    h.u32(144, cmodels as u32);
    h.u32(148, if cmodels > 0 && s.has(136) { INLINE } else { 0 });
    // mapEnts +152, stages +156 and stageCount +160, MapTriggers +164 (set below); smodelNodeCount +188, smodelNodes +192
    let smodel_nodes = s.u16(160) as usize;
    h.u16(188, smodel_nodes as u16);
    h.u32(192, if smodel_nodes > 0 && s.has(164) { INLINE } else { 0 });
    // dynamic entities: counts +196, definitions +200, poses +208, client data +216, collision data +224 (all runtime but the first)
    for k in 0..2u32 {
        let c = s.u16(168 + 2 * k) as usize;
        h.u16(196 + 2 * k as usize, c as u16);
        for (src, dst) in [(172u32, 200usize), (180, 208), (188, 216), (196, 224)] {
            h.u32(dst + 4 * k as usize, if c > 0 && s.has(src + 4 * k) { INLINE } else { 0 });
        }
    }
    h.u32(232, s.u32(204)); // checksum
    let map_ents = s.asset(156);
    let stash = map_ents.and_then(|id| MAP_ENTS.with(|m| m.borrow().get(&id).cloned()));
    if let Some(st) = &stash {
        h.u32(152, INSERT);
        if !st.stages.is_empty() {
            h.u32(156, INLINE);
            h.0[160] = st.stage_names.len() as u8;
        }
        for (k, (c, b)) in st.triggers.iter().enumerate() {
            h.u32(164 + 8 * k, *c);
            h.u32(168 + 8 * k, if !b.is_empty() { INLINE } else { 0 });
        }
    }
    let at = o.put(&h.0);
    if let Some(name) = s.cstr(0) {
        o.put_str(&name);
    }
    // stream offsets of the nine ClipInfo arrays (planes, materials, sides, edges, leaf-brush nodes, leaf brushes, brushes, bounds,
    // contents), for the aliasing copy
    let mut starts: [Option<usize>; 9] = [None; 9];

    // ---- the ClipInfo at +8, in the order the loader reads it
    if let Some((id, wat)) = cx.shared_planes {
        o.outer(at + 12, id, wat);
    } else if planes > 0 && s.has(12) {
        let st = o.put(&read(s, 12, planes, 20, "collision planes")?);
        cx.arrays[PLANES].stream = Some(st);
        starts[0] = Some(st);
    }
    if materials > 0 && s.has(28) {
        let m = read(s, 28, materials, 12, "collision materials")?;
        let mut out = m.clone();
        for i in 0..materials {
            let set = u32::from_le_bytes(m[12 * i..12 * i + 4].try_into().unwrap()) != 0;
            out[12 * i..12 * i + 4].copy_from_slice(&(if set { INLINE } else { 0 }).to_le_bytes());
        }
        starts[1] = Some(o.put(&out));
        let base = s.u32(28);
        for i in 0..materials {
            let r = s.at(base + 12 * i as u32);
            if r.has(0) {
                o.put_str(&super::convert_zone::cstr_at(s.v, r.u32(0)));
            }
        }
    }
    if sides > 0 && s.has(36) {
        let b = read(s, 36, sides, 8, "brush sides")?;
        let start = o.put(&vec![0u8; 8 * sides]);
        cx.arrays[SIDES].stream = Some(start);
        starts[2] = Some(start);
        o.f.bytes[start..start + 8 * sides].copy_from_slice(&b);
        let base = s.u32(36);
        let mut inline_planes: Vec<Vec<u8>> = Vec::new();
        for i in 0..sides {
            let r = s.at(base + 8 * i as u32);
            let w = start + 8 * i;
            cx.plane_word(o, w, r.u32(0), &mut inline_planes)?;
        }
        for p in inline_planes {
            o.put(&p);
        }
    }
    if edges > 0 && s.has(44) {
        let st = o.put(&read(s, 44, edges, 1, "brush edges")?);
        cx.arrays[EDGES].stream = Some(st);
        starts[3] = Some(st);
    }
    if lbn > 0 && s.has(68) {
        let a = read(s, 68, lbn, 20, "leaf-brush nodes")?;
        let start = o.put(&a);
        starts[4] = Some(start);
        let base = s.u32(68);
        let mut inline_data: Vec<Vec<u8>> = Vec::new();
        for i in 0..lbn {
            let r = s.at(base + 20 * i as u32);
            let leaves = r.u16(2) as i16;
            let p = r.u32(8);
            let w = start + 20 * i + 8;
            // Only a leaf (count > 0) holds a pointer here. An interior node holds its split distance at +8 (then range, child
            // offsets): clearing it for every node set every split plane to 0, so traces walked the wrong side of the tree and
            // missed most brushes (mp_citystreets on 3.19: players walked through walls and cars; the server's codol_trace and
            // codol_leafbrushes showed dist 0 on every interior node).
            if leaves > 0 {
                o.set(w, 0);
            }
            if leaves > 0 && p != 0 {
                if let Some(k) = cx.arrays[LEAF_BRUSHES].index_of(p, 2) {
                    cx.pending.push((w, LEAF_BRUSHES, 2 * k));
                    o.set(w, 1); // placeholder; patched by `inner` below
                } else {
                    o.set(w, INLINE);
                    inline_data.push(s.v.bytes(p, 2 * leaves as usize).ok_or("leaf brushes outside the loaded memory")?.to_vec());
                }
            }
        }
        for d in inline_data {
            o.put(&d);
        }
    }
    if lb > 0 && s.has(76) {
        let st = o.put(&read(s, 76, lb, 2, "leaf brushes")?);
        cx.arrays[LEAF_BRUSHES].stream = Some(st);
        starts[5] = Some(st);
    }
    if brushes > 0 && s.has(144) {
        let a = read(s, 144, brushes, 36, "brushes")?;
        let start = o.put(&a);
        starts[6] = Some(start);
        let base = s.u32(144);
        let (mut inline_sides, mut inline_planes, mut inline_edges) = (Vec::new(), Vec::new(), Vec::new());
        let mut order: Vec<(char, usize)> = Vec::new();
        for i in 0..brushes {
            let r = s.at(base + 36 * i as u32);
            let w = start + 36 * i;
            // sides pointer +4: one record in the side array, or its own copy
            o.set(w + 4, 0);
            let p = r.u32(4);
            if p != 0 {
                if let Some(k) = cx.arrays[SIDES].index_of(p, 8) {
                    cx.pending.push((w + 4, SIDES, 8 * k));
                    o.set(w + 4, 1);
                } else {
                    o.set(w + 4, INLINE);
                    inline_sides.push(s.v.bytes(p, 8).ok_or("brush side outside the loaded memory")?.to_vec());
                    order.push(('s', inline_sides.len() - 1));
                    let plane = s.at(p).u32(0);
                    let _ = plane;
                }
            }
            o.set(w + 8, 0);
            let p = r.u32(8);
            if p != 0 {
                if let Some(k) = cx.arrays[EDGES].index_of(p, 1) {
                    cx.pending.push((w + 8, EDGES, k));
                    o.set(w + 8, 1);
                } else {
                    o.set(w + 8, INLINE);
                    inline_edges.push(s.v.bytes(p, 1).ok_or("brush edge outside the loaded memory")?.to_vec());
                    order.push(('e', inline_edges.len() - 1));
                }
            }
        }
        // inline records follow in brush order: side (then its plane), edge
        let (mut si, mut ei) = (0usize, 0usize);
        for i in 0..brushes {
            let r = s.at(base + 36 * i as u32);
            let w = start + 36 * i;
            if u32::from_le_bytes(o.f.bytes[w + 4..w + 8].try_into().unwrap()) == INLINE {
                let side = inline_sides[si].clone();
                si += 1;
                let sat = o.put(&side);
                let plane = u32::from_le_bytes(side[0..4].try_into().unwrap());
                o.set(sat, 0);
                cx.plane_word(o, sat, plane, &mut inline_planes)?;
                for p in inline_planes.drain(..) {
                    o.put(&p);
                }
            }
            if u32::from_le_bytes(o.f.bytes[w + 8..w + 12].try_into().unwrap()) == INLINE {
                o.put(&inline_edges[ei].clone());
                ei += 1;
            }
            let _ = r;
        }
        let _ = order;
    }
    if brushes > 0 && s.has(148) {
        let mut bounds = read(s, 148, brushes, 24, "brush bounds")?;
        // a half size is never negative (a box that is a hair inside out from rounding, e.g. -4.4e-8, would be refused by the loader)
        for b in 0..brushes {
            for k in 0..3 {
                let at = 24 * b + 12 + 4 * k;
                if f32::from_le_bytes(bounds[at..at + 4].try_into().unwrap()) < 0.0 {
                    bounds[at..at + 4].copy_from_slice(&0f32.to_le_bytes());
                }
            }
        }
        starts[7] = Some(o.put(&bounds));
    }
    if brushes > 0 && s.has(152) {
        starts[8] = Some(o.put(&read(s, 152, brushes, 4, "brush contents")?));
    }

    // ---- the second ClipInfo (pInfo): a header whose pointers alias the arrays above
    let pinfo_id = 0x8000_0000 | o.f.id;
    o.anchor(pinfo_id);
    let mut p = Hdr::new(64);
    for (k, &c) in counts.iter().enumerate() {
        p.u32(8 * k, c as u32);
    }
    p.u16(48, brushes as u16);
    let pat = o.put(&p.0);
    // pointers: planes +4, materials +12, sides +20, edges +28, leaf-brush nodes +36, leaf brushes +44, brushes +52, bounds +56,
    // contents +60
    for (start, word) in starts.iter().zip([4usize, 12, 20, 28, 36, 44, 52, 56, 60]) {
        if let Some(t) = start {
            o.inner(pat + word, *t);
        }
    }
    if let Some((id, wat)) = cx.shared_planes {
        o.outer(pat + 4, id, wat);
    }

    // ---- static models
    if statics > 0 && s.has(20) {
        let a = read(s, 20, statics, 76, "static models")?;
        let start = o.put(&a);
        let base = s.u32(20);
        for i in 0..statics {
            o.reference(start + 76 * i, s.at(base + 76 * i as u32).asset(0));
        }
    }
    // ---- nodes (8 bytes: plane pointer first)
    if nodes > 0 && s.has(52) {
        let a = read(s, 52, nodes, 8, "collision nodes")?;
        let start = o.put(&a);
        let base = s.u32(52);
        let mut inline_planes: Vec<Vec<u8>> = Vec::new();
        for i in 0..nodes {
            let w = start + 8 * i;
            cx.plane_word(o, w, s.at(base + 8 * i as u32).u32(0), &mut inline_planes)?;
        }
        for p in inline_planes {
            o.put(&p);
        }
    }
    if leafs > 0 && s.has(60) {
        o.put(&read(s, 60, leafs, 40, "collision leafs")?);
    }
    if verts > 0 && s.has(92) {
        o.put(&read(s, 92, verts, 12, "collision vertices")?);
    }
    if tris > 0 && s.has(100) {
        o.put(&read(s, 100, tris, 6, "collision triangles")?);
    }
    if tris > 0 && s.has(104) {
        let bytes = (((3 * tris as u32 + 31) >> 3) & !3) as usize;
        o.put(&read(s, 104, bytes, 1, "triangle walkability")?);
    }
    if borders > 0 && s.has(112) {
        cx.arrays[BORDERS].stream = Some(o.put(&read(s, 112, borders, 28, "collision borders")?));
    }
    if parts > 0 && s.has(120) {
        let a = read(s, 120, parts, 12, "collision partitions")?;
        let start = o.put(&a);
        let base = s.u32(120);
        let mut inline_borders: Vec<Vec<u8>> = Vec::new();
        for i in 0..parts {
            let w = start + 12 * i + 8;
            let p = s.at(base + 12 * i as u32).u32(8);
            o.set(w, 0);
            if p != 0 {
                if let Some(k) = cx.arrays[BORDERS].index_of(p, 28) {
                    cx.pending.push((w, BORDERS, 28 * k));
                    o.set(w, 1);
                } else {
                    o.set(w, INLINE);
                    inline_borders.push(s.v.bytes(p, 28).ok_or("collision border outside the loaded memory")?.to_vec());
                }
            }
        }
        for b in inline_borders {
            o.put(&b);
        }
    }
    if trees > 0 && s.has(128) {
        o.put(&read(s, 128, trees, 32, "collision trees")?);
    }
    // ---- collision models (72 bytes: the stock 68 with a ClipInfo pointer after the radius)
    if cmodels > 0 && s.has(136) {
        let a = read(s, 136, cmodels, 68, "collision models")?;
        let mut out = vec![0u8; 72 * cmodels];
        for i in 0..cmodels {
            out[72 * i..72 * i + 28].copy_from_slice(&a[68 * i..68 * i + 28]);
            out[72 * i + 32..72 * i + 72].copy_from_slice(&a[68 * i + 28..68 * i + 68]);
        }
        let start = o.put(&out);
        for i in 0..cmodels {
            o.reference(start + 72 * i + 28, Some(pinfo_id));
        }
    }
    // ---- static-model nodes (28 bytes)
    if smodel_nodes > 0 && s.has(164) {
        o.put(&read(s, 164, smodel_nodes, 28, "static model nodes")?);
    }
    // ---- map entities (inline), stages, triggers
    if let Some(st) = &stash {
        o.put(&st.record);
        CONSUMED.with(|c| c.borrow_mut().insert(map_ents.unwrap()));
        if !st.stages.is_empty() {
            let sa = o.put(&st.stages);
            for (i, name) in st.stage_names.iter().enumerate() {
                o.set(sa + 20 * i, if name.is_some() { INLINE } else { 0 });
            }
            for name in st.stage_names.iter().flatten() {
                o.put_str(name);
            }
        }
        for (_, b) in &st.triggers {
            if !b.is_empty() {
                o.put(b);
            }
        }
    }
    // ---- dynamic entities
    for k in 0..2u32 {
        let c = s.u16(168 + 2 * k) as usize;
        if c > 0 && s.has(172 + 4 * k) {
            let a = read(s, 172 + 4 * k, c, 92, "dynamic entities")?;
            let start = o.put(&a);
            let base = s.u32(172 + 4 * k);
            for j in 0..c {
                let r = s.at(base + 92 * j as u32);
                for off in [32u32, 40, 44] {
                    o.reference(start + 92 * j + off as usize, r.asset(off));
                }
            }
        }
    }
    // resolve the aliases into the arrays
    for (w, array, byte) in std::mem::take(&mut cx.pending) {
        if let (PLANES, Some((id, wat))) = (array, cx.shared_planes) {
            o.outer(w, id, wat + byte as usize);
            continue;
        }
        let Some(base) = cx.arrays[array].stream else { return Err("alias into an array that was not written".into()) };
        o.inner(w, base + byte as usize);
    }
    Ok(())
}

impl Ctx<'_, '_, '_> {
    /// Sets the plane pointer word at stream offset `w` for a stock plane pointer `p`: an alias into the plane array when the plane is
    /// one of its elements, else the plane follows (queued in `inline`, to be written by the caller right after the record).
    fn plane_word(&mut self, o: &mut Out, w: usize, p: u32, inline: &mut Vec<Vec<u8>>) -> Result<(), String> {
        o.set(w, 0);
        if p == 0 {
            return Ok(());
        }
        if let Some(k) = self.arrays[PLANES].index_of(p, 20) {
            self.pending.push((w, PLANES, 20 * k));
            o.set(w, 1);
        } else {
            o.set(w, INLINE);
            inline.push(self.s.v.bytes(p, 20).ok_or("collision plane outside the loaded memory")?.to_vec());
        }
        Ok(())
    }
}
