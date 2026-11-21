//! A CoDOL (QA 3.3.3.3 / 3.19) map zone as editable map source: the collision brushes with the material of each side, the brush
//! entities' own brushes, the static models with their placement, and the entity text, written as a CoD4-Radiant style `.map`.
//!
//! Brushes are rebuilt the way the collision code reads them: six axial sides from the brush's bounds (`axialMaterialNum[2][3]`), then
//! `numsides` stored sides whose planes the zone keeps elsewhere. Render geometry (terrain, decals, patches) is not in a
//! collision brush, so this is the collision shell of the map, not its drawn world.

use super::replay::{ClipRec, Replay};
use super::Zone;
use anyhow::{bail, Result};
use std::fmt::Write;

#[derive(Debug, Clone)]
pub struct Side {
    pub normal: [f64; 3],
    pub dist: f64,
    pub material: String,
}

#[derive(Debug, Clone)]
pub struct Brush {
    pub sides: Vec<Side>,
    pub contents: u32,
}

#[derive(Debug, Clone)]
pub struct StaticModel {
    pub name: String,
    pub origin: [f64; 3],
    /// Rows of the placement: forward, left, up (unit vectors).
    pub axis: [[f64; 3]; 3],
    pub scale: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Entity {
    pub keys: Vec<(String, String)>,
}

impl Entity {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Default)]
pub struct MapSource {
    pub name: String,
    pub entities: Vec<Entity>,
    /// The map's own brushes (entity 0, worldspawn).
    pub world: Vec<Brush>,
    /// Brushes of sub model i (`"model" "*i"`), as the collision map keeps them.
    pub submodels: Vec<(u32, Vec<Brush>)>,
    pub models: Vec<StaticModel>,
    pub report: Vec<String>,
}

fn f32_at(d: &[u8], o: usize) -> f64 {
    f32::from_le_bytes(d[o..o + 4].try_into().unwrap()) as f64
}
fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(d[o..o + 2].try_into().unwrap())
}

/// Address -> stream offset, for pointer words that lead to earlier arrays.
struct Resolver {
    spans: Vec<(u32, usize, usize)>,
}

impl Resolver {
    fn new(mut spans: Vec<(u32, usize, usize)>) -> Resolver {
        spans.sort_by_key(|s| s.0);
        Resolver { spans }
    }
    fn offset(&self, ptr: u32) -> Option<usize> {
        let i = self.spans.partition_point(|s| s.0 <= ptr);
        let (address, off, len) = *self.spans.get(i.checked_sub(1)?)?;
        let delta = (ptr - address) as usize;
        (delta < len.max(1)).then_some(off + delta)
    }
    /// Address of the array that starts at stream offset `off`.
    fn address_of(&self, off: usize) -> Option<u32> {
        self.spans.iter().find(|s| s.1 == off).map(|s| s.0)
    }
}

