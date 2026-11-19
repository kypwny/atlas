//! The geometry of a Black Ops II map (`GfxWorld`) as a [`Model`] the map view can show: one surface per material, like
//! `world::world_model` does for the CoDOL and stock MW2 worlds.
//!
//! Layout (from the OpenAssetTools Black Ops II headers and checked against `mp_nuketown_2020`): `GfxWorld::draw` holds the
//! vertex bytes (`vd0`, `vertexDataSize0`), the 16-bit indices and their count. Each `GfxSurface` has a run of triangles
//! (`baseIndex`, `triCount`), a byte offset into the vertex bytes (`vertexDataOffset0`) and a material. The 16-bit indices count
//! vertices from that offset; the vertex count and first-vertex words of the record are not reliable. A vertex is 36 bytes: the
//! position (3 floats), the binormal sign, an RGBA colour, the texture coordinates (two 16-bit floats, U low), the packed
//! normal and tangent (see `t6_models::unpack_unit_vec`) and the lightmap coordinates (two 16-bit values, not used here).
//! Of 300,840 indices in `mp_nuketown_2020`, every one lands on a vertex inside its surface's bounding box this way.

use super::schema_walk::{Game, View, WalkReport, ZoneImage};
use super::t6_models::{T6Material, T6Model, half_to_f32, unpack_unit_vec};
use super::world::{LightKind, MapLight, MapLights};
use crate::model::{Model, Surface, Vertex};
use crate::modelindex::SurfaceTextures;
use std::collections::HashMap;

const VERTEX_BYTES: usize = 36;

#[derive(Clone, Debug, Default)]
pub struct T6WorldSurface {
    /// Byte offset of the surface's vertices in `T6World::vertex_data`.
    pub vertex_offset: u32,
    /// First index and triangle count in `T6World::indices`.
    pub base_index: u32,
    pub tri_count: u16,
    pub material: Option<T6Material>,
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
}

/// A static model placed in the map (`GfxStaticModelDrawInst`): a model and where it stands. A model-space point `p` is at
/// `origin + scale * (p.x * axis[0] + p.y * axis[1] + p.z * axis[2])` in the world.
#[derive(Clone, Debug, Default)]
pub struct T6StaticInstance {
    /// Index into [`T6World::models`].
    pub model: usize,
    pub origin: [f32; 3],
    pub axis: [[f32; 3]; 3],
    pub scale: f32,
    /// The walker's id of the model asset until it is resolved to `model`.
    id: u32,
}

#[derive(Clone, Debug, Default)]
pub struct T6World {
    pub name: String,
    pub mins: [f32; 3],
    pub maxs: [f32; 3],
    pub vertex_data: Vec<u8>,
    pub indices: Vec<u16>,
    pub surfaces: Vec<T6WorldSurface>,
    /// The models the static instances use (only when they were asked for).
    pub models: Vec<T6Model>,
    pub instances: Vec<T6StaticInstance>,
    /// Instances whose model could not be found in the zone (a stub that names a model another zone holds).
    pub static_missing: usize,
}

