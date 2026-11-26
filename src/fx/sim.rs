//! Playing an effect: spawning, moving and drawing its elements.
//!
//! The rules follow the stored data (checked against the effects of CoDOL 3.19 zones):
//!
//! * Velocity samples are in units per millisecond, sampled evenly over an element's life; the `*_delta` fields are their running
//!   integral (in units per sample interval) and are not needed here: the position is the integral of the piecewise linear
//!   velocity, computed exactly. Local velocity runs along the element's axis, world velocity along the world's.
//! * Visual-state samples are spread evenly over the life too. Colours are stored B, G, R, A; the second ("amplitude") colour is
//!   the other end of the random range, not an offset. Sizes, scales and rotations are base plus amplitude times a random number.
//!   `rotation_total` is the integral of `rotation_delta` (radians per millisecond) per interval.
//! * A looping element spawns one element every `spawn[0]` msec, `spawn[1]` times (the effect's looping life is
//!   `interval * (count - 1)`); a one-shot element spawns `spawn[0] + rand * spawn[1]` elements once.
//! * Gravity is a fraction of the world's 800 units/s².
//!
//! What it leaves out: distance fades, lighting, the game's own random table (so no two runs match the game), and collision with
//! anything but an optional ground plane.

use super::def::{flags, Effect, Elem, ElemType, Library, VisState, Visual};
use crate::modelindex::{MaterialState, SurfaceTextures};
use glam::{Mat3, Vec3};
use std::sync::Arc;

const GRAVITY: f32 = 800.0 / 1_000_000.0; // units per msec²
const MAX_PARTICLES: usize = 6000;
const MAX_INSTANCES: usize = 400;

/// A small deterministic generator (xorshift32), seeded per run so a replay looks the same.
#[derive(Clone)]
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// The world axis of an effect: rows forward, left, up (the game's convention).
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub origin: Vec3,
    pub axis: Mat3,
}

impl Frame {
    /// An effect pointing along `forward` (the way the game plays an effect on a surface: forward = the surface normal).
    pub fn pointing(origin: Vec3, forward: Vec3) -> Frame {
        let f = forward.normalize_or(Vec3::Z);
        let helper = if f.z.abs() < 0.99 { Vec3::Z } else { Vec3::X };
        let left = helper.cross(f).normalize();
        let up = f.cross(left);
        Frame { origin, axis: Mat3::from_cols(f, left, up) }
    }

    fn to_world(&self, local: Vec3) -> Vec3 {
        self.axis * local
    }
}

/// Radians (pitch, yaw, roll) to an axis whose columns are forward, left, up.
fn angles_axis(a: [f32; 3]) -> Mat3 {
    let (sp, cp) = a[0].sin_cos();
    let (sy, cy) = a[1].sin_cos();
    let (sr, cr) = a[2].sin_cos();
    let forward = Vec3::new(cp * cy, cp * sy, -sp);
    let right = Vec3::new(-sr * sp * cy + cr * sy, -sr * sp * sy - cr * cy, -sr * cp);
    let up = Vec3::new(cr * sp * cy + sr * sy, cr * sp * sy - sr * cy, cr * cp);
    Mat3::from_cols(forward, -right, up)
}

struct Instance {
    def: Arc<Effect>,
    frame: Frame,
    /// Per looping element: when the next one spawns and how many have.
    looping: Vec<(f64, i32)>,
    /// The particle a runner instance follows.
    follow: Option<u64>,
    stopped: bool,
}

#[derive(Clone)]
struct TrailPoint {
    pos: Vec3,
    born: f64,
}

#[derive(Clone, Copy)]
struct Free {
    pos: Vec3,
    vel: Vec3,
    rest_age: Option<f32>,
}

struct Particle {
    id: u64,
    def: Arc<Effect>,
    elem: usize,
    instance: usize,
    born: f64,
    life: f32,
    /// Spawn point and axis in the world (the effect's frame applied).
    origin: Vec3,
    axis: Mat3,
    /// Offset from the effect origin, for elements that run relative to the effect.
    rel: Vec3,
    rand: [f32; 12],
    gravity: f32,
    visual: usize,
    /// Distance travelled since the last emitted effect, and the distance to the next one.
    emit_travel: f32,
    emit_next: f32,
    last: Option<Vec3>,
    trail: Vec<TrailPoint>,
    /// After the first touch of the ground plane the element flies free: position and velocity (units per msec), and the age at
    /// which it came to rest (its spin stops there too).
    free: Option<Free>,
    runner_started: bool,
}