fn brushes_of(zone: &Zone, c: &ClipRec, r: &Resolver, notes: &mut Vec<String>) -> Vec<Brush> {
    let d = &zone.data;
    let (b0, n) = c.brushes;
    let mut out = Vec::with_capacity(n);
    let sides_base = r.address_of(c.sides.0);
    let mut running = 0usize;
    let (mut bad_plane, mut bad_range) = (0usize, 0usize);
    for i in 0..n {
        let b = b0 + 36 * i;
        let numsides = u16_at(d, b) as usize;
        let mut sides = Vec::with_capacity(numsides + 6);
        let material = |num: i32| -> String {
            usize::try_from(num).ok().and_then(|k| c.material_names.get(k)).cloned().flatten().unwrap_or_else(|| "caulk".to_string())
        };
        if c.bounds != 0 {
            let bo = c.bounds + 24 * i;
            let mid = [f32_at(d, bo), f32_at(d, bo + 4), f32_at(d, bo + 8)];
            let half = [f32_at(d, bo + 12), f32_at(d, bo + 16), f32_at(d, bo + 20)];
            for axis in 0..3 {
                for (side, sign) in [(0usize, -1.0f64), (1, 1.0)] {
                    let num = u16_at(d, b + 12 + 2 * (side * 3 + axis)) as i16 as i32;
                    let mut normal = [0.0; 3];
                    normal[axis] = sign;
                    let dist = if sign > 0.0 { mid[axis] + half[axis] } else { -(mid[axis] - half[axis]) };
                    sides.push(Side { normal, dist, material: material(num) });
                }
            }
        }
        // the stored sides are consecutive in the sides array
        let first = match (sides_base, u32_at(d, b + 4)) {
            (Some(base), p) if p >= base && ((p - base) % 8 == 0) => ((p - base) / 8) as usize,
            _ => running,
        };
        running += numsides;
        for k in 0..numsides {
            let idx = first + k;
            if idx >= c.sides.1 {
                bad_range += 1;
                continue;
            }
            let s = c.sides.0 + 8 * idx;
            let Some(po) = r.offset(u32_at(d, s)) else {
                bad_plane += 1;
                continue;
            };
            if po + 16 > d.len() {
                bad_plane += 1;
                continue;
            }
            let normal = [f32_at(d, po), f32_at(d, po + 4), f32_at(d, po + 8)];
            sides.push(Side { normal, dist: f32_at(d, po + 12), material: material(u16_at(d, s + 4) as i16 as i32) });
        }
        let contents = if c.contents != 0 { u32_at(d, c.contents + 4 * i) } else { 0 };
        out.push(Brush { sides, contents });
    }
    if bad_plane + bad_range > 0 {
        notes.push(format!("collision info {}: {bad_plane} side planes unresolved, {bad_range} sides out of range", c.tag));
    }
    out
}

