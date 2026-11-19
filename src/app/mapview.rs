//! The map view: the world geometry of a zone (CoDOL or stock Modern Warfare 2) in the 3D viewer, a surface per material.

use super::App;
use crate::log;
use crate::model::Model;
use crate::modelindex::SurfaceTextures;
use crate::modeltex::{self, TexImage};
use crate::viewer::{self, Camera, Cull, LightClass, LightMarker, LightView, Shading, Viewer};
use crate::zone::world::{LightKind, MapLights};
use crate::zone::{Zone, export as zexport, fastfile, stock, world};
use anyhow::{Context, Result};
use eframe::egui::{self, Color32, Pos2, Rect};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

type MapResult = Result<(Model, Vec<String>, Vec<SurfaceTextures>, MapLights), String>;

enum Tex {
    Loading,
    Done,
}

pub(super) struct MapView {
    pub(super) active: bool,
    title: String,
    viewer: Option<Viewer>,
    camera: Camera,
    model: Option<Arc<Model>>,
    names: Vec<String>,
    uploaded: bool,
    shading: Shading,
    cull: Cull,
    wire: bool,
    visible: Vec<bool>,
    /// The map's lights, as the viewer draws them (the sun's marker sits at the middle of the play area), what is drawn, and the
    /// light under the pointer or pinned by `ATLAS_VIEW=light=N`.
    lights: MapLights,
    markers: Vec<LightMarker>,
    show_lights: bool,
    light_mask: u8,
    light_pin: Option<usize>,
    light_hover: Option<usize>,
    marker_info: Vec<String>,
    /// `ATLAS_VIEW` options, applied once the map is there (for screenshots).
    pending_view: Option<String>,
    surface_tex: Vec<SurfaceTextures>,
    tex_state: std::collections::HashMap<String, Tex>,
    tex_rx: Option<Receiver<(String, Result<TexImage, String>)>>,
    tex_tx: Option<std::sync::mpsc::Sender<(String, Result<TexImage, String>)>>,
    pub(super) rx: Option<Receiver<MapResult>>,
}

impl MapView {
    /// Options as `ATLAS_VIEW` gives them (`key=value,...`), for screenshots: `lights=1`, `types=sun+omni+spot+hero` (the kinds drawn),
    /// `light=N` (pins the N-th marker: its reach is drawn), `wire=1`, `shading=normals|lit|...`, `yaw=` and `pitch=` in degrees, and
    /// `zoom=` (a factor on the camera distance).
    fn apply_options(&mut self, spec: &str) {
        for kv in spec.split(',') {
            let Some((k, v)) = kv.split_once('=') else { continue };
            match k {
                "lights" => self.show_lights = v != "0",
                "types" => self.light_mask = LightClass::ALL.iter().filter(|c| v.split('+').any(|t| t == c.name())).fold(0, |m, c| m | c.bit()),
                "light" => self.light_pin = v.parse().ok(),
                "wire" => self.wire = v == "1",
                "shading" => self.shading = Shading::ALL.iter().find(|(_, n)| n.to_ascii_lowercase().replace(' ', "") == v).map_or(self.shading, |x| x.0),
                "yaw" => self.camera.yaw = v.parse::<f32>().unwrap_or(0.0).to_radians(),
                "pitch" => self.camera.pitch = v.parse::<f32>().unwrap_or(0.0).to_radians(),
                "zoom" => self.camera.dist *= v.parse::<f32>().unwrap_or(1.0),
                _ => {}
            }
        }
    }

    /// Whether the map or its textures are still being read (the screenshot hook waits for this).
    pub(super) fn is_busy(&self) -> bool {
        self.rx.is_some() || self.tex_state.values().any(|s| matches!(s, Tex::Loading))
    }
}

impl Default for MapView {
    fn default() -> Self {
        MapView { active: false, title: String::new(), viewer: None, camera: Camera::default(), model: None, names: Vec::new(), uploaded: false, shading: Shading::Textured, cull: Cull::None, wire: false, visible: Vec::new(), lights: MapLights::default(), markers: Vec::new(), show_lights: false, light_mask: 0xF, light_pin: None, light_hover: None, marker_info: Vec::new(), pending_view: crate::config::env_var("VIEW").ok(), surface_tex: Vec::new(), tex_state: Default::default(), tex_rx: None, tex_tx: None, rx: None }
    }
}

