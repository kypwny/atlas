//! The Materials tab: the game's materials from the model index with the images each one uses, exported as a JSON description
//! plus those images (in the format of the settings), so a material can be rebuilt in another program.

use super::App;
use crate::imgexport;
use crate::iwi::{self, Header};
use crate::log;
use crate::modelindex::{self, Material};
use crate::query::Query;
use crate::source::Catalog;
use anyhow::{Context, Result};
use eframe::egui;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

const PROPS: &[&str] = &["textures", "images"];

#[derive(Default)]
pub(super) struct MaterialState {
    filter: String,
    applied: Option<(String, usize)>,
    visible: Vec<usize>,
    selected: Option<String>,
    pub(super) message: Option<String>,
    /// Which index the visible list was made from (its address), so a rebuilt index refreshes it.
    index_key: usize,
}

fn semantic_name(s: u8) -> String {
    match s {
        modelindex::SEMANTIC_COLOR => "colour".into(),
        modelindex::SEMANTIC_DETAIL => "detail".into(),
        modelindex::SEMANTIC_NORMAL => "normal".into(),
        modelindex::SEMANTIC_SPECULAR => "specular".into(),
        other => format!("semantic {other}"),
    }
}

fn image_count(m: &Material) -> usize {
    m.textures.iter().filter(|t| t.image.is_some()).count()
}

fn prop(m: &Material, name: &str) -> Option<f64> {
    match name {
        "textures" => Some(m.textures.len() as f64),
        "images" => Some(image_count(m) as f64),
        _ => None,
    }
}

/// The JSON that describes a material.
pub(super) fn material_json(m: &Material) -> serde_json::Value {
    json!({
        "name": m.name,
        "techset": m.techset,
        "render": m.render.map(|r| r.describe()),
        "textures": m.textures.iter().map(|t| json!({
            "semantic": semantic_name(t.semantic),
            "semantic_id": t.semantic,
            "sampler": t.sampler,
            "image": t.image,
        })).collect::<Vec<_>>(),
        "constants": m.constants.iter().map(|(n, v)| json!({"name": n, "value": v})).collect::<Vec<_>>(),
    })
}

/// Writes one image of the catalogue under `dir` in the chosen format; `Ok(false)` when the catalogue does not have it.
fn write_image(cat: &Catalog, name: &str, dir: &Path, format: imgexport::ImageFormat, fix: imgexport::NormalFix) -> Result<bool> {
    let key = name.to_ascii_lowercase();
    let Ok(i) = cat.entries.binary_search_by(|e| e.name.as_str().cmp(key.as_str())) else { return Ok(false) };
    let entry = &cat.entries[i];
    let Some(v) = entry.variants.iter().filter(|v| v.header.is_some()).max_by_key(|v| v.header.as_ref().map_or(0, |h| h.width as u64 * h.height as u64)) else { return Ok(false) };
    let data = cat.providers[v.provider].read(&v.key)?;
    let h = Header::parse(&data)?;
    let level = iwi::decode_level(&data, &h, 0)?;
    let normal = imgexport::looks_like_normal_map(&entry.name, h.is_normal_map());
    imgexport::write_face(&dir.join(format!("{}.{}", super::extras::file_safe(&entry.name), format.extension())), &level, 0, format, normal, fix).context(name.to_string())?;
    Ok(true)
}

/// Writes materials as `materials/<name>.json` and their images as `images/<image>.<ext>` under `out`. Returns (materials, images
/// written, images the catalogue did not have).
pub(super) fn export_materials(mats: &[&Material], cat: Option<&Catalog>, out: &Path, format: imgexport::ImageFormat, fix: imgexport::NormalFix) -> Result<(usize, usize, usize)> {
    let mdir = out.join("materials");
    let idir = out.join("images");
    std::fs::create_dir_all(&mdir)?;
    let mut seen = BTreeSet::new();
    let (mut written, mut missing) = (0usize, 0usize);
    for m in mats {
        let json = serde_json::to_string_pretty(&material_json(m))?;
        std::fs::write(mdir.join(format!("{}.json", super::extras::file_safe(&m.name))), json)?;
        let Some(cat) = cat else { continue };
        for t in &m.textures {
            let Some(img) = &t.image else { continue };
            if !seen.insert(img.to_ascii_lowercase()) {
                continue;
            }
            std::fs::create_dir_all(&idir)?;
            match write_image(cat, img, &idir, format, fix) {
                Ok(true) => written += 1,
                Ok(false) => missing += 1,
                Err(_) => missing += 1,
            }
        }
    }
    Ok((mats.len(), written, missing))
}