/// The brush indices a leaf-brush tree reaches from node `start`: a node with a positive count lists brushes, any other one has
/// two children at `node + childOffset[k]`.
fn tree_brushes(zone: &Zone, c: &ClipRec, r: &Resolver, start: usize) -> Vec<usize> {
    let d = &zone.data;
    let (n0, count) = c.lb_nodes;
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![start];
    while let Some(i) = stack.pop() {
        if i >= count || !seen.insert(i) {
            continue;
        }
        let o = n0 + 20 * i;
        let leaves = u16_at(d, o + 2) as i16;
        if leaves > 0 {
            let ptr = u32_at(d, o + 8);
            let at = if ptr == u32::MAX { c.lb_inline.get(i).copied().filter(|&a| a != 0) } else { r.offset(ptr) };
            if let Some(at) = at {
                for k in 0..leaves as usize {
                    if at + 2 * k + 2 <= d.len() {
                        out.push(u16_at(d, at + 2 * k) as usize);
                    }
                }
            }
        } else {
            for k in 0..2 {
                stack.push(i + u16_at(d, o + 16 + 2 * k) as usize);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Parses the entity text of a map: `{ "key" "value" ... }` blocks.
pub fn parse_entities(text: &str) -> Vec<Entity> {
    let mut ents = Vec::new();
    let mut cur: Option<Entity> = None;
    let mut it = text.chars().peekable();
    let mut pending_key: Option<String> = None;
    while let Some(ch) = it.next() {
        match ch {
            '{' => {
                cur = Some(Entity::default());
                pending_key = None;
            }
            '}' => {
                if let Some(e) = cur.take() {
                    ents.push(e);
                }
            }
            '"' => {
                let mut s = String::new();
                for c in it.by_ref() {
                    if c == '"' {
                        break;
                    }
                    s.push(c);
                }
                match pending_key.take() {
                    None => pending_key = Some(s),
                    Some(k) => {
                        if let Some(e) = cur.as_mut() {
                            e.keys.push((k, s));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    ents
}

pub fn read(zone: &Zone, name: &str) -> Result<MapSource> {
    let mut r = Replay::new(zone).map_err(|f| anyhow::anyhow!("the walker stops in the zone prefix: {} at {}", f.what, f.at))?;
    r.record_addresses(true);
    for i in 0..zone.assets.len() {
        let _ = r.replay_asset(zone, i);
    }
    if r.rec.clips.is_empty() {
        bail!("this zone has no collision map in the QA / 3.19 layout");
    }
    let resolver = Resolver::new(std::mem::take(&mut r.rec.addrs));
    let mut out = MapSource { name: name.to_string(), ..Default::default() };

    if let Some(c) = r.rec.clips.iter().find(|c| c.tag == 0) {
        let all = brushes_of(zone, c, &resolver, &mut out.report);
        // brush model N owns the brushes its leaf-brush tree reaches; the rest is the world
        let (c0, n) = r.rec.cmodels;
        let mut owned = std::collections::HashSet::new();
        let mut models: Vec<(u32, Vec<usize>)> = Vec::new();
        for i in 1..n {
            let node = u32_at(&zone.data, c0 + 72 * i + 68) as usize;
            let list = tree_brushes(zone, c, &resolver, node);
            owned.extend(list.iter().copied());
            models.push((i as u32, list));
        }
        for (i, list) in models {
            out.submodels.push((i, list.iter().filter_map(|&b| all.get(b).cloned()).collect()));
        }
        out.world = all.into_iter().enumerate().filter(|(i, _)| !owned.contains(i)).map(|(_, b)| b).collect();
        out.report.push(format!("{} sub models own {} brushes", out.submodels.len(), owned.len()));
    }

    if let Some(w) = r.rec.world.as_ref() {
        let d = &zone.data;
        for (at, model) in &w.static_models {
            let Some(model) = model else { continue };
            let f = |i: usize| f32_at(d, at + 4 * i);
            out.models.push(StaticModel {
                name: model.clone(),
                origin: [f(0), f(1), f(2)],
                axis: [[f(3), f(4), f(5)], [f(6), f(7), f(8)], [f(9), f(10), f(11)]],
                scale: f(12),
            });
        }
    }

    let d = &zone.data;
    let mut best: Option<String> = None;
    for &(_, at, n) in &r.rec.map_ents {
        if n == 0 || at + n > d.len() {
            continue;
        }
        let text = String::from_utf8_lossy(&d[at..at + n - 1]).into_owned();
        if best.as_ref().map_or(true, |b| text.len() > b.len()) && text.contains("\"classname\"") {
            best = Some(text);
        }
    }
    if let Some(t) = best {
        out.entities = parse_entities(&t);
    } else {
        out.report.push("no entity string in the zone".to_string());
    }
    Ok(out)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    if l == 0.0 { a } else { [a[0] / l, a[1] / l, a[2] / l] }
}

/// Three points of the plane `n . x = d`, in the order Radiant reads them: `(p0 - p1) x (p2 - p1)` is the outward normal.
fn plane_points(n: [f64; 3], d: f64) -> [[f64; 3]; 3] {
    let n = norm(n);
    let helper = if n[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    let u = norm(cross(helper, n));
    let v = cross(n, u);
    // u x v = n
    let c = [n[0] * d, n[1] * d, n[2] * d];
    let s = 64.0;
    [
        [c[0] + u[0] * s, c[1] + u[1] * s, c[2] + u[2] * s],
        c,
        [c[0] + v[0] * s, c[1] + v[1] * s, c[2] + v[2] * s],
    ]
}

fn num(v: f64) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r == 0.0 { "0".to_string() } else { format!("{r}") }
}

fn write_brush(out: &mut String, index: usize, b: &Brush) {
    let _ = writeln!(out, "// brush {index}");
    out.push_str("{\n");
    for s in &b.sides {
        let p = plane_points(s.normal, s.dist);
        let mut line = String::new();
        for pt in p {
            let _ = write!(line, "( {} {} {} ) ", num(pt[0]), num(pt[1]), num(pt[2]));
        }
        let _ = writeln!(out, "{line}{} 128 128 0 0 0 0 lightmap_gray 16384 16384 0 0 0 0", s.material);
    }
    out.push_str("}\n");
}

/// Angles (pitch, yaw, roll) of a placement whose rows are forward, left and up.
pub fn axis_to_angles(axis: &[[f64; 3]; 3]) -> [f64; 3] {
    let f = axis[0];
    let (pitch, yaw) = if f[0] == 0.0 && f[1] == 0.0 {
        (if f[2] > 0.0 { 270.0 } else { 90.0 }, 0.0)
    } else {
        ((-f[2]).atan2((f[0] * f[0] + f[1] * f[1]).sqrt()).to_degrees(), f[1].atan2(f[0]).to_degrees())
    };
    let (sp, cp) = pitch.to_radians().sin_cos();
    let (sy, cy) = yaw.to_radians().sin_cos();
    let _ = cp;
    let right0 = [sy, -cy, 0.0];
    let up0 = [sp * cy, sp * sy, cp];
    let right = [-axis[1][0], -axis[1][1], -axis[1][2]];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let roll = (-dot(right, up0)).atan2(dot(right, right0)).to_degrees();
    let wrap = |a: f64| {
        let a = a % 360.0;
        if a < 0.0 { a + 360.0 } else { a }
    };
    [wrap(pitch), wrap(yaw), wrap(roll)]
}

/// The engine's `AnglesToAxis` (pitch, yaw, roll in degrees): rows forward, left, up. The inverse of [`axis_to_angles`]; a
/// static model's placement in the zone is this matrix (`GfxPackedPlacement`: origin, axis, scale).
pub fn angles_to_axis(angles: &[f64; 3]) -> [[f64; 3]; 3] {
    let (sp, cp) = angles[0].to_radians().sin_cos();
    let (sy, cy) = angles[1].to_radians().sin_cos();
    let (sr, cr) = angles[2].to_radians().sin_cos();
    [
        [cp * cy, cp * sy, -sp],
        [sr * sp * cy - sy * cr, sr * sp * sy + cr * cy, sr * cp],
        [cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_and_axis_are_inverse() {
        for angles in [[0.0, 0.0, 0.0], [0.0, 90.0, 0.0], [10.0, 200.0, 0.0], [350.0, 45.0, 30.0], [20.0, 300.0, 340.0], [90.0, 0.0, 0.0], [270.0, 0.0, 0.0]] {
            let axis = angles_to_axis(&angles);
            let back = angles_to_axis(&axis_to_angles(&axis));
            for r in 0..3 {
                for c in 0..3 {
                    assert!((axis[r][c] - back[r][c]).abs() < 1e-9, "{angles:?}: {axis:?} vs {back:?}");
                }
            }
        }
        // yaw 90: forward points along +y, left along -x
        let a = angles_to_axis(&[0.0, 90.0, 0.0]);
        assert!((a[0][1] - 1.0).abs() < 1e-12 && (a[1][0] + 1.0).abs() < 1e-12 && (a[2][2] - 1.0).abs() < 1e-12);
    }
}

pub fn to_map(m: &MapSource) -> String {
    let mut out = String::new();
    let mut entity_no = 0usize;
    let sub: std::collections::HashMap<u32, &Vec<Brush>> = m.submodels.iter().map(|(i, b)| (*i, b)).collect();
    for e in &m.entities {
        let _ = writeln!(out, "// entity {entity_no}");
        out.push_str("{\n");
        for (k, v) in &e.keys {
            let _ = writeln!(out, "\"{k}\" \"{v}\"");
        }
        let brushes: Option<&Vec<Brush>> = if entity_no == 0 {
            Some(&m.world)
        } else {
            e.get("model").and_then(|v| v.strip_prefix('*')).and_then(|v| v.parse::<u32>().ok()).and_then(|i| sub.get(&i).copied())
        };
        if let Some(bs) = brushes {
            for (i, b) in bs.iter().enumerate() {
                write_brush(&mut out, i, b);
            }
        }
        out.push_str("}\n");
        entity_no += 1;
    }
    for sm in &m.models {
        let _ = writeln!(out, "// entity {entity_no}");
        out.push_str("{\n\"classname\" \"misc_model\"\n");
        let _ = writeln!(out, "\"model\" \"{}\"", sm.name);
        let _ = writeln!(out, "\"origin\" \"{} {} {}\"", num(sm.origin[0]), num(sm.origin[1]), num(sm.origin[2]));
        let a = axis_to_angles(&sm.axis);
        let _ = writeln!(out, "\"angles\" \"{} {} {}\"", num(a[0]), num(a[1]), num(a[2]));
        if (sm.scale - 1.0).abs() > 0.0005 {
            let _ = writeln!(out, "\"modelscale\" \"{}\"", num(sm.scale));
        }
        out.push_str("}\n");
        entity_no += 1;
    }
    out
}
