//! The browser's look and workflow extras: theme, settings window, shortcuts and their cheat sheet, favourites, multi-select
//! with batch export, recent folders and saved searches.

use super::*;
use crate::settings::{self, GridBackground, SavedSearch, Settings};
use eframe::egui::{Key, Modifiers};
use std::collections::BTreeSet;

/// Applies the theme: dark or light, the accent as selection and link colour, and the UI scale.
pub(super) fn apply_theme(ctx: &egui::Context, s: &Settings) {
    let mut v = if s.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    let a = Color32::from_rgb(s.accent[0], s.accent[1], s.accent[2]);
    v.selection.bg_fill = a.gamma_multiply(if s.dark { 0.45 } else { 0.35 });
    v.selection.stroke = egui::Stroke::new(1.0, a);
    v.hyperlink_color = a;
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, a.gamma_multiply(0.8));
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, a);
    ctx.set_visuals(v);
    ctx.set_zoom_factor(s.ui_scale);
}

/// A five-pointed star (drawn, because the interface font has no star glyph): filled when `filled`, otherwise an outline.
pub(super) fn draw_star(painter: &egui::Painter, center: Pos2, r: f32, color: Color32, filled: bool) {
    let points: Vec<Pos2> = (0..10)
        .map(|i| {
            let angle = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            let radius = if i % 2 == 0 { r } else { r * 0.42 };
            center + Vec2::new(angle.cos(), angle.sin()) * radius
        })
        .collect();
    if filled {
        // the outline is star-shaped around its centre, so a fan of triangles fills it
        let mut mesh = egui::epaint::Mesh::default();
        mesh.colored_vertex(center, color);
        for p in &points {
            mesh.colored_vertex(*p, color);
        }
        for i in 0..10u32 {
            mesh.add_triangle(0, 1 + i, 1 + (i + 1) % 10);
        }
        painter.add(egui::Shape::mesh(mesh));
    } else {
        painter.add(egui::Shape::closed_line(points, egui::Stroke::new(1.3, color)));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum BatchKind {
    Png,
    Dds,
    Iwi,
}

struct BatchItem {
    name: String,
    provider: Arc<dyn Provider>,
    key: String,
}

fn file_safe(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).collect()
}

/// Writes every item into `dir` in the chosen form and returns a one-line summary.
fn run_batch(kind: BatchKind, items: Vec<BatchItem>, dir: PathBuf) -> String {
    let (mut ok, mut failed, mut first_error) = (0usize, 0usize, String::new());
    for it in items {
        let result: Result<()> = (|| {
            let data = it.provider.read(&it.key)?;
            let stem = file_safe(&it.name);
            match kind {
                BatchKind::Iwi => std::fs::write(dir.join(format!("{stem}.iwi")), &data)?,
                BatchKind::Dds => std::fs::write(dir.join(format!("{stem}.dds")), convert::iwi_to_dds(&data)?)?,
                BatchKind::Png => {
                    let h = Header::parse(&data)?;
                    let l = iwi::decode_level(&data, &h, 0)?;
                    image::save_buffer(dir.join(format!("{stem}.png")), &l.faces[0], l.width, l.height, image::ColorType::Rgba8)?;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => ok += 1,
            Err(e) => {
                failed += 1;
                if first_error.is_empty() {
                    first_error = format!("{}: {e:#}", it.name);
                }
            }
        }
    }
    if failed == 0 { format!("Wrote {ok} file(s) to {}", dir.display()) } else { format!("Wrote {ok} file(s) to {}, {failed} failed (first: {first_error})", dir.display()) }
}

impl App {
    pub(super) fn save_settings(&self) {
        // test captures must not rewrite the user's settings
        if self.capture.is_some() {
            return;
        }
        if let Some(p) = settings::config_path() {
            let _ = self.settings.save(&p);
        }
    }

    pub(super) fn restyle(&self) {
        apply_theme(&self.ctx, &self.settings);
    }

    fn accent(&self) -> Color32 {
        let a = self.settings.accent;
        Color32::from_rgb(a[0], a[1], a[2])
    }

    /// The images the batch actions work on: the multi-selection, or the single selected image.
    pub(super) fn batch_set(&self) -> Vec<usize> {
        if !self.multi.is_empty() {
            self.multi.iter().copied().collect()
        } else {
            self.selected.into_iter().collect()
        }
    }

    pub(super) fn toggle_favourites(&mut self) {
        let Some(cat) = self.catalog.clone() else { return };
        let set = self.batch_set();
        if set.is_empty() {
            return;
        }
        // all favourites already -> clear them all; otherwise make them all favourites
        let all = set.iter().all(|&i| self.settings.is_favourite(&cat.entries[i].name));
        for &i in &set {
            let name = &cat.entries[i].name;
            if self.settings.is_favourite(name) == all {
                self.settings.toggle_favourite(name);
            }
        }
        self.after_favourites_changed();
    }

    pub(super) fn after_favourites_changed(&mut self) {
        if self.filters.favourites {
            self.applied = None;
        }
        self.save_settings();
    }

    fn best_variant<'a>(entry: &'a source::ImageEntry) -> Option<&'a source::Variant> {
        entry.variants.iter().filter(|v| v.header.is_some()).max_by_key(|v| v.header.as_ref().map_or(0, |h| h.width as u64 * h.height as u64))
    }

    fn batch_items(&self, cat: &Catalog) -> Vec<BatchItem> {
        self.batch_set()
            .into_iter()
            .filter_map(|ei| {
                let e = &cat.entries[ei];
                let v = Self::best_variant(e)?;
                Some(BatchItem { name: e.name.clone(), provider: Arc::clone(&cat.providers[v.provider]), key: v.key.clone() })
            })
            .collect()
    }

    pub(super) fn start_batch(&mut self, kind: BatchKind) {
        let Some(cat) = self.catalog.clone() else { return };
        let items = self.batch_items(&cat);
        if items.is_empty() {
            return;
        }
        let Some(dir) = rfd::FileDialog::new().set_title("Folder for the files").pick_folder() else { return };
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.batch_message = Some(format!("Writing {} file(s) ...", items.len()));
        self.batch_dir = Some(dir.clone());
        std::thread::spawn(move || {
            let _ = tx.send(run_batch(kind, items, dir));
            ctx.request_repaint();
        });
        self.batch_rx = Some(rx);
    }

    pub(super) fn poll_batch(&mut self) {
        if let Some(rx) = &self.batch_rx {
            if let Ok(m) = rx.try_recv() {
                crate::log::log(&m);
                self.batch_message = Some(m);
                self.batch_rx = None;
            }
        }
    }

    /// The bar above the grid: what is selected and what can be done with it.
    pub(super) fn batch_bar(&mut self, ui: &mut egui::Ui, cat: &Arc<Catalog>) {
        let set = self.batch_set();
        if set.is_empty() && self.batch_message.is_none() {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            if !set.is_empty() {
                ui.label(egui::RichText::new(format!("{} selected", set.len())).strong().color(self.accent()));
                if ui.button("Add to IWD").clicked() {
                    let entries: Vec<iwd::Entry> = set.iter().filter_map(|&i| self.pack_entry(cat, i, None)).collect();
                    self.iwd.add(entries);
                }
                if ui.button("Export PNG…").clicked() {
                    self.start_batch(BatchKind::Png);
                }
                if ui.button("Export DDS…").clicked() {
                    self.start_batch(BatchKind::Dds);
                }
                if ui.button("Save IWI…").clicked() {
                    self.start_batch(BatchKind::Iwi);
                }
                let all_fav = set.iter().all(|&i| self.settings.is_favourite(&cat.entries[i].name));
                if ui.button(if all_fav { "Unfavourite" } else { "Favourite" }).clicked() {
                    self.toggle_favourites();
                }
                if ui.button("Copy names").clicked() {
                    ui.ctx().copy_text(set.iter().map(|&i| cat.entries[i].name.clone()).collect::<Vec<_>>().join("\n"));
                }
                if !self.multi.is_empty() && ui.button("Clear").clicked() {
                    self.multi.clear();
                }
            }
            if let Some(m) = &self.batch_message {
                ui.weak(m);
                if self.batch_rx.is_none() {
                    if let Some(d) = self.batch_dir.clone() {
                        if ui.small_button("Open folder").clicked() {
                            super::workflow::reveal(&d);
                        }
                    }
                }
            }
        });
        ui.separator();
    }

    // ---- saved searches -------------------------------------------------------------------------------------

    fn current_search(&self, name: &str) -> SavedSearch {
        let f = &self.filters;
        SavedSearch { name: name.to_string(), text: f.text.clone(), format: f.format.map(|x| x.code()), map: f.map.map(|m| (m == MapType::Cube) as u8), alpha: f.alpha, dupes: f.dupes, favourites: f.favourites }
    }

    fn apply_search(&mut self, s: &SavedSearch) {
        self.filters = Filters {
            text: s.text.clone(),
            format: s.format.and_then(Format::from_code),
            map: s.map.map(|m| if m == 1 { MapType::Cube } else { MapType::Tex2D }),
            provider: None,
            alpha: s.alpha,
            dupes: s.dupes,
            favourites: s.favourites,
        };
    }

    /// The "Saved" menu of the filter bar.
    pub(super) fn saved_searches_ui(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Saved", |ui| {
            let list = self.settings.searches.clone();
            if list.is_empty() {
                ui.weak("no saved searches yet");
            }
            for s in &list {
                ui.horizontal(|ui| {
                    if ui.button(&s.name).clicked() {
                        self.apply_search(s);
                        ui.close();
                    }
                    if ui.small_button("Delete").on_hover_text("Delete this saved search").clicked() {
                        self.settings.searches.retain(|x| x.name != s.name);
                        self.save_settings();
                    }
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.save_search_name).hint_text("name").desired_width(120.0));
                let name = self.save_search_name.trim().to_string();
                if ui.add_enabled(!name.is_empty(), egui::Button::new("Save current")).clicked() {
                    let s = self.current_search(&name);
                    self.settings.save_search(s);
                    self.save_settings();
                    self.save_search_name.clear();
                    ui.close();
                }
            });
        });
    }

    pub(super) fn recent_ui(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Recent", |ui| {
            let list = self.settings.recent.clone();
            if list.is_empty() {
                ui.weak("no recent folders");
            }
            for r in list {
                if ui.button(&r).clicked() {
                    self.open_root(PathBuf::from(&r));
                    ui.close();
                }
            }
        });
    }

    // ---- windows --------------------------------------------------------------------------------------------

    pub(super) fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let before = self.settings.clone();
        egui::Window::new("Settings").open(&mut open).default_width(380.0).show(ctx, |ui| {
            let s = &mut self.settings;
            ui.strong("Look");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.dark, true, "Dark");
                ui.selectable_value(&mut s.dark, false, "Light");
            });
            ui.horizontal(|ui| {
                ui.label("Accent");
                for (rgb, name) in settings::ACCENTS {
                    let c = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                    ui.painter().rect_filled(rect, 4.0, c);
                    if s.accent == rgb {
                        ui.painter().rect_stroke(rect.expand(2.0), 5.0, egui::Stroke::new(2.0, ui.visuals().text_color()), egui::StrokeKind::Outside);
                    }
                    if resp.on_hover_text(name).clicked() {
                        s.accent = rgb;
                    }
                }
                ui.color_edit_button_srgb(&mut s.accent);
            });
            ui.horizontal(|ui| {
                ui.label("Interface scale");
                ui.add(egui::Slider::new(&mut s.ui_scale, settings::SCALE_RANGE.0..=settings::SCALE_RANGE.1).step_by(0.05));
            });
            ui.separator();
            ui.strong("Image grid");
            ui.horizontal(|ui| {
                ui.label("Thumbnail size");
                ui.add(egui::Slider::new(&mut s.thumb, settings::THUMB_RANGE.0..=settings::THUMB_RANGE.1).step_by(16.0));
            });
            ui.horizontal(|ui| {
                ui.label("Thumbnail background");
                for (bg, name) in GridBackground::ALL {
                    ui.selectable_value(&mut s.grid_bg, bg, name);
                }
            });
            ui.separator();
            ui.strong("Data");
            ui.horizontal(|ui| {
                ui.label(format!("{} favourites, {} saved searches, {} recent folders", s.favourites.len(), s.searches.len(), s.recent.len()));
            });
            ui.horizontal(|ui| {
                if ui.button("Clear favourites").clicked() {
                    s.favourites.clear();
                }
                if ui.button("Clear recent folders").clicked() {
                    s.recent.clear();
                }
                if ui.button("Reset look").clicked() {
                    let d = Settings::default();
                    (s.dark, s.accent, s.ui_scale, s.thumb, s.grid_bg) = (d.dark, d.accent, d.ui_scale, d.thumb, d.grid_bg);
                }
            });
            if let Some(p) = settings::config_path() {
                ui.weak(format!("Stored in {}", p.display()));
            }
        });
        self.settings_open = open;
        if self.settings != before {
            if self.settings.thumb != before.thumb {
                self.apply_thumb_size();
            }
            if self.settings.favourites != before.favourites {
                self.after_favourites_changed();
            }
            self.restyle();
            self.save_settings();
        }
    }

    /// A new thumbnail size: thumbnails are made again at that size.
    pub(super) fn apply_thumb_size(&mut self) {
        THUMB_PX.store(self.settings.thumb, Ordering::Relaxed);
        self.thumbs.clear();
    }

    pub(super) fn help_window(&mut self, ctx: &egui::Context) {
        if !self.help_open {
            return;
        }
        let mut open = true;
        egui::Window::new("Keyboard shortcuts").open(&mut open).default_width(360.0).show(ctx, |ui| {
            egui::Grid::new("keys").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                for (k, what) in SHORTCUTS {
                    ui.strong(k);
                    ui.label(what);
                    ui.end_row();
                }
            });
            ui.separator();
            ui.weak("Click selects; Ctrl+click adds or removes; Shift+click selects a range.");
        });
        self.help_open = open;
    }

    // ---- shortcuts ------------------------------------------------------------------------------------------

    pub(super) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused()).is_some() && ctx.egui_wants_keyboard_input();
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        if pressed(Modifiers::COMMAND, Key::F) {
            self.tab = Tab::Images;
            self.focus_search = true;
        }
        if pressed(Modifiers::COMMAND, Key::O) {
            if let Some(dir) = rfd::FileDialog::new().set_title("Game folder").pick_folder() {
                self.open_root(dir);
            }
        }
        if pressed(Modifiers::COMMAND, Key::Comma) {
            self.settings_open = !self.settings_open;
        }
        if pressed(Modifiers::NONE, Key::F1) {
            self.help_open = !self.help_open;
        }
        if typing {
            return;
        }
        if self.tab != Tab::Images {
            return;
        }
        if pressed(Modifiers::COMMAND, Key::A) {
            self.multi = self.visible.iter().copied().collect();
        }
        if pressed(Modifiers::NONE, Key::Escape) {
            self.multi.clear();
            self.batch_message = None;
        }
        if pressed(Modifiers::NONE, Key::F) {
            self.toggle_favourites();
        }
        let mut step: Option<(isize, bool)> = None;
        for (key, d) in [(Key::ArrowLeft, -1isize), (Key::ArrowRight, 1), (Key::ArrowUp, -(self.last_cols as isize)), (Key::ArrowDown, self.last_cols as isize)] {
            if pressed(Modifiers::NONE, key) {
                step = Some((d, false));
            }
            if pressed(Modifiers::SHIFT, key) {
                step = Some((d, true));
            }
        }
        if let Some((d, extend)) = step {
            self.move_selection(d, extend);
        }
    }

    /// Moves the selection through the visible list; with `extend` the range from the anchor becomes the selection.
    fn move_selection(&mut self, delta: isize, extend: bool) {
        if self.visible.is_empty() {
            return;
        }
        let pos = self.selected.and_then(|s| self.visible.iter().position(|&v| v == s));
        let target = match pos {
            Some(p) => (p as isize + delta).clamp(0, self.visible.len() as isize - 1) as usize,
            None => 0,
        };
        let entry = self.visible[target];
        if extend {
            let anchor = self.anchor.and_then(|a| self.visible.iter().position(|&v| v == a)).unwrap_or(pos.unwrap_or(target));
            let (lo, hi) = (anchor.min(target), anchor.max(target));
            self.multi = self.visible[lo..=hi].iter().copied().collect();
        } else {
            self.multi.clear();
            self.anchor = Some(entry);
        }
        self.select(entry);
        self.scroll_to = Some(target / self.last_cols.max(1));
    }

    /// A click on a thumbnail: plain selects, Ctrl toggles, Shift selects a range from the anchor.
    pub(super) fn click_entry(&mut self, ei: usize, m: Modifiers) {
        if m.command {
            if self.multi.is_empty() {
                if let Some(s) = self.selected {
                    self.multi.insert(s);
                }
            }
            if !self.multi.remove(&ei) {
                self.multi.insert(ei);
            }
            self.anchor = Some(ei);
        } else if m.shift {
            let a = self.anchor.or(self.selected).unwrap_or(ei);
            let (pa, pb) = (self.visible.iter().position(|&v| v == a), self.visible.iter().position(|&v| v == ei));
            if let (Some(pa), Some(pb)) = (pa, pb) {
                self.multi = self.visible[pa.min(pb)..=pa.max(pb)].iter().copied().collect::<BTreeSet<_>>();
            }
        } else {
            self.multi.clear();
            self.anchor = Some(ei);
        }
        self.select(ei);
    }
}