/// One element ready to draw.
#[derive(Clone, Debug)]
pub struct Sprite {
    pub pos: Vec3,
    pub color: [f32; 4],
    /// Half width and half height.
    pub size: [f32; 2],
    pub rotation: f32,
    /// Atlas cell as (u0, v0, u1, v1).
    pub uv: [f32; 4],
    pub kind: ElemType,
    /// Element axis (for oriented sprites, decals and models) and the velocity direction (tails, lines, sparks).
    pub axis: Mat3,
    pub dir: Vec3,
    pub scale: f32,
}

/// Quads of one material, back to front.
pub struct Batch {
    pub material: String,
    pub textures: SurfaceTextures,
    pub state: Option<MaterialState>,
    pub sort: u8,
    pub vertices: Vec<QuadVertex>,
}

#[derive(Clone, Copy, Debug)]
pub struct QuadVertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub color: [u8; 4],
}

/// A model an element places.
#[derive(Clone, Debug)]
pub struct ModelDraw {
    pub name: String,
    pub origin: Vec3,
    pub axis: Mat3,
    pub scale: f32,
    pub color: [f32; 4],
}

/// A light an element makes.
#[derive(Clone, Debug)]
pub struct LightDraw {
    pub pos: Vec3,
    pub color: [f32; 4],
    pub radius: f32,
}

#[derive(Default)]
pub struct DrawList {
    pub batches: Vec<Batch>,
    pub models: Vec<ModelDraw>,
    pub lights: Vec<LightDraw>,
}

/// Settings of a run.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Elements that collide stop (or die) on the plane z = ground.
    pub ground: Option<f32>,
    /// Start the effects other elements name (emitted, on impact, on death, runners).
    pub children: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options { ground: Some(0.0), children: true }
    }
}

/// A running set of effects.
pub struct Player {
    lib: Arc<Library>,
    instances: Vec<Instance>,
    particles: Vec<Particle>,
    rng: Rng,
    next_id: u64,
    /// Milliseconds since the start.
    pub time: f64,
    pub options: Options,
    /// Sounds and effects named but not found, in the order met (for the UI).
    pub events: Vec<String>,
    pub missing: Vec<String>,
}

impl Player {
    pub fn new(lib: Arc<Library>) -> Player {
        Player { lib, instances: Vec::new(), particles: Vec::new(), rng: Rng(0x2545_F491), next_id: 1, time: 0.0, options: Options::default(), events: Vec::new(), missing: Vec::new() }
    }

    pub fn library(&self) -> &Arc<Library> {
        &self.lib
    }

    /// Starts over: no effects running, the clock at 0, the same random numbers as the last start.
    pub fn reset(&mut self) {
        self.instances.clear();
        self.particles.clear();
        self.time = 0.0;
        self.rng = Rng(0x2545_F491);
        self.events.clear();
    }

    pub fn particle_count(&self) -> usize {
        self.particles.len()
    }

    /// The effect and element number of every live element (for diagnostics).
    pub fn alive(&self) -> Vec<(String, usize, f32)> {
        self.particles.iter().map(|p| (p.def.name.clone(), p.elem, (self.time - p.born) as f32)).collect()
    }

    /// True while anything is alive or still to spawn.
    pub fn active(&self) -> bool {
        !self.particles.is_empty() || self.instances.iter().any(|i| !i.stopped)
    }

    /// Stops the looping elements of every effect (what is alive plays out).
    pub fn stop_looping(&mut self) {
        for i in &mut self.instances {
            i.stopped = true;
        }
    }

    pub fn spawn(&mut self, def: Arc<Effect>, frame: Frame) {
        self.spawn_at(def, frame, None, false);
    }

    pub fn spawn_named(&mut self, name: &str, frame: Frame) -> bool {
        match self.lib.get(name).cloned() {
            Some(def) => {
                self.spawn(def, frame);
                true
            }
            None => {
                self.note_missing(name);
                false
            }
        }
    }

    fn note_missing(&mut self, name: &str) {
        if !self.missing.iter().any(|m| m == name) {
            self.missing.push(name.to_string());
        }
    }

    fn spawn_at(&mut self, def: Arc<Effect>, frame: Frame, follow: Option<u64>, emitted: bool) {
        if self.instances.len() >= MAX_INSTANCES {
            return;
        }
        let index = self.instances.len();
        let looping = def.looping_elems().iter().map(|e| (self.time + e.delay_msec[0].max(0) as f64, 0)).collect();
        self.instances.push(Instance { def: def.clone(), frame, looping, follow, stopped: false });
        // an effect started by a moving element (`effectEmitted`) spawns its emission elements, once, like one-shots
        let (first_one_shot, count) = if emitted && def.emission > 0 { (def.looping + def.one_shot, def.emission_elems().len()) } else { (def.looping, def.one_shot_elems().len()) };
        for i in 0..count {
            let e = &def.elems[first_one_shot + i];
            if e.has(flags::DISABLED) {
                continue;
            }
            let count = e.spawn[0] as f32 + self.rng.next() * e.spawn[1].max(0) as f32;
            for _ in 0..count.round().max(0.0) as i32 {
                let delay = e.delay_msec[0] as f32 + self.rng.next() * e.delay_msec[1] as f32;
                self.spawn_elem(index, first_one_shot + i, self.time + delay.max(0.0) as f64);
            }
        }
    }

