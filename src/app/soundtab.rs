//! The Sounds tab: the loaded sounds of one zone (Call of Duty 4, Modern Warfare 2, Modern Warfare 3), listed, played and
//! exported as WAV. They are read from the zone file, so no game has to be running.

use super::App;
use crate::log;
use crate::query::Query;
use crate::sound::{self, Sound};
use crate::zone::{export as zexport, fastfile};
use eframe::egui::{self, Color32, Sense, Stroke};
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
    message: Option<String>,
    rx: Option<Receiver<Result<Vec<Sound>, String>>>,
    export_rx: Option<Receiver<(Vec<(String, Done)>, String)>>,
    audio: Audio,
    /// Waveform of the selected sound: (sound index, peaks 0..1).
    wave: Option<(usize, Vec<f32>)>,
    /// What an export did to each sound, by name.
    done: HashMap<String, Done>,
    /// Bumped whenever the list changes, so the filter runs again.
    generation: usize,
    /// `ATLAS_SOUND_ZONE=<end of the zone's path>`: the zone to open when the list is there (for screenshots).
    pending: Option<String>,
}

impl SoundState {
    pub(super) fn is_busy(&self) -> bool {
        self.rx.is_some() || self.export_rx.is_some()
    }
}

fn peaks(s: &Sound, buckets: usize) -> Vec<f32> {
    let Some(pcm) = s.pcm16() else { return Vec::new() };
    let ch = s.channels.max(1) as usize;
    let frames = pcm.len() / ch;
    if frames == 0 {
        return Vec::new();
    }
    (0..buckets)
        .map(|b| {
            let (a, z) = (b * frames / buckets, ((b + 1) * frames / buckets).max(b * frames / buckets + 1).min(frames));
            let mut m = 0i32;
            for f in a..z {
                for c in 0..ch {
                    m = m.max((pcm[f * ch + c] as i32).abs());
                }
            }
            m as f32 / 32768.0
        })
        .collect()
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
        self.sounds = SoundState { zones, for_root: Some(root), pending: crate::config::env_var("SOUND_ZONE").ok(), ..Default::default() };
    }

    fn start_sound_load(&mut self, zone: usize) {
        let Some((name, path)) = self.sounds.zones.get(zone).cloned() else { return };
        self.sounds.zone = Some(zone);
        self.sounds.sounds.clear();
        self.sounds.selected = None;
        self.sounds.wave = None;
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
            self.sounds.wave = Some((i, peaks(&s, 600)));
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
            if let Some(m) = &self.sounds.message {
                ui.weak(m);
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

        egui::Panel::bottom("sound_player").resizable(false).show(ui, |ui| self.sound_player(ui));
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, self.sounds.visible.len(), |ui, range| {
            for k in range {
                let i = self.sounds.visible[k];
                let s = self.sounds.sounds[i].clone();
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
            ui.strong(&s.name);
            ui.weak(format!("{} · {} Hz · {} ch · {} KiB · {}", s.format_name(), s.rate, s.channels, s.data.len() / 1024, s.zone));
            if ui.button("Copy name").clicked() {
                ui.ctx().copy_text(s.name.clone());
            }
        });
        if let Some(e) = &self.sounds.audio.error {
            ui.colored_label(Color32::from_rgb(220, 120, 80), e);
        }
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 70.0), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
        if let Some((wi, p)) = &self.sounds.wave {
            if *wi == i && !p.is_empty() {
                let (mid, half) = (rect.center().y, rect.height() / 2.0 - 3.0);
                let accent = Color32::from_rgb(self.settings.accent[0], self.settings.accent[1], self.settings.accent[2]);
                for (k, v) in p.iter().enumerate() {
                    let x = rect.left() + (k as f32 + 0.5) / p.len() as f32 * rect.width();
                    painter.line_segment([egui::pos2(x, mid - v * half), egui::pos2(x, mid + v * half)], Stroke::new(1.0, accent));
                }
                if let (Some(pos), Some(len)) = (self.sounds.audio.position(), s.seconds()) {
                    if len > 0.0 {
                        let x = rect.left() + (pos.as_secs_f32() / len).clamp(0.0, 1.0) * rect.width();
                        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], Stroke::new(1.5, Color32::WHITE));
                    }
                }
            }
        }
    }
}
