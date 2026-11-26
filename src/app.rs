//! The image browser window (egui on wgpu, Vulkan by default).

use crate::iwi::{self, Format, Header, Level, MapType};
use crate::iwd;
use crate::iwdtool::IwdTool;
use crate::modeltab::ModelTab;
use crate::{convert, dds};
use crate::source::{self, Catalog, Progress, Provider};
use anyhow::Result;
use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2};
use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew, wgpu};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use crate::query::Query;
use crate::settings::{GridBackground, Settings};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};

mod datatab;
mod extras;
mod effecttab;
mod mapview;
mod materialtab;
mod soundtab;
mod workflow;

/// Opens the folder holding `path` in the file manager (selecting the file).
pub fn reveal_path(path: &std::path::Path) {
    workflow::reveal(path)
}
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

/// Edge of the thumbnails being made, in pixels (the "thumbnail size" setting).
static THUMB_PX: AtomicU32 = AtomicU32::new(128);

/// Opens the window. Vulkan is requested explicitly (it is the same API on
/// Windows and Linux/Proton); if no Vulkan driver is present we retry with
/// whatever wgpu considers its primary backends.
pub fn run(root: Option<PathBuf>) -> Result<()> {
    let first = launch(root.clone(), true);
    match first {
        Ok(()) => Ok(()),
        Err(e) if std::env::var_os("WGPU_BACKEND").is_none() => {
            eprintln!("Vulkan start-up failed ({e}); retrying with the default backends");
            launch(root, false)
        }
        Err(e) => Err(e),
    }
}

fn launch(root: Option<PathBuf>, vulkan_only: bool) -> Result<()> {
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    if vulkan_only && std::env::var_os("WGPU_BACKEND").is_none() {
        setup.instance_descriptor.backends = wgpu::Backends::VULKAN;
    }
    let mut viewport = egui::ViewportBuilder::default().with_inner_size([1360.0, 840.0]).with_title("Atlas - Call of Duty Asset Workbench");
    // the logo (assets/logo) as the window and taskbar icon
    if let Ok(img) = image::load_from_memory(include_bytes!("../assets/logo/icon-256.png")) {
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        viewport = viewport.with_icon(egui::IconData { rgba: rgba.into_raw(), width, height });
    }
    if crate::config::env_var_os("SHOT").is_some() {
        // Test capture: never steal focus or show up on the user's screen.
        viewport = viewport.with_active(false).with_position([-20000.0, -20000.0]).with_taskbar(false);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(setup), ..Default::default() },
        ..Default::default()
    };
    eframe::run_native(
        "Atlas",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, root)) as Box<dyn eframe::App>)),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}

// ---------------------------------------------------------------- thumbnails

struct Job {
    entry: usize,
    provider: Arc<dyn Provider>,
    key: String,
    header: Header,
}

#[derive(Default)]
struct QueueState {
    stack: Vec<Job>,
    inflight: HashSet<usize>,
    stop: bool,
}

/// Latest-first work queue; the UI replaces its contents every frame with what
/// is currently visible, so scrolling never leaves stale work behind.
struct JobQueue {
    state: Mutex<QueueState>,
    cv: Condvar,
}

impl JobQueue {
    fn replace(&self, jobs: Vec<Job>) {
        let mut s = self.state.lock().unwrap();
        s.stack = jobs.into_iter().filter(|j| !s.inflight.contains(&j.entry)).collect();
        self.cv.notify_all();
    }

    fn pop(&self) -> Option<Job> {
        let mut s = self.state.lock().unwrap();
        loop {
            if s.stop {
                return None;
            }
            if let Some(j) = s.stack.pop() {
                s.inflight.insert(j.entry);
                return Some(j);
            }
            s = self.cv.wait(s).unwrap();
        }
    }

    fn finish(&self, entry: usize) {
        self.state.lock().unwrap().inflight.remove(&entry);
    }

    fn stop(&self) {
        self.state.lock().unwrap().stop = true;
        self.cv.notify_all();
    }
}

struct ThumbResult {
    entry: usize,
    image: Option<ColorImage>,
}

/// Opaque preview of the first face; alpha-only images show their alpha as grey.
fn make_thumb(job: &Job) -> Result<ColorImage> {
    let data = job.provider.read(&job.key)?;
    let h = &job.header;
    let thumb = THUMB_PX.load(Ordering::Relaxed);
    let level = iwi::decode_level(&data, h, iwi::level_for_size(h, thumb))?;
    let mut px = level.faces[0].clone();
    let alpha_only = matches!(h.format(), Some(Format::Alpha | Format::WaveletAlpha));
    for p in px.chunks_exact_mut(4) {
        if alpha_only {
            let a = p[3];
            p[0] = a;
            p[1] = a;
            p[2] = a;
        }
        p[3] = 255;
    }
    let (w, hh) = (level.width, level.height);
    if w.max(hh) > thumb {
        let scale = thumb as f32 / w.max(hh) as f32;
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((hh as f32 * scale).round() as u32).max(1);
        let src = image::RgbaImage::from_raw(w, hh, px).ok_or_else(|| anyhow::anyhow!("bad buffer"))?;
        let small = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::Triangle);
        Ok(ColorImage::from_rgba_unmultiplied([nw as usize, nh as usize], small.as_raw()))
    } else {
        Ok(ColorImage::from_rgba_unmultiplied([w as usize, hh as usize], &px))
    }
}

