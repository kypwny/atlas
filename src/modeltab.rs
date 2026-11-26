//! The "Models" tab: list of streamed models on the left, the 3D view in the
//! middle, surface details on the right.

use crate::model::Model;
use crate::modelindex::{MaterialState, ModelIndex, SurfaceTextures};
use crate::modelsource::{self, ModelCatalog};
use crate::modeltex::{self, TexImage};
use crate::source::Catalog;
use std::collections::HashMap;
use crate::viewer::{self, Camera, Cull, Shading, Viewer};
use eframe::egui::{self, Color32, Pos2, Rect, Sense};
use eframe::egui_wgpu::RenderState;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

type Loaded = (u64, Result<Model, String>);

#[derive(Clone, PartialEq)]
enum TexState {
    Loading,
    Ready,
    Failed(String),
}

fn index_config_file() -> Option<std::path::PathBuf> {
    crate::config::config_dir().map(|d| d.join("model_index.txt"))
}

pub struct ModelTab {
    catalog: Option<Arc<ModelCatalog>>,
    filter: String,
    applied: Option<String>,
    visible: Vec<usize>,
    selected: Option<usize>,
    model: Option<Arc<Model>>,
    surface_visible: Vec<bool>,
    uploaded: bool,
    loading: bool,
    error: Option<String>,
    generation: u64,
    tx: Sender<Loaded>,
    rx: Receiver<Loaded>,
    camera: Camera,
    shading: Shading,
    cull: Cull,
    wire: bool,
    viewer: Option<Viewer>,
    ctx: Option<egui::Context>,
    index: Option<Arc<ModelIndex>>,
    /// How the mouse moves the camera (set from the settings every frame).
    pub camera_style: viewer::CameraStyle,
    index_status: String,
    index_jobs: usize,
    index_progress: Arc<(std::sync::atomic::AtomicUsize, std::sync::atomic::AtomicUsize)>,
    root: Option<std::path::PathBuf>,
    zones: crate::zone::export::ZoneSurvey,
    export_options: crate::export::Options,
    export_message: Option<(bool, String)>,
    export_anims: String,
    export_rx: Option<std::sync::mpsc::Receiver<(bool, String)>>,
    index_tx: Sender<Result<ModelIndex, String>>,
    index_rx: Receiver<Result<ModelIndex, String>>,
    images: Option<Arc<Catalog>>,
    surface_tex: Vec<SurfaceTextures>,
    use_normal: bool,
    use_spec: bool,
    surface_states: Vec<Option<MaterialState>>,
    surface_env: Vec<Option<[f32; 4]>>,
    tex_state: HashMap<String, TexState>,
    tex_tx: Sender<(String, Result<TexImage, String>)>,
    tex_rx: Receiver<(String, Result<TexImage, String>)>,
}

impl Default for ModelTab {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelTab {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        let (index_tx, index_rx) = channel();
        let (tex_tx, tex_rx) = channel();
        Self {
            catalog: None,
            filter: String::new(),
            applied: None,
            visible: Vec::new(),
            selected: None,
            model: None,
            surface_visible: Vec::new(),
            uploaded: false,
            loading: false,
            error: None,
            generation: 0,
            tx,
            rx,
            camera: Camera::default(),
            shading: Shading::Textured,
            cull: Cull::Material,
            wire: false,
            viewer: None,
            ctx: None,
            index: None,
            camera_style: Default::default(),
            index_status: String::new(),
            index_jobs: 0,
            index_progress: Arc::new(Default::default()),
            root: None,
            zones: Default::default(),
            export_options: crate::export::Options::default(),
            export_message: None,
            export_anims: String::new(),
            export_rx: None,
            index_tx,
            index_rx,
            images: None,
            surface_tex: Vec::new(),
            use_normal: true,
            use_spec: true,
            surface_states: Vec::new(),
            surface_env: Vec::new(),
            tex_state: HashMap::new(),
            tex_tx,
            tex_rx,
        }
    }