/// Where the view should frame the map: the middle 70% of the visible vertices on every axis (the dense part is the play area). Maps carry far-off geometry (terrain
/// and props outside the play area), and fitting the whole of it puts the camera so far away that panning and zooming become
/// far too coarse for the part that is actually played.
fn framing_bounds(model: &Model, visible: &[bool]) -> Option<([f32; 3], [f32; 3])> {
    let total: usize = model.surfaces.iter().zip(visible).filter(|(_, v)| **v).map(|(s, _)| s.vertices.len()).sum();
    if total == 0 {
        return None;
    }
    let stride = (total / 200_000).max(1);
    let mut axes: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut n = 0usize;
    for (s, _) in model.surfaces.iter().zip(visible).filter(|(_, v)| **v) {
        for v in &s.vertices {
            if n % stride == 0 {
                for a in 0..3 {
                    axes[a].push(v.pos[a]);
                }
            }
            n += 1;
        }
    }
    let (mut lo, mut hi) = ([0f32; 3], [0f32; 3]);
    for a in 0..3 {
        axes[a].sort_by(|x, y| x.total_cmp(y));
        let len = axes[a].len();
        let (a0, a1) = (axes[a][len * 15 / 100], axes[a][(len * 85 / 100).min(len - 1)]);
        // a margin around the dense middle, so the play area fills the view with some room to move
        let pad = (a1 - a0) * 0.25;
        lo[a] = a0 - pad;
        hi[a] = a1 + pad;
    }
    Some((lo, hi))
}

/// The lights of the map as viewer markers: omni and spot lights (the Com world's primary ones and the hero ones), and the sun as an arrow
/// of length `sun_length` from `centre`. The other directional lights (some maps list the sun several times) and unused slots are left out.
fn markers_of(l: &MapLights, centre: [f32; 3], sun_length: f32) -> (Vec<LightMarker>, Vec<String>) {
    let (mut out, mut info) = (Vec::new(), Vec::new());
    let v3 = |v: [f32; 3], d: usize| format!("({:.d$}, {:.d$}, {:.d$})", v[0], v[1], v[2]);
    if let Some(s) = l.sun() {
        out.push(LightMarker { pos: centre, color: s.color, dir: s.direction, radius: sun_length, class: LightClass::Sun, half_angle: 0.0 });
        info.push(format!("sun: direction {}, colour {}", v3(s.direction, 3), v3(s.color, 2)));
    }
    for (n, x) in l.primary.iter().chain(l.hero.iter()).enumerate() {
        let class = match (x.kind, x.hero) {
            (LightKind::Omni, false) => LightClass::Omni,
            (LightKind::Spot, false) => LightClass::Spot,
            (LightKind::Omni | LightKind::Spot, true) => LightClass::Hero,
            _ => continue,
        };
        let half_angle = x.cos_outer.clamp(-1.0, 1.0).acos();
        let mut text = format!("{} light {}{}: radius {}, colour {}, origin {}", x.kind.name(), if x.hero { "hero " } else { "" }, if x.hero { n - l.primary.len() } else { n }, x.radius, v3(x.color, 2), v3(x.origin, 1));
        if x.kind == LightKind::Spot {
            text += &format!(", cone {:.0} degrees, direction {}", half_angle.to_degrees() * 2.0, v3(x.direction, 2));
        }
        if let Some(name) = &x.name {
            text += &format!(", {name}");
        }
        out.push(LightMarker { pos: x.origin, color: x.color, dir: x.direction, radius: x.radius, class, half_angle });
        info.push(text);
    }
    (out, info)
}

/// The point of a marker the pointer picks: its position, or the sun's end of the arrow.
fn anchor(m: &LightMarker) -> [f32; 3] {
    if m.class == LightClass::Sun {
        std::array::from_fn(|a| m.pos[a] + m.dir[a] * m.radius)
    } else {
        m.pos
    }
}