fn spawn_workers(ctx: egui::Context, queue: Arc<JobQueue>, tx: Sender<ThumbResult>) {
    let n = std::thread::available_parallelism().map(|n| n.get().saturating_sub(1)).unwrap_or(2).clamp(2, 8);
    for _ in 0..n {
        let (ctx, queue, tx) = (ctx.clone(), Arc::clone(&queue), tx.clone());
        std::thread::spawn(move || {
            while let Some(job) = queue.pop() {
                let image = make_thumb(&job).ok();
                queue.finish(job.entry);
                if tx.send(ThumbResult { entry: job.entry, image }).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
    }
}

enum Thumb {
    Ready(TextureHandle),
    Failed,
}

// ------------------------------------------------------------------- filters

#[derive(Clone, PartialEq, Default)]
struct Filters {
    text: String,
    format: Option<Format>,
    map: Option<MapType>,
    provider: Option<usize>,
    alpha: Option<bool>,
    dupes: bool,
    favourites: bool,
}

fn map_name(m: MapType) -> &'static str {
    match m {
        MapType::Tex2D => "2D",
        MapType::Cube => "Cubemap",
        MapType::Volume => "Volume",
        MapType::Tex1D => "1D",
    }
}

// ------------------------------------------------------------------- preview

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Rgb,
    Rgba,
    R,
    G,
    B,
    A,
}

fn apply_mode(rgba: &[u8], mode: Mode) -> Vec<u8> {
    let mut out = rgba.to_vec();
    for p in out.chunks_exact_mut(4) {
        match mode {
            Mode::Rgb => p[3] = 255,
            Mode::Rgba => {}
            Mode::R | Mode::G | Mode::B | Mode::A => {
                let v = p[match mode {
                    Mode::R => 0,
                    Mode::G => 1,
                    Mode::B => 2,
                    _ => 3,
                }];
                p[0] = v;
                p[1] = v;
                p[2] = v;
                p[3] = 255;
            }
        }
    }
    out
}

struct Loaded {
    generation: u64,
    data: Option<Arc<Vec<u8>>>,
    level: Result<Level, String>,
}

struct Preview {
    generation: u64,
    entry: Option<usize>,
    variant: usize,
    data: Option<Arc<Vec<u8>>>,
    level_index: usize,
    face: usize,
    level: Option<Level>,
    error: Option<String>,
    loading: bool,
    mode: Mode,
    texture: Option<TextureHandle>,
    texture_key: (u64, usize, u8),
    zoom: Option<f32>,
    pan: Vec2,
    tx: Sender<Loaded>,
    rx: Receiver<Loaded>,
}

impl Preview {
    fn new() -> Self {
        let (tx, rx) = channel();
        Self {
            generation: 0,
            entry: None,
            variant: 0,
            data: None,
            level_index: 0,
            face: 0,
            level: None,
            error: None,
            loading: false,
            mode: Mode::Rgba,
            texture: None,
            texture_key: (u64::MAX, 0, 0),
            zoom: None,
            pan: Vec2::ZERO,
            tx,
            rx,
        }
    }
}

fn mode_id(m: Mode) -> u8 {
    m as u8
}

// ----------------------------------------------------------------------- app

struct ScanState {
    progress: Arc<Progress>,
    rx: Receiver<Catalog>,
}

/// Development aid: `ATLAS_SHOT=out.png` (optionally `ATLAS_SELECT=name`)
/// renders the window off-screen, saves one frame and exits.
struct Capture {
    path: PathBuf,
    select: Option<String>,
    started: std::time::Instant,
    ready_since: Option<std::time::Instant>,
    requested: bool,
    models_started: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Images,
    Models,
    Maps,
    Materials,
    Sounds,
    Effects,
    Data,
}

struct App {
    tab: Tab,
    models: ModelTab,
    wgpu_state: Option<eframe::egui_wgpu::RenderState>,
    capture: Option<Capture>,
    root: Option<PathBuf>,
    scan: Option<ScanState>,
    catalog: Option<Arc<Catalog>>,
    status: String,
    filters: Filters,
    applied: Option<Filters>,
    visible: Vec<usize>,
    formats_seen: Vec<Format>,
    selected: Option<usize>,
    thumbs: HashMap<usize, Thumb>,
    queue: Arc<JobQueue>,
    thumb_rx: Receiver<ThumbResult>,
    preview: Preview,
    ctx: egui::Context,
    convert: ConvertTool,
    iwd: IwdTool,
    /// IWI versions found in the catalogue: (version, files).
    versions: Vec<(u8, usize)>,
    settings: Settings,
    settings_open: bool,
    help_open: bool,
    /// Images selected with Ctrl/Shift-click (the plain selection is `selected`).
    multi: BTreeSet<usize>,
    /// Where a Shift-click range starts.
    anchor: Option<usize>,
    /// Columns of the grid as last drawn (for the arrow keys).
    last_cols: usize,
    /// Grid scroll state of the last frame: offset and viewport height.
    grid_view: (f32, f32),
    /// A row the keyboard moved to, to scroll into view.
    scroll_to: Option<usize>,
    games_open: bool,
    log_open: bool,
    game_cache: workflow::KnownGames,
    maps: workflow::MapsState,
    data: datatab::DataState,
    sounds: soundtab::SoundState,
    materials: materialtab::MaterialState,
    mapview: mapview::MapView,
    effects: effecttab::EffectState,
    /// `ATLAS_MAP=<zone file name>`: open that zone's map when the Maps tab has its list (for screenshots).
    pending_map: Option<String>,
    /// The folder the last batch export wrote to.
    batch_dir: Option<PathBuf>,
    focus_search: bool,
    /// `ATLAS_UI=demo`: select a few images once the catalogue is there (for screenshots).
    ui_demo: bool,
    save_search_name: String,
    batch_message: Option<String>,
    batch_rx: Option<Receiver<(String, Vec<(String, bool)>)>>,
    /// What the last exports did to each image, by name: written (true) or failed (false).
    image_marks: HashMap<String, bool>,
}

fn config_file() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("root.txt"))
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, root: Option<PathBuf>) -> Self {
        let (thumb_tx, thumb_rx) = channel();
        let queue = Arc::new(JobQueue { state: Mutex::new(QueueState::default()), cv: Condvar::new() });
        spawn_workers(cc.egui_ctx.clone(), Arc::clone(&queue), thumb_tx.clone());
        let saved = config_file().and_then(|f| std::fs::read_to_string(f).ok()).map(|s| PathBuf::from(s.trim()));
        let capture = crate::config::env_var_os("SHOT").map(|p| Capture {
            path: PathBuf::from(p),
            select: crate::config::env_var("SELECT").ok(),
            started: std::time::Instant::now(),
            ready_since: None,
            requested: false,
            models_started: false,
        });
        let mut app = Self {
            tab: if crate::config::env_var("TAB").is_ok_and(|t| t == "models") { Tab::Models } else { Tab::Images },
            models: ModelTab::new(),
            wgpu_state: cc.wgpu_render_state.clone(),
            capture,
            root: None,
            scan: None,
            catalog: None,
            status: "Choose the game folder (the one containing main, hires or an images folder).".into(),
            filters: Filters::default(),
            applied: None,
            visible: Vec::new(),
            formats_seen: Vec::new(),
            selected: None,
            thumbs: HashMap::new(),
            queue,
            thumb_rx,
            preview: Preview::new(),
            ctx: cc.egui_ctx.clone(),
            convert: ConvertTool::default(),
            iwd: IwdTool::default(),
            versions: Vec::new(),
            settings: Settings::default(),
            settings_open: false,
            help_open: false,
            multi: BTreeSet::new(),
            anchor: None,
            last_cols: 1,
            grid_view: (0.0, 600.0),
            scroll_to: None,
            games_open: false,
            log_open: false,
            game_cache: Default::default(),
            maps: Default::default(),
            data: Default::default(),
            sounds: Default::default(),
            materials: Default::default(),
            mapview: Default::default(),
            effects: Default::default(),
            pending_map: crate::config::env_var("MAP").ok(),
            batch_dir: None,
            focus_search: false,
            ui_demo: false,
            save_search_name: String::new(),
            batch_message: None,
            batch_rx: None,
            image_marks: HashMap::new(),
        };
        // settings: a test capture uses the defaults (and `ATLAS_UI` can ask for a look), the user's own run reads the file
        if app.capture.is_none() {
            if let Some(p) = crate::settings::config_path() {
                app.settings = Settings::load(&p);
            }
        }
        if let Ok(ui) = crate::config::env_var("UI") {
            for part in ui.split(',') {
                match part {
                    "light" => app.settings.dark = false,
                    "settings" => app.settings_open = true,
                    "help" => app.help_open = true,
                    "big" => app.settings.thumb = 192,
                    "checker" => app.settings.grid_bg = GridBackground::Checker,
                    "green" => app.settings.accent = crate::settings::ACCENTS[1].0,
                    "demo" => app.ui_demo = true,
                    "games" => app.games_open = true,
                    "log" => app.log_open = true,
                    "maps" => app.tab = Tab::Maps,
                    "data" => app.tab = Tab::Data,
                    "sounds" => app.tab = Tab::Sounds,
                    "effects" => app.tab = Tab::Effects,
                    "materials" => app.tab = Tab::Materials,
                    _ => {}
                }
            }
        }
        THUMB_PX.store(app.settings.thumb, Ordering::Relaxed);
        app.restyle();
        // Development aid: open the Pack IWD window with a folder already added (for screenshots).
        if let Some(dir) = crate::config::env_var_os("PACK").map(PathBuf::from) {
            if let Ok(e) = iwd::add_dir(&dir, &iwd::AddOptions::default()) {
                app.iwd.add(e);
            }
        }
        if let Some(r) = root.or(saved).filter(|p| p.is_dir()) {
            app.open_root(r);
        }
        app
    }

    fn open_root(&mut self, root: PathBuf) {
        crate::log::log(format!("opening {}", root.display()));
        self.settings.add_recent(&root);
        self.save_settings();
        if let Some(f) = config_file() {
            if let Some(dir) = f.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(f, root.to_string_lossy().as_bytes());
        }
        self.models.set_root(&root);
        self.models.set_images(None);
        let providers = source::discover(&root);
        self.thumbs.clear();
        self.selected = None;
        self.catalog = None;
        self.applied = None;
        self.visible.clear();
        self.preview = Preview::new();
        self.queue.replace(Vec::new());
        if providers.is_empty() {
            self.status = format!("No image folders or IWD archives found under {}", root.display());
            self.root = Some(root);
            self.scan = None;
            return;
        }
        let progress = Arc::new(Progress::default());
        let (tx, rx) = channel();
        let p = Arc::clone(&progress);
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let cat = source::scan(providers, &p);
            let _ = tx.send(cat);
            ctx.request_repaint();
        });
        self.status = format!("Scanning {} …", root.display());
        self.root = Some(root);
        self.scan = Some(ScanState { progress, rx });
    }

    fn poll_scan(&mut self) {
        let Some(scan) = &self.scan else { return };
        match scan.rx.try_recv() {
            Ok(cat) => {
                let mut seen: Vec<Format> = cat.entries.iter().flat_map(|e| &e.variants).filter_map(|v| v.header.as_ref()?.format()).collect();
                seen.sort();
                seen.dedup();
                self.formats_seen = seen;
                let variants: usize = cat.entries.iter().map(|e| e.variants.len()).sum();
                self.status = format!("{} images ({} files) from {} sources", cat.entries.len(), variants, cat.providers.len());
                crate::log::log(&self.status);
                self.versions = cat.versions();
                let cat = Arc::new(cat);
                self.models.set_images(Some(Arc::clone(&cat)));
                self.catalog = Some(cat);
                self.scan = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                self.ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Err(_) => self.scan = None,
        }
    }

    fn poll_thumbs(&mut self) {
        for _ in 0..24 {
            let Ok(r) = self.thumb_rx.try_recv() else { break };
            let t = match r.image {
                Some(img) => Thumb::Ready(self.ctx.load_texture(
                    format!("thumb{}", r.entry),
                    img,
                    TextureOptions { magnification: egui::TextureFilter::Nearest, minification: egui::TextureFilter::Linear, ..Default::default() },
                )),
                None => Thumb::Failed,
            };
            self.thumbs.insert(r.entry, t);
        }
    }

    fn refilter(&mut self) {
        let Some(cat) = self.catalog.clone() else { return };
        if self.applied.as_ref() == Some(&self.filters) {
            return;
        }
        let f = &self.filters;
        let q = Query::parse(&f.text, &["width", "height", "mips", "size", "depth"]);
        self.visible = cat
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let size = e.variants.iter().map(|v| v.size).max().unwrap_or(0) as f64;
                let prop = |p: &str| -> Option<f64> {
                    let h = e.header()?;
                    match p {
                        "width" => Some(h.width as f64),
                        "height" => Some(h.height as f64),
                        "depth" => Some(h.depth as f64),
                        "mips" => Some(h.mip_count() as f64),
                        "size" => Some(size),
                        _ => None,
                    }
                };
                if !q.matches(&e.name, prop) {
                    return false;
                }
                if f.dupes && e.variants.len() < 2 {
                    return false;
                }
                if f.favourites && !self.settings.is_favourite(&e.name) {
                    return false;
                }
                if let Some(p) = f.provider {
                    if !e.variants.iter().any(|v| v.provider == p) {
                        return false;
                    }
                }
                let h = e.header();
                if let Some(fm) = f.format {
                    if h.and_then(|h| h.format()) != Some(fm) {
                        return false;
                    }
                }
                if let Some(m) = f.map {
                    if h.map(|h| h.map_type()) != Some(m) {
                        return false;
                    }
                }
                if let Some(a) = f.alpha {
                    if h.and_then(|h| h.format()).is_none_or(|fm| fm.has_alpha() != a) {
                        return false;
                    }
                }
                true
            })
            .map(|(i, _)| i)
            .collect();
        self.applied = Some(f.clone());
    }

    // ---- selection / preview ------------------------------------------------

    fn select(&mut self, entry: usize) {
        self.selected = Some(entry);
        self.preview.entry = Some(entry);
        self.preview.variant = 0;
        self.preview.data = None;
        self.preview.level_index = 0;
        self.preview.face = 0;
        self.preview.zoom = None;
        self.preview.pan = Vec2::ZERO;
        self.request_level();
    }

    /// (Re)decodes the preview's current level on a worker thread.
    fn request_level(&mut self) {
        let Some(cat) = self.catalog.clone() else { return };
        let Some(ei) = self.preview.entry else { return };
        let Some(v) = cat.entries[ei].variants.get(self.preview.variant) else { return };
        let Some(header) = v.header.clone() else {
            self.preview.error = v.error.clone().or(Some("unreadable header".into()));
            self.preview.level = None;
            self.preview.loading = false;
            return;
        };
        self.preview.generation += 1;
        self.preview.loading = true;
        self.preview.error = None;
        let generation = self.preview.generation;
        let (provider, key) = (Arc::clone(&cat.providers[v.provider]), v.key.clone());
        let cached = self.preview.data.clone();
        let level_index = self.preview.level_index.min(header.mip_count() - 1);
        let (tx, ctx) = (self.preview.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let data = match cached {
                Some(d) => Ok(d),
                None => provider.read(&key).map(Arc::new).map_err(|e| format!("{e:#}")),
            };
            let msg = match data {
                Ok(d) => {
                    let level = iwi::decode_level(&d, &header, level_index).map_err(|e| format!("{e:#}"));
                    Loaded { generation, data: Some(d), level }
                }
                Err(e) => Loaded { generation, data: None, level: Err(e) },
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    fn poll_preview(&mut self) {
        while let Ok(m) = self.preview.rx.try_recv() {
            if m.generation != self.preview.generation {
                continue;
            }
            self.preview.loading = false;
            if let Some(d) = m.data {
                self.preview.data = Some(d);
            }
            match m.level {
                Ok(l) => {
                    self.preview.face = self.preview.face.min(l.faces.len() - 1);
                    self.preview.level = Some(l);
                    self.preview.error = None;
                }
                Err(e) => {
                    self.preview.level = None;
                    self.preview.error = Some(e);
                }
            }
            self.preview.texture_key = (u64::MAX, 0, 0);
        }
    }

    fn sync_texture(&mut self) {
        let p = &mut self.preview;
        let key = (p.generation, p.face, mode_id(p.mode));
        if key == p.texture_key {
            return;
        }
        p.texture_key = key;
        let Some(l) = &p.level else {
            p.texture = None;
            return;
        };
        let px = apply_mode(&l.faces[p.face], p.mode);
        let img = ColorImage::from_rgba_unmultiplied([l.width as usize, l.height as usize], &px);
        let opts = TextureOptions { magnification: egui::TextureFilter::Nearest, minification: egui::TextureFilter::Linear, ..Default::default() };
        match &mut p.texture {
            Some(t) => t.set(img, opts),
            None => p.texture = Some(self.ctx.load_texture("preview", img, opts)),
        }
    }

    // ---- UI ------------------------------------------------------------------

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.tab, Tab::Images, "Images");
            ui.selectable_value(&mut self.tab, Tab::Models, "Models");
            ui.selectable_value(&mut self.tab, Tab::Maps, "Maps");
            ui.selectable_value(&mut self.tab, Tab::Materials, "Materials");
            ui.selectable_value(&mut self.tab, Tab::Sounds, "Sounds");
            ui.selectable_value(&mut self.tab, Tab::Effects, "Effects");
            ui.selectable_value(&mut self.tab, Tab::Data, "Data");
            ui.separator();
            if ui.button("Open game folder…").clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_title("Game folder").pick_folder() {
                    self.open_root(dir);
                }
            }
            if ui.add_enabled(self.root.is_some(), egui::Button::new("Rescan")).clicked() {
                if let Some(r) = self.root.clone() {
                    self.open_root(r);
                }
            }
            if ui.button("DDS to IWI…").clicked() {
                self.convert.open = true;
            }
            if ui.button("Pack IWD…").clicked() {
                self.iwd.open = true;
            }
            self.recent_ui(ui);
            if ui.button("Settings").on_hover_text("Ctrl+,").clicked() {
                self.settings_open = true;
            }
            if ui.button("?").on_hover_text("Keyboard shortcuts (F1)").clicked() {
                self.help_open = true;
            }
        });
        if self.tab != Tab::Images {
            return;
        }
        let Some(cat) = self.catalog.clone() else { return };
        ui.horizontal_wrapped(|ui| {
            let search = ui.add(egui::TextEdit::singleline(&mut self.filters.text).id_salt("search_box").hint_text("search names…  (Ctrl+F)").desired_width(220.0));
            if self.focus_search {
                search.request_focus();
                self.focus_search = false;
            }
            egui::ComboBox::from_id_salt("fmt")
                .selected_text(self.filters.format.map_or("Any format", |f| f.name()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.format, None, "Any format");
                    for f in self.formats_seen.clone() {
                        ui.selectable_value(&mut self.filters.format, Some(f), f.name());
                    }
                });
            egui::ComboBox::from_id_salt("map")
                .selected_text(self.filters.map.map_or("Any type", map_name))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.map, None, "Any type");
                    for m in [MapType::Tex2D, MapType::Cube] {
                        ui.selectable_value(&mut self.filters.map, Some(m), map_name(m));
                    }
                });
            egui::ComboBox::from_id_salt("alpha")
                .selected_text(match self.filters.alpha {
                    None => "Alpha: any",
                    Some(true) => "Has alpha",
                    Some(false) => "No alpha",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.alpha, None, "Alpha: any");
                    ui.selectable_value(&mut self.filters.alpha, Some(true), "Has alpha");
                    ui.selectable_value(&mut self.filters.alpha, Some(false), "No alpha");
                });
            egui::ComboBox::from_id_salt("src")
                .selected_text(self.filters.provider.map_or("All sources".to_string(), |p| cat.providers[p].label().to_string()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filters.provider, None, "All sources");
                    for (i, p) in cat.providers.iter().enumerate() {
                        ui.selectable_value(&mut self.filters.provider, Some(i), p.label());
                    }
                });
            ui.checkbox(&mut self.filters.dupes, "Duplicates only");
            ui.checkbox(&mut self.filters.favourites, "Favourites only");
            self.saved_searches_ui(ui);
            if ui.button("Add shown to IWD").on_hover_text("Adds every image in the list (the largest version of each) to the Pack IWD window").clicked() {
                let entries: Vec<iwd::Entry> = self.visible.iter().filter_map(|&i| self.pack_entry(&cat, i, None)).collect();
                self.iwd.add(entries);
            }
        });
    }

    /// The bar at the bottom: what the current tab last reported, scan progress, counts, and where things are.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // right side first: buttons, the engine, the folder
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Log").on_hover_text("What the long jobs report").clicked() {
                    self.log_open = true;
                }
                if ui.small_button("Games").on_hover_text("The folders you have opened and whether each is ready").clicked() {
                    self.games_open = true;
                }
                if let Some(r) = &self.root {
                    ui.weak(r.display().to_string());
                }
            // which game the assets are for, from the IWI version bytes
            if let Some((v, n)) = self.versions.first().copied() {
                let e = iwi::engine(v);
                let mixed = self.versions.len() > 1;
                let text = if mixed {
                    self.versions.iter().map(|(v, n)| format!("{} ×{n}", iwi::engine(*v).label(*v))).collect::<Vec<_>>().join(", ")
                } else {
                    e.label(v)
                };
                let r = ui.label(egui::RichText::new(format!("Assets: {text}")).strong().color(ui.visuals().hyperlink_color));
                r.on_hover_text(format!(
                    "Engine: {}
From the IWI version byte in {} file(s).{}",
                    e.engine,
                    n,
                    if e.checked { "" } else { "
This version's layout is not yet checked against real files here." }
                ));
            }
                // the rest, from the left
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if let Some(s) = &self.scan {
                        let (d, t) = (s.progress.done.load(Ordering::Relaxed), s.progress.total.load(Ordering::Relaxed));
                        ui.spinner();
                        if t > 0 {
                            ui.add(egui::ProgressBar::new(d as f32 / t as f32).desired_width(160.0).text(format!("{d} / {t}")));
                        }
                    }
                    let message: Option<String> = match self.tab {
                        Tab::Images => self.batch_message.clone().or_else(|| Some(self.status.clone())),
                        Tab::Maps => self.maps.message.clone(),
                        Tab::Materials => self.materials.message.clone(),
                        Tab::Sounds => self.sounds.message.clone(),
                        Tab::Effects => self.effects.message.clone(),
                        Tab::Data => self.data.message.clone(),
                        Tab::Models => None,
                    };
                    if let Some(m) = message.filter(|m| !m.is_empty()) {
                        ui.label(m);
                    }
                    if self.tab == Tab::Images {
                        if let Some(cat) = &self.catalog {
                            ui.weak(format!("· {} of {} shown", self.visible.len(), cat.entries.len()));
                        }
                    }
                });
            });
        });
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        let Some(cat) = self.catalog.clone() else {
            ui.centered_and_justified(|ui| {
                ui.label(&self.status);
            });
            return;
        };
        self.batch_bar(ui, &cat);
        let thumb = self.settings.thumb as f32;
        let (cell_w, cell_h) = (thumb + 12.0, thumb + 34.0);
        let cols = ((ui.available_width() / cell_w).floor() as usize).max(1);
        self.last_cols = cols;
        let rows = self.visible.len().div_ceil(cols);
        let mut want: Vec<Job> = Vec::new();
        let mut clicked: Option<(usize, egui::Modifiers)> = None;
        let mut star_clicked = None;
        let accent = ui.visuals().hyperlink_color;
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if let Some(row) = self.scroll_to.take() {
            // keep the keyboard's row in view without jumping
            let (offset, height) = self.grid_view;
            let (top, bottom) = (row as f32 * cell_h, (row + 1) as f32 * cell_h);
            if top < offset {
                area = area.vertical_scroll_offset(top);
            } else if bottom > offset + height {
                area = area.vertical_scroll_offset(bottom - height);
            }
        }
        let out = area.show_rows(ui, cell_h, rows, |ui, range| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for row in range {
                ui.horizontal(|ui| {
                    for col in 0..cols {
                        let Some(&ei) = self.visible.get(row * cols + col) else { break };
                        let entry = &cat.entries[ei];
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(cell_w, cell_h), Sense::click());
                        let painter = ui.painter_at(rect);
                        let multi = self.multi.contains(&ei);
                        if multi || self.selected == Some(ei) {
                            painter.rect_filled(rect.shrink(2.0), 4.0, ui.visuals().selection.bg_fill);
                            if multi {
                                painter.rect_stroke(rect.shrink(2.0), 4.0, egui::Stroke::new(1.5, accent), egui::StrokeKind::Inside);
                            }
                        } else if resp.hovered() {
                            painter.rect_filled(rect.shrink(2.0), 4.0, ui.visuals().widgets.hovered.bg_fill);
                        }
                        let img_rect = Rect::from_min_size(rect.min + Vec2::new(6.0, 6.0), Vec2::splat(thumb));
                        match self.settings.grid_bg {
                            GridBackground::Dark => {
                                painter.rect_filled(img_rect, 2.0, Color32::from_gray(28));
                            }
                            GridBackground::Light => {
                                painter.rect_filled(img_rect, 2.0, Color32::from_gray(205));
                            }
                            GridBackground::Checker => {
                                painter.rect_filled(img_rect, 2.0, Color32::from_gray(70));
                                let step = 10.0;
                                let n = (thumb / step).ceil() as i32;
                                for y in 0..n {
                                    for x in 0..n {
                                        if (x + y) % 2 == 0 {
                                            let cell = Rect::from_min_size(img_rect.min + Vec2::new(x as f32 * step, y as f32 * step), Vec2::splat(step)).intersect(img_rect);
                                            painter.rect_filled(cell, 0.0, Color32::from_gray(95));
                                        }
                                    }
                                }
                            }
                        }
                        match self.thumbs.get(&ei) {
                            Some(Thumb::Ready(t)) => {
                                let s = t.size_vec2();
                                // never drawn larger than it was made: small images stay small and sharp
                                let fit = Rect::from_center_size(img_rect.center(), s.min(Vec2::splat(thumb)));
                                painter.image(t.id(), fit, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
                            }
                            Some(Thumb::Failed) => {
                                painter.text(img_rect.center(), egui::Align2::CENTER_CENTER, "?", egui::FontId::proportional(28.0), Color32::from_gray(120));
                            }
                            None => {
                                if let Some(v) = entry.variants.iter().find(|v| v.header.is_some()) {
                                    want.push(Job {
                                        entry: ei,
                                        provider: Arc::clone(&cat.providers[v.provider]),
                                        key: v.key.clone(),
                                        header: v.header.clone().unwrap(),
                                    });
                                } else {
                                    painter.text(img_rect.center(), egui::Align2::CENTER_CENTER, "✕", egui::FontId::proportional(28.0), Color32::from_gray(120));
                                }
                            }
                        }
                        if let Some(&ok) = self.image_marks.get(&entry.name) {
                            let at = Pos2::new(img_rect.min.x + 9.0, img_rect.max.y - 9.0);
                            painter.circle_filled(at, 5.0, Color32::from_black_alpha(150));
                            painter.circle_filled(at, 3.5, if ok { Color32::from_rgb(90, 200, 120) } else { Color32::from_rgb(230, 90, 90) });
                            if resp.hovered() && ui.input(|i| i.pointer.hover_pos()).is_some_and(|p| p.distance(at) < 8.0) {
                                egui::Tooltip::always_open(ui.ctx().clone(), ui.layer_id(), resp.id.with("mark"), egui::PopupAnchor::Pointer).show(|ui| {
                                    ui.label(if ok { "Exported" } else { "Export failed" });
                                });
                            }
                        }
                        let label = if entry.variants.len() > 1 { format!("{} ×{}", entry.name, entry.variants.len()) } else { entry.name.clone() };
                        let galley = painter.layout(label, egui::FontId::proportional(11.5), ui.visuals().text_color(), cell_w - 8.0);
                        painter.galley(Pos2::new(rect.min.x + 5.0, img_rect.max.y + 4.0), galley, ui.visuals().text_color());
                        // favourite star, top right of the picture (always shown for favourites, on hover otherwise)
                        let fav = self.settings.is_favourite(&entry.name);
                        let star = Rect::from_min_size(Pos2::new(img_rect.max.x - 22.0, img_rect.min.y + 2.0), Vec2::splat(20.0));
                        if fav || resp.hovered() {
                            let colour = if fav { Color32::from_rgb(255, 205, 60) } else { Color32::from_gray(150) };
                            painter.circle_filled(star.center(), 10.0, Color32::from_black_alpha(120));
                            extras::draw_star(&painter, star.center(), 7.5, colour, fav);
                        }
                        let star_resp = ui.interact(star, resp.id.with("star"), Sense::click());
                        if star_resp.clicked() {
                            star_clicked = Some(ei);
                        } else if resp.clicked() {
                            clicked = Some((ei, ui.input(|i| i.modifiers)));
                        }
                    }
                });
            }
        });
        self.grid_view = (out.state.clamped_offset().y, out.inner_rect.height());
        // Last pushed = first popped: queue the top-left items last so they load first.
        want.reverse();
        self.queue.replace(want);
        if let Some((ei, m)) = clicked {
            self.click_entry(ei, m);
        }
        if let Some(ei) = star_clicked {
            let name = cat.entries[ei].name.clone();
            self.settings.toggle_favourite(&name);
            self.after_favourites_changed();
        }
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let Some(cat) = self.catalog.clone() else { return };
        let Some(ei) = self.selected else {
            ui.weak("Select an image.");
            return;
        };
        let entry = &cat.entries[ei];
        ui.horizontal(|ui| {
            ui.heading(&entry.name);
            if ui.button("Copy name").clicked() {
                ui.ctx().copy_text(entry.name.clone());
            }
            let fav = self.settings.is_favourite(&entry.name);
            if ui.button(if fav { "Unfavourite" } else { "Favourite" }).on_hover_text("F").clicked() {
                self.settings.toggle_favourite(&entry.name);
                self.after_favourites_changed();
            }
            if ui.button("Add to IWD").on_hover_text("Adds the shown version to the Pack IWD window").clicked() {
                let entries: Vec<iwd::Entry> = self.pack_entry(&cat, ei, Some(self.preview.variant)).into_iter().collect();
                self.iwd.add(entries);
            }
        });

        if entry.variants.len() > 1 {
            ui.label(format!("{} versions", entry.variants.len()));
            let mut pick = self.preview.variant;
            for (i, v) in entry.variants.iter().enumerate() {
                let dims = v.header.as_ref().map_or("?".to_string(), |h| format!("{}×{}", h.width, h.height));
                let text = format!("{}  ·  {}  ·  {} KiB", cat.providers[v.provider].label(), dims, v.size / 1024);
                ui.radio_value(&mut pick, i, text);
            }
            if pick != self.preview.variant {
                self.preview.variant = pick;
                self.preview.data = None;
                self.preview.level_index = 0;
                self.preview.zoom = None;
                self.request_level();
            }
        } else if let Some(v) = entry.variants.first() {
            ui.weak(format!("{}  ·  {} KiB", cat.providers[v.provider].label(), v.size / 1024));
        }

        let variant = entry.variants.get(self.preview.variant);
        if let Some(h) = variant.and_then(|v| v.header.as_ref()) {
            egui::Grid::new("info").num_columns(2).show(ui, |ui| {
                ui.weak("Format");
                ui.label(h.format().map_or(format!("unknown ({})", h.format), |f| f.name().to_string()));
                ui.end_row();
                ui.weak("Size");
                ui.label(format!("{} × {}{}", h.width, h.height, if h.depth > 1 { format!(" × {}", h.depth) } else { String::new() }));
                ui.end_row();
                ui.weak("Type");
                ui.label(map_name(h.map_type()));
                ui.end_row();
                ui.weak("Mip levels");
                ui.label(h.mip_count().to_string());
                ui.end_row();
                ui.weak("Flags");
                ui.label(h.flag_names().join(", "));
                ui.end_row();
                ui.weak("Game");
                let e = iwi::engine(h.version);
                ui.label(e.label(h.version)).on_hover_text(e.engine);
                ui.end_row();
            });
        }

        ui.separator();
        let header = variant.and_then(|v| v.header.clone());
        if let Some(h) = &header {
            ui.horizontal_wrapped(|ui| {
                let before = self.preview.mode;
                for (m, name) in [(Mode::Rgb, "RGB"), (Mode::Rgba, "RGBA"), (Mode::R, "R"), (Mode::G, "G"), (Mode::B, "B"), (Mode::A, "A")] {
                    ui.selectable_value(&mut self.preview.mode, m, name);
                }
                let _ = before;
            });
            ui.horizontal_wrapped(|ui| {
                let n = h.mip_count();
                if n > 1 {
                    let mut l = self.preview.level_index;
                    let (w, hh) = h.mip_dims(l);
                    ui.label(format!("Mip {l} ({w}×{hh})"));
                    if ui.add(egui::Slider::new(&mut l, 0..=n - 1).show_value(false)).changed() {
                        self.preview.level_index = l;
                        self.request_level();
                    }
                }
                if h.face_count() > 1 {
                    ui.label("Face");
                    let mut f = self.preview.face;
                    for i in 0..h.face_count() {
                        ui.selectable_value(&mut f, i, i.to_string());
                    }
                    if f != self.preview.face {
                        self.preview.face = f;
                    }
                }
                if ui.button("Fit").clicked() {
                    self.preview.zoom = None;
                    self.preview.pan = Vec2::ZERO;
                }
                if ui.button("1:1").clicked() {
                    self.preview.zoom = Some(1.0);
                    self.preview.pan = Vec2::ZERO;
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.add_enabled(self.preview.level.is_some(), egui::Button::new(format!("Export {}…", self.settings.image_format.label()))).clicked() {
                    self.export_png(&entry.name, false);
                }
                if h.face_count() > 1 && ui.add_enabled(self.preview.level.is_some(), egui::Button::new("Export 6 faces…")).clicked() {
                    self.export_png(&entry.name, true);
                }
                if ui.add_enabled(self.preview.data.is_some(), egui::Button::new("Export DDS…")).clicked() {
                    if let Some(d) = self.preview.data.clone() {
                        let result = convert::iwi_to_dds(&d).and_then(|dds| {
                            if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{}.dds", entry.name)).add_filter("DDS", &["dds"]).save_file() {
                                std::fs::write(path, dds)?;
                            }
                            Ok(())
                        });
                        if let Err(e) = result {
                            self.status = format!("DDS export failed: {e:#}");
                        }
                    }
                }
                if ui.add_enabled(self.preview.data.is_some(), egui::Button::new("Save IWI copy…")).clicked() {
                    if let (Some(d), Some(path)) =
                        (self.preview.data.clone(), rfd::FileDialog::new().set_file_name(format!("{}.iwi", entry.name)).add_filter("IWI", &["iwi"]).save_file())
                    {
                        if let Err(e) = std::fs::write(&path, d.as_slice()) {
                            self.status = format!("Saving failed: {e}");
                        }
                    }
                }
            });
        }
        self.canvas(ui);
    }

    /// An IWD entry for catalogue image `ei`: the given version, or the one with the most pixels.
    fn pack_entry(&self, cat: &Catalog, ei: usize, variant: Option<usize>) -> Option<iwd::Entry> {
        let entry = &cat.entries[ei];
        let v = match variant {
            Some(i) => entry.variants.get(i)?,
            None => entry.variants.iter().filter(|v| v.header.is_some()).max_by_key(|v| v.header.as_ref().map_or(0, |h| h.width as u64 * h.height as u64))?,
        };
        Some(iwd::from_provider(Arc::clone(&cat.providers[v.provider]), v.key.clone(), &entry.name, v.size))
    }

    fn export_png(&mut self, name: &str, all_faces: bool) {
        let Some(l) = &self.preview.level else { return };
        let (format, fix) = (self.settings.image_format, self.settings.normal_fix);
        let normal = self.catalog.as_ref().and_then(|c| self.selected.map(|i| c.entries[i].header())).flatten().map_or(false, |h| crate::imgexport::looks_like_normal_map(name, h.is_normal_map()))
            || crate::imgexport::looks_like_normal_map(name, false);
        let ext = format.extension();
        let result: Result<()> = (|| {
            if all_faces {
                let Some(dir) = rfd::FileDialog::new().set_title("Folder for the six faces").pick_folder() else { return Ok(()) };
                for i in 0..l.faces.len() {
                    crate::imgexport::write_face(&dir.join(format!("{name}_face{i}.{ext}")), l, i, format, normal, fix)?;
                }
            } else if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{name}.{ext}")).add_filter(format.label(), &[ext]).save_file() {
                crate::imgexport::write_face(&path, l, self.preview.face, format, normal, fix)?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.status = format!("Export failed: {e}");
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, Color32::from_gray(20));
        if self.preview.loading && self.preview.level.is_none() {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "loading…", egui::FontId::proportional(16.0), Color32::GRAY);
            return;
        }
        if let Some(e) = &self.preview.error {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, e, egui::FontId::proportional(14.0), Color32::LIGHT_RED);
            return;
        }
        let (Some(tex), Some(level)) = (&self.preview.texture, &self.preview.level) else { return };
        let native = Vec2::new(level.width as f32, level.height as f32);
        let fit = (rect.width() / native.x).min(rect.height() / native.y).min(8.0);
        let mut zoom = self.preview.zoom.unwrap_or(fit);

        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let old = zoom;
                zoom = (zoom * (scroll * 0.0025).exp()).clamp(0.05, 64.0);
                if let Some(p) = resp.hover_pos() {
                    // Keep the point under the cursor fixed.
                    let c = rect.center() + self.preview.pan;
                    let rel = (p - c) / old;
                    self.preview.pan += rel * (old - zoom);
                }
                self.preview.zoom = Some(zoom);
            }
        }
        if resp.dragged() {
            self.preview.pan += resp.drag_delta();
        }
        let size = native * zoom;
        let img = Rect::from_center_size(rect.center() + self.preview.pan, size);
        let vis = img.intersect(rect);
        if vis.is_positive() {
            let tile = 12.0;
            let (a, b) = (Color32::from_gray(52), Color32::from_gray(76));
            painter.rect_filled(vis, 0.0, a);
            let x0 = ((vis.min.x - img.min.x) / tile).floor() as i32;
            let y0 = ((vis.min.y - img.min.y) / tile).floor() as i32;
            let x1 = ((vis.max.x - img.min.x) / tile).ceil() as i32;
            let y1 = ((vis.max.y - img.min.y) / tile).ceil() as i32;
            for ty in y0..y1 {
                for tx in x0..x1 {
                    if (tx + ty) & 1 == 1 {
                        let r = Rect::from_min_size(img.min + Vec2::new(tx as f32, ty as f32) * tile, Vec2::splat(tile)).intersect(vis);
                        if r.is_positive() {
                            painter.rect_filled(r, 0.0, b);
                        }
                    }
                }
            }
            painter.image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        painter.text(
            rect.left_bottom() + Vec2::new(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{}×{}  {:.0}%", level.width, level.height, zoom * 100.0),
            egui::FontId::monospace(12.0),
            Color32::from_gray(180),
        );
    }
}