/// Decodes the `GfxWorld` whose header is at `hdr` (called from the walker's hook).
pub(crate) fn read_world(v: &View, hdr: u32, materials: &HashMap<u32, T6Material>, want_static: bool) -> Option<T6World> {
    let m = |s: &str, n: &str| v.member(s, n);
    let draw = hdr + m("GfxWorld", "draw")?;
    let size0 = v.u32(draw + m("GfxWorldDraw", "vertexDataSize0")?)? as usize;
    let vd0 = v.ptr(draw + m("GfxWorldDraw", "vd0")?)?;
    let index_count = v.u32(draw + m("GfxWorldDraw", "indexCount")?)? as usize;
    let indices_ptr = v.ptr(draw + m("GfxWorldDraw", "indices")?)?;
    if vd0 == 0 || indices_ptr == 0 {
        return None;
    }
    let mut w = T6World { name: v.cstr(v.ptr(hdr + m("GfxWorld", "name")?)?).unwrap_or_default(), ..Default::default() };
    let (mn, mx) = (hdr + m("GfxWorld", "mins")?, hdr + m("GfxWorld", "maxs")?);
    for k in 0..3u32 {
        w.mins[k as usize] = v.f32(mn + 4 * k)?;
        w.maxs[k as usize] = v.f32(mx + 4 * k)?;
    }
    w.vertex_data = v.bytes(vd0, size0)?.to_vec();
    w.indices = v.bytes(indices_ptr, index_count * 2)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let count = v.u32(hdr + m("GfxWorld", "surfaceCount")?)?;
    let dpvs = hdr + m("GfxWorld", "dpvs")?;
    if want_static {
        let n = v.u32(dpvs + m("GfxWorldDpvsStatic", "smodelCount")?)?;
        let insts = v.ptr(dpvs + m("GfxWorldDpvsStatic", "smodelDrawInsts")?)?;
        if insts != 0 {
            let size = v.struct_size("GfxStaticModelDrawInst")?;
            let (place, model) = (m("GfxStaticModelDrawInst", "placement")?, m("GfxStaticModelDrawInst", "model")?);
            let (po, pa, ps) = (m("GfxPackedPlacement", "origin")?, m("GfxPackedPlacement", "axis")?, m("GfxPackedPlacement", "scale")?);
            for i in 0..n {
                let at = insts + i * size;
                let f = |o: u32| v.f32(o);
                let mut inst = T6StaticInstance { scale: f(at + place + ps)?, ..Default::default() };
                for k in 0..3u32 {
                    inst.origin[k as usize] = f(at + place + po + 4 * k)?;
                    for j in 0..3u32 {
                        inst.axis[k as usize][j as usize] = f(at + place + pa + 12 * k + 4 * j)?;
                    }
                }
                match v.asset_id_at_slot(at + model) {
                    Some(id) => {
                        inst.id = id;
                        w.instances.push(inst);
                    }
                    None => w.static_missing += 1,
                }
            }
        }
    }
    let surfaces = v.ptr(dpvs + m("GfxWorldDpvsStatic", "surfaces")?)?;
    if surfaces == 0 {
        return Some(w);
    }
    let size = v.struct_size("GfxSurface")?;
    let (tris, material, bounds) = (m("GfxSurface", "tris")?, m("GfxSurface", "material")?, m("GfxSurface", "bounds")?);
    let (vo0, tri_count, base) = (m("srfTriangles_t", "vertexDataOffset0")?, m("srfTriangles_t", "triCount")?, m("srfTriangles_t", "baseIndex")?);
    for i in 0..count {
        let s = surfaces + i * size;
        let mut surf = T6WorldSurface {
            vertex_offset: v.u32(s + tris + vo0)?,
            base_index: v.u32(s + tris + base)?,
            tri_count: v.u16(s + tris + tri_count)?,
            material: v.asset_id_at_slot(s + material).and_then(|id| materials.get(&id)).cloned(),
            ..Default::default()
        };
        for k in 0..3u32 {
            surf.mins[k as usize] = v.f32(s + bounds + 4 * k)?;
            surf.maxs[k as usize] = v.f32(s + bounds + 12 + 4 * k)?;
        }
        w.surfaces.push(surf);
    }
    Some(w)
}