    pub fn set_root(&mut self, root: &Path) {
        self.root = Some(root.to_path_buf());
        self.zones = crate::zone::export::survey(root);
        let cat = modelsource::scan(modelsource::discover(root));
        self.catalog = if cat.providers.is_empty() { None } else { Some(Arc::new(cat)) };
        self.applied = None;
        self.visible.clear();
        self.selected = None;
        self.model = None;
        self.error = None;
        self.uploaded = false;
        self.generation += 1;
        self.loading = false;
        self.surface_tex.clear();
        self.tex_state.clear();
        for candidate in [
            crate::config::env_var_os("INDEX").map(std::path::PathBuf::from),
            index_config_file().and_then(|f| std::fs::read_to_string(f).ok()).map(|s| std::path::PathBuf::from(s.trim())),
            Some(root.join("atlas-model-index.tsv")),
            Some(root.join("codol-assets-model-index.tsv")),
        ]
        .into_iter()
        .flatten()
        {
            if candidate.is_file() {
                self.load_index(candidate);
                return;
            }
        }
        if self.index.is_none() {
            self.index_status = "No model index loaded: models are shown without textures.".into();
        }
    }

    /// Loads the model index (see the modelindex module) on a background thread.
    pub fn load_index(&mut self, path: std::path::PathBuf) {
        self.index_jobs += 1;
        self.index_status = format!("Reading {} ...", path.display());
        if let Some(f) = index_config_file() {
            if let Some(dir) = f.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(f, path.to_string_lossy().as_bytes());
        }
        let (tx, ctx) = (self.index_tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(ModelIndex::load(&path).map_err(|e| format!("{e:#}")));
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    /// Builds the model index by replaying the game's zones in-process, caches it and loads it.
    pub fn build_index(&mut self) {
        let Some(root) = self.root.clone() else { return };
        self.index_jobs += 1;
        self.index_progress.0.store(0, std::sync::atomic::Ordering::Relaxed);
        self.index_progress.1.store(0, std::sync::atomic::Ordering::Relaxed);
        self.index_status = "Reading the game's zones ...".into();
        let (tx, ctx, progress) = (self.index_tx.clone(), self.ctx.clone(), Arc::clone(&self.index_progress));
        std::thread::spawn(move || {
            let result = crate::zone::export::build_index(&root, &progress).map_err(|e| format!("{e:#}")).map(|text| {
                if let Some(f) = index_config_file() {
                    let cache = f.with_file_name("model_index_cache.tsv");
                    if std::fs::write(&cache, &text).is_ok() {
                        let _ = std::fs::write(&f, cache.to_string_lossy().as_bytes());
                    }
                }
                ModelIndex::parse(&text)
            });
            let _ = tx.send(result);
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    /// The catalogue of model files, when the game folder has any.
    pub fn catalog(&self) -> Option<Arc<ModelCatalog>> {
        self.catalog.clone()
    }

    /// The model index, when one is loaded.
    pub fn index(&self) -> Option<Arc<ModelIndex>> {
        self.index.clone()
    }

    pub fn set_images(&mut self, images: Option<Arc<Catalog>>) {
        self.images = images;
        self.tex_state.clear();
        self.request_textures();
    }

    pub fn is_busy(&self) -> bool {
        self.loading || self.index_jobs > 0 || self.tex_state.values().any(|s| *s == TexState::Loading)
    }

    /// Starts decoding every image the current model needs that is not on the GPU yet.
    fn request_textures(&mut self) {
        let (Some(images), Some(viewer)) = (self.images.clone(), self.viewer.as_ref()) else { return };
        let wanted: Vec<String> = self.surface_tex.iter().flat_map(|t| [&t.color, &t.normal, &t.spec]).flatten().cloned().collect();
        for name in wanted {
            let key = name.to_ascii_lowercase();
            if self.tex_state.contains_key(&key) || viewer.has_texture(&key) {
                continue;
            }
            self.tex_state.insert(key.clone(), TexState::Loading);
            let (tx, ctx, images) = (self.tex_tx.clone(), self.ctx.clone(), Arc::clone(&images));
            std::thread::spawn(move || {
                let r = modeltex::load_image(&images, &key).map_err(|e| format!("{e:#}"));
                let _ = tx.send((key, r));
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            });
        }
    }

    pub fn has_model(&self) -> bool {
        self.model.is_some()
    }

    /// Test hook: view options as `key=value,...` (shading, cull, wire, yaw, pitch).
    pub fn apply_options(&mut self, spec: &str) {
        for kv in spec.split(',') {
            let Some((k, v)) = kv.split_once('=') else { continue };
            match k {
                "shading" => {
                    self.shading = match v {
                        "normals" => Shading::Normals,
                        "uv" => Shading::UvChecker,
                        "uvcolor" => Shading::UvColor,
                        "color" => Shading::VertexColor,
                        "surfaces" => Shading::Surfaces,
                        _ => Shading::Lit,
                    }
                }
                "cull" => {
                    self.cull = match v {
                        "none" => Cull::None,
                        "front" => Cull::Front,
                        "back" => Cull::Back,
                        _ => Cull::Material,
                    }
                }
                "wire" => self.wire = v == "1",
                "normal" => self.use_normal = v != "0",
                "spec" => self.use_spec = v != "0",
                "yaw" => self.camera.yaw = v.parse::<f32>().unwrap_or(0.0).to_radians(),
                "pitch" => self.camera.pitch = v.parse::<f32>().unwrap_or(0.0).to_radians(),
                _ => {}
            }
        }
    }

    pub fn select_by_name(&mut self, name: &str) -> bool {
        let Some(cat) = self.catalog.clone() else { return false };
        match cat.entries.iter().position(|e| e.name.eq_ignore_ascii_case(name)) {
            Some(i) => {
                self.select(i);
                true
            }
            None => false,
        }
    }

    fn select(&mut self, entry: usize) {
        let Some(cat) = self.catalog.clone() else { return };
        self.selected = Some(entry);
        self.generation += 1;
        self.loading = true;
        self.error = None;
        let generation = self.generation;
        let e = &cat.entries[entry];
        let (provider, key) = (Arc::clone(&cat.providers[e.provider]), e.key.clone());
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let result = provider.read(&key).and_then(|b| Model::parse(&b)).map_err(|e| format!("{e:#}"));
            let _ = tx.send((generation, result));
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
    }

    fn poll(&mut self) {
        while let Ok(r) = self.index_rx.try_recv() {
            self.index_jobs = self.index_jobs.saturating_sub(1);
            match r {
                Ok(idx) => {
                    self.index_status = format!("Model index: {} models, {} materials from {} zones", idx.models.len(), idx.material_count(), idx.zones);
                    self.index = Some(Arc::new(idx));
                    self.resolve_surface_images();
                }
                Err(e) => self.index_status = format!("Model index failed to load: {e}"),
            }
        }
        while let Ok((name, r)) = self.tex_rx.try_recv() {
            match r {
                Ok(img) => {
                    if let Some(v) = &mut self.viewer {
                        v.upload_texture(&name, &img);
                    }
                    self.tex_state.insert(name, TexState::Ready);
                }
                Err(e) => {
                    self.tex_state.insert(name, TexState::Failed(e));
                }
            }
        }
        while let Ok((generation, result)) = self.rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.loading = false;
            match result {
                Ok(m) => {
                    if let Some((lo, hi)) = m.bounds() {
                        self.camera.fit(lo, hi);
                    }
                    self.surface_visible = vec![true; m.surfaces.len()];
                    self.model = Some(Arc::new(m));
                    self.uploaded = false;
                    self.resolve_surface_images();
                }
                Err(e) => {
                    self.model = None;
                    self.error = Some(e);
                    self.uploaded = false;
                }
            }
        }
    }

    /// Looks up the colour image of every surface of the selected file in the index.
    fn resolve_surface_images(&mut self) {
        let (Some(cat), Some(sel), Some(model)) = (self.catalog.clone(), self.selected, self.model.clone()) else { return };
        let file = &cat.entries[sel].name;
        self.surface_tex = match &self.index {
            Some(idx) => idx.surface_textures(file, model.surfaces.len()),
            None => vec![SurfaceTextures::default(); model.surfaces.len()],
        };
        self.surface_env = match &self.index {
            Some(idx) => idx.surface_env(file, model.surfaces.len()),
            None => vec![None; model.surfaces.len()],
        };
        self.surface_states = match &self.index {
            Some(idx) => idx.surface_states(file, model.surfaces.len()),
            None => vec![None; model.surfaces.len()],
        };
        self.uploaded = false;
        self.request_textures();
    }

    fn refilter(&mut self) {
        let Some(cat) = self.catalog.clone() else { return };
        if self.applied.as_deref() == Some(self.filter.as_str()) {
            return;
        }
        let words: Vec<String> = self.filter.split_whitespace().map(str::to_lowercase).collect();
        self.visible = cat
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let n = e.name.to_lowercase();
                words.iter().all(|w| n.contains(w.as_str()))
            })
            .map(|(i, _)| i)
            .collect();
        self.applied = Some(self.filter.clone());
    }

    /// Export controls: formats, textures, and the button that writes the selected model with its skeleton and skin weights.
    fn export_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let o = &mut self.export_options;
            ui.checkbox(&mut o.formats.glb, "glTF").on_hover_text("Binary glTF (.glb) with skeleton, skin and embedded textures");
            ui.checkbox(&mut o.formats.obj, "OBJ").on_hover_text("OBJ + MTL, no skeleton");
            ui.checkbox(&mut o.formats.xmodel_export, "XMODEL_EXPORT").on_hover_text("Version 6 text format of the CoD modding tools");
            ui.checkbox(&mut o.formats.semodel, "SEModel").on_hover_text("SE-Tools binary model, in centimetres");
            ui.checkbox(&mut o.formats.cast, "Cast").on_hover_text("Cast (.cast) with the skeleton, skin weights, materials and the animations named below: imports into Blender 3.6+ and Maya through dtzxporter's Cast add-on, in centimetres");
            ui.checkbox(&mut o.images, "textures");
            ui.checkbox(&mut o.formats.seanim, "SEAnim").on_hover_text("Also write the animations named below as xanim/<name>.seanim, and into the glTF");
        });
        ui.horizontal_wrapped(|ui| {
            let o = &mut self.export_options;
            ui.label("units:");
            egui::ComboBox::from_id_salt("export_unit")
                .selected_text(crate::export::Unit::ALL.iter().find(|(u, _)| *u == o.unit).map_or("", |(_, n)| *n))
                .show_ui(ui, |ui| {
                    for (u, name) in crate::export::Unit::ALL {
                        ui.selectable_value(&mut o.unit, u, name);
                    }
                })
                .response
                .on_hover_text("The unit positions are written in. The game works in inches; glTF, OBJ and XMODEL_EXPORT keep them, SEModel and Cast write centimetres, unless you pick one here for all");
            ui.label("× ");
            ui.add(egui::DragValue::new(&mut o.scale).speed(0.01).range(0.0001..=1000.0)).on_hover_text("An extra factor on top of the unit");
            ui.checkbox(&mut o.bake, "bake animations").on_hover_text("Key every frame of each animation (rotations interpolated) instead of the game's sparse keys, for programs that do not interpolate. Applies to SEAnim and Cast");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("animations:");
            ui.add(egui::TextEdit::singleline(&mut self.export_anims).hint_text("names, or prefix* (comma separated)").desired_width(260.0)).on_hover_text("Found by reading the game's zones (about 15 seconds); leave empty for none");
        });
        ui.horizontal_wrapped(|ui| {
            let any = self.export_options.formats.glb || self.export_options.formats.obj || self.export_options.formats.xmodel_export || self.export_options.formats.semodel || self.export_options.formats.cast;
            let busy = self.export_rx.is_some();
            let ready = self.model.is_some() && self.selected.is_some() && any && !busy;
            if ui.add_enabled(ready, egui::Button::new("Export model...").small()).on_hover_text("Writes xmodel/ and images/ folders under the folder you pick").clicked() {
                self.export_selected();
            }
            if let Some((ok, m)) = &self.export_message {
                ui.colored_label(if *ok { Color32::LIGHT_GREEN } else { Color32::LIGHT_RED }, m);
            }
        });
    }

    fn export_selected(&mut self) {
        let (Some(cat), Some(sel), Some(model)) = (self.catalog.clone(), self.selected, self.model.clone()) else { return };
        let Some(dir) = rfd::FileDialog::new().set_title("Export folder").pick_folder() else { return };
        let file = cat.entries[sel].name.clone();
        let em = crate::export::ExportModel::from_streamed(&model, self.index.as_deref(), &file);
        let (opts, images, root) = (self.export_options.clone(), self.images.clone(), self.root.clone());
        let names: Vec<String> = self.export_anims.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = self.ctx.clone();
        self.export_message = Some((true, if names.is_empty() { "Exporting ...".into() } else { "Reading the zones for animations, then exporting ...".into() }));
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<String> {
                let (mut anims, mut notes) = (Vec::new(), String::new());
                if !names.is_empty() {
                    let root = root.ok_or_else(|| anyhow::anyhow!("no game folder"))?;
                    let (found, problems) = crate::export::find_animations(&root, &names)?;
                    notes = format!("; {} animation(s){}", found.len(), if problems.is_empty() { String::new() } else { format!(", {} could not be decoded", problems.len()) });
                    anims = found;
                }
                let (files, missing) = crate::export::export(&em, &dir, images.as_deref(), &opts, &anims)?;
                Ok(format!(
                    "Wrote {} files to {}{}{}{}",
                    files.len(),
                    dir.display(),
                    if em.bones.is_empty() { " (no skeleton in the index: one root bone)" } else { "" },
                    if missing.is_empty() { String::new() } else { format!("; {} image(s) not found", missing.len()) },
                    notes
                ))
            })();
            let _ = tx.send(match result {
                Ok(m) => (true, m),
                Err(e) => (false, format!("{e:#}")),
            });
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
        self.export_rx = Some(rx);
    }

    /// Which fastfile version(s) the game folder's zones are, from their headers.
    fn zone_line(&self, ui: &mut egui::Ui) {
        use crate::zone::fastfile::version_info;
        let z = &self.zones;
        if z.total == 0 {
            ui.weak("Zones: none found in this folder (the model index needs the game's .ff files).");
            return;
        }
        let text = z
            .by_version
            .iter()
            .map(|((f, v), n)| format!("{n} × fastfile v{v} ({})", version_info(*f, *v).build))
            .collect::<Vec<_>>()
            .join(", ");
        let all_readable = !z.by_version.is_empty() && z.by_version.iter().all(|((f, v), _)| version_info(*f, *v).replay);
        let r = ui.label(egui::RichText::new(format!("Zones: {text}")).color(ui.visuals().hyperlink_color));
        r.on_hover_text(format!(
            "From the version field in each .ff header.{}{}",
            if z.unreadable > 0 { format!("
{} file(s) are not fastfiles.", z.unreadable) } else { String::new() },
            if all_readable { "
The model index can read these zones." } else { "
The model index reads CoDOL QA 3.3.3.3 (v423) and 3.19 (v461) zones; others are skipped." }
        ));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, state: Option<&RenderState>) {
        self.ctx = Some(ui.ctx().clone());
        self.poll();
        if let Some(rx) = &self.export_rx {
            if let Ok(m) = rx.try_recv() {
                self.export_message = Some(m);
                self.export_rx = None;
            }
        }
        self.refilter();
        if self.viewer.is_none() {
            self.viewer = state.map(Viewer::new);
            self.request_textures();
        }
        if !self.uploaded {
            if let Some(v) = &mut self.viewer {
                v.set_model(self.model.as_deref(), &self.surface_tex, &self.surface_states, &self.surface_env);
                self.uploaded = true;
            }
        }
        egui::Panel::left("model_list").resizable(true).default_size(320.0).show(ui, |ui| self.list(ui));
        egui::Panel::right("model_info").resizable(true).default_size(270.0).show(ui, |ui| self.info(ui));
        egui::CentralPanel::default().show(ui, |ui| self.view(ui));
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        let Some(cat) = self.catalog.clone() else {
            ui.add_space(8.0);
            ui.label("No `models` folder found. Choose the game folder (it contains main/models).");
            return;
        };
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("search models…").desired_width(f32::INFINITY));
        ui.weak(format!("{} of {} models", self.visible.len(), cat.entries.len()));
        let many = cat.providers.len() > 1;
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, 20.0, self.visible.len(), |ui, range| {
            for i in range {
                let ei = self.visible[i];
                let e = &cat.entries[ei];
                let text = if many { format!("{}   {} KiB · {}", e.name, e.size.div_ceil(1024), cat.providers[e.provider].label()) } else { format!("{}   {} KiB", e.name, e.size.div_ceil(1024)) };
                if ui.selectable_label(self.selected == Some(ei), text).clicked() {
                    clicked = Some(ei);
                }
            }
        });
        if let Some(ei) = clicked {
            self.select(ei);
        }
    }

    fn info(&mut self, ui: &mut egui::Ui) {
        if self.loading {
            ui.weak("loading…");
        }
        if let Some(e) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, e);
        }
        let Some(m) = self.model.clone() else {
            if !self.loading && self.error.is_none() {
                ui.weak("Select a model.");
            }
            return;
        };
        ui.horizontal(|ui| {
            ui.heading(&m.name);
            if ui.button("Copy name").clicked() {
                ui.ctx().copy_text(m.name.clone());
            }
        });
        ui.label(format!("{} surfaces · {} vertices · {} triangles", m.surfaces.len(), m.vertex_count(), m.triangle_count()));
        if let Some((lo, hi)) = m.bounds() {
            ui.weak(format!("size {:.2} × {:.2} × {:.2}", hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]));
        }
        let skinned = m.surfaces.iter().filter(|s| s.blend_counts.iter().any(|&c| c > 0)).count();
        if skinned > 0 {
            ui.weak(format!("{skinned} surface(s) carry bone weights; shown in the pose stored in the file."));
        }
        ui.separator();
        self.zone_line(ui);
        ui.horizontal_wrapped(|ui| {
            if self.index_jobs > 0 {
                let (d, t) = (self.index_progress.0.load(std::sync::atomic::Ordering::Relaxed), self.index_progress.1.load(std::sync::atomic::Ordering::Relaxed));
                if t > 0 {
                    ui.weak(format!("Reading zones {d} / {t} ..."));
                } else {
                    ui.weak(&self.index_status);
                }
            } else {
                ui.weak(&self.index_status);
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(self.index_jobs == 0 && self.root.is_some(), egui::Button::new("Build model index").small()).on_hover_text("Reads the game's zones to learn each model's materials and textures (about 15 seconds)").clicked() {
                self.build_index();
            }
            if ui.small_button("Load model index...").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Model index", &["tsv", "txt"]).pick_file() {
                    self.load_index(p);
                }
            }
        });
        self.export_ui(ui);
        ui.horizontal(|ui| {
            if ui.small_button("All").clicked() {
                self.surface_visible.iter_mut().for_each(|v| *v = true);
            }
            if ui.small_button("None").clicked() {
                self.surface_visible.iter_mut().for_each(|v| *v = false);
            }
        });
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, s) in m.surfaces.iter().enumerate() {
                ui.horizontal(|ui| {
                    let t = viewer::surface_tint(i);
                    let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                    ui.painter().rect_filled(r, 2.0, Color32::from_rgb(t[0], t[1], t[2]));
                    if let Some(v) = self.surface_visible.get_mut(i) {
                        ui.checkbox(v, format!("{i}: {} verts, {} tris", s.vertices.len(), s.indices.len() / 3));
                    }
                });
                if let Some(Some(st)) = self.surface_states.get(i) {
                    ui.weak(format!("    {}", st.describe()));
                }
                if let Some(t) = self.surface_tex.get(i) {
                    for (label, name) in [("colour", &t.color), ("normal", &t.normal), ("spec", &t.spec)] {
                        let Some(name) = name else { continue };
                        let state = self.tex_state.get(&name.to_ascii_lowercase());
                        let (mark, color) = match state {
                            Some(TexState::Ready) => ("OK", Color32::LIGHT_GREEN),
                            Some(TexState::Failed(_)) => ("missing", Color32::LIGHT_RED),
                            _ => ("...", Color32::GRAY),
                        };
                        let r = ui.colored_label(color, format!("    {label}: {name}  [{mark}]"));
                        if let Some(TexState::Failed(e)) = state {
                            r.on_hover_text(e);
                        }
                    }
                }
            }
        });
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("shading").selected_text(Shading::ALL.iter().find(|(s, _)| *s == self.shading).map_or("", |x| x.1)).show_ui(ui, |ui| {
                for (s, name) in Shading::ALL {
                    ui.selectable_value(&mut self.shading, s, name);
                }
            });
            egui::ComboBox::from_id_salt("cull").selected_text(Cull::ALL.iter().find(|(c, _)| *c == self.cull).map_or("", |x| x.1)).show_ui(ui, |ui| {
                for (c, name) in Cull::ALL {
                    ui.selectable_value(&mut self.cull, c, name);
                }
            });
            ui.checkbox(&mut self.wire, "Wireframe");
            ui.checkbox(&mut self.use_normal, "Normal maps");
            ui.checkbox(&mut self.use_spec, "Specular");
            if ui.button("Reset view").clicked() {
                if let Some((lo, hi)) = self.model.as_ref().and_then(|m| m.bounds()) {
                    self.camera.fit(lo, hi);
                }
            }
            ui.weak(self.camera_style.hint());
        });
        let (resp, size) = viewer::viewport_rect(ui);
        let rect = resp.rect;
        viewer::camera_input(ui, &resp, rect.height(), &mut self.camera, self.camera_style, 1.0);
        if let Some(v) = &mut self.viewer {
            v.set_features(self.use_normal, self.use_spec);
        }
        let tex = match &mut self.viewer {
            Some(v) if self.model.is_some() => v.render(size, &self.camera, self.shading, self.cull, self.wire, &self.surface_visible),
            _ => None,
        };
        match tex {
            Some(id) => {
                ui.painter().image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                ui.painter().rect_filled(rect, 0.0, Color32::from_gray(23));
                let msg = if self.viewer.is_none() { "The GPU renderer is not available" } else if self.loading { "loading…" } else { "Select a model on the left" };
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(16.0), Color32::GRAY);
            }
        }
    }
}