    fn spawn_elem(&mut self, instance: usize, elem: usize, at: f64) {
        if self.particles.len() >= MAX_PARTICLES {
            return;
        }
        let inst = &self.instances[instance];
        let def = inst.def.clone();
        let e = &def.elems[elem];
        let frame = inst.frame;
        let mut rand = [0f32; 12];
        for r in &mut rand {
            *r = self.rng.next();
        }
        let life = (e.life_msec[0] as f32 + rand[0] * e.life_msec[1] as f32).max(1.0);
        // spawn origin: the fixed offset, then the sphere or cylinder offset
        let mut local = Vec3::new(e.origin[0].at(rand[1]), e.origin[1].at(rand[2]), e.origin[2].at(rand[3]));
        let mut offset_dir = Vec3::ZERO;
        match e.flags & flags::SPAWN_OFFSET_MASK {
            flags::SPAWN_OFFSET_SPHERE => {
                let d = random_unit(&mut self.rng);
                let r = e.offset_radius.at(self.rng.next());
                offset_dir = d;
                local += d * r;
            }
            flags::SPAWN_OFFSET_CYLINDER => {
                let a = self.rng.next() * std::f32::consts::TAU;
                let d = Vec3::new(0.0, a.cos(), a.sin());
                let r = e.offset_radius.at(self.rng.next());
                offset_dir = d;
                local += d * r + Vec3::X * e.offset_height.at(self.rng.next());
            }
            _ => {}
        }
        let angles = [e.angles[0].at(rand[4]), e.angles[1].at(rand[5]), e.angles[2].at(rand[6])];
        let mut axis = frame.axis * angles_axis(angles);
        // velocity "relative to offset": the element's forward axis is the direction it was pushed out in
        if e.flags & flags::RUN_MASK == flags::RUN_RELATIVE_TO_OFFSET && offset_dir != Vec3::ZERO {
            let f = frame.to_world(offset_dir).normalize();
            axis = Frame::pointing(Vec3::ZERO, f).axis;
        }
        let rel = frame.to_world(local);
        let visual = if e.visuals.len() > 1 { (self.rng.next() * e.visuals.len() as f32) as usize % e.visuals.len() } else { 0 };
        let emit_next = next_emit_dist(e, &mut self.rng);
        let id = self.next_id;
        self.next_id += 1;
        self.particles.push(Particle {
            id,
            def: def.clone(),
            elem,
            instance,
            born: at,
            life,
            origin: frame.origin + rel,
            axis,
            rel,
            rand,
            gravity: if e.has(flags::HAS_GRAVITY) { e.gravity.at(rand[7]) } else { 0.0 },
            visual,
            emit_travel: 0.0,
            emit_next,
            last: None,
            trail: Vec::new(),
            free: None,
            runner_started: false,
        });
    }

    /// Where a particle is `age` msec after it spawned (before any landing).
    fn position(&self, p: &Particle, age: f32) -> Vec3 {
        let e = &p.def.elems[p.elem];
        let mut pos = p.origin;
        if e.flags & flags::RUN_MASK == flags::RUN_RELATIVE_TO_EFFECT {
            if let Some(i) = self.instances.get(p.instance) {
                pos = i.frame.origin + p.rel;
            }
        }
        let (local, world) = integrate_velocity(e, &p.rand, age, p.life);
        pos += p.axis * local + world;
        pos.z -= 0.5 * p.gravity * GRAVITY * age * age;
        pos
    }