fn light_at(v: &View, at: u32, primary: bool, name: Option<String>) -> Option<MapLight> {
    // ComPrimaryLight (196 bytes) and GfxHeroLight (56 bytes) start alike: type, three bytes, colour, direction, origin, radius, cosHalfFovOuter
    // and cosHalfFovInner (at 44, 48 in the primary light, 40, 44, 48 in the hero light)
    let sname = if primary { "ComPrimaryLight" } else { "GfxHeroLight" };
    let m = |n: &str| v.member(sname, n);
    let vec3 = |n: &str| -> Option<[f32; 3]> { let o = at + m(n)?; Some([v.f32(o)?, v.f32(o + 4)?, v.f32(o + 8)?]) };
    let ty = v.u8(at + m("type")?)?;
    Some(MapLight {
        kind: LightKind::of_t6(ty),
        ty,
        color: vec3("color")?,
        direction: vec3("dir")?,
        origin: vec3("origin")?,
        radius: v.f32(at + m("radius")?)?,
        cos_outer: v.f32(at + m("cosHalfFovOuter")?)?,
        cos_inner: v.f32(at + m("cosHalfFovInner")?)?,
        exponent: if primary { v.u8(at + m("exponent")?)? as i32 } else { v.u32(at + m("exponent")?)? as i32 },
        hero: !primary,
        name,
    })
}

/// Reads the primary lights of the `ComWorld` whose header is at `hdr` (called from the walker's hook).
pub(crate) fn read_com_lights(v: &View, hdr: u32, out: &mut MapLights) {
    let Some(count) = v.member("ComWorld", "primaryLightCount").and_then(|o| v.u32(hdr + o)) else { return };
    let Some(ptr) = v.member("ComWorld", "primaryLights").and_then(|o| v.ptr(hdr + o)) else { return };
    let Some(size) = v.struct_size("ComPrimaryLight") else { return };
    if ptr == 0 || count > 4096 {
        return;
    }
    let def = v.member("ComPrimaryLight", "defName");
    for i in 0..count {
        let at = ptr + i * size;
        let name = def.and_then(|o| v.ptr(at + o)).and_then(|p| v.cstr(p));
        match light_at(v, at, true, name) {
            Some(l) => out.primary.push(l),
            None => break,
        }
    }
}

/// Reads the hero-only lights and the sun's light index of the `GfxWorld` whose header is at `hdr`.
pub(crate) fn read_hero_lights(v: &View, hdr: u32, out: &mut MapLights) {
    out.sun_index = v.member("GfxWorld", "sunPrimaryLightIndex").and_then(|o| v.u32(hdr + o));
    let Some(count) = v.member("GfxWorld", "heroLightCount").and_then(|o| v.u32(hdr + o)) else { return };
    let Some(ptr) = v.member("GfxWorld", "heroLights").and_then(|o| v.ptr(hdr + o)) else { return };
    let Some(size) = v.struct_size("GfxHeroLight") else { return };
    if ptr == 0 || count > 4096 {
        return;
    }
    for i in 0..count {
        match light_at(v, ptr + i * size, false, None) {
            Some(l) => out.hero.push(l),
            None => break,
        }
    }
}

fn vertex(b: &[u8]) -> Vertex {
    let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let u = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let tc = u(20);
    let unit = |v: [f32; 3]| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if l > 1e-6 { [v[0] / l, v[1] / l, v[2] / l] } else { v }
    };
    Vertex {
        pos: [f(0), f(4), f(8)],
        binormal_sign: f(12),
        color: [b[16], b[17], b[18], b[19]],
        uv: [half_to_f32((tc & 0xFFFF) as u16), half_to_f32((tc >> 16) as u16)],
        normal: unit(unpack_unit_vec(u(24))),
        tangent: unit(unpack_unit_vec(u(28))),
    }
}

impl T6World {
    /// Points the instances at the models decoded from the zone (`ids` maps the walker's asset ids to indices of `decoded`);
    /// only the models the instances use are kept, and an instance whose model is missing is dropped and counted.
    pub(crate) fn resolve_static_models(&mut self, ids: &HashMap<u32, usize>, decoded: &[T6Model]) {
        let mut local: HashMap<usize, usize> = HashMap::new();
        let mut kept = Vec::new();
        for mut inst in std::mem::take(&mut self.instances) {
            let Some(&idx) = ids.get(&inst.id) else {
                self.static_missing += 1;
                continue;
            };
            inst.model = match local.get(&idx) {
                Some(&l) => l,
                None => {
                    self.models.push(decoded[idx].clone());
                    local.insert(idx, self.models.len() - 1);
                    self.models.len() - 1
                }
            };
            kept.push(inst);
        }
        self.instances = kept;
    }

