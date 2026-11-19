//! The "Pack IWD" window: collect files, edit their paths inside the archive, write an IWD.

use crate::convert;
use crate::iwd::{self, AddOptions, Entry, WriteOptions};
use eframe::egui::{self, Color32};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

const USAGES: [(&str, &str); 5] = [
    ("color", "Colour / diffuse (0x341)"),
    ("normal", "Normal map (0x40001)"),
    ("ui", "UI, no mipmaps, clamped (0x173)"),
    ("clamp", "Clamped, with mipmaps (0x171)"),
    ("raw", "No flags (0x0)"),
];

pub struct IwdTool {
    pub open: bool,
    entries: Vec<Entry>,
    /// Archive the list was loaded from, so it can be saved back over it.
    opened: Option<PathBuf>,
    filter: String,
    convert_dds: bool,
    usage: usize,
    genmips: bool,
    store: bool,
    message: Option<(bool, String)>,
    job: Option<Receiver<(bool, String)>>,
    /// The archive last written (or being written).
    last_out: Option<PathBuf>,
}

impl Default for IwdTool {
    fn default() -> Self {
        Self {
            open: false,
            entries: Vec::new(),
            opened: None,
            filter: String::new(),
            convert_dds: true,
            usage: 0,
            genmips: true,
            store: false,
            message: None,
            job: None,
            last_out: None,
        }
    }
}

fn fmt_size(n: u64) -> String {
    if n >= 1 << 20 { format!("{:.1} MB", n as f64 / 1048576.0) } else if n >= 1 << 10 { format!("{:.1} KB", n as f64 / 1024.0) } else { format!("{n} B") }
}

impl IwdTool {
    /// Adds entries (from the browser); an existing entry with the same path is replaced.
    pub fn add(&mut self, new: Vec<Entry>) {
        let n = new.len();
        for e in new {
            if let Some(old) = self.entries.iter_mut().find(|o| o.path.eq_ignore_ascii_case(&e.path)) {
                *old = e;
            } else {
                self.entries.push(e);
            }
        }
        self.open = true;
        self.message = Some((true, format!("Added {n} file(s).")));
    }

    fn add_options(&self) -> AddOptions {
        AddOptions {
            dds: self.convert_dds.then(|| convert::Options { usage: USAGES[self.usage].0.to_string(), flags: None, genmips: self.genmips }),
        }
    }

    fn report<T>(&mut self, r: anyhow::Result<T>, ok: impl FnOnce(&mut Self, T)) {
        match r {
            Ok(v) => ok(self, v),
            Err(e) => self.message = Some((false, format!("{e:#}"))),
        }
    }