    /// Advances the clock by `dt` milliseconds.
    pub fn update(&mut self, dt: f64) {
        let dt = dt.clamp(0.0, 250.0);
        let now = self.time + dt;
        // looping elements
        for i in 0..self.instances.len() {
            if self.instances[i].stopped {
                continue;
            }
            // a runner's effect follows its particle
            if let Some(id) = self.instances[i].follow {
                match self.particles.iter().find(|p| p.id == id) {
                    Some(p) => {
                        let age = (self.time - p.born) as f32;
                        let pos = self.position(p, age.clamp(0.0, p.life));
                        self.instances[i].frame.origin = pos;
                    }
                    None => self.instances[i].stopped = true,
                }
            }
            let def = self.instances[i].def.clone();
            let mut all_done = true;
            for (k, e) in def.looping_elems().iter().enumerate() {
                if e.has(flags::DISABLED) {
                    continue;
                }
                let interval = e.spawn[0].max(1) as f64;
                let count = e.spawn[1];
                loop {
                    let (next, done) = self.instances[i].looping[k];
                    if done >= count.max(1) {
                        break;
                    }
                    all_done = false;
                    if next > now {
                        break;
                    }
                    self.spawn_elem(i, k, next);
                    self.instances[i].looping[k] = (next + interval, done + 1);
                    if self.particles.len() >= MAX_PARTICLES {
                        break;
                    }
                }
            }
            if all_done {
                self.instances[i].stopped = true;
            }
        }
        self.time = now;
        // move, emit and retire particles
        let mut spawns: Vec<(String, Frame, Option<u64>, bool)> = Vec::new();
        let mut dead = Vec::new();
        for idx in 0..self.particles.len() {
            let p = &self.particles[idx];
            let age = (now - p.born) as f32;
            if age < 0.0 {
                continue;
            }
            let e = &p.def.elems[p.elem];
            let mut free = p.free;
            let mut pos = match free {
                Some(f) => f.pos,
                None => self.position(p, age.min(p.life)),
            };
            let collides = e.has(flags::USE_COLLISION) || e.has(flags::USE_MODEL_PHYSICS);
            if let (Some(g), true) = (self.options.ground, collides) {
                match &mut free {
                    // first touch: from here on a ballistic flight that bounces (the element's reflection factor) and slides to rest
                    None if pos.z < g && p.origin.z >= g - 0.01 => {
                        let before = self.position(p, (age - dt as f32).max(0.0).min(p.life));
                        let vel = (pos - before) / (dt as f32).max(1.0);
                        pos.z = g;
                        if self.options.children {
                            if let Some(n) = &e.on_impact {
                                spawns.push((n.clone(), Frame::pointing(pos, Vec3::Z), None, false));
                            }
                        }
                        if e.has(flags::DIE_ON_TOUCH) {
                            dead.push(idx);
                        }
                        let refl = e.reflection.at(p.rand[10]).clamp(0.0, 1.0);
                        free = Some(Free { pos, vel: Vec3::new(vel.x * 0.6, vel.y * 0.6, -vel.z * refl), rest_age: None });
                    }
                    Some(f) if f.rest_age.is_none() => {
                        let gravity = if p.gravity != 0.0 { p.gravity } else { 1.0 };
                        let refl = e.reflection.at(p.rand[10]).clamp(0.0, 1.0);
                        let steps = (dt / 8.0).ceil().max(1.0) as usize;
                        let h = dt as f32 / steps as f32;
                        for _ in 0..steps {
                            f.vel.z -= gravity * GRAVITY * h;
                            f.pos += f.vel * h;
                            if f.pos.z < g {
                                f.pos.z = g;
                                f.vel.z = -f.vel.z * refl;
                                f.vel.x *= 0.6;
                                f.vel.y *= 0.6;
                                // too slow to leave the ground again: it lies there
                                if f.vel.z < 0.05 && f.vel.length() < 0.03 {
                                    f.vel = Vec3::ZERO;
                                    f.rest_age = Some(age);
                                    break;
                                }
                            }
                        }
                        pos = f.pos;
                    }
                    _ => {}
                }
            }
            let runner = e.elem_type == ElemType::Runner && !p.runner_started;
            let runner_name = runner.then(|| e.visuals.get(p.visual).and_then(|v| if let Visual::Name(n) = v { Some(n.clone()) } else { None })).flatten();
            let trail = e.elem_type == ElemType::Trail;
            let split = e.trail.as_ref().map_or(0.0, |t| if t.inv_split_dist > 0.0 { 1.0 / t.inv_split_dist } else { 0.0 });
            let emitted = e.emitted.clone();
            let life = p.life;
            let id = p.id;
            let p = &mut self.particles[idx];
            p.free = free;
            if let Some(name) = runner_name {
                p.runner_started = true;
                if self.options.children {
                    spawns.push((name, Frame { origin: pos, axis: p.axis }, Some(id), false));
                }
            }
            if let Some(last) = p.last {
                let moved = (pos - last).length();
                if let Some(name) = &emitted {
                    p.emit_travel += moved;
                    if p.emit_next > 0.0 && p.emit_travel >= p.emit_next && self.options.children {
                        p.emit_travel = 0.0;
                        let dir = (pos - last).normalize_or(Vec3::Z);
                        spawns.push((name.clone(), Frame::pointing(pos, dir), None, true));
                    }
                }
            }
            if trail && (p.trail.last().is_none_or(|t| split <= 0.0 || (pos - t.pos).length() >= split)) {
                p.trail.push(TrailPoint { pos, born: now });
            }
            p.last = Some(pos);
            if age >= life {
                dead.push(idx);
            }
        }
        dead.sort_unstable();
        dead.dedup();
        for &idx in dead.iter().rev() {
            let p = self.particles.swap_remove(idx);
            let e = &p.def.elems[p.elem];
            if self.options.children {
                if let (Some(n), Some(last)) = (&e.on_death, p.last) {
                    spawns.push((n.clone(), Frame { origin: last, axis: p.axis }, None, false));
                }
            }
        }
        for (name, frame, follow, emitted) in spawns {
            match self.lib.get(&name).cloned() {
                Some(def) => self.spawn_at(def, frame, follow, emitted),
                None => self.note_missing(&name),
            }
        }
        // drop finished instances that nothing refers to
        if self.particles.is_empty() && self.instances.iter().all(|i| i.stopped) {
            self.instances.clear();
        }
        // sounds
        for p in &self.particles {
            let e = &p.def.elems[p.elem];
            if e.elem_type == ElemType::Sound && p.last.is_some() && (now - p.born) < dt {
                if let Some(v) = e.visuals.get(p.visual) {
                    self.events.push(format!("sound {}", v.label()));
                }
            }
        }
    }

