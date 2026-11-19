//! Workflow around the browser: the Games window (what each known folder is and whether it is ready), the Log window, the Maps
//! tab (zones with their assets: pack the images a zone uses into an IWD, or write the zone's asset list) and "show in folder".

use super::*;
use crate::gameinfo::{self, GameInfo};
use crate::log;
use crate::zone::{export as zexport, fastfile};
use std::collections::HashMap;
use std::path::Path;

/// Opens the folder that holds `path` in the file manager, selecting the file when it is one.
pub(super) fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::process::Command;
        if path.is_file() {
            let _ = Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
        } else {
            let _ = Command::new("explorer").arg(path).spawn();
        }
    }
    #[cfg(not(windows))]
    {
        let dir = if path.is_file() { path.parent().unwrap_or(path) } else { path };
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// One zone file of the open folder.
pub(super) struct ZoneRow {
    name: String,
    path: PathBuf,
    build: String,
    size: u64,
}

/// What a background zone job found.
enum MapJob {
    /// Assets of a zone, to be turned into IWD entries on the UI thread.
    Pack { zone: String, assets: Vec<(&'static str, String)> },
    Message(String),
}

#[derive(Default)]
pub(super) struct MapsState {
    rows: Vec<ZoneRow>,
    for_root: Option<PathBuf>,
    filter: String,
    selected: Option<usize>,
    message: Option<String>,
    rx: Option<Receiver<MapJob>>,
}

/// The asset list as zone builder CSV lines (`type,name`), with a comment header.
pub(super) fn asset_list_text(zone: &str, assets: &[(&'static str, String)]) -> String {
    let mut s = format!("// asset list of {zone}, written by atlas\n// only the types whose names the zone reader records: xmodel, xmodelsurfs, material, techset, image, xanim\n");
    for (ty, name) in assets {
        s.push_str(&format!("{ty},{name}\n"));
    }
    s
}

impl App {
    // ---- Games window ---------------------------------------------------------------------------------------

    pub(super) fn games_window(&mut self, ctx: &egui::Context) {
        if !self.games_open {
            return;
        }
        let mut open = true;
        // known folders: the one open now and the recent ones
        let mut folders: Vec<String> = Vec::new();
        if let Some(r) = &self.root {
            folders.push(r.display().to_string());
        }
        for r in &self.settings.recent {
            if !folders.contains(r) {
                folders.push(r.clone());
            }
        }
        let mut open_folder = None;
        let mut remove = None;
        let mut check = None;
        egui::Window::new("Games").open(&mut open).default_width(560.0).default_height(420.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Add a folder…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Game folder").pick_folder() {
                        open_folder = Some(dir);
                    }
                }
                ui.weak("Folders you open are remembered here. Each is looked at once, when shown.");
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if folders.is_empty() {
                    ui.weak("No folder yet: use Add a folder, or open one from the top bar.");
                }
                for f in &folders {
                    let info = self.game_cache.entry(f.clone()).or_insert_with(|| gameinfo::detect(Path::new(f), false)).clone();
                    let current = self.root.as_ref().is_some_and(|r| r.display().to_string() == *f);
                    ui.horizontal(|ui| {
                        ui.heading(&info.name);
                        if current {
                            ui.label(egui::RichText::new("open").color(ui.visuals().hyperlink_color));
                        }
                    });
                    ui.weak(f);
                    for z in &info.zones {
                        ui.label(format!("• {z}"));
                    }
                    for i in &info.images {
                        ui.label(format!("• images: {i}"));
                    }
                    for n in &info.notes {
                        let (mark, colour) = if n.ok { ("ok", Color32::LIGHT_GREEN) } else { ("!", Color32::LIGHT_RED) };
                        ui.colored_label(colour, format!("[{mark}] {}", n.text));
                    }
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!current, egui::Button::new("Open")).clicked() {
                            open_folder = Some(PathBuf::from(f));
                        }
                        if ui.button("Show in folder").clicked() {
                            reveal(Path::new(f));
                        }
                        if ui.button("Check again").on_hover_text("Looks again, including whether the client key can be found").clicked() {
                            check = Some(f.clone());
                        }
                        if ui.button("Forget").clicked() {
                            remove = Some(f.clone());
                        }
                    });
                    ui.separator();
                }
            });
        });
        self.games_open = open;
        if let Some(f) = check {
            let info = gameinfo::detect(Path::new(&f), true);
            log::log(format!("checked {f}: {}", info.name));
            self.game_cache.insert(f, info);
        }
        if let Some(f) = remove {
            self.settings.recent.retain(|r| *r != f);
            self.game_cache.remove(&f);
            self.save_settings();
        }
        if let Some(dir) = open_folder {
            self.open_root(dir);
        }
    }

    // ---- Log window -----------------------------------------------------------------------------------------

    pub(super) fn log_window(&mut self, ctx: &egui::Context) {
        if !self.log_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Log").open(&mut open).default_width(640.0).default_height(300.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(log::lines().join("\n"));
                }
                if ui.button("Clear").clicked() {
                    log::clear();
                }
                ui.weak(format!("{} lines", log::count()));
            });
            egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                for l in log::lines() {
                    ui.label(egui::RichText::new(l).monospace().size(12.0));
                }
            });
        });
        self.log_open = open;
        // keep refreshing while it is open, so lines from background jobs appear
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }

    // ---- Maps tab ---------------------------------------------------------------------------------------------

    fn load_zone_rows(&mut self) {
        let Some(root) = self.root.clone() else { return };
        if self.maps.for_root.as_ref() == Some(&root) {
            return;
        }
        let mut rows: Vec<ZoneRow> = zexport::survey_files(&root)
            .into_iter()
            .filter_map(|p| {
                let peek = fastfile::peek(&p).ok()?;
                Some(ZoneRow {
                    name: p.file_name()?.to_string_lossy().into_owned(),
                    build: fastfile::version_info(peek.family, peek.version).build.to_string(),
                    size: std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
                    path: p,
                })
            })
            .collect();
        rows.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
        self.maps.rows = rows;
        self.maps.for_root = Some(root);
        self.maps.selected = None;
    }

    fn poll_maps(&mut self) {
        let Some(rx) = &self.maps.rx else { return };
        let Ok(job) = rx.try_recv() else { return };
        self.maps.rx = None;
        match job {
            MapJob::Message(m) => {
                log::log(&m);
                self.maps.message = Some(m);
            }
            MapJob::Pack { zone, assets } => {
                let Some(cat) = self.catalog.clone() else {
                    self.maps.message = Some("Open the game folder's images first (Images tab).".into());
                    return;
                };
                let (mut entries, mut missing) = (Vec::new(), 0usize);
                for (ty, name) in assets.iter().filter(|(t, _)| *t == "image") {
                    let _ = ty;
                    let key = name.to_ascii_lowercase();
                    match cat.entries.binary_search_by(|e| e.name.as_str().cmp(key.as_str())) {
                        Ok(i) => entries.extend(self.pack_entry(&cat, i, None)),
                        Err(_) => missing += 1,
                    }
                }
                let m = format!("{zone}: {} image(s) added to the Pack IWD window{}", entries.len(), if missing > 0 { format!(", {missing} not installed as images here") } else { String::new() });
                log::log(&m);
                self.iwd.add(entries);
                self.maps.message = Some(m);
            }
        }
    }

    /// Runs `work` on a thread against the opened zone; the key comes from the game folder.
    fn zone_job(&mut self, row: usize, work: impl FnOnce(&crate::zone::Zone, String) -> MapJob + Send + 'static) {
        let Some(root) = self.root.clone() else { return };
        let (path, name) = (self.maps.rows[row].path.clone(), self.maps.rows[row].name.clone());
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.maps.message = Some(format!("Reading {name} ..."));
        log::log(format!("reading zone {name}"));
        std::thread::spawn(move || {
            let job = (|| -> Result<MapJob> {
                let key = zexport::find_key(&root)?;
                let zone = crate::zone::Zone::open(&path, &key)?;
                Ok(work(&zone, name))
            })()
            .unwrap_or_else(|e| MapJob::Message(format!("{e:#}")));
            let _ = tx.send(job);
            ctx.request_repaint();
        });
        self.maps.rx = Some(rx);
    }

    pub(super) fn maps_ui(&mut self, ui: &mut egui::Ui) {
        self.load_zone_rows();
        self.poll_maps();
        if let Some(want) = self.pending_map.clone() {
            // a zone file name, or the end of its path (`iw4x/x86/english/mp_rust.ff`) to pick one of several folders' copies
            let tail = want.replace('\\', "/").to_ascii_lowercase();
            if let Some(row) = self.maps.rows.iter().find(|r| r.name.eq_ignore_ascii_case(&want) || (tail.contains('/') && r.path.to_string_lossy().replace('\\', "/").to_ascii_lowercase().ends_with(&tail))) {
                let (path, name) = (row.path.clone(), row.name.clone());
                self.pending_map = None;
                self.open_map(path, name);
            }
        }
        if self.root.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label("Open a game folder first.");
            });
            return;
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.maps.filter).hint_text("filter zones…").desired_width(220.0));
            ui.weak(format!("{} zones", self.maps.rows.len()));
        });
        let words: Vec<String> = self.maps.filter.split_whitespace().map(str::to_lowercase).collect();
        let shown: Vec<usize> = (0..self.maps.rows.len()).filter(|&i| words.iter().all(|w| self.maps.rows[i].name.to_ascii_lowercase().contains(w))).collect();
        // actions for the selected zone
        let selected = self.maps.selected.filter(|i| *i < self.maps.rows.len());
        ui.horizontal_wrapped(|ui| {
            let busy = self.maps.rx.is_some();
            let can = selected.is_some() && !busy;
            if ui.add_enabled(can, egui::Button::new("Pack its images into an IWD")).on_hover_text("Reads the zone and adds the images it defines to the Pack IWD window").clicked() {
                if let Some(i) = selected {
                    self.zone_job(i, |z, name| MapJob::Pack { zone: name, assets: zexport::zone_asset_list(z) });
                }
            }
            if ui.add_enabled(can, egui::Button::new("Write asset list…")).on_hover_text("Zone builder CSV lines (type,name) for the assets the zone defines").clicked() {
                if let Some(i) = selected {
                    let stem = self.maps.rows[i].name.trim_end_matches(".ff").to_string();
                    if let Some(out) = rfd::FileDialog::new().set_file_name(format!("{stem}.csv")).add_filter("CSV", &["csv"]).save_file() {
                        self.zone_job(i, move |z, name| {
                            let assets = zexport::zone_asset_list(z);
                            match std::fs::write(&out, asset_list_text(&name, &assets)) {
                                Ok(()) => MapJob::Message(format!("{name}: {} assets written to {}", assets.len(), out.display())),
                                Err(e) => MapJob::Message(format!("{}: {e}", out.display())),
                            }
                        });
                    }
                }
            }
            if ui.add_enabled(selected.is_some() && self.mapview.rx.is_none(), egui::Button::new("View map")).on_hover_text("Shows the zone's world geometry in the 3D viewer, a surface per material").clicked() {
                if let Some(i) = selected {
                    let (path, name) = (self.maps.rows[i].path.clone(), self.maps.rows[i].name.clone());
                    self.open_map(path, name);
                }
            }
            if ui.add_enabled(selected.is_some(), egui::Button::new("Show in folder")).clicked() {
                if let Some(i) = selected {
                    reveal(&self.maps.rows[i].path);
                }
            }
            if let Some(m) = &self.maps.message {
                ui.weak(m);
            }
        });
        ui.separator();
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, shown.len(), |ui, range| {
            for k in range {
                let i = shown[k];
                let r = &self.maps.rows[i];
                let text = format!("{:<44} {:>9}   {}", r.name, format!("{:.1} MB", r.size as f64 / 1048576.0), r.build);
                if ui.selectable_label(self.maps.selected == Some(i), egui::RichText::new(text).monospace()).clicked() {
                    self.maps.selected = Some(i);
                }
            }
        });
    }
}

pub(super) type KnownGames = HashMap<String, GameInfo>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_asset_list_is_zone_builder_csv_with_a_comment_header() {
        let t = asset_list_text("mp_x.ff", &[("xmodel", "a_b".into()), ("image", "c".into())]);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("//") && lines[1].starts_with("//"));
        assert_eq!(&lines[2..], ["xmodel,a_b", "image,c"]);
    }
}