    fn start_save(&mut self, ctx: &egui::Context, out: PathBuf) {
        let entries = self.entries.clone();
        let out2 = out.clone();
        let opts = WriteOptions { level: if self.store { None } else { Some(6) } };
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = iwd::write(&entries, &out, opts).and_then(|s| {
                let v = iwd::verify(&out)?;
                let mut msg = format!(
                    "Wrote {} ({} files, {}{}). Read back: {} files, {} images",
                    out.display(),
                    s.files,
                    fmt_size(s.bytes),
                    if s.replaced_duplicates > 0 { format!(", {} duplicate path(s) replaced", s.replaced_duplicates) } else { String::new() },
                    v.files,
                    v.images
                );
                if !v.bad_images.is_empty() {
                    msg.push_str(&format!("; {} image header problem(s), first: {}", v.bad_images.len(), v.bad_images[0]));
                }
                Ok((v.bad_images.is_empty(), msg))
            });
            let _ = tx.send(match result {
                Ok(r) => r,
                Err(e) => (false, format!("{e:#}")),
            });
            ctx.request_repaint();
        });
        crate::log::log(format!("writing {}", out2.display()));
        self.last_out = Some(out2);
        self.job = Some(rx);
        self.message = Some((true, "Writing…".into()));
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.job {
            if let Ok(m) = rx.try_recv() {
                crate::log::log(&m.1);
                self.message = Some(m);
                self.job = None;
            }
        }
        if !self.open {
            return;
        }
        let mut open = self.open;
        egui::Window::new("Pack IWD").open(&mut open).default_width(760.0).default_height(520.0).show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Open IWD…").on_hover_text("Load an existing archive to edit; untouched files are copied without recompression").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("IWD", &["iwd"]).pick_file() {
                        let r = iwd::load(&p);
                        self.report(r, |s, e| {
                            s.message = Some((true, format!("Loaded {} file(s) from {}.", e.len(), p.display())));
                            s.entries = e;
                            s.opened = Some(p);
                        });
                    }
                }
                if ui.button("Add files…").clicked() {
                    if let Some(files) = rfd::FileDialog::new().set_title("Files to add").pick_files() {
                        let r = iwd::add_files(&files, &self.add_options());
                        self.report(r, |s, e| s.add(e));
                    }
                }
                if ui.button("Add folder…").on_hover_text("Every file below the folder; paths are kept relative to it (a folder holding images/x.iwi gives images/x.iwi; a folder named images is placed under images/)").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_title("Folder to add").pick_folder() {
                        let r = iwd::add_dir(&dir, &self.add_options());
                        self.report(r, |s, e| s.add(e));
                    }
                }
                if ui.button("Clear").clicked() {
                    self.entries.clear();
                    self.opened = None;
                    self.message = None;
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.convert_dds, "Convert .dds to IWI");
                ui.add_enabled_ui(self.convert_dds, |ui| {
                    egui::ComboBox::from_id_salt("iwd_usage").selected_text(USAGES[self.usage].1).show_ui(ui, |ui| {
                        for (i, (_, label)) in USAGES.iter().enumerate() {
                            ui.selectable_value(&mut self.usage, i, *label);
                        }
                    });
                    ui.checkbox(&mut self.genmips, "generate mipmaps");
                });
            });
            ui.weak("Add images from the browser with “Add to IWD” on an image or “Add shown to IWD” in the filter bar.");
            ui.separator();

            let dups = iwd::duplicates(&self.entries);
            let total: u64 = self.entries.iter().map(|e| e.size).sum();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("filter paths…").desired_width(220.0));
                ui.weak(format!("{} files, {}{}", self.entries.len(), fmt_size(total), if dups.is_empty() { String::new() } else { format!(", {} duplicate path(s)", dups.len()) }));
            });

            let needle = self.filter.to_ascii_lowercase();
            let shown: Vec<usize> = (0..self.entries.len()).filter(|&i| needle.is_empty() || self.entries[i].path.to_ascii_lowercase().contains(&needle)).collect();
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
            let mut remove = None;
            egui::ScrollArea::vertical().max_height(ui.available_height() - 90.0).auto_shrink([false, false]).show_rows(ui, row_h, shown.len(), |ui, range| {
                for k in range {
                    let i = shown[k];
                    let e = &mut self.entries[i];
                    ui.horizontal(|ui| {
                        if ui.small_button("X").on_hover_text("Remove from the list").clicked() {
                            remove = Some(i);
                        }
                        let bad = iwd::check_path(&e.path).is_some();
                        let dup = dups.contains(&e.path.to_ascii_lowercase());
                        let resp = ui.add(egui::TextEdit::singleline(&mut e.path).desired_width(360.0).text_color(if bad {
                            Color32::LIGHT_RED
                        } else if dup {
                            Color32::YELLOW
                        } else {
                            ui.visuals().text_color()
                        }));
                        // names are kept lowercase with forward slashes (entries copied as is keep whatever was typed)
                        if resp.lost_focus() && !matches!(e.source, iwd::Source::Archive { .. }) {
                            e.path = iwd::archive_path(&e.path);
                        }
                        if let Some(why) = iwd::check_path(&e.path) {
                            resp.on_hover_text(why);
                        } else if dup {
                            resp.on_hover_text("Another entry has this path; the later one is written");
                        }
                        ui.weak(fmt_size(e.size));
                        ui.weak(e.source.describe());
                    });
                }
            });
            if let Some(i) = remove {
                self.entries.remove(i);
            }

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.store, "Store without compression");
                let busy = self.job.is_some();
                let can = !self.entries.is_empty() && !busy;
                if ui.add_enabled(can, egui::Button::new("Save IWD…")).clicked() {
                    let mut dialog = rfd::FileDialog::new().add_filter("IWD", &["iwd"]).set_file_name(self.opened.as_ref().and_then(|p| p.file_name()).map_or("custom.iwd".to_string(), |n| n.to_string_lossy().into_owned()));
                    if let Some(dir) = self.opened.as_ref().and_then(|p| p.parent()) {
                        dialog = dialog.set_directory(dir);
                    }
                    if let Some(out) = dialog.save_file() {
                        self.start_save(ui.ctx(), out);
                    }
                }
                if let Some(p) = self.opened.clone() {
                    if ui.add_enabled(can, egui::Button::new("Save over opened archive")).on_hover_text(p.display().to_string()).clicked() {
                        self.start_save(ui.ctx(), p);
                    }
                }
            });
            if let Some((ok, m)) = &self.message {
                ui.colored_label(if *ok { Color32::LIGHT_GREEN } else { Color32::LIGHT_RED }, m);
                if *ok && self.job.is_none() {
                    if let Some(p) = self.last_out.clone().filter(|p| p.exists()) {
                        if ui.small_button("Show in folder").clicked() {
                            crate::app::reveal_path(&p);
                        }
                    }
                }
            }
        });
        self.open = open;
    }
}
