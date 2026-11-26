//! Effect definitions read from a zone, and a library of them by name with the materials they use.

use crate::modelindex::{MaterialState, SurfaceTextures};
use crate::zone::replay::{FxElemRec, FxRec, FxVisualRec, Recorder, Replay};
use crate::zone::{Zone, fastfile};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

/// Element flags in the IW4 numbering (CoDOL zones store bits 8 to 15 one bit higher; [`Elem::flags`] is converted).
pub mod flags {
    pub const SPAWN_RELATIVE_TO_EFFECT: u32 = 0x2;
    pub const SPAWN_OFFSET_SPHERE: u32 = 0x10;
    pub const SPAWN_OFFSET_CYLINDER: u32 = 0x20;
    pub const SPAWN_OFFSET_MASK: u32 = 0x30;
    pub const RUN_RELATIVE_TO_SPAWN: u32 = 0x40;
    pub const RUN_RELATIVE_TO_EFFECT: u32 = 0x80;
    pub const RUN_RELATIVE_TO_OFFSET: u32 = 0xC0;
    pub const RUN_MASK: u32 = 0xC0;
    pub const USE_COLLISION: u32 = 0x100;
    pub const DIE_ON_TOUCH: u32 = 0x200;
    pub const HAS_VELOCITY_GRAPH_LOCAL: u32 = 0x100_0000;
    pub const HAS_VELOCITY_GRAPH_WORLD: u32 = 0x200_0000;
    pub const HAS_GRAVITY: u32 = 0x400_0000;
    pub const USE_MODEL_PHYSICS: u32 = 0x800_0000;
    pub const NONUNIFORM_SCALE: u32 = 0x1000_0000;
    pub const DISABLED: u32 = 0x8000_0000;
}

/// Element types in the CoDOL numbering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElemType {
    Billboard,
    Oriented,
    Rotated,
    Tail,
    Line,
    Trail,
    Cloud,
    SparkCloud,
    SparkFountain,
    Model,
    OmniLight,
    SpotLight,
    Sound,
    Decal,
    Runner,
    Unknown(u8),
}

impl ElemType {
    pub fn from_codol(t: u8) -> ElemType {
        use ElemType::*;
        match t {
            0 => Billboard,
            1 => Oriented,
            2 => Rotated,
            3 => Tail,
            4 => Line,
            5 => Trail,
            6 => Cloud,
            7 => SparkCloud,
            8 => SparkFountain,
            9 => Model,
            10 => OmniLight,
            11 => SpotLight,
            12 => Sound,
            13 => Decal,
            14 => Runner,
            n => Unknown(n),
        }
    }