impl App {
    fn drive_capture(&mut self, ctx: &egui::Context) {
        let Some(c) = &mut self.capture else { return };
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        if c.started.elapsed().as_secs() > 60 {
            eprintln!("capture timed out");
            std::process::exit(2);
        }
        let models_tab = self.tab == Tab::Models;
        if models_tab && !c.models_started {
            c.models_started = true;
            if let Ok(o) = crate::config::env_var("VIEW") {
                self.models.apply_options(&o);
            }
            if crate::config::env_var_os("BUILD_INDEX").is_some() {
                self.models.build_index();
            }
            if let Some(n) = c.select.clone() {
                self.models.select_by_name(&n);
            }
            return;
        }
        if !models_tab {
            let Some(cat) = self.catalog.clone() else { return };
            if let (Some(name), None) = (c.select.clone(), self.selected) {
                if let Some(i) = cat.entries.iter().position(|e| e.name == name) {
                    self.select(i);
                }
                return;
            }
        }
        let c = self.capture.as_mut().unwrap();
        let busy = if models_tab {
            self.models.is_busy() || self.scan.is_some() || !self.models.has_model()
        } else if self.tab == Tab::Maps {
            self.scan.is_some() || self.mapview.is_busy()
        } else if self.tab == Tab::Data {
            self.data.rx.is_some()
        } else if self.tab == Tab::Sounds {
            self.sounds.is_busy()
        } else if self.tab == Tab::Effects {
            self.effects.is_busy() || self.scan.is_some()
        } else {
            self.preview.loading || self.queue.state.lock().unwrap().inflight.len() > 0 || self.thumbs.len() < self.visible.len().min(12)
        };
        if busy {
            c.ready_since = None;
            return;
        }
        let since = *c.ready_since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed().as_millis() > 800 && !c.requested {
            c.requested = true;
            let (path, ctx2) = (c.path.clone(), ctx.clone());
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let res = image::save_buffer(&path, img.as_raw(), w as u32, h as u32, image::ColorType::Rgba8);
                eprintln!("capture: {}x{} -> {} ({:?})", w, h, path.display(), res.is_ok());
                ctx2.send_viewport_cmd(egui::ViewportCommand::Close);
            });
        }
    }
}

