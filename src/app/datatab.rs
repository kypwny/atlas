//! The Data tab: the game's string tables, weapons, localised strings and raw files, read from every zone of the open
//! folder (the same collection `atlas --exportdata` writes), browsable and exportable as CSV, JSON and files.

use super::App;
use crate::data::{GameData, Table};
use crate::log;
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Kind {
    #[default]
    Tables,
    Weapons,
    Strings,
    Files,
}

#[derive(Default)]
pub(super) struct DataState {
    data: Option<Arc<GameData>>,
    for_root: Option<PathBuf>,
    kind: Kind,
    filter: String,
    selected: Option<String>,
    /// Weapon tables, built once per load (full and summary, per layout).
    weapons: Vec<(&'static str, Table, Table)>,
    full: bool,
    message: Option<String>,
    progress: Arc<(AtomicUsize, AtomicUsize)>,
    pub(super) rx: Option<Receiver<Result<GameData, String>>>,
}

fn grid(ui: &mut egui::Ui, t: &Table, limit: usize) {
    let rows = t.rows.min(limit);
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        egui::Grid::new(("data-grid", &t.name)).striped(true).num_columns(t.columns.max(1)).show(ui, |ui| {
            for r in 0..rows {
                for c in 0..t.columns {
                    let v = t.cell(r, c);
                    if r == 0 {
                        ui.strong(v);
                    } else {
                        ui.label(egui::RichText::new(v).monospace());
                    }
                }
                ui.end_row();
            }
        });
    });
    if t.rows > limit {
        ui.weak(format!("first {limit} of {} rows; export for all", t.rows));
    }
}

impl App {
    fn start_data_job(&mut self) {
        let Some(root) = self.root.clone() else { return };
        let (tx, rx) = channel();
        let progress = self.data.progress.clone();
        progress.0.store(0, Ordering::Relaxed);
        progress.1.store(0, Ordering::Relaxed);
        self.data.rx = Some(rx);
        self.data.for_root = Some(root.clone());
        self.data.message = None;
        std::thread::spawn(move || {
            let r = crate::data::collect(&root, Some(&progress)).map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
        });
    }

    fn poll_data(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.data.rx else { return };
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
        let Ok(result) = rx.try_recv() else { return };
        self.data.rx = None;
        match result {
            Ok(d) => {
                log::log(format!("data: {} zones read, {} tables, {} strings, {} raw files, {} weapons", d.zones_read, d.tables.len(), d.localize.len(), d.rawfiles.len(), d.weapons.len()));
                self.data.weapons = d.weapon_grids();
                self.data.data = Some(Arc::new(d));
            }
            Err(e) => {
                log::log(format!("data: {e}"));
                self.data.message = Some(e);
            }
        }
    }