    /// The elements to draw, seen from `eye` with the camera's right and up vectors.
    pub fn draw(&self, eye: Vec3, right: Vec3, up: Vec3) -> DrawList {
        let mut out = DrawList::default();
        let mut sprites: Vec<(usize, Sprite, f32)> = Vec::new();
        // (material key) -> batch index
        let mut by_key: std::collections::HashMap<(String, u8), usize> = std::collections::HashMap::new();
        for p in &self.particles {
            let age = (self.time - p.born) as f32;
            if age < 0.0 {
                continue;
            }
            let e = &p.def.elems[p.elem];
            let t = (age / p.life).clamp(0.0, 1.0);
            let vis = visual_state(e, &p.rand, t, p.life);
            let pos = p.free.map_or_else(|| self.position(p, age.min(p.life)), |f| f.pos);
            let color = [vis.color[2] as f32 / 255.0, vis.color[1] as f32 / 255.0, vis.color[0] as f32 / 255.0, vis.color[3] as f32 / 255.0];
            let rotation = e.initial_rotation.at(p.rand[8]) + vis.rotation_total;
            let dir = if let Some(f) = p.free {
                f.vel.normalize_or(p.axis.col(0))
            } else {
                let ahead = self.position(p, (age + 8.0).min(p.life.max(age + 8.0)));
                (ahead - pos).normalize_or(p.axis.col(0))
            };
            // angular velocity turns the element's axis (radians per msec)
            let spin_age = p.free.and_then(|f| f.rest_age).unwrap_or(age);
            let spin = Vec3::new(e.angular_velocity[0].at(p.rand[9]), e.angular_velocity[1].at(p.rand[10]), e.angular_velocity[2].at(p.rand[11])) * spin_age;
            let axis = p.axis * angles_axis(spin.to_array());
            match e.elem_type {
                ElemType::Model => {
                    if let Some(Visual::Model(name)) = e.visuals.get(p.visual) {
                        out.models.push(ModelDraw { name: name.clone(), origin: pos, axis, scale: if vis.scale > 0.0 { vis.scale } else { 1.0 }, color });
                    }
                }
                ElemType::OmniLight | ElemType::SpotLight => {
                    out.lights.push(LightDraw { pos, color, radius: vis.size[0].max(1.0) });
                }
                ElemType::Sound | ElemType::Runner => {}
                ElemType::Trail => {
                    if let Some(Visual::Material { name, textures, state }) = e.visuals.get(p.visual) {
                        let key = (name.clone(), e.sort_order);
                        let b = *by_key.entry(key).or_insert_with(|| {
                            out.batches.push(Batch { material: name.clone(), textures: textures.clone(), state: *state, sort: e.sort_order, vertices: Vec::new() });
                            out.batches.len() - 1
                        });
                        let repeat = e.trail.as_ref().map_or(0.0, |t| t.repeat_dist as f32).max(1.0);
                        let mut points: Vec<(Vec3, f32)> = p.trail.iter().map(|tp| (tp.pos, ((self.time - tp.born) as f32 / p.life).clamp(0.0, 1.0))).collect();
                        points.push((pos, 0.0));
                        ribbon(&mut out.batches[b].vertices, &points, eye, repeat, |a| {
                            let v = visual_state(e, &p.rand, a, p.life);
                            ([v.color[2], v.color[1], v.color[0], v.color[3]], v.size[0])
                        });
                    }
                }
                _ => {
                    let Some(Visual::Material { name, .. }) = e.visuals.get(p.visual) else { continue };
                    let half = if e.has(flags::NONUNIFORM_SCALE) { [vis.size[0], vis.size[1]] } else { [vis.size[0], vis.size[0]] };
                    let uv = atlas_cell(e, &p.rand, age, t);
                    let key = (name.clone(), e.sort_order);
                    let b = *by_key.entry(key).or_insert_with(|| {
                        let (textures, state) = match &e.visuals[p.visual] {
                            Visual::Material { textures, state, .. } => (textures.clone(), *state),
                            _ => unreachable!(),
                        };
                        out.batches.push(Batch { material: name.clone(), textures, state, sort: e.sort_order, vertices: Vec::new() });
                        out.batches.len() - 1
                    });
                    if let (ElemType::SparkFountain, Some(f)) = (e.elem_type, &e.fountain) {
                        for (spark_pos, spark_dir, spark_t) in fountain_sparks(f, p.id, pos, axis, age, self.options.ground) {
                            let v = visual_state(e, &p.rand, spark_t, p.life);
                            let c = [v.color[2] as f32 / 255.0, v.color[1] as f32 / 255.0, v.color[0] as f32 / 255.0, v.color[3] as f32 / 255.0];
                            let size = [v.size[0].max(0.5), f.spark_length * 0.5];
                            let depth = (spark_pos - eye).length_squared();
                            sprites.push((b, Sprite { pos: spark_pos, color: c, size, rotation: 0.0, uv, kind: ElemType::Line, axis, dir: spark_dir, scale: 1.0 }, depth));
                        }
                        continue;
                    }
                    let depth = (pos - eye).length_squared();
                    sprites.push((b, Sprite { pos, color, size: half, rotation, uv, kind: e.elem_type, axis, dir, scale: vis.scale }, depth));
                }
            }
        }
        // back to front
        sprites.sort_by(|a, b| b.2.total_cmp(&a.2));
        for (b, s, _) in sprites {
            quad(&mut out.batches[b].vertices, &s, eye, right, up);
        }
        out.batches.retain(|b| !b.vertices.is_empty());
        out.batches.sort_by_key(|b| b.sort);
        out
    }
}

