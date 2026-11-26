//! The Effects tab: the effects of a Call of Duty Online zone, listed and played in the 3D viewer.
//!
//! Picking a zone reads its effects together with those of `common_mp` (most effects draw materials that zone loads, and many
//! start effects defined there). The player is `fx::sim`; what it does not do is listed there.

use super::App;
use crate::fx::sim::{Frame, Player};
use crate::fx::def::LoadedModel;
use crate::fx::{ElemType, Library, Visual};
use crate::log;
use crate::modelindex::SurfaceTextures;
use crate::modeltex::{self, TexImage};
use crate::viewer::{self, Camera, Cull, Shading, Viewer};
use crate::zone::{export as zexport, fastfile};
use eframe::egui::{self, Color32, Pos2, Rect};
use glam::Vec3;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

type LoadResult = Result<(Library, String), String>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Facing {
    /// The effect's forward axis points up (how an effect plays on a floor).
    Up,
    /// Forward along +X (how `playfx` without a direction plays it).
    Forward,
}

pub(super) struct EffectState {
    zones: Vec<(String, PathBuf)>,
    for_root: Option<PathBuf>,
    zone: Option<usize>,
    lib: Option<Arc<Library>>,
    names: Vec<String>,
    filter: String,
    selected: Option<String>,
    /// The effect the camera was last framed for.
    framed: Option<String>,
    rx: Option<Receiver<LoadResult>>,
    pub(super) message: Option<String>,
    viewer: Option<Viewer>,
    camera: Camera,
    ground_uploaded: bool,
    player: Option<Player>,
    playing: bool,
    repeat: bool,
    ground: bool,
    children: bool,
    speed: f32,
    facing: Facing,
    last: Option<Instant>,
    /// Restart the loop this many msec after the effect ends.
    idle: f64,
    tex_state: HashMap<String, bool>,
    /// The effect whose images and models `wanted_*` list.
    assets_for: Option<String>,
    wanted_images: Vec<String>,
    wanted_models: Vec<String>,
    /// Models effect elements place: `None` while loading or when not found.
    models: HashMap<String, Option<Arc<LoadedModel>>>,
    model_tx: Sender<(String, Option<Arc<LoadedModel>>)>,
    model_rx: Receiver<(String, Option<Arc<LoadedModel>>)>,
    tex_tx: Option<Sender<(String, Result<TexImage, String>)>>,
    tex_rx: Option<Receiver<(String, Result<TexImage, String>)>>,
}

impl Default for EffectState {
    fn default() -> Self {
        let (tex_tx, tex_rx) = channel();
        let (model_tx, model_rx) = channel();
        EffectState {
            zones: Vec::new(),
            for_root: None,
            zone: None,
            lib: None,
            names: Vec::new(),
            filter: String::new(),
            selected: None,
            framed: None,
            rx: None,
            message: None,
            viewer: None,
            camera: Camera { dist: 260.0, radius: 120.0, target: Vec3::new(0.0, 0.0, 40.0), ..Default::default() },
            ground_uploaded: false,
            player: None,
            playing: true,
            repeat: true,
            ground: true,
            children: true,
            speed: 1.0,
            facing: Facing::Up,
            last: None,
            idle: 0.0,
            tex_state: HashMap::new(),
            assets_for: None,
            wanted_images: Vec::new(),
            wanted_models: Vec::new(),
            models: HashMap::new(),
            model_tx,
            model_rx,
            tex_tx: Some(tex_tx),
            tex_rx: Some(tex_rx),
        }
    }
}

impl EffectState {
    /// True the first time `name` is played (the camera frames it then, not on every restart).
    fn selected_changed(&mut self, name: &str) -> bool {
        if self.framed.as_deref() == Some(name) {
            return false;
        }
        self.framed = Some(name.to_string());
        true
    }

    /// An effect is playing: the window needs a new frame continuously.
    pub(super) fn animating(&self) -> bool {
        self.playing && self.player.is_some()
    }

    pub(super) fn is_busy(&self) -> bool {
        self.rx.is_some() || self.tex_state.values().any(|done| !done)
    }
}