    /// One model surface per material (names in the same order, textures alongside), each holding only the vertices its
    /// triangles use. A surface whose indices run outside the vertex bytes is skipped.
    pub fn to_model(&self) -> (Model, Vec<String>, Vec<SurfaceTextures>) {
        self.to_model_with(false)
    }

    /// Like [`to_model`](Self::to_model); with `with_static` the placed static models (LOD 0, instance transforms applied)
    /// are added to the surfaces of their materials. A model with no geometry (a stub) adds nothing.
    pub fn to_model_with(&self, with_static: bool) -> (Model, Vec<String>, Vec<SurfaceTextures>) {
        let mut by_material: HashMap<String, usize> = HashMap::new();
        let (mut names, mut textures, mut surfaces): (Vec<String>, Vec<SurfaceTextures>, Vec<Surface>) = Default::default();
        for s in &self.surfaces {
            let (base, n) = (s.base_index as usize, 3 * s.tri_count as usize);
            let Some(ix) = self.indices.get(base..base + n) else { continue };
            if n == 0 || ix.iter().any(|&i| s.vertex_offset as usize + (i as usize + 1) * VERTEX_BYTES > self.vertex_data.len()) {
                continue;
            }
            let name = s.material.as_ref().map(|m| m.name.trim_start_matches(',').to_string()).unwrap_or_else(|| "unknown".into());
            let slot = *by_material.entry(name.clone()).or_insert_with(|| {
                names.push(name.clone());
                textures.push(s.material.as_ref().map(|m| SurfaceTextures { color: m.color.clone(), normal: m.normal.clone(), spec: m.spec.clone() }).unwrap_or_default());
                surfaces.push(Surface { vertices: Vec::new(), indices: Vec::new(), rigid: Vec::new(), blend_counts: [0; 4], blend: Vec::new(), header: [0; 84] });
                surfaces.len() - 1
            });
            let out = &mut surfaces[slot];
            let mut remap: HashMap<u16, u32> = HashMap::new();
            for &i in ix {
                let new = *remap.entry(i).or_insert_with(|| {
                    let o = s.vertex_offset as usize + i as usize * VERTEX_BYTES;
                    out.vertices.push(vertex(&self.vertex_data[o..o + VERTEX_BYTES]));
                    out.vertices.len() as u32 - 1
                });
                out.indices.push(new);
            }
        }
        if with_static {
            for inst in &self.instances {
                let Some(m) = self.models.get(inst.model) else { continue };
                let Some(lod) = m.lods.first() else { continue };
                let a = inst.axis;
                let place = |p: [f32; 3]| -> [f32; 3] { std::array::from_fn(|c| inst.origin[c] + inst.scale * (p[0] * a[0][c] + p[1] * a[1][c] + p[2] * a[2][c])) };
                let turn = |p: [f32; 3]| -> [f32; 3] { std::array::from_fn(|c| p[0] * a[0][c] + p[1] * a[1][c] + p[2] * a[2][c]) };
                for (k, s) in m.lod_surfaces(0).iter().enumerate() {
                    if s.vertices.is_empty() || s.triangles.is_empty() {
                        continue;
                    }
                    let mat = m.materials.get(lod.surf_index as usize + k).and_then(|x| x.as_ref());
                    let name = mat.map(|x| x.name.trim_start_matches(',').to_string()).unwrap_or_else(|| "unknown".into());
                    let slot = *by_material.entry(name.clone()).or_insert_with(|| {
                        names.push(name.clone());
                        textures.push(mat.map(|x| SurfaceTextures { color: x.color.clone(), normal: x.normal.clone(), spec: x.spec.clone() }).unwrap_or_default());
                        surfaces.push(Surface { vertices: Vec::new(), indices: Vec::new(), rigid: Vec::new(), blend_counts: [0; 4], blend: Vec::new(), header: [0; 84] });
                        surfaces.len() - 1
                    });
                    let out = &mut surfaces[slot];
                    let base = out.vertices.len() as u32;
                    out.vertices.extend(s.vertices.iter().map(|v| Vertex { pos: place(v.pos), normal: turn(v.normal), tangent: turn(v.tangent), binormal_sign: v.binormal_sign, uv: v.uv, color: v.color }));
                    out.indices.extend(s.triangles.iter().flat_map(|t| t.map(|i| base + i as u32)));
                }
            }
        }
        (Model { name: if self.name.is_empty() { "map".into() } else { self.name.clone() }, surfaces }, names, textures)
    }
}

