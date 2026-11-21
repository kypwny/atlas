//! Render surfaces for new brushes: the faces of a brush that is not in the zone are added to the GfxWorld as draw surfaces.
//!
//! What a 3.19 (and QA) map's static world is made of, as measured on the shipped maps (`tests/worldshape.rs`):
//!
//! * **Surfaces** (24 bytes) are stored in draw order: first the lit opaque ones, then lit transparent, shadow casters and
//!   emissive ones; the header holds the begin and end of each of these four ranges, the number of static surfaces and the number
//!   without decals. Behind the static surfaces come the surfaces of the brush models (doors, ...): `models[i]` (60 bytes) has
//!   a surface count at +52, a first surface at +54 and a count without decals at +56; model 0 is the whole static world.
//!   A surface says where its vertices (`firstVertex`, 44-byte vertices, indices are 16 bit and relative to it) and its triangles
//!   (`baseIndex`) are, names its material (a reference to a slot of the world's material table) and its light indices.
//! * **Sorted surface indices**: two lists of surface numbers one behind the other (static count entries, then the no-decal
//!   count). The aabb tree of each cell names runs of this list: a node's (count, first) at +26 / +28 for the first list and +30 /
//!   +32 for the second one (positions in the whole array, so the second list starts at the static count).
//! * Surface numbers also appear in the sky's surface lists, the shadow geometry and the reflection probe lists.
//!
//! A new surface goes behind the last surface of its material in the lit opaque range; every surface number from there on moves
//! up by one, the sorted lists get the number once in each list (at the end of the run of the leaf the face lies in), and the
//! leaf and its ancestors grow by one entry and their boxes by the face.
//! **Not verified in the real client**: the structure is checked against the invariants the shipped maps hold
//! ([`check`]) and read back with the Atlas viewer, nothing more.

use super::replay::{Replay, INSERT};
use super::Zone;
use anyhow::{bail, Context, Result};

/// A polygon to draw: corners counter-clockwise seen from outside the brush, its outward normal and the material name.
#[derive(Debug, Clone)]
pub struct NewFace {
    pub corners: Vec<[f64; 3]>,
    pub normal: [f64; 3],
    pub material: String,
}

#[derive(Debug, Default)]
pub struct Added {
    pub surfaces: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub skipped: Vec<String>,
}

/// The dpvs block of the header (QA header offsets; 3.19 has one more word at +0x34).
const P: usize = 980 + 456;
/// Texture repeat in world units.
const TILE: f64 = 128.0;

struct Pos {
    verts: usize,
    inds: usize,
    surf: usize,
    bounds: usize,
    sorted: usize,
    cells: Vec<(usize, usize)>,
    lists: Vec<(u8, usize, usize)>,
    models: usize,
}

struct W {
    b: Vec<u8>,
    shift: usize,
    pos: Pos,
    names: Vec<Option<String>>,
    winding_flip: bool,
}

fn pack_unit(v: [f64; 3]) -> u32 {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
    let c = |x: f64| ((x / l * 127.0 + 127.0).round().clamp(0.0, 254.0)) as u32;
    c(v[0]) | (c(v[1]) << 8) | (c(v[2]) << 16) | (63 << 24)
}