    pub fn name(self) -> &'static str {
        use ElemType::*;
        match self {
            Billboard => "billboard sprite",
            Oriented => "oriented sprite",
            Rotated => "rotated sprite",
            Tail => "tail",
            Line => "line",
            Trail => "trail",
            Cloud => "cloud",
            SparkCloud => "spark cloud",
            SparkFountain => "spark fountain",
            Model => "model",
            OmniLight => "omni light",
            SpotLight => "spot light",
            Sound => "sound",
            Decal => "decal",
            Runner => "runner",
            Unknown(_) => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Range {
    pub base: f32,
    pub amp: f32,
}

impl Range {
    pub fn at(self, r: f32) -> f32 {
        self.base + self.amp * r
    }
}

/// `FxElemVec3Range`: a base vector and a random amplitude.
#[derive(Clone, Copy, Debug, Default)]
pub struct Vec3Range {
    pub base: [f32; 3],
    pub amp: [f32; 3],
}

/// `FxElemVelStateSample`: velocity and its integral, in the element's frame (`local`) and the world's.
#[derive(Clone, Copy, Debug, Default)]
pub struct VelSample {
    pub local_velocity: Vec3Range,
    pub local_delta: Vec3Range,
    pub world_velocity: Vec3Range,
    pub world_delta: Vec3Range,
}

/// `FxElemVisualState`.
#[derive(Clone, Copy, Debug, Default)]
pub struct VisState {
    pub color: [u8; 4],
    pub rotation_delta: f32,
    pub rotation_total: f32,
    pub size: [f32; 2],
    pub scale: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct VisSample {
    pub base: VisState,
    pub amp: VisState,
}

/// `FxElemAtlas`: how a sprite steps through the cells of its texture.
#[derive(Clone, Copy, Debug, Default)]
pub struct Atlas {
    pub behavior: u8,
    pub index: u8,
    pub fps: u8,
    pub loop_count: u8,
    pub col_bits: u8,
    pub row_bits: u8,
    pub entry_count: i16,
}

/// `FxTrailDef`: the cross-section of a trail ribbon and how it is cut.
#[derive(Clone, Debug, Default)]
pub struct Trail {
    pub scroll_time_msec: i32,
    pub repeat_dist: i32,
    pub inv_split_dist: f32,
    pub inv_split_arc_dist: f32,
    pub inv_split_time: f32,
    /// (x, y, u) of each cross-section vertex.
    pub verts: Vec<[f32; 3]>,
    pub indices: Vec<u16>,
}

/// `FxSparkFountainDef`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fountain {
    pub gravity: f32,
    pub bounce_frac: f32,
    pub bounce_rand: f32,
    pub spark_spacing: f32,
    pub spark_length: f32,
    pub spark_count: i32,
    pub loop_time: f32,
    pub vel_min: f32,
    pub vel_max: f32,
    pub vel_cone_frac: f32,
    pub rest_speed: f32,
    pub boost_time: f32,
    pub boost_factor: f32,
}

/// What an element draws.
#[derive(Clone, Debug)]
pub enum Visual {
    Material { name: String, textures: SurfaceTextures, state: Option<MaterialState> },
    Model(String),
    /// A sound alias, or the effect a runner starts.
    Name(String),
    None,
}

impl Visual {
    pub fn label(&self) -> &str {
        match self {
            Visual::Material { name, .. } | Visual::Model(name) | Visual::Name(name) => name,
            Visual::None => "-",
        }
    }
}

/// `FxElemDef`.
#[derive(Clone, Debug)]
pub struct Elem {
    /// IW4 numbering, see [`flags`].
    pub flags: u32,
    pub elem_type: ElemType,
    /// Looping: (interval msec, count); one-shot: (count base, count amplitude).
    pub spawn: [i32; 2],
    pub spawn_range: Range,
    pub fade_in: Range,
    pub fade_out: Range,
    pub cull_radius: f32,
    pub delay_msec: [i32; 2],
    pub life_msec: [i32; 2],
    pub origin: [Range; 3],
    pub offset_radius: Range,
    pub offset_height: Range,
    pub angles: [Range; 3],
    pub angular_velocity: [Range; 3],
    pub initial_rotation: Range,
    pub gravity: Range,
    pub reflection: Range,
    pub atlas: Atlas,
    pub vel: Vec<VelSample>,
    pub vis: Vec<VisSample>,
    pub visuals: Vec<Visual>,
    pub coll_bounds: [f32; 6],
    pub on_impact: Option<String>,
    pub on_death: Option<String>,
    pub emitted: Option<String>,
    pub emit_dist: Range,
    pub emit_dist_variance: Range,
    pub trail: Option<Trail>,
    pub fountain: Option<Fountain>,
    pub sort_order: u8,
    pub lighting_frac: u8,
}

impl Elem {
    pub fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }
}

/// `FxEffectDef`: the elements in three runs, looping, one-shot, then emission.
#[derive(Clone, Debug)]
pub struct Effect {
    pub name: String,
    pub flags: u32,
    pub total_size: i32,
    pub looping_life_msec: i32,
    pub looping: usize,
    pub one_shot: usize,
    pub emission: usize,
    pub elems: Vec<Elem>,
    /// The zone it was read from.
    pub zone: String,
}

impl Effect {
    pub fn looping_elems(&self) -> &[Elem] {
        &self.elems[..self.looping.min(self.elems.len())]
    }
    pub fn one_shot_elems(&self) -> &[Elem] {
        let a = self.looping.min(self.elems.len());
        &self.elems[a..(a + self.one_shot).min(self.elems.len())]
    }
    pub fn emission_elems(&self) -> &[Elem] {
        let a = (self.looping + self.one_shot).min(self.elems.len());
        &self.elems[a..]
    }
    /// A rough radius the effect reaches: spawn offsets plus the distance its fastest elements travel plus their size.
    pub fn reach(&self) -> f32 {
        let mut r = 0.0f32;
        for e in &self.elems {
            let origin = e.origin.iter().map(|o| o.base.abs() + o.amp.abs()).fold(0.0, f32::max);
            let offset = e.offset_radius.base.abs() + e.offset_radius.amp.abs();
            let life = (e.life_msec[0] + e.life_msec[1].max(0)) as f32;
            let speed = e.vel.iter().map(|v| {
                let m = |r: &Vec3Range| (0..3).map(|i| (r.base[i].abs() + r.amp[i].abs()).powi(2)).sum::<f32>().sqrt();
                m(&v.local_velocity) + m(&v.world_velocity)
            }).fold(0.0, f32::max);
            let size = e.vis.iter().map(|v| v.base.size[0].abs() + v.amp.size[0].abs()).fold(0.0, f32::max);
            let travel = (speed * life * 0.25).min(500.0);
            r = r.max(origin + offset + travel + size);
        }
        r.clamp(16.0, 2000.0)
    }