const SHORTCUTS: [(&str, &str); 12] = [
    ("Ctrl+F", "search the image names"),
    ("Ctrl+O", "open a game folder"),
    ("Ctrl+,", "settings"),
    ("F1", "this list"),
    ("Arrow keys", "move through the thumbnails"),
    ("Shift+arrows", "extend the selection"),
    ("Ctrl+A", "select everything shown"),
    ("Esc", "clear the selection"),
    ("F", "favourite or unfavourite the selection"),
    ("Click", "select one image"),
    ("Ctrl+click", "add or remove an image"),
    ("Shift+click", "select a range"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_lose_only_the_characters_windows_forbids() {
        assert_eq!(file_safe("~spec-rg&x~1f#0"), "~spec-rg&x~1f#0");
        assert_eq!(file_safe("a/b\\c:d*e?f\"g<h>i|j"), "a_b_c_d_e_f_g_h_i_j");
    }

    #[test]
    fn a_batch_writes_every_file_and_counts_failures() {
        struct Fake;
        impl Provider for Fake {
            fn label(&self) -> &str {
                "fake"
            }
            fn list(&self) -> Result<Vec<source::Listing>> {
                Ok(vec![])
            }
            fn read(&self, key: &str) -> Result<Vec<u8>> {
                if key == "bad" { anyhow::bail!("unreadable") } else { Ok(vec![1, 2, 3]) }
            }
        }
        let dir = std::env::temp_dir().join(format!("codol-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p: Arc<dyn Provider> = Arc::new(Fake);
        let items = vec![
            BatchItem { name: "good one".into(), provider: Arc::clone(&p), key: "k".into() },
            BatchItem { name: "bad:one".into(), provider: Arc::clone(&p), key: "bad".into() },
        ];
        let summary = run_batch(BatchKind::Iwi, items, dir.clone());
        assert!(summary.contains("Wrote 1 file(s)") && summary.contains("1 failed") && summary.contains("bad:one"), "{summary}");
        assert_eq!(std::fs::read(dir.join("good one.iwi")).unwrap(), vec![1, 2, 3]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