impl W {
    fn r16(&self, o: usize) -> usize {
        u16::from_le_bytes([self.b[o], self.b[o + 1]]) as usize
    }
    fn r32(&self, o: usize) -> usize {
        u32::from_le_bytes(self.b[o..o + 4].try_into().unwrap()) as usize
    }
    fn rf(&self, o: usize) -> f32 {
        f32::from_le_bytes(self.b[o..o + 4].try_into().unwrap())
    }
    fn w16(&mut self, o: usize, v: usize) {
        self.b[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes());
    }
    fn w32(&mut self, o: usize, v: usize) {
        self.b[o..o + 4].copy_from_slice(&(v as u32).to_le_bytes());
    }
    fn wf(&mut self, o: usize, v: f32) {
        self.b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    /// Where a QA-layout header offset is.
    fn ho(&self, o: usize) -> usize {
        if o >= 0x34 { o + self.shift } else { o }
    }
    fn dp(&self, k: usize) -> usize {
        self.ho(P + k)
    }

    /// Replaces `old_len` bytes at `at` and moves what lies behind. `owner` is the start of the array being edited (an array that
    /// begins exactly at `at` stays where it is only when it is that one).
    fn splice(&mut self, owner: usize, at: usize, old_len: usize, new: &[u8]) {
        self.b.splice(at..at + old_len, new.iter().copied());
        let delta = new.len() as isize - old_len as isize;
        let f = |p: &mut usize| {
            if *p > at || (*p == at && *p != owner) {
                *p = (*p as isize + delta) as usize;
            }
        };
        let p = &mut self.pos;
        for x in [&mut p.verts, &mut p.inds, &mut p.surf, &mut p.bounds, &mut p.sorted, &mut p.models] {
            f(x);
        }
        for c in p.cells.iter_mut() {
            f(&mut c.0);
        }
        for l in p.lists.iter_mut() {
            f(&mut l.1);
        }
    }

    fn node(&self, cell: usize, j: usize) -> usize {
        self.pos.cells[cell].0 + 44 * j
    }

    fn bounds_of(&self, o: usize) -> ([f32; 3], [f32; 3]) {
        (std::array::from_fn(|k| self.rf(o + 4 * k)), std::array::from_fn(|k| self.rf(o + 12 + 4 * k)))
    }

    fn add_face(&mut self, face: &NewFace) -> Result<Option<String>> {
        let k = face.corners.len();
        if k < 3 {
            return Ok(Some("a face with fewer than three corners".into()));
        }
        let (op_begin, op_end) = (self.r32(self.dp(12)), self.r32(self.dp(16)));
        // the last surface of this material in the lit opaque range, and a reusable surface of it to copy the lighting from
        let same: Vec<usize> = (op_begin..op_end.min(self.names.len())).filter(|&i| self.names[i].as_deref() == Some(face.material.as_str())).collect();
        let Some(&last) = same.last() else { return Ok(Some(format!("material {} has no lit opaque surface in this map", face.material))) };
        let centre: [f32; 3] = std::array::from_fn(|a| (face.corners.iter().map(|c| c[a]).sum::<f64>() / k as f64) as f32);
        let reusable: Vec<usize> = same.iter().copied().filter(|&i| (self.r32(self.pos.surf + 24 * i + 16) as u32) < INSERT).collect();
        let dist2 = |i: usize| {
            let (c, h) = self.bounds_of(self.pos.bounds + 24 * i);
            (0..3).map(|a| ((centre[a] - c[a]).abs() - h[a]).max(0.0).powi(2)).sum::<f32>()
        };
        let Some(&tmpl) = reusable.iter().min_by(|&&a, &&b| dist2(a).total_cmp(&dist2(b))) else {
            return Ok(Some(format!("every surface of {} loads the material itself (no reusable reference)", face.material)));
        };
        let kk = last + 1;

        // the leaf: the nearest one with a run in both lists
        let mut best: Option<(f32, f32, usize, usize)> = None;
        for (c, &(_, n)) in self.pos.cells.iter().enumerate() {
            for j in 0..n {
                let o = self.node(c, j);
                if self.r16(o + 24) != 0 || self.r16(o + 26) == 0 || self.r16(o + 30) == 0 {
                    continue;
                }
                let (cen, half) = self.bounds_of(o);
                let d: f32 = (0..3).map(|a| ((centre[a] - cen[a]).abs() - half[a]).max(0.0).powi(2)).sum();
                let vol = half[0] * half[1] * half[2];
                if best.map_or(true, |(bd, bv, _, _)| d < bd || (d == bd && vol < bv)) {
                    best = Some((d, vol, c, j));
                }
            }
        }
        let (_, _, cell, leaf) = best.context("the map has no leaf with surfaces")?;
        // the path root .. leaf
        let n = self.pos.cells[cell].1;
        let mut parent: Vec<Option<usize>> = vec![None; n];
        for j in 0..n {
            let o = self.node(cell, j);
            let cc = self.r16(o + 24);
            if cc > 0 {
                let first = j + self.r32(o + 40) / 44;
                for c in first..(first + cc).min(n) {
                    parent[c] = Some(j);
                }
            }
        }
        let mut path = vec![leaf];
        while let Some(p) = parent[*path.last().unwrap()] {
            if path.contains(&p) {
                bail!("the cell tree has a loop");
            }
            path.push(p);
        }
        if *path.last().unwrap() != 0 {
            bail!("a tree node of cell {cell} is not reachable from its root");
        }

        // geometry: vertices (44 bytes), fan indices
        let normal = face.normal;
        let (u, v) = {
            let u = if normal[2].abs() > 0.707 { [1.0, 0.0, 0.0] } else { let l = (normal[0] * normal[0] + normal[1] * normal[1]).sqrt().max(1e-9); [-normal[1] / l, normal[0] / l, 0.0] };
            let v = [normal[1] * u[2] - normal[2] * u[1], normal[2] * u[0] - normal[0] * u[2], normal[0] * u[1] - normal[1] * u[0]];
            (u, v)
        };
        let tv = self.r32(self.pos.surf + 24 * tmpl + 4);
        let tv_at = self.pos.verts + 44 * tv;
        let lightmap: [u8; 8] = self.b[tv_at + 28..tv_at + 36].try_into().unwrap();
        let sign = if (0..3).map(|a| (normal[(a + 1) % 3] * u[(a + 2) % 3] - normal[(a + 2) % 3] * u[(a + 1) % 3]) * v[a]).sum::<f64>() >= 0.0 { 1.0f32 } else { -1.0 };
        let mut verts: Vec<u8> = Vec::with_capacity(44 * k);
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for c in &face.corners {
            for a in 0..3 {
                lo[a] = lo[a].min(c[a]);
                hi[a] = hi[a].max(c[a]);
                verts.extend_from_slice(&(c[a] as f32).to_le_bytes());
            }
            verts.extend_from_slice(&sign.to_le_bytes());
            verts.extend_from_slice(&[255, 255, 255, 255]);
            let (tu, tvv) = ((0..3).map(|a| c[a] * u[a]).sum::<f64>() / TILE, (0..3).map(|a| c[a] * v[a]).sum::<f64>() / TILE);
            verts.extend_from_slice(&(tu as f32).to_le_bytes());
            verts.extend_from_slice(&(tvv as f32).to_le_bytes());
            verts.extend_from_slice(&lightmap);
            verts.extend_from_slice(&pack_unit(normal).to_le_bytes());
            verts.extend_from_slice(&pack_unit(u).to_le_bytes());
        }
        let mut indices: Vec<u8> = Vec::new();
        for i in 1..k - 1 {
            let tri = if self.winding_flip { [0, i + 1, i] } else { [0, i, i + 1] };
            for t in tri {
                indices.extend_from_slice(&(t as u16).to_le_bytes());
            }
        }
        let (vcount_at, icount_at, surf_total_at) = (self.ho(132), self.ho(156), 16);
        let (vcount, icount) = (self.r32(vcount_at), self.r32(icount_at));
        let mut record = [0u8; 24];
        record[4..8].copy_from_slice(&(vcount as u32).to_le_bytes());
        record[8..10].copy_from_slice(&(k as u16).to_le_bytes());
        record[10..12].copy_from_slice(&((k - 2) as u16).to_le_bytes());
        record[12..16].copy_from_slice(&(icount as u32).to_le_bytes());
        record[16..24].copy_from_slice(&self.b[self.pos.surf + 24 * tmpl + 16..self.pos.surf + 24 * tmpl + 24]);

        // ---- header words
        let (p4, p8) = (self.dp(4), self.dp(8));
        let (static0, nd0) = (self.r32(p4), self.r32(p8));
        self.w32(p4, static0 + 1);
        self.w32(p8, nd0 + 1);
        let total = self.r32(surf_total_at);
        self.w32(surf_total_at, total + 1);
        for kw in (12..=40).step_by(4) {
            let a = self.dp(kw);
            let w = self.r32(a);
            if w >= kk {
                self.w32(a, w + 1);
            }
        }
        let usage = ((static0 + 1 + 31) / 32 + 3) / 4 * 4;
        let ua = self.dp(48);
        self.w32(ua, usage);
        self.w32(vcount_at, vcount + k);
        self.w32(icount_at, icount + indices.len() / 2);

        // ---- surface numbers elsewhere
        let (m0, mn) = (self.pos.models, self.r32(self.ho(220)));
        let m0c = self.r16(m0 + 52);
        self.w16(m0 + 52, m0c + 1);
        let m0n = self.r16(m0 + 56);
        self.w16(m0 + 56, m0n + 1);
        for i in 1..mn {
            let o = m0 + 60 * i;
            let (cnt, start) = (self.r16(o + 52), self.r16(o + 54));
            if cnt > 0 && start >= kk {
                self.w16(o + 54, start + 1);
            }
        }
        for (kind, at, count) in self.pos.lists.clone() {
            for i in 0..count {
                if kind == 0 {
                    let x = self.r32(at + 4 * i);
                    if x >= kk {
                        self.w32(at + 4 * i, x + 1);
                    }
                } else {
                    let x = self.r16(at + 2 * i);
                    if x >= kk {
                        self.w16(at + 2 * i, x + 1);
                    }
                }
            }
        }

        // ---- the sorted lists and the tree runs
        let lo_o = self.node(cell, leaf);
        let (start1, cnt1, cnd, snd) = (self.r16(lo_o + 28), self.r16(lo_o + 26), self.r16(lo_o + 30), self.r16(lo_o + 32));
        let (pos1, pos2) = (start1 + cnt1, snd + cnd + 1);
        let (sat, sn) = (self.pos.sorted, static0 + nd0);
        let mut vals: Vec<u16> = (0..sn).map(|i| self.r16(sat + 2 * i) as u16).collect();
        for x in vals.iter_mut() {
            if *x as usize >= kk {
                *x += 1;
            }
        }
        if pos1 > static0 || pos2 > sn + 1 {
            bail!("a leaf run lies outside the sorted list");
        }
        vals.insert(pos1, kk as u16);
        vals.insert(pos2, kk as u16);
        let bytes: Vec<u8> = vals.iter().flat_map(|x| x.to_le_bytes()).collect();
        let owner = self.pos.sorted;
        self.splice(owner, sat, 2 * sn, &bytes);
        // tree nodes (positions are read again: the splice moved what lay behind the sorted list)
        let (nlo, nhi) = (std::array::from_fn::<f32, 3, _>(|a| lo[a] as f32), std::array::from_fn::<f32, 3, _>(|a| hi[a] as f32));
        for c in 0..self.pos.cells.len() {
            for j in 0..self.pos.cells[c].1 {
                let o = self.node(c, j);
                let in_path = c == cell && path.contains(&j);
                let (cnt, st, cd, sd) = (self.r16(o + 26), self.r16(o + 28), self.r16(o + 30), self.r16(o + 32));
                if cnt > 0 {
                    if in_path {
                        self.w16(o + 26, cnt + 1);
                    } else if st >= pos1 {
                        self.w16(o + 28, st + 1);
                    }
                }
                if cd > 0 {
                    // the first insertion moved every start of the second list; the second one adds to the leaf's runs
                    let st2 = if sd >= pos1 { sd + 1 } else { sd };
                    if in_path {
                        self.w16(o + 30, cd + 1);
                        self.w16(o + 32, st2);
                    } else {
                        self.w16(o + 32, if st2 >= pos2 { st2 + 1 } else { st2 });
                    }
                }
                if in_path {
                    let (cen, half) = self.bounds_of(o);
                    for a in 0..3 {
                        let (l, h) = ((cen[a] - half[a]).min(nlo[a]), (cen[a] + half[a]).max(nhi[a]));
                        self.wf(o + 4 * a, (l + h) * 0.5);
                        self.wf(o + 12 + 4 * a, (h - l) * 0.5);
                    }
                }
            }
        }

        // ---- the arrays that grow: vertices and indices at the end, the surface and its bounds at kk
        let (va, owner) = (self.pos.verts + 44 * vcount, self.pos.verts);
        self.splice(owner, va, 0, &verts);
        let (ia, owner) = (self.pos.inds + 2 * icount, self.pos.inds);
        self.splice(owner, ia, 0, &indices);
        let (sa, owner) = (self.pos.surf + 24 * kk, self.pos.surf);
        self.splice(owner, sa, 0, &record);
        let mut bb = [0u8; 24];
        for a in 0..3 {
            bb[4 * a..4 * a + 4].copy_from_slice(&(((lo[a] + hi[a]) * 0.5) as f32).to_le_bytes());
            bb[12 + 4 * a..16 + 4 * a].copy_from_slice(&(((hi[a] - lo[a]) * 0.5) as f32).to_le_bytes());
        }
        let (ba, owner) = (self.pos.bounds + 24 * kk, self.pos.bounds);
        self.splice(owner, ba, 0, &bb);
        self.names.insert(kk, Some(face.material.clone()));
        Ok(None)
    }
}

/// Adds one surface per face to the GfxWorld asset in `blob` (the bytes of asset `asset` of `zone`; static model edits may
/// already be in it). Faces whose material the map's lit opaque surfaces do not use are skipped and named in the result.
pub fn add_surfaces(zone: &Zone, blob: &mut Vec<u8>, asset: usize, faces: &[NewFace]) -> Result<Added> {
    let mut r = Replay::new(zone).map_err(|f| anyhow::anyhow!("the walker stops in the zone prefix: {} at {}", f.what, f.at))?;
    for i in 0..zone.assets.len() {
        let _ = r.replay_asset(zone, i);
    }
    let w = r.rec.world.take().context("this zone has no GfxWorld")?;
    if w.sorted.0 == 0 || w.surface_bounds == 0 || w.vertices.0 == 0 || w.indices.0 == 0 || w.brush_models.0 == 0 {
        bail!("this zone's GfxWorld lacks an array the surface writer needs");
    }
    let start = zone.assets[asset].start;
    let rel = |o: usize| o - start;
    let pos = Pos {
        verts: rel(w.vertices.0),
        inds: rel(w.indices.0),
        surf: rel(w.surfaces.first().map(|s| s.0).context("the world has no surfaces")?),
        bounds: rel(w.surface_bounds),
        sorted: rel(w.sorted.0),
        cells: w.cell_nodes.iter().map(|&(a, n)| (rel(a), n)).collect(),
        // reflection probe lists (kinds 2 and 3) hold numbers that are no surface numbers (they pass the surface count)
        lists: w.lists.iter().filter(|l| l.0 <= 1).map(|&(k, a, n)| (k, rel(a), n)).collect(),
        models: rel(w.brush_models.0),
    };
    let names: Vec<Option<String>> = w.surfaces.iter().map(|s| s.1.clone()).collect();
    let mut state = W { b: std::mem::take(blob), shift: if zone.version == super::fastfile::VERSION_319 { 4 } else { 0 }, pos, names, winding_flip: false };
    // which way the world's own triangles run, compared with their vertex normals
    {
        let (mut pos_n, mut neg_n) = (0, 0);
        let end = (state.r32(16)).min(400);
        for i in 0..end {
            let o = state.pos.surf + 24 * i;
            let (first, tris, base) = (state.r32(o + 4), state.r16(o + 10), state.r32(o + 12));
            if tris == 0 || base + 3 > w.indices.1 {
                continue;
            }
            let vi = |k: usize| first + state.r16(state.pos.inds + 2 * (base + k));
            let p = |k: usize| -> Option<[f32; 3]> {
                let a = state.pos.verts + 44 * vi(k);
                (a + 44 <= state.b.len() && vi(k) < w.vertices.1).then(|| [state.rf(a), state.rf(a + 4), state.rf(a + 8)])
            };
            let (Some(a), Some(b), Some(c)) = (p(0), p(1), p(2)) else { continue };
            let n = crate::model::unpack_unit(state.r32(state.pos.verts + 44 * vi(0) + 36) as u32);
            let e1: [f32; 3] = std::array::from_fn(|k| b[k] - a[k]);
            let e2: [f32; 3] = std::array::from_fn(|k| c[k] - a[k]);
            let cr = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
            let d: f32 = (0..3).map(|k| cr[k] * n[k]).sum();
            if d > 0.0 { pos_n += 1 } else if d < 0.0 { neg_n += 1 }
        }
        state.winding_flip = neg_n > pos_n;
    }
    let mut out = Added::default();
    for f in faces {
        match state.add_face(f)? {
            None => {
                out.surfaces += 1;
                out.vertices += f.corners.len();
                out.triangles += f.corners.len() - 2;
            }
            Some(why) => out.skipped.push(why),
        }
    }
    *blob = state.b;
    Ok(out)
}

/// What the shipped maps hold of their static world, checked on a zone: returns the violations (empty when sound).
pub fn check(zone: &Zone) -> Result<Vec<String>> {
    let mut r = Replay::new(zone).map_err(|f| anyhow::anyhow!("the walker stops in the zone prefix: {} at {}", f.what, f.at))?;
    for i in 0..zone.assets.len() {
        r.replay_asset(zone, i).map_err(|f| anyhow::anyhow!("asset {i}: {} at {:#x}", f.what, f.at))?;
    }
    let w = r.rec.world.take().context("no GfxWorld")?;
    let d = &zone.data;
    let shift = if zone.version == super::fastfile::VERSION_319 { 4 } else { 0 };
    let u16_at = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as usize;
    let h = w.header;
    let ho = |o: usize| h + if o >= 0x34 { o + shift } else { o };
    let mut bad = Vec::new();
    let total = u32_at(h + 16);
    let static_n = u32_at(ho(P + 4));
    let nd = u32_at(ho(P + 8));
    if w.surfaces.len() != total {
        bad.push(format!("surface records {} != header {}", w.surfaces.len(), total));
    }
    if static_n > total || nd > static_n {
        bad.push(format!("static {static_n}, no-decal {nd}, total {total}"));
    }
    let cats: Vec<usize> = (0..8).map(|k| u32_at(ho(P + 12 + 4 * k))).collect();
    if cats[0] != 0 || cats[7] > static_n {
        bad.push(format!("category words {cats:?} against {static_n}"));
    }
    for k in 0..4 {
        if cats[2 * k] > cats[2 * k + 1] {
            bad.push(format!("category {k} runs backwards: {cats:?}"));
        }
    }
    if (0..3).any(|k| cats[2 * k + 1] != cats[2 * k + 2]) {
        bad.push(format!("categories are not adjacent: {cats:?}"));
    }
    // sorted lists: each a set of distinct surface numbers below the static count
    let (sat, sn) = w.sorted;
    if sn != static_n + nd {
        bad.push(format!("sorted list has {sn} entries, expected {}", static_n + nd));
    } else {
        for (name, from, len) in [("first", 0, static_n), ("no-decal", static_n, nd)] {
            let mut seen = vec![false; static_n.max(1)];
            for i in from..from + len {
                let v = u16_at(sat + 2 * i);
                if v >= static_n || std::mem::replace(&mut seen[v], true) {
                    bad.push(format!("{name} sorted list: entry {i} = {v} repeats or is out of range"));
                    break;
                }
            }
        }
    }
    // tree runs
    for (c, &(at, n)) in w.cell_nodes.iter().enumerate() {
        for j in 0..n {
            let o = at + 44 * j;
            let (cc, cnt, st, cd, sd) = (u16_at(o + 24), u16_at(o + 26), u16_at(o + 28), u16_at(o + 30), u16_at(o + 32));
            if cnt > 0 && st + cnt > static_n {
                bad.push(format!("cell {c} node {j}: run {st}+{cnt} past {static_n}"));
            }
            if cd > 0 && (sd < static_n || sd + cd > static_n + nd) {
                bad.push(format!("cell {c} node {j}: no-decal run {sd}+{cd} outside {static_n}..{}", static_n + nd));
            }
            if cc > 0 {
                let first = j + u32_at(o + 40) / 44;
                for k in first..(first + cc).min(n) {
                    let q = at + 44 * k;
                    let (c1, s1, c2, s2) = (u16_at(q + 26), u16_at(q + 28), u16_at(q + 30), u16_at(q + 32));
                    if c1 > 0 && (s1 < st || s1 + c1 > st + cnt) {
                        bad.push(format!("cell {c} node {j}: child {k} run {s1}+{c1} not inside {st}+{cnt}"));
                    }
                    if c2 > 0 && (s2 < sd || s2 + c2 > sd + cd) {
                        bad.push(format!("cell {c} node {j}: child {k} no-decal run {s2}+{c2} not inside {sd}+{cd}"));
                    }
                }
            }
        }
    }
    // brush models and lists
    let (bm, bn) = w.brush_models;
    if bn > 0 && (u16_at(bm + 52) != static_n || u16_at(bm + 56) != nd) {
        bad.push(format!("model 0 has {} / {} surfaces, expected {static_n} / {nd}", u16_at(bm + 52), u16_at(bm + 56)));
    }
    for i in 1..bn {
        let o = bm + 60 * i;
        let (cnt, st) = (u16_at(o + 52), u16_at(o + 54));
        if cnt > 0 && (st < static_n || st + cnt > total) {
            bad.push(format!("brush model {i}: surfaces {st}+{cnt} outside {static_n}..{total}"));
        }
    }
    for &(kind, at, n) in w.lists.iter().filter(|l| l.0 <= 1) {
        for i in 0..n {
            // sky surfaces may lie behind the static ones; shadow geometry names static surfaces
            let (v, limit) = if kind == 0 { (u32_at(at + 4 * i), total) } else { (u16_at(at + 2 * i), static_n) };
            if v >= limit {
                bad.push(format!("list of kind {kind}: value {v} >= {limit}"));
                break;
            }
        }
    }
    // surface records
    let inn = w.indices.1;
    for (i, (rec, _)) in w.surfaces.iter().enumerate() {
        let (tris, base) = (u16_at(rec + 10), u32_at(rec + 12));
        if base + 3 * tris > inn {
            bad.push(format!("surface {i}: indices {base}+{} past {inn}", 3 * tris));
            break;
        }
    }
    let usage = u32_at(ho(P + 48));
    if usage != ((static_n + 31) / 32 + 3) / 4 * 4 {
        bad.push(format!("usage count {usage} for {static_n} static surfaces"));
    }
    Ok(bad)
}