    /// Every effect name this one can start (emitted, on impact, on death, runners).
    pub fn references(&self) -> Vec<String> {
        let mut out = Vec::new();
        for e in &self.elems {
            out.extend([&e.on_impact, &e.on_death, &e.emitted].into_iter().flatten().cloned());
            if e.elem_type == ElemType::Runner {
                out.extend(e.visuals.iter().filter_map(|v| if let Visual::Name(n) = v { Some(n.clone()) } else { None }));
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

fn f32_at(d: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn i32_at(d: &[u8], o: usize) -> i32 {
    i32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}
fn range(d: &[u8], o: usize) -> Range {
    Range { base: f32_at(d, o), amp: f32_at(d, o + 4) }
}
fn vec3_range(d: &[u8], o: usize) -> Vec3Range {
    Vec3Range { base: std::array::from_fn(|i| f32_at(d, o + 4 * i)), amp: std::array::from_fn(|i| f32_at(d, o + 12 + 4 * i)) }
}
fn vis_state(d: &[u8], o: usize) -> VisState {
    VisState {
        color: [d[o], d[o + 1], d[o + 2], d[o + 3]],
        rotation_delta: f32_at(d, o + 4),
        rotation_total: f32_at(d, o + 8),
        size: [f32_at(d, o + 12), f32_at(d, o + 16)],
        scale: f32_at(d, o + 20),
    }
}

/// CoDOL element flags to the IW4 numbering: CoDOL inserted a flag at 0x100, so its bits 9 to 16 are IW4's 8 to 15.
pub fn iw4_flags(codol: u32) -> u32 {
    (codol & 0xFFFF_00FF) | ((codol >> 1) & 0xFF00)
}

/// A model an effect element places, with the images and blend state of each surface.
pub struct LoadedModel {
    pub model: crate::model::Model,
    pub textures: Vec<SurfaceTextures>,
    pub states: Vec<Option<MaterialState>>,
}

/// The models named in `wanted` whose first LOD's geometry the zone itself holds (effect debris usually does; world models stream).
fn zone_models(zone: &Zone, rec: &Recorder, mats: &MaterialTable, wanted: &std::collections::HashSet<String>) -> HashMap<String, Arc<LoadedModel>> {
    let d = &zone.data;
    let mut out = HashMap::new();
    for m in &rec.models {
        let Some(name) = m.name.as_ref().map(|n| n.trim_start_matches(',').to_ascii_lowercase()) else { continue };
        if !wanted.contains(&name) || out.contains_key(&name) {
            continue;
        }
        let surfs = if m.lod_start[0] >= 0 { Some(m.lod_start[0] as usize) } else { rec.aliases.get(&(3, m.lod_ref[0])).copied() };
        let Some(meshes) = surfs.and_then(|s| rec.surf_meshes.get(&s)) else { continue };
        let mut surfaces = Vec::new();
        for sm in meshes {
            let (Some((v, nv)), Some((t, nt))) = (sm.vertices, sm.triangles) else { continue };
            if let Some(s) = crate::model::surface_from_zone(&d[sm.header..sm.header + 84], &d[v..v + 32 * nv], &d[t..t + 6 * nt]) {
                surfaces.push(s);
            }
        }
        if surfaces.is_empty() || surfaces.len() != meshes.len() {
            continue;
        }
        // the model's material slots, in surface order
        let material = |k: usize| -> (SurfaceTextures, Option<MaterialState>) {
            let start = m.mat_start.get(k).and_then(|&s| usize::try_from(s).ok()).or_else(|| m.mat_ref.get(k).and_then(|r| rec.aliases.get(&(5, *r))).copied());
            let name = start.and_then(|s| rec.material_by_start.get(&s)).and_then(|&i| rec.materials[i].name.clone());
            name.and_then(|n| mats.get(&n).or_else(|| mats.get(n.trim_start_matches(','))).cloned()).unwrap_or_default()
        };
        let (textures, states) = (0..surfaces.len()).map(material).unzip();
        out.insert(name.clone(), Arc::new(LoadedModel { model: crate::model::Model { name, surfaces }, textures, states }));
    }
    out
}

/// A material's colour / normal / specular images and its blend state, by material name.
pub type MaterialTable = HashMap<String, (SurfaceTextures, Option<MaterialState>)>;

pub fn material_table(rec: &Recorder) -> MaterialTable {
    let textures = crate::zone::world::material_textures(rec);
    let mut out = MaterialTable::new();
    for m in &rec.materials {
        let Some(name) = &m.name else { continue };
        let state = MaterialState::decode(&m.header, &m.state_bits);
        out.entry(name.clone()).or_insert_with(|| (textures.get(name).cloned().unwrap_or_default(), state));
    }
    out
}

fn elem_of(d: &[u8], r: &FxElemRec, vis_stride: usize, codol: bool, mats: &MaterialTable) -> Elem {
    let e = r.at;
    let raw_flags = u32::from_le_bytes(d[e..e + 4].try_into().unwrap());
    let vel_count = d[e + 178] as usize + 1;
    let vis_count = d[e + 179] as usize + 1;
    let vel = r.vel.map_or(Vec::new(), |at| {
        (0..vel_count)
            .map(|i| {
                let o = at + 96 * i;
                VelSample { local_velocity: vec3_range(d, o), local_delta: vec3_range(d, o + 24), world_velocity: vec3_range(d, o + 48), world_delta: vec3_range(d, o + 72) }
            })
            .collect()
    });
    let half = vis_stride / 2;
    let vis = r.vis.map_or(Vec::new(), |at| (0..vis_count).map(|i| VisSample { base: vis_state(d, at + vis_stride * i), amp: vis_state(d, at + vis_stride * i + half) }).collect());
    let visuals = r
        .visuals
        .iter()
        .map(|v| match v {
            FxVisualRec::Material(Some(name), _) => {
                let (textures, state) = mats.get(name).or_else(|| mats.get(name.trim_start_matches(','))).cloned().unwrap_or_default();
                Visual::Material { name: name.trim_start_matches(',').to_string(), textures, state }
            }
            FxVisualRec::Model(Some(name)) => Visual::Model(name.trim_start_matches(',').to_string()),
            FxVisualRec::Name(Some(name)) => Visual::Name(name.clone()),
            _ => Visual::None,
        })
        .collect();
    let trail = (r.elem_type == 5).then_some(r.extended).flatten().map(|x| Trail {
        scroll_time_msec: i32_at(d, x),
        repeat_dist: i32_at(d, x + 4),
        inv_split_dist: f32_at(d, x + 8),
        inv_split_arc_dist: f32_at(d, x + 12),
        inv_split_time: f32_at(d, x + 16),
        verts: r.trail_verts.map_or(Vec::new(), |(at, n)| (0..n).map(|i| [f32_at(d, at + 20 * i), f32_at(d, at + 20 * i + 4), f32_at(d, at + 20 * i + 16)]).collect()),
        indices: r.trail_inds.map_or(Vec::new(), |(at, n)| (0..n).map(|i| u16::from_le_bytes([d[at + 2 * i], d[at + 2 * i + 1]])).collect()),
    });
    let fountain = (r.elem_type == 8).then_some(r.extended).flatten().map(|x| Fountain {
        gravity: f32_at(d, x),
        bounce_frac: f32_at(d, x + 4),
        bounce_rand: f32_at(d, x + 8),
        spark_spacing: f32_at(d, x + 12),
        spark_length: f32_at(d, x + 16),
        spark_count: i32_at(d, x + 20),
        loop_time: f32_at(d, x + 24),
        vel_min: f32_at(d, x + 28),
        vel_max: f32_at(d, x + 32),
        vel_cone_frac: f32_at(d, x + 36),
        rest_speed: f32_at(d, x + 40),
        boost_time: f32_at(d, x + 44),
        boost_factor: f32_at(d, x + 48),
    });
    let r3 = |o: usize| [range(d, o), range(d, o + 8), range(d, o + 16)];
    Elem {
        flags: if codol { iw4_flags(raw_flags) } else { raw_flags },
        elem_type: ElemType::from_codol(r.elem_type),
        spawn: [i32_at(d, e + 4), i32_at(d, e + 8)],
        spawn_range: range(d, e + 12),
        fade_in: range(d, e + 20),
        fade_out: range(d, e + 28),
        cull_radius: f32_at(d, e + 36),
        delay_msec: [i32_at(d, e + 40), i32_at(d, e + 44)],
        life_msec: [i32_at(d, e + 48), i32_at(d, e + 52)],
        origin: r3(e + 56),
        offset_radius: range(d, e + 80),
        offset_height: range(d, e + 88),
        angles: r3(e + 96),
        angular_velocity: r3(e + 120),
        initial_rotation: range(d, e + 144),
        gravity: range(d, e + 152),
        reflection: range(d, e + 160),
        atlas: Atlas {
            behavior: d[e + 168],
            index: d[e + 169],
            fps: d[e + 170],
            loop_count: d[e + 171],
            col_bits: d[e + 172],
            row_bits: d[e + 173],
            entry_count: i16::from_le_bytes([d[e + 174], d[e + 175]]),
        },
        vel,
        vis,
        visuals,
        coll_bounds: std::array::from_fn(|i| f32_at(d, e + 192 + 4 * i)),
        on_impact: r.on_impact.clone(),
        on_death: r.on_death.clone(),
        emitted: r.emitted.clone(),
        emit_dist: range(d, e + 228),
        emit_dist_variance: range(d, e + 236),
        trail,
        fountain,
        sort_order: d[e + 248],
        lighting_frac: d[e + 249],
    }
}

pub fn effect_of(d: &[u8], r: &FxRec, codol: bool, mats: &MaterialTable, zone: &str) -> Option<Effect> {
    let name = r.name.clone()?.trim_start_matches(',').to_string();
    let h = r.header;
    Some(Effect {
        name,
        flags: i32_at(d, h + 4) as u32,
        total_size: i32_at(d, h + 8),
        looping_life_msec: i32_at(d, h + 12),
        looping: i32_at(d, h + 16).max(0) as usize,
        one_shot: i32_at(d, h + 20).max(0) as usize,
        emission: i32_at(d, h + 24).max(0) as usize,
        elems: r.elems.iter().map(|e| elem_of(d, e, r.vis_stride, codol, mats)).collect(),
        zone: zone.to_string(),
    })
}

/// The effects of one CoDOL zone (1.1.1.13, QA 3.3.3.3, 3.19) and the materials it loads.
pub fn read_zone(path: &Path, key: &fastfile::ArchiveKey) -> Result<(Vec<Effect>, MaterialTable, HashMap<String, Arc<LoadedModel>>)> {
    let peek = fastfile::peek(path)?;
    if !matches!(peek.version, fastfile::VERSION_C1 | fastfile::VERSION_QA | fastfile::VERSION_319) {
        bail!("{}: effects are read from Call of Duty Online zones only", path.display());
    }
    let zone = Zone::open(path, key)?;
    let mut r = Replay::new(&zone).map_err(|f| anyhow::anyhow!("the walker stops in the zone prefix: {} at {}", f.what, f.at))?;
    let mut failed = 0;
    for i in 0..zone.assets.len() {
        if r.replay_asset(&zone, i).is_err() {
            failed += 1;
        }
    }
    if failed > 0 {
        crate::log::log(format!("effects: {failed} assets of {} did not read", path.display()));
    }
    let mats = material_table(&r.rec);
    let zone_name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let effects: Vec<Effect> = r.rec.effects.iter().filter_map(|f| effect_of(&zone.data, f, true, &mats, &zone_name)).collect();
    let wanted = effects
        .iter()
        .flat_map(|e| e.elems.iter().filter(|el| el.elem_type == ElemType::Model).flat_map(|el| el.visuals.iter()))
        .filter_map(|v| if let Visual::Model(m) = v { Some(m.to_ascii_lowercase()) } else { None })
        .collect();
    let models = zone_models(&zone, &r.rec, &mats, &wanted);
    Ok((effects, mats, models))
}

/// Effects by name (lower case) across the zones read so far, and every material those zones load.
#[derive(Default, Clone)]
pub struct Library {
    pub effects: HashMap<String, Arc<Effect>>,
    pub materials: MaterialTable,
    /// Models effects place whose geometry the zones hold, by name (lower case).
    pub models: HashMap<String, Arc<LoadedModel>>,
    pub zones: Vec<String>,
}

impl Library {
    /// Adds a zone's effects; an effect already present keeps its first definition unless that one is empty.
    pub fn add(&mut self, zone: &str, effects: Vec<Effect>, materials: MaterialTable, models: HashMap<String, Arc<LoadedModel>>) {
        for (k, v) in models {
            self.models.entry(k).or_insert(v);
        }
        // a zone may hold an empty stand-in for an effect another zone defines: a definition with elements wins
        for e in effects {
            let key = e.name.to_ascii_lowercase();
            match self.effects.get(&key) {
                Some(old) if !old.elems.is_empty() || e.elems.is_empty() => {}
                _ => {
                    self.effects.insert(key, Arc::new(e));
                }
            }
        }
        for (k, v) in materials {
            self.materials.entry(k).or_insert(v);
        }
        self.zones.push(zone.to_string());
    }

    pub fn get(&self, name: &str) -> Option<&Arc<Effect>> {
        self.effects.get(&name.trim_start_matches(',').to_ascii_lowercase())
    }

    /// Fills in the textures and blend state of materials an effect only names (loaded by another zone of the library).
    pub fn resolve_materials(&mut self) {
        let mats = &self.materials;
        for e in self.effects.values_mut() {
            if !e.elems.iter().flat_map(|el| &el.visuals).any(|v| matches!(v, Visual::Material { textures, .. } if textures.color.is_none())) {
                continue;
            }
            let e = Arc::make_mut(e);
            for v in e.elems.iter_mut().flat_map(|el| el.visuals.iter_mut()) {
                if let Visual::Material { name, textures, state } = v {
                    if textures.color.is_none() {
                        if let Some((t, s)) = mats.get(name.as_str()) {
                            *textures = t.clone();
                            *state = *s;
                        }
                    }
                }
            }
        }
    }

    /// The effect `name` and every effect it can start, directly or through others.
    pub fn family(&self, name: &str) -> Vec<Arc<Effect>> {
        let mut names = vec![name.to_ascii_lowercase()];
        let mut out = Vec::new();
        let mut i = 0;
        while i < names.len() && i < 256 {
            if let Some(e) = self.get(&names[i]) {
                for r in e.references() {
                    let r = r.trim_start_matches(',').to_ascii_lowercase();
                    if !names.contains(&r) {
                        names.push(r);
                    }
                }
                out.push(e.clone());
            }
            i += 1;
        }
        out
    }

    /// Every image the materials of `name` and the effects it starts draw (lower case).
    pub fn images_for(&self, name: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .family(name)
            .iter()
            .flat_map(|e| e.elems.iter().flat_map(|el| el.visuals.iter()))
            .filter_map(|v| if let Visual::Material { textures: SurfaceTextures { color: Some(c), .. }, .. } = v { Some(c.to_ascii_lowercase()) } else { None })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every model the elements of `name` and the effects it starts place (lower case).
    pub fn models_for(&self, name: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .family(name)
            .iter()
            .flat_map(|e| e.elems.iter().filter(|el| el.elem_type == ElemType::Model).flat_map(|el| el.visuals.iter()))
            .filter_map(|v| if let Visual::Model(m) = v { Some(m.to_ascii_lowercase()) } else { None })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    pub fn read_zone(&mut self, path: &Path, key: &fastfile::ArchiveKey) -> Result<usize> {
        let (effects, mats, models) = read_zone(path, key).with_context(|| path.display().to_string())?;
        let n = effects.len();
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.add(&name, effects, mats, models);
        self.resolve_materials();
        Ok(n)
    }
}