/// Walks a Black Ops II zone and returns the map it holds, with its material names and their textures; `None` when the zone has
/// no `GfxWorld`.
pub fn model_of_t6_zone(zone: &ZoneImage) -> Option<(Model, Vec<String>, Vec<SurfaceTextures>)> {
    model_of_t6_zone_with(zone, false)
}

/// [`model_of_t6_zone`] with the map's placed static models added when `with_static` is set.
pub fn model_of_t6_zone_with(zone: &ZoneImage, with_static: bool) -> Option<(Model, Vec<String>, Vec<SurfaceTextures>)> {
    let (models, _) = super::t6_models::read_zone_with(zone, false, true, with_static);
    models.world.map(|w| w.to_model_with(with_static))
}

/// [`model_of_t6_zone_with`] and the map's lights.
pub fn map_of_t6_zone(zone: &ZoneImage, with_static: bool) -> Option<(Model, Vec<String>, Vec<SurfaceTextures>, MapLights)> {
    let (models, _) = super::t6_models::read_zone_with(zone, false, true, with_static);
    let lights = models.lights;
    models.world.map(|w| {
        let (m, n, t) = w.to_model_with(with_static);
        (m, n, t, lights)
    })
}

/// Opens a Black Ops II fastfile and returns its map and lights (see [`map_of_t6_zone`]) with the walk's report.
pub fn map_of_t6_file(path: &std::path::Path, with_static: bool) -> anyhow::Result<(Option<(Model, Vec<String>, Vec<SurfaceTextures>, MapLights)>, WalkReport)> {
    use anyhow::Context;
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let zone = ZoneImage::open(Game::T6, &name, &bytes).with_context(|| format!("opening {}", path.display()))?;
    let (models, report) = super::t6_models::read_zone_with(&zone, false, true, with_static);
    let lights = models.lights;
    Ok((
        models.world.map(|w| {
            let (m, n, t) = w.to_model_with(with_static);
            (m, n, t, lights)
        }),
        report,
    ))
}

/// Opens a Black Ops II fastfile and returns its map (see [`model_of_t6_zone`]) with the walk's report.
pub fn model_of_t6_file(path: &std::path::Path) -> anyhow::Result<(Option<(Model, Vec<String>, Vec<SurfaceTextures>)>, WalkReport)> {
    model_of_t6_file_with(path, false)
}

/// [`model_of_t6_file`] with the map's placed static models added when `with_static` is set.
pub fn model_of_t6_file_with(path: &std::path::Path, with_static: bool) -> anyhow::Result<(Option<(Model, Vec<String>, Vec<SurfaceTextures>)>, WalkReport)> {
    use anyhow::Context;
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let zone = ZoneImage::open(Game::T6, &name, &bytes).with_context(|| format!("opening {}", path.display()))?;
    let (models, report) = super::t6_models::read_zone_with(&zone, false, true, with_static);
    Ok((models.world.map(|w| w.to_model_with(with_static)), report))
}
