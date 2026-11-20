//! The Sounds tab: the loaded sounds of one zone (Call of Duty 4, Modern Warfare 2, Modern Warfare 3), listed, played and
//! exported as WAV. They are read from the zone file, so no game has to be running.

use super::App;
use crate::log;
use crate::query::Query;
use crate::sound::{self, Sound};
use crate::soundview::{self, View};
use crate::zone::{export as zexport, fastfile};
use eframe::egui::{self, Color32, ColorImage, Sense, Stroke, TextureHandle};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Done {
    Exported,
    Failed,
}

/// Playback through the default output device, opened when the first sound plays.
#[derive(Default)]
struct Audio {
    sink: Option<rodio::MixerDeviceSink>,
    player: Option<rodio::Player>,
    started: Option<Instant>,
    error: Option<String>,
}

impl Audio {
    fn play(&mut self, s: &Sound) -> Result<(), String> {
        self.stop();
        let pcm = s.pcm16().ok_or_else(|| format!("{} is not played here; export it instead", s.format_name()))?;
        let ch = std::num::NonZeroU16::new(s.channels.max(1)).unwrap();
        let rate = std::num::NonZeroU32::new(s.rate.max(1)).ok_or("no sample rate")?;
        if self.sink.is_none() {
            let mut sink = rodio::DeviceSinkBuilder::open_default_sink().map_err(|e| format!("no audio output: {e}"))?;
            sink.log_on_drop(false);
            self.sink = Some(sink);
        }
        let samples: Vec<f32> = pcm.iter().map(|&v| v as f32 / 32768.0).collect();
        let player = rodio::Player::connect_new(self.sink.as_ref().unwrap().mixer());
        player.append(rodio::buffer::SamplesBuffer::new(ch, rate, samples));
        self.player = Some(player);
        self.started = Some(Instant::now());
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(p) = self.player.take() {
            p.stop();
        }
        self.started = None;
    }

    fn seek(&mut self, to: Duration) {
        if let Some(p) = &self.player {
            let _ = p.try_seek(to);
        }
    }

    fn playing(&self) -> bool {
        self.player.as_ref().is_some_and(|p| !p.empty() && !p.is_paused())
    }

    fn position(&self) -> Option<Duration> {
        self.player.as_ref().filter(|p| !p.empty()).map(|p| p.get_pos())
    }
}

#[derive(Default)]
pub(super) struct SoundState {
    zones: Vec<(String, PathBuf)>,
    for_root: Option<PathBuf>,
    zone: Option<usize>,
    sounds: Vec<Arc<Sound>>,
    filter: String,
    applied: Option<(String, usize)>,
    visible: Vec<usize>,
    selected: Option<usize>,
    pub(super) message: Option<String>,
    rx: Option<Receiver<Result<Vec<Sound>, String>>>,
    export_rx: Option<Receiver<(Vec<(String, Done)>, String)>>,
    audio: Audio,
    /// What was worked out about the selected sound: (sound index, figures and curves, the spectrogram as a texture).
    view: Option<(usize, Arc<View>, Option<TextureHandle>)>,
    visual: Visual,
    /// What an export did to each sound, by name.
    done: HashMap<String, Done>,
    /// Bumped whenever the list changes, so the filter runs again.
    generation: usize,
    /// `ATLAS_SOUND_ZONE=<end of the zone's path>`: the zone to open when the list is there (for screenshots).
    pending: Option<String>,
    switched: u8,
    switch_to: Option<usize>,
}

