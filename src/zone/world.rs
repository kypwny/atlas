//! A map's drawable geometry as a `Model` the viewer can show: the world's surfaces, grouped by material.

use super::replay::{Recorder, WorldRec};
use crate::model::{Model, Surface, Vertex, unpack_unit};
use crate::modelindex::{SEMANTIC_COLOR, SEMANTIC_NORMAL, SEMANTIC_SPECULAR, SurfaceTextures};
use std::collections::HashMap;

fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}

/// Builds the map as one model surface per material, with the material names in the same order. Vertices follow the world's
/// 44-byte layout (position, binormal sign, colour, texture and lightmap coordinates, packed normal and tangent); a surface's
/// triangles index its own run of vertices. A surface whose records run outside the image is skipped.
pub fn world_model(image: &[u8], w: &WorldRec) -> (Model, Vec<String>) {
    let (vat, vcount) = w.vertices;
    let (iat, icount) = w.indices;
    let mut by_material: HashMap<String, usize> = HashMap::new();
    let mut names: Vec<String> = Vec::new();
    let mut surfaces: Vec<Surface> = Vec::new();
    // per source surface: a map from the world's vertex number to the material surface's own vertex number
    for (record, material) in &w.surfaces {
        if record + 24 > image.len() {
            continue;
        }
        // surfaces share banks of vertices: `first` is where the bank starts and the 16-bit indices count from it. Only the
        // vertices a surface uses are copied (the word at +8 is the bank size in stock zones and flags in CoDOL ones).
        let (first, triangles, base) = (u32_at(image, record + 4) as usize, u16_at(image, record + 10) as usize, u32_at(image, record + 12) as usize);
        if triangles == 0 || base + 3 * triangles > icount || first >= vcount {
            continue;
        }
        let indices: Vec<u32> = (0..3 * triangles).map(|k| u16_at(image, iat + 2 * (base + k)) as u32).collect();
        if indices.iter().any(|&i| first + i as usize >= vcount) {
            continue;
        }
        let name = material.clone().unwrap_or_else(|| "unknown".into());
        let slot = *by_material.entry(name.clone()).or_insert_with(|| {
            names.push(name.clone());
            surfaces.push(Surface { vertices: Vec::new(), indices: Vec::new(), rigid: Vec::new(), blend_counts: [0; 4], blend: Vec::new(), header: [0; 84] });
            surfaces.len() - 1
        });
        let s = &mut surfaces[slot];
        let mut remap: HashMap<u32, u32> = HashMap::new();
        for &i in &indices {
            let new = *remap.entry(i).or_insert_with(|| {
                let o = vat + 44 * (first + i as usize);
                s.vertices.push(Vertex {
                    pos: [f32_at(image, o), f32_at(image, o + 4), f32_at(image, o + 8)],
                    binormal_sign: f32_at(image, o + 12),
                    color: [image[o + 16], image[o + 17], image[o + 18], image[o + 19]],
                    uv: [f32_at(image, o + 20), f32_at(image, o + 24)],
                    normal: unpack_unit(u32_at(image, o + 36)),
                    tangent: unpack_unit(u32_at(image, o + 40)),
                });
                s.vertices.len() as u32 - 1
            });
            s.indices.push(new);
        }
    }
    (Model { name: w.name.clone().unwrap_or_else(|| "map".into()), surfaces }, names)
}

/// What a light does, from the engine's light type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightKind {
    Sun,
    Spot,
    Omni,
    /// Type 0 (an unused slot) or a value this reader does not know.
    Other,
}

impl LightKind {
    /// Modern Warfare 2 and CoDOL: 1 is directional (the sun), 2 omni (a point light: its definition is `light_point_linear`), 3 spot.
    pub fn of(ty: u8) -> LightKind {
        match ty {
            1 => LightKind::Sun,
            2 => LightKind::Omni,
            3 => LightKind::Spot,
            _ => LightKind::Other,
        }
    }

    /// Black Ops II: 1 is directional, 2 spot (with a cone and a gobo definition); 5 stands for the point lights of the hero list (no
    /// direction or cone), and the primary lights of type 5 are positional too.
    pub fn of_t6(ty: u8) -> LightKind {
        match ty {
            1 => LightKind::Sun,
            2 => LightKind::Spot,
            3..=6 => LightKind::Omni,
            _ => LightKind::Other,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LightKind::Sun => "sun",
            LightKind::Spot => "spot",
            LightKind::Omni => "omni",
            LightKind::Other => "other",
        }
    }
}

/// One light of a map: a primary light of the Com world (`ComPrimaryLight`) or a hero-only light of the GfxWorld (`GfxHeroLight`).
#[derive(Debug, Clone, PartialEq)]
pub struct MapLight {
    pub kind: LightKind,
    /// The engine's type byte.
    pub ty: u8,
    pub origin: [f32; 3],
    /// Linear colour; values above 1 are intensities.
    pub color: [f32; 3],
    /// Where the light points (for the sun: the direction its rays travel).
    pub direction: [f32; 3],
    /// Reach in game units (0 for the sun).
    pub radius: f32,
    pub cos_outer: f32,
    pub cos_inner: f32,
    /// Spot exponent (a byte in primary lights, an int in hero lights).
    pub exponent: i32,
    /// A hero-only light (lights only the player and weapon models) rather than a primary light.
    pub hero: bool,
    /// The light definition's name (primary lights of the CoDOL and stock zones).
    pub name: Option<String>,
}