fn load_map(path: &std::path::Path, root: &std::path::Path) -> Result<(Model, Vec<String>, Vec<SurfaceTextures>, MapLights)> {
    // a Black Ops II zone (TAff0100): its world goes through the schema walker
    if std::fs::File::open(path).and_then(|mut f| { use std::io::Read; let mut m = [0u8; 8]; f.read_exact(&mut m)?; Ok(m) }).is_ok_and(|m| m.starts_with(b"TAff")) {
        let (model, report) = crate::zone::t6_world::map_of_t6_file(path, false)?;
        return model.with_context(|| format!("the zone has no world ({} of {} assets walked)", report.assets_walked, report.assets_total));
    }
    let peek = fastfile::peek(path)?;
    if peek.version == stock::VERSION_IW4_PC && !peek.big_endian {
        let (_, image) = stock::open(&std::fs::read(path)?)?;
        return stock::map_of_image(&image).context("the zone has no world, or its walk stopped before reaching it");
    }
    let key = zexport::find_key(root)?;
    let zone = Zone::open(path, &key)?;
    world::map_of_zone(&zone).context("the zone has no world")
}

impl App {
    /// Reads the map of the zone at `path` in the background and shows it when it is ready.
    pub(super) fn open_map(&mut self, path: PathBuf, name: String) {
        let Some(root) = self.root.clone() else { return };
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.mapview.active = true;
        self.mapview.title = name.clone();
        self.mapview.model = None;
        self.mapview.uploaded = false;
        self.mapview.markers.clear();
        self.mapview.marker_info.clear();
        self.mapview.lights = MapLights::default();
        self.mapview.light_pin = None;
        self.mapview.rx = Some(rx);
        log::log(format!("reading the map of {name}"));
        std::thread::spawn(move || {
            let r = load_map(&path, &root).map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
    }

    fn poll_mapview(&mut self) {
        let Some(rx) = &self.mapview.rx else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.mapview.rx = None;
        match result {
            Ok((model, names, textures, lights)) => {
                self.mapview.surface_tex = textures;
                self.mapview.tex_state.clear();
                let (tx, rx) = channel();
                self.mapview.tex_tx = Some(tx);
                self.mapview.tex_rx = Some(rx);
                let triangles: usize = model.surfaces.iter().map(|s| s.indices.len() / 3).sum();
                log::log(format!("map {}: {} materials, {triangles} triangles", self.mapview.title, names.len()));
                // the sky box is far bigger than the level, and shadow-catcher planes are not part of it: leave both out of sight and out of the framing
                let visible: Vec<bool> = names.iter().map(|n| { let n = n.to_ascii_lowercase(); !n.contains("sky") && !n.contains("shadow") }).collect();
                let bounds = framing_bounds(&model, &visible);
                if let Some((lo, hi)) = bounds {
                    self.mapview.camera.fit(lo, hi);
                }
                // the sun's arrow starts in the middle of the play area and is a third as long as the area is wide
                let (centre, extent) = bounds.map_or(([0.0; 3], 1000.0), |(lo, hi)| (std::array::from_fn(|a| (lo[a] + hi[a]) * 0.5), (0..3).map(|a| (hi[a] - lo[a]).powi(2)).sum::<f32>().sqrt()));
                log::log(format!("map {}: {} primary lights, {} hero lights", self.mapview.title, lights.primary.iter().filter(|l| l.kind != LightKind::Other).count(), lights.hero.len()));
                (self.mapview.markers, self.mapview.marker_info) = markers_of(&lights, centre, extent * 0.3);
                self.mapview.lights = lights;
                self.mapview.visible = visible;
                self.mapview.names = names;
                self.mapview.model = Some(Arc::new(model));
                self.mapview.uploaded = false;
                if let Some(spec) = self.mapview.pending_view.take() {
                    self.mapview.apply_options(&spec);
                }
            }
            Err(e) => {
                log::log(format!("map {}: {e}", self.mapview.title));
                self.mapview.title = format!("{}: {e}", self.mapview.title);
            }
        }
    }

    /// Decodes the images the map's materials name (four worker threads) once the image catalogue and the viewer exist.
    fn request_map_textures(&mut self) {
        let (Some(images), Some(tx)) = (self.catalog.clone(), self.mapview.tex_tx.clone()) else { return };
        if self.mapview.viewer.is_none() {
            return;
        }
        let mut wanted: Vec<String> = self.mapview.surface_tex.iter().flat_map(|t| [&t.color, &t.normal, &t.spec]).flatten().map(|n| n.to_ascii_lowercase()).collect();
        wanted.sort();
        wanted.dedup();
        wanted.retain(|k| !self.mapview.tex_state.contains_key(k));
        if wanted.is_empty() {
            return;
        }
        for k in &wanted {
            self.mapview.tex_state.insert(k.clone(), Tex::Loading);
        }
        let per = wanted.len().div_ceil(4).max(1);
        for chunk in wanted.chunks(per) {
            let (chunk, tx, images, ctx) = (chunk.to_vec(), tx.clone(), std::sync::Arc::clone(&images), self.ctx.clone());
            std::thread::spawn(move || {
                for key in chunk {
                    let r = modeltex::load_image(&images, &key).map_err(|e| format!("{e:#}"));
                    let _ = tx.send((key, r));
                    ctx.request_repaint();
                }
            });
        }
    }

    fn poll_map_textures(&mut self) {
        let Some(rx) = &self.mapview.tex_rx else { return };
        let mut got = Vec::new();
        while let Ok(x) = rx.try_recv() {
            got.push(x);
        }
        for (key, r) in got {
            if let (Ok(img), Some(v)) = (&r, self.mapview.viewer.as_mut()) {
                v.upload_texture(&key, img);
            }
            self.mapview.tex_state.insert(key, Tex::Done);
        }
    }

    pub(super) fn mapview_ui(&mut self, ui: &mut egui::Ui) {
        self.poll_mapview();
        self.request_map_textures();
        self.poll_map_textures();
        if self.mapview.viewer.is_none() {
            self.mapview.viewer = self.wgpu_state.as_ref().map(Viewer::new);
            self.mapview.uploaded = false;
        }
        if !self.mapview.uploaded {
            if let Some(v) = &mut self.mapview.viewer {
                v.set_model(self.mapview.model.as_deref(), &self.mapview.surface_tex, &[], &[]);
                v.set_lights(self.mapview.markers.clone());
                self.mapview.uploaded = true;
            }
        }
        let mv = &mut self.mapview;
        ui.horizontal_wrapped(|ui| {
            ui.strong(&mv.title);
            if let Some(m) = &mv.model {
                let triangles: usize = m.surfaces.iter().map(|s| s.indices.len() / 3).sum();
                let (done, total) = (mv.tex_state.values().filter(|s| matches!(s, Tex::Done)).count(), mv.tex_state.len());
                ui.weak(format!("{} materials, {triangles} triangles, {done}/{total} images", mv.names.len()));
            }
            egui::ComboBox::from_id_salt("map_shading").selected_text(Shading::ALL.iter().find(|(s, _)| *s == mv.shading).map_or("", |x| x.1)).show_ui(ui, |ui| {
                for (s, name) in Shading::ALL {
                    ui.selectable_value(&mut mv.shading, s, name);
                }
            });
            egui::ComboBox::from_id_salt("map_cull").selected_text(match mv.cull {
                Cull::Material => "material culling",
                Cull::None => "no culling",
                Cull::Back => "back faces culled",
                Cull::Front => "front faces culled",
            }).show_ui(ui, |ui| {
                ui.selectable_value(&mut mv.cull, Cull::None, "no culling");
                ui.selectable_value(&mut mv.cull, Cull::Back, "back faces culled");
                ui.selectable_value(&mut mv.cull, Cull::Front, "front faces culled");
            });
            ui.checkbox(&mut mv.wire, "Wireframe");
            let count = |c: LightClass| mv.markers.iter().filter(|m| m.class == c).count();
            let shown = mv.markers.len() - count(LightClass::Sun);
            ui.checkbox(&mut mv.show_lights, format!("Lights ({shown})")).on_hover_text("Marks each light of the map: a diamond for omni and spot lights (a spot also shows its axis), a square for hero-only lights, an arrow towards the sun. Hover a marker for its values and reach.");
            if mv.show_lights {
                for c in LightClass::ALL {
                    let mut on = mv.light_mask & c.bit() != 0;
                    let label = if c == LightClass::Sun { c.name().to_string() } else { format!("{} {}", c.name(), count(c)) };
                    if ui.checkbox(&mut on, label).changed() {
                        mv.light_mask = (mv.light_mask & !c.bit()) | if on { c.bit() } else { 0 };
                    }
                }
            }
            if ui.button("Reset view").clicked() {
                if let Some(m) = &mv.model {
                    if let Some((lo, hi)) = framing_bounds(m, &mv.visible) {
                        mv.camera.fit(lo, hi);
                    }
                }
            }
            if ui.button("Close map").clicked() {
                mv.active = false;
                mv.model = None;
                mv.viewer = None;
            }
            ui.weak("drag: rotate · right-drag: pan · wheel: zoom · Shift: fine");
        });
        let (resp, size) = viewer::viewport_rect(ui);
        let rect = resp.rect;
        // holding Shift slows every movement to a fifth, for fine positioning
        let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
        if resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta() * fine;
            mv.camera.orbit(d.x, d.y);
        }
        if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta() * fine;
            mv.camera.pan(d.x, d.y, rect.height());
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y) * fine;
            if scroll != 0.0 {
                mv.camera.zoom(scroll);
            }
        }
        // the light nearest the pointer (within 14 pixels) is the one whose values and reach are shown
        mv.light_hover = None;
        if mv.show_lights && resp.hovered() {
            if let Some(p) = resp.hover_pos() {
                let ppp = ui.ctx().pixels_per_point();
                let (px, py) = ((p.x - rect.min.x) * ppp, (p.y - rect.min.y) * ppp);
                let nearest = mv
                    .markers
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| mv.light_mask & m.class.bit() != 0)
                    .filter_map(|(i, m)| mv.camera.project(anchor(m), size).map(|q| (i, (q[0] - px).powi(2) + (q[1] - py).powi(2))))
                    .filter(|(_, d)| *d < (14.0 * ppp).powi(2))
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                mv.light_hover = nearest.map(|(i, _)| i);
            }
        }
        let highlight = mv.light_hover.or(mv.light_pin).filter(|i| *i < mv.markers.len());
        let view = LightView { show: mv.show_lights, mask: mv.light_mask, highlight };
        let tex = match &mut mv.viewer {
            Some(v) if mv.model.is_some() => {
                v.set_light_view(view);
                v.render(size, &mv.camera, mv.shading, mv.cull, mv.wire, &mv.visible)
            }
            _ => None,
        };
        match tex {
            Some(id) => {
                ui.painter().image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                if let (true, Some(text)) = (mv.show_lights, highlight.and_then(|i| mv.marker_info.get(i))) {
                    let galley = ui.painter().layout_no_wrap(text.clone(), egui::FontId::monospace(12.0), Color32::WHITE);
                    let at = rect.left_bottom() + egui::vec2(8.0, -8.0 - galley.size().y);
                    ui.painter().rect_filled(Rect::from_min_size(at - egui::vec2(4.0, 2.0), galley.size() + egui::vec2(8.0, 4.0)), 3.0, Color32::from_black_alpha(190));
                    ui.painter().galley(at, galley, Color32::WHITE);
                }
            }
            None => {
                ui.painter().rect_filled(rect, 0.0, Color32::from_gray(23));
                let msg = if mv.viewer.is_none() { "The GPU renderer is not available" } else if mv.rx.is_some() { "reading the map…" } else { "no map" };
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(16.0), Color32::GRAY);
            }
        }
    }
}