impl SoundState {
    pub(super) fn is_busy(&self) -> bool {
        self.rx.is_some() || self.export_rx.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Visual {
    #[default]
    Waveform,
    Spectrogram,
    Spectrum,
    Envelope,
}

impl Visual {
    /// `ATLAS_SOUND_VISUAL=spectrogram|spectrum|loudness`: the visual to open with (for screenshots).
    fn from_env() -> Visual {
        match crate::config::env_var("SOUND_VISUAL").ok().as_deref() {
            Some("spectrogram") => Visual::Spectrogram,
            Some("spectrum") => Visual::Spectrum,
            Some("loudness") => Visual::Envelope,
            _ => Visual::Waveform,
        }
    }

    const ALL: [(Visual, &'static str); 4] = [(Visual::Waveform, "Waveform"), (Visual::Spectrogram, "Spectrogram"), (Visual::Spectrum, "Spectrum"), (Visual::Envelope, "Loudness")];
}

fn clock(seconds: f32) -> String {
    let ms = (seconds * 1000.0).round() as u64;
    format!("{}:{:02}.{:03}", ms / 60000, ms / 1000 % 60, ms % 1000)
}

fn db_text(v: f32) -> String {
    if v.is_finite() { format!("{:.1} dB", v + 0.0).replace("-0.0", "0.0") } else { "-inf".to_string() }
}

fn spectrogram_image(v: &View) -> ColorImage {
    let (w, h) = (v.spec_cols, soundview::SPEC_BINS);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let c = soundview::heat(v.spectrogram[x * h + (h - 1 - y)]);
            rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    ColorImage::from_rgba_unmultiplied([w, h], &rgba)
}

impl App {
    fn load_sound_zones(&mut self) {
        let Some(root) = self.root.clone() else { return };
        if self.sounds.for_root.as_ref() == Some(&root) {
            return;
        }
        let mut zones: Vec<(String, PathBuf)> = zexport::survey_files(&root)
            .into_iter()
            .filter(|p| fastfile::peek(p).ok().and_then(|k| sound::game_of(&k)).is_some())
            .map(|p| {
                let rel = p.strip_prefix(&root).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                (rel, p)
            })
            .collect();
        zones.sort_by(|a, b| a.0.to_ascii_lowercase().cmp(&b.0.to_ascii_lowercase()));
        self.sounds = SoundState { zones, for_root: Some(root), pending: crate::config::env_var("SOUND_ZONE").ok(), visual: Visual::from_env(), ..Default::default() };
    }

    fn start_sound_load(&mut self, zone: usize) {
        let Some((name, path)) = self.sounds.zones.get(zone).cloned() else { return };
        self.sounds.zone = Some(zone);
        self.sounds.sounds.clear();
        // the filtered list still holds indexes into the old sounds: drop it now, not at the next filter run
        self.sounds.visible.clear();
        self.sounds.applied = None;
        self.sounds.selected = None;
        self.sounds.view = None;
        self.sounds.audio.stop();
        self.sounds.generation += 1;
        self.sounds.message = Some(format!("Reading {name} …"));
        log::log(format!("sounds: reading {name}"));
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let r = sound::read_zone(&path).map_err(|e| format!("{e:#}"));
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.sounds.rx = Some(rx);
    }

    fn poll_sounds(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.sounds.rx {
            ctx.request_repaint_after(Duration::from_millis(200));
            if let Ok(r) = rx.try_recv() {
                self.sounds.rx = None;
                match r {
                    Ok(list) => {
                        let m = format!("{} sounds", list.len());
                        log::log(format!("sounds: {m}"));
                        self.sounds.message = Some(m);
                        self.sounds.sounds = list.into_iter().map(Arc::new).collect();
                        self.sounds.sounds.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
                        self.sounds.generation += 1;
                        // development aid: `ATLAS_SOUND_SWITCH=<zone>` opens a second zone once the first has loaded (for reproducing zone switches)
                        if let Some(next) = crate::config::env_var("SOUND_SWITCH").ok().filter(|_| self.sounds.pending.is_none() && self.sounds.switched == 0) {
                            self.sounds.switched = 1;
                            if let Some(i) = self.sounds.zones.iter().position(|z| z.0.to_ascii_lowercase().ends_with(&next.to_ascii_lowercase())) {
                                self.sounds.switch_to = Some(i);
                                return;
                            }
                        }
                        if self.capture.is_some() {
                            if let Some(i) = (0..self.sounds.sounds.len()).find(|&i| self.sounds.sounds[i].frames().is_some_and(|f| f > 2000)) {
                                self.select_sound(i, false);
                            }
                        }
                    }
                    Err(e) => {
                        log::log(format!("sounds: {e}"));
                        self.sounds.message = Some(e);
                    }
                }
            }
        }
        if let Some(rx) = &self.sounds.export_rx {
            if let Ok((marks, msg)) = rx.try_recv() {
                self.sounds.export_rx = None;
                for (n, d) in marks {
                    self.sounds.done.insert(n, d);
                }
                log::log(format!("sounds: {msg}"));
                self.sounds.message = Some(msg);
            }
        }
    }

    fn refilter_sounds(&mut self) {
        let key = (self.sounds.filter.clone(), self.sounds.generation);
        if self.sounds.applied.as_ref() == Some(&key) {
            return;
        }
        let q = Query::parse(&self.sounds.filter, sound::PROPERTIES);
        self.sounds.visible = (0..self.sounds.sounds.len())
            .filter(|&i| {
                let s = &self.sounds.sounds[i];
                q.matches(&s.name.to_lowercase(), |p| sound::property(s, p))
            })
            .collect();
        self.sounds.applied = Some(key);
    }

    fn select_sound(&mut self, i: usize, play: bool) {
        self.sounds.selected = Some(i);
        if let Some(s) = self.sounds.sounds.get(i).cloned() {
            let view = s.pcm16().and_then(|p| soundview::analyse(&p, s.channels.max(1) as usize, s.rate)).map(Arc::new);
            self.sounds.view = view.map(|v| {
                let tex = self.ctx.load_texture("sound_spectrogram", spectrogram_image(&v), egui::TextureOptions::LINEAR);
                (i, v, Some(tex))
            });
            if play {
                self.play_sound(&s);
            }
        }
    }

    fn play_sound(&mut self, s: &Sound) {
        if let Err(e) = self.sounds.audio.play(s) {
            self.sounds.audio.error = Some(e);
        } else {
            self.sounds.audio.error = None;
        }
    }

    fn export_sounds(&mut self, which: Vec<usize>) {
        let Some(dir) = rfd::FileDialog::new().set_title("Folder for the sounds").pick_folder() else { return };
        let list: Vec<Arc<Sound>> = which.iter().filter_map(|&i| self.sounds.sounds.get(i).cloned()).collect();
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.sounds.message = Some(format!("Writing {} sounds …", list.len()));
        std::thread::spawn(move || {
            let mut marks = Vec::new();
            for s in &list {
                let r = sound::write_all(&[s.as_ref()], &dir);
                marks.push((s.name.clone(), if r.is_ok() { Done::Exported } else { Done::Failed }));
            }
            let failed = marks.iter().filter(|(_, d)| *d == Done::Failed).count();
            let msg = format!("{} sounds written to {}{}", list.len() - failed, dir.display(), if failed > 0 { format!(", {failed} failed") } else { String::new() });
            let _ = tx.send((marks, msg));
            ctx.request_repaint();
        });
        self.sounds.export_rx = Some(rx);
    }

    fn export_one_sound(&mut self, i: usize) {
        let Some(s) = self.sounds.sounds.get(i).cloned() else { return };
        let (ext, bytes) = s.export_bytes();
        let stem = s.name.rsplit(['/', '\\']).next().unwrap_or(&s.name).to_string();
        let Some(path) = rfd::FileDialog::new().set_file_name(format!("{stem}.{ext}")).add_filter(ext, &[ext]).save_file() else { return };
        let r = std::fs::write(&path, bytes);
        self.sounds.done.insert(s.name.clone(), if r.is_ok() { Done::Exported } else { Done::Failed });
        self.sounds.message = Some(match r {
            Ok(()) => format!("{} written", path.display()),
            Err(e) => format!("{}: {e}", path.display()),
        });
    }

    pub(super) fn sounds_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.load_sound_zones();
        self.poll_sounds(&ctx);
        if let Some(want) = self.sounds.pending.take() {
            let want = want.replace('\\', "/").to_ascii_lowercase();
            if let Some(i) = self.sounds.zones.iter().position(|z| z.0.to_ascii_lowercase().ends_with(&want)) {
                self.start_sound_load(i);
            }
        }
        self.refilter_sounds();
        // after the filter and before the rows, like a click on the zone box
        if let Some(i) = self.sounds.switch_to.take() {
            self.start_sound_load(i);
        }
        if self.root.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label("Open a game folder first.");
            });
            return;
        }
        let busy = self.sounds.rx.is_some();
        ui.horizontal_wrapped(|ui| {
            let current = self.sounds.zone.and_then(|z| self.sounds.zones.get(z)).map_or("Choose a zone…".to_string(), |z| z.0.clone());
            let mut pick = None;
            egui::ComboBox::from_id_salt("sound_zone").width(280.0).selected_text(current).show_ui(ui, |ui| {
                for (i, (name, _)) in self.sounds.zones.iter().enumerate() {
                    if ui.selectable_label(self.sounds.zone == Some(i), name).clicked() {
                        pick = Some(i);
                    }
                }
            });
            if let Some(i) = pick {
                self.start_sound_load(i);
            }
            ui.weak(format!("{} zones with loaded sounds", self.sounds.zones.len()));
            ui.separator();
            let r = ui.add(egui::TextEdit::singleline(&mut self.sounds.filter).hint_text("filter…  length:>2  rate:44100  !name").desired_width(260.0));
            if self.focus_search {
                self.focus_search = false;
                r.request_focus();
            }
            ui.separator();
            let sel = self.sounds.selected;
            if ui.add_enabled(sel.is_some(), egui::Button::new("Export…")).on_hover_text("E").clicked() {
                if let Some(i) = sel {
                    self.export_one_sound(i);
                }
            }
            if ui.add_enabled(!self.sounds.visible.is_empty() && self.sounds.export_rx.is_none(), egui::Button::new(format!("Export {} shown…", self.sounds.visible.len()))).clicked() {
                self.export_sounds(self.sounds.visible.clone());
            }
            if busy {
                ui.spinner();
            }
        });
        ui.separator();

        // keys, unless a text box has the keyboard
        if !ctx.egui_wants_keyboard_input() {
            let (space, up, down, e, copy) = ctx.input(|i| (i.key_pressed(egui::Key::Space), i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::E) && !i.modifiers.command, i.modifiers.command && i.key_pressed(egui::Key::C)));
            let pos = self.sounds.selected.and_then(|s| self.sounds.visible.iter().position(|&v| v == s));
            if down || up {
                let next = match (pos, down) {
                    (Some(p), true) => (p + 1).min(self.sounds.visible.len().saturating_sub(1)),
                    (Some(p), false) => p.saturating_sub(1),
                    (None, _) => 0,
                };
                if let Some(&i) = self.sounds.visible.get(next) {
                    self.select_sound(i, true);
                }
            }
            if space {
                if let Some(s) = self.sounds.selected.and_then(|i| self.sounds.sounds.get(i).cloned()) {
                    if self.sounds.audio.playing() {
                        self.sounds.audio.stop();
                    } else {
                        self.play_sound(&s);
                    }
                }
            }
            if e {
                if let Some(i) = self.sounds.selected {
                    self.export_one_sound(i);
                }
            }
            if copy {
                if let Some(s) = self.sounds.selected.and_then(|i| self.sounds.sounds.get(i)) {
                    ctx.copy_text(s.name.clone());
                }
            }
        }
        if self.sounds.audio.playing() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        egui::Panel::right("sound_meta").resizable(true).default_size(330.0).show(ui, |ui| self.sound_details(ui));
        egui::Panel::bottom("sound_player").resizable(false).show(ui, |ui| self.sound_player(ui));
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, self.sounds.visible.len(), |ui, range| {
            for k in range {
                let Some(&i) = self.sounds.visible.get(k) else { continue };
                let Some(s) = self.sounds.sounds.get(i).cloned() else { continue };
                let mark = match self.sounds.done.get(&s.name) {
                    Some(Done::Exported) => "+",
                    Some(Done::Failed) => "!",
                    None => " ",
                };
                let len = s.seconds().map_or("?".to_string(), |l| format!("{l:.2}s"));
                let text = format!("{mark} {:<52} {:<11} {:>6} Hz {}ch {:>8}", s.name, s.format_name(), s.rate, s.channels, len);
                if ui.selectable_label(self.sounds.selected == Some(i), egui::RichText::new(text).monospace()).clicked() {
                    self.select_sound(i, true);
                }
            }
        });
    }

    /// The panel on the right: the header figures and what the analysis found.
    fn sound_details(&mut self, ui: &mut egui::Ui) {
        let Some(s) = self.sounds.selected.and_then(|i| self.sounds.sounds.get(i)).cloned() else {
            ui.weak("Select a sound.");
            return;
        };
        ui.add(egui::Label::new(egui::RichText::new(&s.name).strong()).wrap());
        ui.horizontal(|ui| {
            let playing = self.sounds.audio.playing();
            if ui.button(if playing { "Stop" } else { "Play" }).clicked() {
                if playing {
                    self.sounds.audio.stop();
                } else {
                    self.play_sound(&s);
                }
            }
            if ui.button("Export…").clicked() {
                if let Some(i) = self.sounds.selected {
                    self.export_one_sound(i);
                }
            }
            if ui.button("Copy name").clicked() {
                ui.ctx().copy_text(s.name.clone());
            }
        });
        ui.separator();
        let category = s.name.split(['/', '\\']).next().filter(|c| *c != s.name).unwrap_or("-").to_string();
        let seconds = s.seconds();
        let kbps = seconds.filter(|&l| l > 0.0).map(|l| format!("{:.0} kbps", s.data.len() as f32 * 8.0 / l / 1000.0));
        egui::Grid::new("sound_header").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            let mut row = |ui: &mut egui::Ui, k: &str, v: String| {
                ui.weak(k);
                ui.label(v);
                ui.end_row();
            };
            row(ui, "category", category);
            row(ui, "zone", s.zone.clone());
            row(ui, "codec", format!("{} (tag {:#x})", s.format_name(), s.format));
            row(ui, "rate", format!("{} Hz", s.rate));
            row(ui, "channels", s.channels.to_string());
            if s.bits > 0 {
                row(ui, "bits", s.bits.to_string());
            }
            if s.block_align > 0 {
                row(ui, "block", format!("{} bytes", s.block_align));
            }
            row(ui, "length", seconds.map_or("?".into(), clock));
            row(ui, "frames", s.frames().map_or("?".into(), |f| f.to_string()));
            row(ui, "size", format!("{:.1} KiB", s.data.len() as f32 / 1024.0));
            if let Some(k) = kbps {
                row(ui, "bitrate", k);
            }
        });
        let Some((vi, view, _)) = &self.sounds.view else {
            ui.separator();
            ui.weak("Not analysed: this format is not decoded here.");
            return;
        };
        if Some(*vi) != self.sounds.selected {
            return;
        }
        let st = &view.stats;
        ui.separator();
        egui::Grid::new("sound_stats").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
            let mut row = |ui: &mut egui::Ui, k: &str, v: String| {
                ui.weak(k);
                ui.label(v);
                ui.end_row();
            };
            row(ui, "peak", db_text(st.peak_db));
            row(ui, "rms", db_text(st.rms_db));
            row(ui, "centroid", format!("{:.0} Hz", st.centroid_hz));
            row(ui, "crossings", st.crossings.to_string());
            row(ui, "silent", if st.silent { "yes" } else { "no" }.to_string());
            row(ui, "decoded", format!("{} frames, {}", st.frames, clock(st.seconds)));
            row(ui, "attack", format!("{:.3}s", st.attack_s));
            row(ui, "head / tail", format!("{:.3}s / {:.3}s", st.head_s, st.tail_s));
        });
    }

    /// The strip at the bottom: transport and one of the visuals of the selected sound.
    fn sound_player(&mut self, ui: &mut egui::Ui) {
        let Some(i) = self.sounds.selected else {
            ui.weak("Select a sound to play it: ↑ ↓ to move, Space to play or stop, E to export, Ctrl+C to copy the name.");
            return;
        };
        let Some(s) = self.sounds.sounds.get(i).cloned() else { return };
        ui.horizontal(|ui| {
            if ui.button(if self.sounds.audio.playing() { "Stop" } else { "Play" }).clicked() {
                if self.sounds.audio.playing() {
                    self.sounds.audio.stop();
                } else {
                    self.play_sound(&s);
                }
            }
            let pos = self.sounds.audio.position().filter(|_| self.sounds.audio.playing());
            ui.monospace(format!("{} / {}", clock(pos.map_or(0.0, |p| p.as_secs_f32())), s.seconds().map_or("?".into(), clock)));
            ui.separator();
            for (v, name) in Visual::ALL {
                ui.selectable_value(&mut self.sounds.visual, v, name);
            }
            if self.sounds.visual != Visual::Spectrum {
                ui.weak("click to seek while playing");
            }
        });
        if let Some(e) = &self.sounds.audio.error {
            ui.colored_label(Color32::from_rgb(220, 120, 80), e);
        }
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 150.0), Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
        let accent = Color32::from_rgb(self.settings.accent[0], self.settings.accent[1], self.settings.accent[2]);
        let Some((vi, view, tex)) = self.sounds.view.clone() else {
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "not decoded here", egui::FontId::proportional(13.0), Color32::from_gray(120));
            return;
        };
        if vi != i {
            return;
        }
        let grid = Stroke::new(1.0, Color32::from_white_alpha(18));
        let label = |p: egui::Pos2, t: String| painter.text(p, egui::Align2::LEFT_TOP, t, egui::FontId::monospace(10.0), Color32::from_gray(140));
        match self.sounds.visual {
            Visual::Waveform => {
                let (mid, half) = (rect.center().y, rect.height() / 2.0 - 4.0);
                painter.line_segment([egui::pos2(rect.left(), mid), egui::pos2(rect.right(), mid)], grid);
                for (k, v) in view.peaks.iter().enumerate() {
                    let x = rect.left() + (k as f32 + 0.5) / view.peaks.len() as f32 * rect.width();
                    painter.line_segment([egui::pos2(x, mid - v * half), egui::pos2(x, mid + v * half)], Stroke::new(1.0, accent));
                }
            }
            Visual::Spectrogram => {
                if let Some(t) = &tex {
                    painter.image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
                let nyq = view.rate as f32 / 2.0;
                for f in [1000.0f32, 5000.0, 10000.0, 15000.0, 20000.0] {
                    if f < nyq {
                        let y = rect.bottom() - f / nyq * rect.height();
                        painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], grid);
                        label(egui::pos2(rect.left() + 3.0, y), format!("{} kHz", f / 1000.0));
                    }
                }
            }
            Visual::Envelope => {
                let n = view.envelope_db.len().max(2);
                for d in [-20.0f32, -40.0, -60.0, -80.0] {
                    let y = rect.top() + d / soundview::FLOOR_DB * rect.height();
                    painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], grid);
                    label(egui::pos2(rect.left() + 3.0, y), format!("{d} dB"));
                }
                let pts: Vec<egui::Pos2> = view.envelope_db.iter().enumerate().map(|(k, d)| egui::pos2(rect.left() + k as f32 / (n - 1) as f32 * rect.width(), rect.top() + d.min(0.0) / soundview::FLOOR_DB * rect.height())).collect();
                for p in &pts {
                    painter.line_segment([*p, egui::pos2(p.x, rect.bottom())], Stroke::new(1.0, accent.gamma_multiply(0.35)));
                }
                painter.add(egui::Shape::line(pts, Stroke::new(1.5, accent)));
            }
            Visual::Spectrum => {
                let n = view.spectrum_db.len().max(1);
                let nyq = view.rate as f32 / 2.0;
                for d in [-20.0f32, -40.0, -60.0, -80.0] {
                    let y = rect.top() + d / soundview::FLOOR_DB * rect.height();
                    painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], grid);
                    label(egui::pos2(rect.left() + 3.0, y), format!("{d} dB"));
                }
                let bw = rect.width() / n as f32;
                for (k, d) in view.spectrum_db.iter().enumerate() {
                    let top = rect.top() + d.min(0.0) / soundview::FLOOR_DB * rect.height();
                    let x = rect.left() + k as f32 * bw;
                    painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x + 0.5, top), egui::pos2(x + bw - 0.5, rect.bottom())), 0.0, accent.gamma_multiply(0.85));
                }
                // frequency marks on the log axis, 20 Hz to Nyquist
                let (lo, hi) = (20f32.ln(), nyq.max(40.0).ln());
                for f in [100.0f32, 1000.0, 10000.0] {
                    if f < nyq {
                        let x = rect.left() + (f.ln() - lo) / (hi - lo) * rect.width();
                        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], grid);
                        label(egui::pos2(x + 3.0, rect.bottom() - 13.0), if f >= 1000.0 { format!("{} kHz", f / 1000.0) } else { format!("{f} Hz") });
                    }
                }
            }
        }
        // playhead for the time-based visuals, and a click to seek
        if self.sounds.visual != Visual::Spectrum {
            if let Some(len) = s.seconds().filter(|l| *l > 0.0) {
                if let Some(pos) = self.sounds.audio.position().filter(|_| self.sounds.audio.playing()) {
                    let x = rect.left() + (pos.as_secs_f32() / len).clamp(0.0, 1.0) * rect.width();
                    painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], Stroke::new(1.5, Color32::WHITE));
                }
                if resp.clicked() && self.sounds.audio.playing() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let t = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * len;
                        self.sounds.audio.seek(Duration::from_secs_f32(t));
                    }
                }
            }
        }
    }
}