/// The sparks of a spark fountain `age` msec after it started: (position, direction of travel, age as a fraction of the loop).
///
/// From `FxSparkFountainDef` (msec and units): `spark_count` sparks leave every `loop_time`, evenly spread over it (the first
/// `boost_time` msec `boost_factor` times as fast), with speeds between `vel_min` and `vel_max` inside a cone around the element's
/// forward axis (`vel_cone_frac` of a half turn), under `gravity` (units per msec², negative is down), bouncing off the ground with
/// `bounce_frac` plus up to `bounce_rand`.
fn fountain_sparks(f: &super::def::Fountain, seed: u64, origin: Vec3, axis: Mat3, age: f32, ground: Option<f32>) -> Vec<(Vec3, Vec3, f32)> {
    let count = f.spark_count.clamp(0, 256) as usize;
    let loop_time = f.loop_time.max(1.0);
    let mut out = Vec::with_capacity(count);
    let forward = axis.col(0);
    let (side_a, side_b) = (axis.col(1), axis.col(2));
    for i in 0..count {
        let mut rng = Rng((seed as u32).wrapping_mul(2_654_435_761).wrapping_add(i as u32 * 40_503 + 1) | 1);
        for _ in 0..3 {
            rng.next();
        }
        let launch = i as f32 / count as f32 * loop_time;
        if age < launch {
            continue;
        }
        let a = (age - launch) % loop_time;
        let angle = rng.next() * f.vel_cone_frac.clamp(0.0, 1.0) * std::f32::consts::PI;
        let around = rng.next() * std::f32::consts::TAU;
        let d = forward * angle.cos() + (side_a * around.cos() + side_b * around.sin()) * angle.sin();
        let mut speed = f.vel_min + (f.vel_max - f.vel_min) * rng.next();
        if launch < f.boost_time && f.boost_factor > 0.0 {
            speed *= f.boost_factor;
        }
        let v0 = d * speed;
        let mut pos = origin + v0 * a + Vec3::Z * (0.5 * f.gravity * a * a);
        let mut vel = v0 + Vec3::Z * (f.gravity * a);
        if let Some(g) = ground {
            if pos.z < g && origin.z >= g - 0.01 {
                let bounce = (f.bounce_frac + f.bounce_rand * rng.next()).clamp(0.0, 1.0);
                pos.z = g + (g - pos.z) * bounce;
                vel.z = -vel.z * bounce;
            }
        }
        if vel.length() < f.rest_speed * 0.1 {
            continue;
        }
        out.push((pos, vel.normalize_or(forward), a / loop_time));
    }
    out
}