/// The "DDS to IWI" tool window.
struct ConvertTool {
    open: bool,
    dds: Option<PathBuf>,
    info: String,
    usage: usize,
    flags: String,
    genmips: bool,
    message: Option<(bool, String)>,
}

impl Default for ConvertTool {
    fn default() -> Self {
        Self { open: false, dds: None, info: String::new(), usage: 0, flags: String::new(), genmips: false, message: None }
    }
}

const USAGES: [(&str, &str); 5] = [
    ("color", "Colour / diffuse (0x341)"),
    ("normal", "Normal map (0x40001)"),
    ("ui", "UI, no mipmaps, clamped (0x173)"),
    ("clamp", "Clamped, with mipmaps (0x171)"),
    ("raw", "No flags (0x0)"),
];

impl ConvertTool {
    fn show(&mut self, ctx: &egui::Context) {
        let mut open = self.open;
        egui::Window::new("DDS to IWI").open(&mut open).default_width(420.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Choose DDS…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("DDS", &["dds"]).pick_file() {
                        self.info = match std::fs::read(&p).map_err(anyhow::Error::from).and_then(|b| dds::parse(&b)) {
                            Ok(d) => d.describe(),
                            Err(e) => format!("{e:#}"),
                        };
                        self.dds = Some(p);
                        self.message = None;
                    }
                }
                match &self.dds {
                    Some(p) => ui.label(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                    None => ui.weak("no file chosen"),
                };
            });
            if !self.info.is_empty() {
                ui.weak(&self.info);
            }
            egui::ComboBox::from_label("Flags preset").selected_text(USAGES[self.usage].1).show_ui(ui, |ui| {
                for (i, (_, label)) in USAGES.iter().enumerate() {
                    ui.selectable_value(&mut self.usage, i, *label);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Custom flags (hex)");
                ui.add(egui::TextEdit::singleline(&mut self.flags).hint_text("overrides the preset").desired_width(120.0));
            });
            ui.checkbox(&mut self.genmips, "Generate mipmaps when the DDS has only one level");
            ui.weak("Level 0 is kept as is; lower levels are filtered and re-encoded. Without this, a single-level DDS gets no mipmaps.");
            if ui.add_enabled(self.dds.is_some(), egui::Button::new("Convert and save…")).clicked() {
                self.message = match self.convert() {
                    Ok(m) => m.map(|m| (true, m)),
                    Err(e) => Some((false, format!("{e:#}"))),
                };
            }
            if let Some((ok, m)) = &self.message {
                ui.colored_label(if *ok { Color32::LIGHT_GREEN } else { Color32::LIGHT_RED }, m);
            }
        });
        self.open = open;
    }

    fn convert(&self) -> Result<Option<String>> {
        let Some(path) = &self.dds else { return Ok(None) };
        let opts = convert::Options {
            usage: USAGES[self.usage].0.to_string(),
            flags: if self.flags.trim().is_empty() { None } else { Some(convert::parse_flags(&self.flags)?) },
            genmips: self.genmips,
        };
        let (bytes, summary) = convert::dds_to_iwi(&std::fs::read(path)?, &opts)?;
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "image".into());
        let mut dialog = rfd::FileDialog::new().set_file_name(format!("{stem}.iwi")).add_filter("IWI", &["iwi"]);
        if let Some(dir) = path.parent() {
            dialog = dialog.set_directory(dir);
        }
        match dialog.save_file() {
            Some(out) => {
                std::fs::write(&out, bytes)?;
                Ok(Some(format!("{summary}\nSaved {}", out.display())))
            }
            None => Ok(None),
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.queue.stop();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Dropping a folder onto the window opens it.
        let dropped = ui.input(|i| i.raw.dropped_files.iter().find_map(|f| Some(f.path().to_path_buf())));
        if let Some(p) = dropped {
            let dir = if p.is_dir() { p } else { p.parent().map(|d| d.to_path_buf()).unwrap_or(p) };
            self.open_root(dir);
        }
        self.poll_scan();
        self.poll_thumbs();
        self.poll_preview();
        self.refilter();
        if self.ui_demo && self.visible.len() > 6 {
            self.ui_demo = false;
            let first = self.visible[1];
            self.multi = self.visible[1..5].iter().copied().collect();
            self.anchor = Some(first);
            self.select(first);
            if let Some(cat) = self.catalog.clone() {
                let name = cat.entries[self.visible[2]].name.clone();
                self.settings.toggle_favourite(&name);
            }
        }
        self.sync_texture();

        self.drive_capture(ui.ctx());
        self.poll_batch();
        self.handle_shortcuts(ui.ctx());
        self.convert.show(ui.ctx());
        self.iwd.show(ui.ctx());
        self.settings_window(ui.ctx());
        self.help_window(ui.ctx());
        self.games_window(ui.ctx());
        self.log_window(ui.ctx());

        egui::Panel::top("top").show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        match self.tab {
            Tab::Images => {
                egui::Panel::right("details").resizable(true).default_size(460.0).show(ui, |ui| self.details(ui));
                egui::CentralPanel::default().show(ui, |ui| self.grid(ui));
            }
            Tab::Models => {
                self.models.camera_style = self.settings.camera_style;
                let state = self.wgpu_state.clone();
                self.models.ui(ui, state.as_ref());
            }
            Tab::Maps => {
                if self.mapview.active {
                    egui::Panel::left("map_zones").resizable(true).default_size(380.0).show(ui, |ui| self.maps_ui(ui));
                    egui::CentralPanel::default().show(ui, |ui| self.mapview_ui(ui));
                } else {
                    egui::CentralPanel::default().show(ui, |ui| self.maps_ui(ui));
                }
            }
            Tab::Materials => {
                egui::CentralPanel::default().show(ui, |ui| self.materials_ui(ui));
            }
            Tab::Sounds => {
                egui::CentralPanel::default().show(ui, |ui| self.sounds_ui(ui));
            }
            Tab::Effects => self.effects_ui(ui),
            Tab::Data => {
                egui::CentralPanel::default().show(ui, |ui| self.data_ui(ui));
            }
        }
    }
}