impl App {
    fn load_effect_zones(&mut self) {
        let Some(root) = self.root.clone() else { return };
        if self.effects.for_root.as_ref() == Some(&root) {
            return;
        }
        let mut zones: Vec<(String, PathBuf)> = zexport::zone_files(&root)
            .into_iter()
            .filter(|p| fastfile::peek(p).is_ok_and(|k| matches!(k.version, fastfile::VERSION_C1 | fastfile::VERSION_QA | fastfile::VERSION_319)))
            .map(|p| (p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), p))
            .collect();
        zones.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
        self.effects = EffectState { zones, for_root: Some(root), ..Default::default() };
        if let Ok(want) = crate::config::env_var("FX_ZONE") {
            if let Some(i) = self.effects.zones.iter().position(|z| z.0.eq_ignore_ascii_case(&want)) {
                self.start_effect_load(i);
            }
        }
    }

    fn start_effect_load(&mut self, zone: usize) {
        let (Some((name, path)), Some(root)) = (self.effects.zones.get(zone).cloned(), self.root.clone()) else { return };
        self.effects.zone = Some(zone);
        self.effects.message = Some(format!("Reading the effects of {name} …"));
        log::log(format!("effects: reading {name}"));
        let common: Option<PathBuf> = self.effects.zones.iter().find(|z| z.0.eq_ignore_ascii_case("common_mp.ff")).map(|z| z.1.clone()).filter(|p| *p != path);
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let r = (|| -> anyhow::Result<(Library, String)> {
                let key = zexport::find_key(&root)?;
                let mut lib = Library::default();
                let n = lib.read_zone(&path, &key)?;
                let mut note = format!("{n} effects in {name}");
                if let Some(c) = common {
                    match lib.read_zone(&c, &key) {
                        Ok(m) => note += &format!(", {m} more from common_mp for the effects they start"),
                        Err(e) => log::log(format!("effects: common_mp: {e:#}")),
                    }
                }
                Ok((lib, note))
            })();
            let _ = tx.send(r.map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        self.effects.rx = Some(rx);
    }

    fn poll_effects(&mut self) {
        let Some(rx) = &self.effects.rx else { return };
        let Ok(r) = rx.try_recv() else { return };
        self.effects.rx = None;
        match r {
            Ok((lib, note)) => {
                let zone_stem = self.effects.zone.and_then(|z| self.effects.zones.get(z)).map(|z| z.0.trim_end_matches(".ff").to_ascii_lowercase()).unwrap_or_default();
                // the list shows the zone's own effects; common_mp's are there to be started by them
                let mut names: Vec<String> = lib.effects.values().filter(|e| e.zone.eq_ignore_ascii_case(&zone_stem) && !e.elems.is_empty()).map(|e| e.name.clone()).collect();
                names.sort_by_key(|n| n.to_ascii_lowercase());
                log::log(format!("effects: {note}"));
                self.effects.message = Some(note);
                self.effects.names = names;
                let lib = Arc::new(lib);
                self.effects.lib = Some(lib.clone());
                self.effects.player = Some(Player::new(lib));
                let want = crate::config::env_var("FX_EFFECT").ok();
                let pick = want.and_then(|w| self.effects.names.iter().find(|n| n.eq_ignore_ascii_case(&w)).cloned()).or_else(|| self.effects.selected.clone().filter(|s| self.effects.names.contains(s)));
                self.effects.selected = None;
                if let Some(p) = pick {
                    self.play_effect(&p);
                }
            }
            Err(e) => {
                log::log(format!("effects: {e}"));
                self.effects.message = Some(e);
            }
        }
    }

    fn play_effect(&mut self, name: &str) {
        let fx = &mut self.effects;
        fx.selected = Some(name.to_string());
        fx.idle = 0.0;
        let forward = if fx.facing == Facing::Up { Vec3::Z } else { Vec3::X };
        if let Some(p) = &mut fx.player {
            p.reset();
            p.options.ground = fx.ground.then_some(0.0);
            p.options.children = fx.children;
            p.spawn_named(name, Frame::pointing(Vec3::ZERO, forward));
        }
        fx.playing = true;
        fx.last = None;
        if fx.selected_changed(name) {
            if let Some(e) = fx.lib.as_ref().and_then(|l| l.get(name)) {
                let r = e.reach();
                fx.camera.radius = r;
                fx.camera.dist = r * 1.8;
                fx.camera.target = Vec3::new(0.0, 0.0, r * 0.3);
            }
        }
    }

    /// Asks for the images and models the playing effect (and what it starts) needs and nobody asked for yet. Runs every frame, so
    /// what could not be asked for earlier (the image catalogue still being scanned, the model catalogue not there yet) is asked
    /// for as soon as it can be.
    fn request_effect_assets(&mut self) {
        let fx = &mut self.effects;
        let (Some(lib), Some(name)) = (fx.lib.clone(), fx.selected.clone()) else { return };
        if fx.assets_for.as_deref() != Some(name.as_str()) {
            fx.assets_for = Some(name.clone());
            fx.wanted_images = lib.images_for(&name);
            fx.wanted_models = lib.models_for(&name);
        }
        // models: from the zones the library read, else streamed files with their materials from the model index
        let new_models: Vec<String> = fx.wanted_models.iter().filter(|m| !fx.models.contains_key(*m)).cloned().collect();
        if !new_models.is_empty() {
            let (catalog, index) = (self.models.catalog(), self.models.index());
            let (tx, ctx) = (fx.model_tx.clone(), self.ctx.clone());
            for m in &new_models {
                fx.models.insert(m.clone(), None);
            }
            std::thread::spawn(move || {
                for m in new_models {
                    let l = crate::fx::view::find_model(&lib, catalog.as_deref(), index.as_deref(), &m);
                    if l.is_none() {
                        log::log(format!("effects: model {m} not found"));
                    }
                    let _ = tx.send((m, l));
                    ctx.request_repaint();
                }
            });
        }
        let Some(images) = self.catalog.clone() else { return };
        let mut wanted: Vec<String> = fx.wanted_images.clone();
        for l in fx.models.values().flatten() {
            wanted.extend(l.textures.iter().flat_map(|t| [&t.color, &t.normal, &t.spec]).flatten().map(|n| n.to_ascii_lowercase()));
        }
        wanted.retain(|k| !fx.tex_state.contains_key(k));
        wanted.sort();
        wanted.dedup();
        if wanted.is_empty() {
            return;
        }
        for k in &wanted {
            fx.tex_state.insert(k.clone(), false);
        }
        let (tx, ctx) = (fx.tex_tx.clone().expect("texture channel"), self.ctx.clone());
        std::thread::spawn(move || {
            for key in wanted {
                let r = modeltex::load_image(&images, &key).map_err(|e| format!("{e:#}"));
                let _ = tx.send((key, r));
                ctx.request_repaint();
            }
        });
    }

    fn poll_effect_models(&mut self) {
        while let Ok((name, l)) = self.effects.model_rx.try_recv() {
            self.effects.models.insert(name, l);
        }
    }

    fn poll_effect_textures(&mut self) {
        let Some(rx) = &self.effects.tex_rx else { return };
        let mut got = Vec::new();
        while let Ok(x) = rx.try_recv() {
            got.push(x);
        }
        for (key, r) in got {
            match (&r, self.effects.viewer.as_mut()) {
                (Ok(img), Some(v)) => v.upload_texture(&key, img),
                (Err(e), _) => log::log(format!("effects: {e}")),
                _ => {}
            }
            self.effects.tex_state.insert(key, true);
        }
    }

    pub(super) fn effects_ui(&mut self, ui: &mut egui::Ui) {
        self.load_effect_zones();
        self.poll_effects();
        self.poll_effect_models();
        self.request_effect_assets();
        self.poll_effect_textures();
        if self.root.is_none() {
            egui::CentralPanel::default().show(ui, |ui| ui.label("Open a Call of Duty Online game folder to browse its effects."));
            return;
        }
        egui::Panel::left("fx_list").resizable(true).default_size(340.0).show(ui, |ui| self.effect_list(ui));
        egui::Panel::bottom("fx_elems").resizable(true).default_size(180.0).show(ui, |ui| self.effect_elements(ui));
        egui::CentralPanel::default().show(ui, |ui| self.effect_view(ui));
    }

    fn effect_list(&mut self, ui: &mut egui::Ui) {
        let mut load = None;
        let mut play = None;
        let fx = &mut self.effects;
        let current = fx.zone.and_then(|z| fx.zones.get(z)).map_or("pick a zone", |z| z.0.as_str()).to_string();
        ui.horizontal(|ui| {
            ui.label("Zone");
            egui::ComboBox::from_id_salt("fx_zone").width(240.0).selected_text(current).show_ui(ui, |ui| {
                for (i, (name, _)) in fx.zones.iter().enumerate() {
                    if ui.selectable_label(fx.zone == Some(i), name).clicked() {
                        load = Some(i);
                    }
                }
            });
            if fx.rx.is_some() {
                ui.spinner();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.text_edit_singleline(&mut fx.filter);
        });
        ui.separator();
        let filter = fx.filter.to_ascii_lowercase();
        let shown: Vec<&String> = fx.names.iter().filter(|n| filter.is_empty() || n.to_ascii_lowercase().contains(&filter)).collect();
        ui.weak(format!("{} of {} effects", shown.len(), fx.names.len()));
        let row = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show_rows(ui, row, shown.len(), |ui, range| {
            for name in &shown[range] {
                let on = fx.selected.as_deref() == Some(name.as_str());
                if ui.selectable_label(on, name.as_str()).clicked() {
                    play = Some((*name).clone());
                }
            }
        });
        if let Some(i) = load {
            self.start_effect_load(i);
        }
        if let Some(n) = play {
            self.play_effect(&n);
        }
    }

    fn effect_elements(&mut self, ui: &mut egui::Ui) {
        let fx = &self.effects;
        let (Some(lib), Some(name)) = (&fx.lib, &fx.selected) else {
            ui.weak("Pick an effect to see its elements.");
            return;
        };
        let Some(e) = lib.get(name) else { return };
        ui.horizontal(|ui| {
            ui.strong(&e.name);
            ui.weak(format!("{} looping, {} one-shot, {} emission elements", e.looping, e.one_shot, e.emission));
            if let Some(p) = &fx.player {
                if !p.missing.is_empty() {
                    ui.colored_label(Color32::from_rgb(230, 170, 90), format!("not found: {}", p.missing.join(", ")));
                }
            }
        });
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            egui::Grid::new("fx_elem_grid").striped(true).num_columns(7).show(ui, |ui| {
                for h in ["#", "run", "type", "spawn", "life ms", "visual", "starts"] {
                    ui.strong(h);
                }
                ui.end_row();
                for (i, el) in e.elems.iter().enumerate() {
                    let run = if i < e.looping { "looping" } else if i < e.looping + e.one_shot { "one-shot" } else { "emission" };
                    ui.label(i.to_string());
                    ui.label(run);
                    ui.label(el.elem_type.name());
                    ui.label(if i < e.looping { format!("every {} ms, {}×", el.spawn[0], el.spawn[1]) } else { format!("{} + {}", el.spawn[0], el.spawn[1]) });
                    ui.label(format!("{} + {}", el.life_msec[0], el.life_msec[1]));
                    let visual = el.visuals.iter().map(|v| v.label()).collect::<Vec<_>>().join(", ");
                    let missing = el.visuals.iter().any(|v| matches!(v, Visual::Material { textures, .. } if textures.color.is_none()));
                    if missing && el.elem_type != ElemType::Model {
                        ui.colored_label(Color32::from_rgb(230, 170, 90), visual).on_hover_text("This material's images were not found in the zone or common_mp: drawn as a soft spot");
                    } else {
                        ui.label(visual);
                    }
                    let starts: Vec<String> = [("impact", &el.on_impact), ("death", &el.on_death), ("emits", &el.emitted)].iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} {v}"))).collect();
                    ui.label(starts.join(", "));
                    ui.end_row();
                }
            });
        });
    }

    fn effect_view(&mut self, ui: &mut egui::Ui) {
        if self.effects.viewer.is_none() {
            self.effects.viewer = self.wgpu_state.as_ref().map(Viewer::new);
            self.effects.ground_uploaded = false;
            // images uploaded to the old viewer are gone
            self.effects.tex_state.clear();
        }
        let mut replay = false;
        let fx = &mut self.effects;
        ui.horizontal_wrapped(|ui| {
            if ui.button(if fx.playing { "Pause" } else { "Play" }).clicked() {
                fx.playing = !fx.playing;
                fx.last = None;
            }
            if ui.button("Restart").on_hover_text("Plays the effect again from the start (R)").clicked() {
                replay = true;
            }
            if let Some(p) = &mut fx.player {
                if ui.button("Stop looping").on_hover_text("Lets the looping elements end; what is alive plays out").clicked() {
                    p.stop_looping();
                }
            }
            ui.checkbox(&mut fx.repeat, "Repeat").on_hover_text("Starts the effect again a second after it has ended (decals it left behind do not count)");
            ui.add(egui::Slider::new(&mut fx.speed, 0.05..=4.0).logarithmic(true).text("speed"));
            ui.separator();
            if ui.checkbox(&mut fx.ground, "Ground").on_hover_text("Elements that collide stop on the floor (and start their impact effect)").changed() {
                replay = true;
            }
            if ui.checkbox(&mut fx.children, "Child effects").on_hover_text("Starts the effects elements emit, start on impact or death, and runners").changed() {
                replay = true;
            }
            egui::ComboBox::from_id_salt("fx_facing").selected_text(if fx.facing == Facing::Up { "forward: up" } else { "forward: +X" }).show_ui(ui, |ui| {
                replay |= ui.selectable_value(&mut fx.facing, Facing::Up, "forward: up").changed();
                replay |= ui.selectable_value(&mut fx.facing, Facing::Forward, "forward: +X").changed();
            });
            if let Some(p) = &fx.player {
                let state = if p.playing() { "" } else if fx.repeat { ", ended: repeats" } else { ", ended" };
                ui.weak(format!("{:.2} s, {} elements{state}", p.time / 1000.0, p.particle_count()));
            }
            if self.catalog.is_none() {
                ui.colored_label(Color32::from_rgb(230, 170, 90), "images: waiting for the image catalogue (soft spots until then)");
            } else {
                let pending = fx.tex_state.values().filter(|d| !**d).count();
                if pending > 0 {
                    ui.weak(format!("loading {pending} images"));
                }
            }
            ui.weak(self.settings.camera_style.hint());
        });
        if ui.input(|i| i.key_pressed(egui::Key::R)) && !ui.ctx().egui_wants_keyboard_input() {
            replay = true;
        }
        if replay {
            if let Some(n) = self.effects.selected.clone() {
                self.play_effect(&n);
            }
        }
        let fx = &mut self.effects;
        // the clock
        let now = Instant::now();
        let dt = fx.last.map_or(0.0, |l| now.duration_since(l).as_secs_f64() * 1000.0);
        fx.last = Some(now);
        let mut restart = false;
        if let (true, Some(p)) = (fx.playing, &mut fx.player) {
            p.update(dt * fx.speed as f64);
            // decals left behind do not hold the repeat back
            if !p.playing() && fx.selected.is_some() {
                fx.idle += dt;
                if fx.repeat && fx.idle > 1000.0 {
                    restart = true;
                }
            }
            ui.ctx().request_repaint();
        }
        let (resp, size) = viewer::viewport_rect(ui);
        let rect = resp.rect;
        viewer::camera_input(ui, &resp, rect.height(), &mut fx.camera, self.settings.camera_style, 1.0);
        let tex = match (&mut fx.viewer, &fx.player) {
            (Some(v), player) => {
                if !fx.ground_uploaded {
                    let g = crate::fx::view::ground_model();
                    // drawn without writing depth: sprites that dip below the floor show through it instead of being cut off
                    let floor = crate::modelindex::MaterialState { sort_key: 0, src: 2, dst: 1, op: 0, alpha_test: 0, cull: 1, depth_write: false, polygon_offset: false, technique: 0 };
                    v.set_model(Some(&g), &[SurfaceTextures::default()], &[Some(floor)], &[]);
                    fx.ground_uploaded = true;
                }
                let (eye, right, up) = fx.camera.eye_axes();
                let list = player.as_ref().map(|p| p.draw(eye, right, up));
                let batches = list.as_ref().map_or(Vec::new(), |l| crate::fx::view::batches(l, right, up));
                v.set_fx(&batches);
                v.set_fx_models(&list.as_ref().map_or(Vec::new(), |l| crate::fx::view::models(l, &fx.models)));
                v.render(size, &fx.camera, Shading::Textured, Cull::None, false, &[true])
            }
            _ => None,
        };
        match tex {
            Some(id) => {
                ui.painter().image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                ui.painter().rect_filled(rect, 0.0, Color32::from_gray(23));
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "The GPU renderer is not available", egui::FontId::proportional(16.0), Color32::GRAY);
            }
        }
        if restart {
            if let Some(n) = self.effects.selected.clone() {
                self.play_effect(&n);
            }
        }
    }
}