impl App {
    pub(super) fn materials_ui(&mut self, ui: &mut egui::Ui) {
        let Some(index) = self.models.index() else {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                ui.label("Materials come from the model index, which reads the game's zones.");
                if ui.add_enabled(self.root.is_some() && !self.models.is_busy(), egui::Button::new("Build the model index")).clicked() {
                    self.models.build_index();
                }
                if self.root.is_none() {
                    ui.weak("Open a game folder first.");
                }
            });
            return;
        };
        let all = index.materials();
        let key = Arc::as_ptr(&index) as usize;
        if self.materials.index_key != key {
            self.materials.index_key = key;
            self.materials.applied = None;
        }
        let ftext = self.materials.filter.clone();
        if self.materials.applied.as_ref().map(|a| &a.0) != Some(&ftext) {
            let q = Query::parse(&ftext, PROPS);
            self.materials.visible = (0..all.len()).filter(|&i| q.matches(&all[i].name.to_lowercase(), |p| prop(all[i], p))).collect();
            self.materials.applied = Some((ftext, 0));
        }
        ui.horizontal_wrapped(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut self.materials.filter).hint_text("filter…  images:>2  !decal").desired_width(260.0));
            if self.focus_search {
                self.focus_search = false;
                r.request_focus();
            }
            ui.weak(format!("{} of {} materials", self.materials.visible.len(), all.len()));
            ui.separator();
            let (format, fix) = (self.settings.image_format, self.settings.normal_fix);
            let sel = self.materials.selected.as_ref().and_then(|n| all.iter().find(|m| &m.name == n).copied());
            if ui.add_enabled(sel.is_some(), egui::Button::new("Export…")).on_hover_text(format!("Writes the material as JSON and its images as {}", format.label())).clicked() {
                if let (Some(m), Some(dir)) = (sel, rfd::FileDialog::new().set_title("Folder for the material").pick_folder()) {
                    self.finish_material_export(&[m], &dir, format, fix);
                }
            }
            if ui.add_enabled(!self.materials.visible.is_empty(), egui::Button::new(format!("Export {} shown…", self.materials.visible.len()))).clicked() {
                if let Some(dir) = rfd::FileDialog::new().set_title("Folder for the materials").pick_folder() {
                    let list: Vec<&Material> = self.materials.visible.iter().map(|&i| all[i]).collect();
                    self.finish_material_export(&list, &dir, format, fix);
                }
            }
        });
        ui.separator();

        let sel = self.materials.selected.clone().and_then(|n| all.iter().find(|m| m.name == n).copied());
        egui::Panel::right("material_details").resizable(true).default_size(420.0).show(ui, |ui| match sel {
            None => {
                ui.weak("Select a material.");
            }
            Some(m) => {
                ui.horizontal(|ui| {
                    ui.heading(&m.name);
                    if ui.button("Copy name").clicked() {
                        ui.ctx().copy_text(m.name.clone());
                    }
                });
                if let Some(t) = &m.techset {
                    ui.weak(format!("techset {t}"));
                }
                if let Some(r) = m.render {
                    ui.label(r.describe());
                }
                ui.separator();
                for t in &m.textures {
                    ui.horizontal(|ui| {
                        ui.weak(format!("{:<9}", semantic_name(t.semantic)));
                        match &t.image {
                            Some(img) => {
                                if ui.link(img).on_hover_text("Show in the Images tab").clicked() {
                                    self.filters.text = img.clone();
                                    self.applied = None;
                                    self.tab = super::Tab::Images;
                                }
                            }
                            None => {
                                ui.weak(format!("(hash {:08x})", t.hash));
                            }
                        }
                    });
                }
                if !m.constants.is_empty() {
                    ui.separator();
                    for (n, v) in &m.constants {
                        ui.weak(format!("{n}: {:.3} {:.3} {:.3} {:.3}", v[0], v[1], v[2], v[3]));
                    }
                }
            }
        });
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, self.materials.visible.len(), |ui, range| {
            for k in range {
                let m = all[self.materials.visible[k]];
                let text = format!("{:<54} {} images  {}", m.name, image_count(m), m.render.map_or(String::new(), |r| r.describe()));
                if ui.selectable_label(self.materials.selected.as_deref() == Some(m.name.as_str()), egui::RichText::new(text).monospace()).clicked() {
                    self.materials.selected = Some(m.name.clone());
                }
            }
        });
    }

    fn finish_material_export(&mut self, mats: &[&Material], dir: &Path, format: imgexport::ImageFormat, fix: imgexport::NormalFix) {
        let cat = self.catalog.clone();
        let msg = match export_materials(mats, cat.as_deref(), dir, format, fix) {
            Ok((m, i, miss)) => format!("{m} material(s) and {i} image(s) written to {}{}", dir.display(), if miss > 0 { format!("; {miss} image(s) not found in the Images tab's catalogue") } else { String::new() }),
            Err(e) => format!("{e:#}"),
        };
        log::log(format!("materials: {msg}"));
        self.materials.message = Some(msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modelindex::Texture;

    #[test]
    fn a_material_describes_itself_as_json() {
        let m = Material {
            name: "wpn_ak47".into(),
            textures: vec![Texture { semantic: modelindex::SEMANTIC_COLOR, sampler: 1, hash: 7, image: Some("ak47_col".into()) }, Texture { semantic: modelindex::SEMANTIC_NORMAL, sampler: 1, hash: 8, image: None }],
            techset: Some("wc_lit".into()),
            render: None,
            constants: vec![("envMapParms".into(), [1.0, 0.0, 0.0, 0.0])],
        };
        let j = material_json(&m);
        assert_eq!(j["name"], "wpn_ak47");
        assert_eq!(j["textures"][0]["semantic"], "colour");
        assert_eq!(j["textures"][0]["image"], "ak47_col");
        assert!(j["textures"][1]["image"].is_null());
        assert_eq!(image_count(&m), 1);
        assert_eq!(prop(&m, "images"), Some(1.0));
        let dir = std::env::temp_dir().join(format!("atlas-mat-{}", std::process::id()));
        let (n, imgs, missing) = export_materials(&[&m], None, &dir, imgexport::ImageFormat::Png, imgexport::NormalFix::None).unwrap();
        assert_eq!((n, imgs, missing), (1, 0, 0));
        assert!(dir.join("materials/wpn_ak47.json").is_file());
        std::fs::remove_dir_all(&dir).ok();
    }
}