    pub(super) fn data_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_data(&ctx);
        if self.root.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label("Open a game folder first.");
            });
            return;
        }
        // read the folder's data the first time the tab is shown, and again when the folder changes
        if self.data.rx.is_none() && (self.data.for_root != self.root) {
            self.data.data = None;
            self.data.weapons.clear();
            self.data.selected = None;
            self.start_data_job();
        }
        let busy = self.data.rx.is_some();
        ui.horizontal_wrapped(|ui| {
            for (k, label) in [(Kind::Tables, "String tables"), (Kind::Weapons, "Weapons"), (Kind::Strings, "Localised text"), (Kind::Files, "Raw files")] {
                ui.selectable_value(&mut self.data.kind, k, label);
            }
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.data.filter).hint_text("filter…").desired_width(200.0));
            if self.data.kind == Kind::Weapons {
                ui.checkbox(&mut self.data.full, "every field").on_hover_text("All fields with a known place in the weapon record, not just the main stats");
            }
            ui.separator();
            if ui.add_enabled(self.data.data.is_some() && !busy, egui::Button::new("Export all…").small()).on_hover_text("Writes string tables as CSV, localised text as CSV and JSON, raw files and the weapon tables to a folder").clicked() {
                if let (Some(out), Some(d)) = (rfd::FileDialog::new().set_title("Export the data tables to").pick_folder(), self.data.data.clone()) {
                    let r = d.write(&out);
                    let m = match r {
                        Ok(()) => format!("written to {}", out.display()),
                        Err(e) => format!("{}: {e}", out.display()),
                    };
                    log::log(format!("data: {m}"));
                    self.data.message = Some(m);
                }
            }
            if ui.add_enabled(!busy, egui::Button::new("Read again").small()).clicked() {
                self.data.for_root = None;
            }
            if busy {
                let (done, total) = (self.data.progress.0.load(Ordering::Relaxed), self.data.progress.1.load(Ordering::Relaxed));
                ui.spinner();
                ui.weak(format!("reading zones {done} / {total}"));
            }
            if let Some(m) = &self.data.message {
                ui.weak(m);
            }
        });
        ui.separator();
        let Some(data) = self.data.data.clone() else { return };
        let words: Vec<String> = self.data.filter.split_whitespace().map(str::to_lowercase).collect();
        let matches = |s: &str| words.iter().all(|w| s.to_ascii_lowercase().contains(w));
        match self.data.kind {
            Kind::Tables => {
                let names: Vec<&String> = data.tables.keys().filter(|n| matches(n)).collect();
                if self.data.selected.as_ref().is_none_or(|s| !data.tables.contains_key(s)) {
                    self.data.selected = names.iter().find(|n| data.tables[**n].rows > 1).or(names.first()).map(|n| (*n).clone());
                }
                ui.columns(2, |cols| {
                    egui::ScrollArea::vertical().id_salt("data-names").auto_shrink([false, false]).show(&mut cols[0], |ui| {
                        for n in &names {
                            let t = &data.tables[*n];
                            let label = format!("{}   ({}x{})", t.name, t.columns, t.rows);
                            if ui.selectable_label(self.data.selected.as_deref() == Some(n.as_str()), label).clicked() {
                                self.data.selected = Some((*n).clone());
                            }
                        }
                    });
                    if let Some(t) = self.data.selected.as_ref().and_then(|n| data.tables.get(n)) {
                        cols[1].weak(format!("{} - from {}", t.name, t.zone));
                        grid(&mut cols[1], t, 400);
                    }
                });
            }
            Kind::Weapons => {
                if self.data.weapons.is_empty() {
                    ui.label("No weapon records were read (the folder's weapon layout is not known).");
                }
                let weapons = std::mem::take(&mut self.data.weapons);
                for (tag, full, short) in &weapons {
                    let t = if self.data.full { full } else { short };
                    // filter rows by name or display text
                    let rows: Vec<usize> = (1..t.rows).filter(|&r| matches(&format!("{} {}", t.cell(r, 0), t.cell(r, 2)))).collect();
                    ui.weak(format!("{tag} layout: {} of {} weapons", rows.len(), t.rows - 1));
                    let mut view = Table { name: format!("{}-view", t.name), zone: String::new(), columns: t.columns, rows: 1 + rows.len(), cells: Vec::new() };
                    view.cells.extend((0..t.columns).map(|c| t.cell(0, c).to_string()));
                    for r in rows {
                        view.cells.extend((0..t.columns).map(|c| t.cell(r, c).to_string()));
                    }
                    grid(ui, &view, 600);
                }
                self.data.weapons = weapons;
            }
            Kind::Strings => {
                let keys: Vec<&String> = data.localize.iter().filter(|(k, (t, _))| matches(k) || matches(t)).map(|(k, _)| k).collect();
                ui.weak(format!("{} of {} strings", keys.len(), data.localize.len()));
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, keys.len(), |ui, range| {
                    for i in range {
                        let (text, zone) = &data.localize[keys[i]];
                        ui.label(egui::RichText::new(format!("{:<44} {}   [{zone}]", keys[i], text.replace('\n', " | "))).monospace());
                    }
                });
            }
            Kind::Files => {
                let names: Vec<&String> = data.rawfiles.keys().filter(|n| matches(n)).collect();
                ui.columns(2, |cols| {
                    egui::ScrollArea::vertical().id_salt("data-files").auto_shrink([false, false]).show(&mut cols[0], |ui| {
                        for n in &names {
                            let f = &data.rawfiles[*n];
                            if ui.selectable_label(self.data.selected.as_deref() == Some(n.as_str()), format!("{}   ({} bytes)", f.name, f.bytes.len())).clicked() {
                                self.data.selected = Some((*n).clone());
                            }
                        }
                    });
                    if let Some(f) = self.data.selected.as_ref().and_then(|n| data.rawfiles.get(n)) {
                        cols[1].weak(format!("{} - from {}", f.name, f.zone));
                        let text = String::from_utf8_lossy(&f.bytes[..f.bytes.len().min(60_000)]).into_owned();
                        egui::ScrollArea::both().id_salt("data-file-text").auto_shrink([false, false]).show(&mut cols[1], |ui| {
                            ui.label(egui::RichText::new(text).monospace());
                        });
                    }
                });
            }
        }
    }
}