fn next_emit_dist(e: &Elem, rng: &mut Rng) -> f32 {
    if e.emitted.is_none() {
        return 0.0;
    }
    (e.emit_dist.at(rng.next()) + e.emit_dist_variance.at(rng.next()) * (rng.next() * 2.0 - 1.0)).max(1.0)
}

fn random_unit(rng: &mut Rng) -> Vec3 {
    loop {
        let v = Vec3::new(rng.next() * 2.0 - 1.0, rng.next() * 2.0 - 1.0, rng.next() * 2.0 - 1.0);
        let l = v.length_squared();
        if l > 1e-4 && l <= 1.0 {
            return v / l.sqrt();
        }
    }
}

/// The local and world displacement `age` msec into a life of `life` msec: the integral of the piecewise linear velocity samples.
fn integrate_velocity(e: &Elem, rand: &[f32; 12], age: f32, life: f32) -> (Vec3, Vec3) {
    let n = e.vel.len();
    if n < 2 {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    let intervals = (n - 1) as f32;
    let step = life / intervals;
    let s = (age / step).clamp(0.0, intervals);
    let r = Vec3::new(rand[1], rand[2], rand[3]);
    let vel = |i: usize, world: bool| {
        let v = if world { &e.vel[i].world_velocity } else { &e.vel[i].local_velocity };
        Vec3::from(v.base) + Vec3::from(v.amp) * r
    };
    let mut out = [Vec3::ZERO; 2];
    for (k, world) in [false, true].into_iter().enumerate() {
        let mut sum = Vec3::ZERO;
        let whole = s.floor() as usize;
        for i in 0..whole.min(n - 1) {
            sum += (vel(i, world) + vel(i + 1, world)) * 0.5 * step;
        }
        let f = s - whole as f32;
        if f > 0.0 && whole + 1 < n {
            let a = vel(whole, world);
            let b = vel(whole + 1, world);
            // the integral of a + (b - a) x over [0, f], times the interval length
            sum += (a * f + (b - a) * (0.5 * f * f)) * step;
        }
        out[k] = sum;
    }
    (out[0], out[1])
}

fn lerp_state(a: &VisState, b: &VisState, f: f32) -> VisState {
    let l = |x: f32, y: f32| x + (y - x) * f;
    VisState {
        color: std::array::from_fn(|i| l(a.color[i] as f32, b.color[i] as f32).round().clamp(0.0, 255.0) as u8),
        rotation_delta: l(a.rotation_delta, b.rotation_delta),
        rotation_total: l(a.rotation_total, b.rotation_total),
        size: [l(a.size[0], b.size[0]), l(a.size[1], b.size[1])],
        scale: l(a.scale, b.scale),
    }
}

/// The visual state at `t` (0..1) of the life: the samples interpolated, with this element's random values.
fn visual_state(e: &Elem, rand: &[f32; 12], t: f32, life: f32) -> VisState {
    let n = e.vis.len();
    if n == 0 {
        return VisState { color: [255; 4], size: [8.0; 2], scale: 1.0, ..Default::default() };
    }
    let resolve = |i: usize| {
        let s = &e.vis[i];
        let rc = rand[4];
        VisState {
            color: std::array::from_fn(|k| (s.base.color[k] as f32 + (s.amp.color[k] as f32 - s.base.color[k] as f32) * rc).round().clamp(0.0, 255.0) as u8),
            rotation_delta: s.base.rotation_delta + s.amp.rotation_delta * rand[5],
            rotation_total: s.base.rotation_total + s.amp.rotation_total * rand[5],
            size: [s.base.size[0] + s.amp.size[0] * rand[6], s.base.size[1] + s.amp.size[1] * rand[7]],
            scale: s.base.scale + s.amp.scale * rand[6],
        }
    };
    if n == 1 {
        let mut v = resolve(0);
        v.rotation_total = 0.0;
        return v;
    }
    let intervals = (n - 1) as f32;
    let s = (t * intervals).clamp(0.0, intervals);
    let i = (s.floor() as usize).min(n - 2);
    let mut v = lerp_state(&resolve(i), &resolve(i + 1), s - i as f32);
    // rotation_total is per sample interval: to radians over the elapsed time
    v.rotation_total *= life / intervals;
    v
}

/// The atlas cell (u0, v0, u1, v1) of an element `age` msec (`t` of its life) old.
fn atlas_cell(e: &Elem, rand: &[f32; 12], age: f32, t: f32) -> [f32; 4] {
    let a = &e.atlas;
    let cols = 1u32 << a.col_bits.min(8);
    let rows = 1u32 << a.row_bits.min(8);
    let entries = (a.entry_count.max(1) as u32).min(cols * rows);
    if entries <= 1 {
        return [0.0, 0.0, 1.0 / cols as f32, 1.0 / rows as f32];
    }
    let start = match a.behavior & 3 {
        1 => (rand[9] * entries as f32) as u32,
        _ => a.index as u32,
    };
    let step = if a.behavior & 4 != 0 {
        (t * entries as f32) as u32
    } else {
        let frames = (age / 1000.0 * a.fps as f32) as u32;
        if a.behavior & 8 != 0 && a.loop_count > 0 { frames.min(entries * a.loop_count as u32 - 1) } else { frames }
    };
    let cell = (start + step) % entries;
    let (c, r) = (cell % cols, cell / cols);
    let (w, h) = (1.0 / cols as f32, 1.0 / rows as f32);
    [c as f32 * w, r as f32 * h, (c + 1) as f32 * w, (r + 1) as f32 * h]
}

fn color8(c: [f32; 4]) -> [u8; 4] {
    c.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Two triangles for a sprite.
fn quad(out: &mut Vec<QuadVertex>, s: &Sprite, eye: Vec3, right: Vec3, up: Vec3) {
    let (sin, cos) = s.rotation.sin_cos();
    let (mut a, mut b) = match s.kind {
        // facing along the element's forward axis
        ElemType::Oriented | ElemType::Decal => (s.axis.col(1), s.axis.col(2)),
        // stretched along the direction of travel, turned to face the camera
        ElemType::Tail | ElemType::Line | ElemType::SparkCloud | ElemType::SparkFountain => {
            let view = (eye - s.pos).normalize_or(Vec3::Z);
            let side = s.dir.cross(view).normalize_or(right);
            let len = if s.kind == ElemType::Line { s.size[1] } else { s.size[1].max(s.size[0]) };
            let c = color8(s.color);
            let head = s.pos;
            let tail = s.pos - s.dir * len * 2.0;
            let w = s.size[0];
            let v = [head + side * w, head - side * w, tail - side * w, tail + side * w];
            let uv = [[s.uv[2], s.uv[1]], [s.uv[2], s.uv[3]], [s.uv[0], s.uv[3]], [s.uv[0], s.uv[1]]];
            for k in [0usize, 1, 2, 0, 2, 3] {
                out.push(QuadVertex { pos: v[k].to_array(), uv: uv[k], color: c });
            }
            return;
        }
        _ => (right, up),
    };
    // rotate in the sprite's plane
    let (ra, rb) = (a * cos + b * sin, b * cos - a * sin);
    a = ra * s.size[0];
    b = rb * s.size[1];
    let c = color8(s.color);
    let v = [s.pos - a + b, s.pos + a + b, s.pos + a - b, s.pos - a - b];
    let uv = [[s.uv[0], s.uv[1]], [s.uv[2], s.uv[1]], [s.uv[2], s.uv[3]], [s.uv[0], s.uv[3]]];
    for k in [0usize, 1, 2, 0, 2, 3] {
        out.push(QuadVertex { pos: v[k].to_array(), uv: uv[k], color: c });
    }
}

/// A camera-facing ribbon through `points` (position, age 0..1), `at(age)` giving colour and half width.
fn ribbon(out: &mut Vec<QuadVertex>, points: &[(Vec3, f32)], eye: Vec3, repeat: f32, at: impl Fn(f32) -> ([u8; 4], f32)) {
    if points.len() < 2 {
        return;
    }
    let mut dist = 0.0f32;
    let mut prev: Option<(Vec3, Vec3, f32, [u8; 4])> = None;
    for i in 0..points.len() {
        let (p, age) = points[i];
        let along = if i + 1 < points.len() { points[i + 1].0 - p } else { p - points[i - 1].0 };
        let side = along.cross(eye - p).normalize_or(Vec3::Z);
        let (c, w) = at(age);
        if i > 0 {
            dist += (p - points[i - 1].0).length();
        }
        let v = dist / repeat;
        if let Some((pp, ps, pv, pc)) = prev {
            let (pw_side, w_side) = (ps, side * w);
            let quad = [(pp + pw_side, [0.0, pv], pc), (pp - pw_side, [1.0, pv], pc), (p - w_side, [1.0, v], c), (p + w_side, [0.0, v], c)];
            for k in [0usize, 1, 2, 0, 2, 3] {
                out.push(QuadVertex { pos: quad[k].0.to_array(), uv: quad[k].1, color: quad[k].2 });
            }
        }
        prev = Some((p, side * w, v, c));
    }
}