/// The lights a map zone holds, as read while walking it.
#[derive(Debug, Clone, Default)]
pub struct MapLights {
    /// The Com world's primary lights in zone order (index 0 is an unused slot in most maps).
    pub primary: Vec<MapLight>,
    /// The GfxWorld's hero-only lights.
    pub hero: Vec<MapLight>,
    /// The GfxWorld's `sunPrimaryLightIndex`: which primary light is the sun.
    pub sun_index: Option<u32>,
}

impl MapLights {
    /// Primary lights that are used (type not 0) followed by the hero lights.
    pub fn all(&self) -> impl Iterator<Item = &MapLight> {
        self.primary.iter().filter(|l| l.kind != LightKind::Other).chain(self.hero.iter())
    }

    /// The sun: the primary light the world names, or else the first directional one.
    pub fn sun(&self) -> Option<&MapLight> {
        self.sun_index.and_then(|i| self.primary.get(i as usize)).filter(|l| l.kind == LightKind::Sun).or_else(|| self.primary.iter().find(|l| l.kind == LightKind::Sun))
    }

    pub fn is_empty(&self) -> bool {
        self.primary.is_empty() && self.hero.is_empty()
    }
}

/// The colour, normal and specular image names of every material the walk saw, by material name.
pub fn material_textures(rec: &Recorder) -> HashMap<String, SurfaceTextures> {
    let mut out = HashMap::new();
    for m in &rec.materials {
        let Some(name) = &m.name else { continue };
        // an image is named where it was loaded (inline) or through the address an earlier load registered
        let name_of = |t: &super::replay::TexRec| -> Option<String> {
            let start = if t.image_start >= 0 { Some(t.image_start as usize) } else { rec.aliases.get(&(10, t.image_ref)).copied() };
            start.and_then(|s| rec.images.get(&s).cloned().flatten())
        };
        let image = |semantic: u8| m.textures.iter().filter(|t| t.semantic == semantic).find_map(name_of);
        let color = image(SEMANTIC_COLOR).or_else(|| m.textures.iter().find_map(name_of));
        out.entry(name.clone()).or_insert(SurfaceTextures { color, normal: image(SEMANTIC_NORMAL), spec: image(SEMANTIC_SPECULAR) });
    }
    out
}

/// The surface textures of a model built by `world_model`, in the order of its material names.
pub fn textures_for(names: &[String], by_material: &HashMap<String, SurfaceTextures>) -> Vec<SurfaceTextures> {
    names.iter().map(|n| by_material.get(n).cloned().unwrap_or_default()).collect()
}

/// Replays a zone image (CoDOL, with its directory) and returns the map it holds, its material names and their textures.
pub fn model_of_zone(zone: &super::Zone) -> Option<(Model, Vec<String>, Vec<SurfaceTextures>)> {
    map_of_zone(zone).map(|(m, n, t, _)| (m, n, t))
}

/// Like [`model_of_zone`], with the lights the walk met (the Com world's primary lights and the GfxWorld's hero lights).
pub fn map_of_zone(zone: &super::Zone) -> Option<(Model, Vec<String>, Vec<SurfaceTextures>, MapLights)> {
    let mut r = super::replay::Replay::new(zone).ok()?;
    for i in 0..zone.assets.len() {
        let _ = r.replay_asset(zone, i);
    }
    let w = r.rec.world.take()?;
    let (model, names) = world_model(&zone.data, &w);
    let textures = textures_for(&names, &material_textures(&r.rec));
    Some((model, names, textures, std::mem::take(&mut r.rec.lights)))
}

/// The 980 bytes a CoDOL (QA 3.3.3.3 / 3.19) GfxWorld header holds between the sun and the outdoor image, which IW4's does not
/// (offsets are from the insert's start, which is IW4 header offset 412). Decoded from the headers of QA map zones; the field
/// names describe what the values look like, not what the engine calls them.
#[derive(Debug, Clone, PartialEq)]
pub struct WorldInsert {
    /// Two floats at +4 (256 / 0.51 in mp_afghan) and the count of records in use.
    pub lead: [f32; 2],
    pub count: u32,
    /// Up to 24 records of nine words from +24: two halves (0.5), four colour-like factors, a distance-like value (200, 150, 120, 100 ... 33, 70 ...),
    /// and two zero words. A map uses the first `count`; the rest of the table is zero.
    pub records: Vec<[f32; 9]>,
    /// Very small scales (1e-5..1e-3) at +0x394, +0x3a8 and +0x3bc (zero words between).
    pub scales: [f32; 3],
    /// +0x3c4: an RGBA colour that ends in 1.0 (the insert's last 16 bytes).
    pub colour: [f32; 4],
}

pub const INSERT_OFFSET: usize = 412;
pub const INSERT_SIZE: usize = 980;
pub const INSERT_RECORDS: usize = 24;

/// Reads the insert out of a CoDOL GfxWorld header (the asset's first 1,608 bytes).
pub fn world_insert(header: &[u8]) -> Option<WorldInsert> {
    if header.len() < INSERT_OFFSET + INSERT_SIZE {
        return None;
    }
    let w = |o: usize| u32::from_le_bytes(header[INSERT_OFFSET + o..INSERT_OFFSET + o + 4].try_into().unwrap());
    let f = |o: usize| f32::from_bits(w(o));
    let count = w(12);
    if count as usize > INSERT_RECORDS {
        return None;
    }
    let records = (0..count as usize).map(|i| std::array::from_fn(|k| f(24 + i * 36 + k * 4))).collect();
    Some(WorldInsert {
        lead: [f(4), f(8)],
        count,
        records,
        scales: [f(0x394), f(0x3a8), f(0x3bc)],
        colour: [f(0x3c4), f(0x3c8), f(0x3cc), f(0x3d0)],
    })
}
