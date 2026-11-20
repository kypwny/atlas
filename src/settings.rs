//! User settings of the browser: look (theme, accent, scale, thumbnails) and the things that make it quicker to work in
//! (recent folders, favourites, saved searches). Stored as JSON in the user's config folder; a missing or damaged file
//! gives the defaults, and values are clamped on load.

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridBackground {
    Dark,
    Light,
    Checker,
}

impl GridBackground {
    pub const ALL: [(GridBackground, &'static str); 3] = [(GridBackground::Dark, "Dark"), (GridBackground::Light, "Light"), (GridBackground::Checker, "Checkerboard")];

    fn code(self) -> &'static str {
        match self {
            GridBackground::Dark => "dark",
            GridBackground::Light => "light",
            GridBackground::Checker => "checker",
        }
    }

    fn from_code(s: &str) -> Option<Self> {
        Some(match s {
            "dark" => GridBackground::Dark,
            "light" => GridBackground::Light,
            "checker" => GridBackground::Checker,
            _ => return None,
        })
    }
}

/// A named set of filters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavedSearch {
    pub name: String,
    pub text: String,
    /// IWI format code and map type (0 = 2D, 1 = cube), as stored in the files.
    pub format: Option<u8>,
    pub map: Option<u8>,
    pub alpha: Option<bool>,
    pub dupes: bool,
    pub favourites: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub dark: bool,
    pub accent: [u8; 3],
    pub ui_scale: f32,
    /// Thumbnail edge in pixels.
    pub thumb: u32,
    pub grid_bg: GridBackground,
    /// Most recent first.
    pub recent: Vec<String>,
    /// Image names (lowercase).
    pub favourites: BTreeSet<String>,
    pub searches: Vec<SavedSearch>,
    /// The format the Export button of an image writes, and how normal maps are repaired on the way out.
    pub image_format: crate::imgexport::ImageFormat,
    pub normal_fix: crate::imgexport::NormalFix,
    /// How the mouse moves the camera of the model and map viewers.
    pub camera_style: crate::viewer::CameraStyle,
}

pub const MAX_RECENT: usize = 10;
pub const THUMB_RANGE: (u32, u32) = (64, 256);
pub const SCALE_RANGE: (f32, f32) = (0.75, 2.0);
pub const ACCENTS: [([u8; 3], &str); 6] = [([90, 170, 255], "Blue"), ([80, 200, 140], "Green"), ([240, 170, 60], "Amber"), ([230, 90, 90], "Red"), ([180, 120, 240], "Violet"), ([120, 200, 230], "Cyan")];

impl Default for Settings {
    fn default() -> Self {
        Settings { dark: true, accent: ACCENTS[0].0, ui_scale: 1.0, thumb: 128, grid_bg: GridBackground::Dark, recent: Vec::new(), favourites: BTreeSet::new(), searches: Vec::new(), image_format: Default::default(), normal_fix: Default::default(), camera_style: Default::default() }
    }
}

pub fn config_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|d| d.join("settings.json"))
}

impl Settings {
    pub fn to_json(&self) -> Value {
        json!({
            "dark": self.dark,
            "accent": self.accent,
            "ui_scale": self.ui_scale,
            "thumb": self.thumb,
            "grid_background": self.grid_bg.code(),
            "image_format": self.image_format.code(),
            "normal_fix": self.normal_fix.code(),
            "camera_style": self.camera_style.code(),
            "recent": self.recent,
            "favourites": self.favourites.iter().collect::<Vec<_>>(),
            "searches": self.searches.iter().map(|s| json!({
                "name": s.name, "text": s.text, "format": s.format, "map": s.map, "alpha": s.alpha, "dupes": s.dupes, "favourites": s.favourites,
            })).collect::<Vec<_>>(),
        })
    }

    /// Reads settings from JSON; anything missing or out of range falls back to the default.
    pub fn from_json(v: &Value) -> Settings {
        let d = Settings::default();
        let strings = |k: &str| -> Vec<String> { v[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default() };
        let accent = v["accent"].as_array().and_then(|a| (a.len() == 3).then(|| [0, 1, 2].map(|i| a[i].as_u64().unwrap_or(0).min(255) as u8))).unwrap_or(d.accent);
        let mut recent = strings("recent");
        recent.truncate(MAX_RECENT);
        Settings {
            dark: v["dark"].as_bool().unwrap_or(d.dark),
            accent,
            ui_scale: (v["ui_scale"].as_f64().unwrap_or(d.ui_scale as f64) as f32).clamp(SCALE_RANGE.0, SCALE_RANGE.1),
            thumb: (v["thumb"].as_u64().unwrap_or(d.thumb as u64) as u32).clamp(THUMB_RANGE.0, THUMB_RANGE.1),
            grid_bg: v["grid_background"].as_str().and_then(GridBackground::from_code).unwrap_or(d.grid_bg),
            recent,
            image_format: v["image_format"].as_str().and_then(crate::imgexport::ImageFormat::from_code).unwrap_or(d.image_format),
            normal_fix: v["normal_fix"].as_str().and_then(crate::imgexport::NormalFix::from_code).unwrap_or(d.normal_fix),
            camera_style: v["camera_style"].as_str().and_then(crate::viewer::CameraStyle::from_code).unwrap_or(d.camera_style),
            favourites: strings("favourites").into_iter().collect(),
            searches: v["searches"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| {
                            Some(SavedSearch {
                                name: s["name"].as_str()?.to_string(),
                                text: s["text"].as_str().unwrap_or("").to_string(),
                                format: s["format"].as_u64().map(|x| x as u8),
                                map: s["map"].as_u64().map(|x| x as u8),
                                alpha: s["alpha"].as_bool(),
                                dupes: s["dupes"].as_bool().unwrap_or(false),
                                favourites: s["favourites"].as_bool().unwrap_or(false),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn load(path: &Path) -> Settings {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).map(|v| Settings::from_json(&v)).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&self.to_json()).unwrap_or_default())
    }

    /// Moves a folder to the top of the recent list.
    pub fn add_recent(&mut self, dir: &Path) {
        let s = dir.display().to_string();
        self.recent.retain(|r| *r != s);
        self.recent.insert(0, s);
        self.recent.truncate(MAX_RECENT);
    }

    /// Toggles an image's favourite mark; returns whether it is now a favourite.
    pub fn toggle_favourite(&mut self, name: &str) -> bool {
        let n = name.to_ascii_lowercase();
        if !self.favourites.remove(&n) {
            self.favourites.insert(n);
            return true;
        }
        false
    }

    pub fn is_favourite(&self, name: &str) -> bool {
        self.favourites.contains(&name.to_ascii_lowercase())
    }

    /// Saves (or replaces) a search under its name.
    pub fn save_search(&mut self, s: SavedSearch) {
        match self.searches.iter_mut().find(|x| x.name == s.name) {
            Some(old) => *old = s,
            None => self.searches.push(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Settings {
        let mut s = Settings { dark: false, accent: [1, 2, 3], ui_scale: 1.25, thumb: 192, grid_bg: GridBackground::Checker, ..Default::default() };
        s.add_recent(Path::new("D:/a"));
        s.add_recent(Path::new("D:/b"));
        s.toggle_favourite("Falloff_Linear");
        s.save_search(SavedSearch { name: "icons".into(), text: "icon".into(), format: Some(13), map: Some(0), alpha: Some(true), dupes: true, favourites: false });
        s
    }

    #[test]
    fn settings_survive_a_save_and_load() {
        let s = sample();
        assert_eq!(Settings::from_json(&s.to_json()), s);
        let path = std::env::temp_dir().join(format!("codol-settings-{}.json", std::process::id()));
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_or_damaged_file_gives_the_defaults_and_values_are_clamped() {
        assert_eq!(Settings::load(Path::new("/nonexistent/codol.json")), Settings::default());
        let damaged = std::env::temp_dir().join(format!("codol-settings-bad-{}.json", std::process::id()));
        std::fs::write(&damaged, b"{ not json").unwrap();
        assert_eq!(Settings::load(&damaged), Settings::default());
        let _ = std::fs::remove_file(&damaged);
        let s = Settings::from_json(&json!({"ui_scale": 9.0, "thumb": 5, "accent": [1, 2], "grid_background": "plaid"}));
        assert_eq!((s.ui_scale, s.thumb, s.accent, s.grid_bg), (SCALE_RANGE.1, THUMB_RANGE.0, Settings::default().accent, GridBackground::Dark));
    }

    #[test]
    fn recent_folders_are_unique_newest_first_and_capped() {
        let mut s = Settings::default();
        for i in 0..15 {
            s.add_recent(Path::new(&format!("D:/dir{i}")));
        }
        s.add_recent(Path::new("D:/dir10"));
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0], "D:/dir10");
        assert_eq!(s.recent.iter().filter(|r| *r == "D:/dir10").count(), 1);
    }

    #[test]
    fn favourites_toggle_without_regard_to_case() {
        let mut s = Settings::default();
        assert!(s.toggle_favourite("A_B"));
        assert!(s.is_favourite("a_b"));
        assert!(!s.toggle_favourite("a_B"));
        assert!(!s.is_favourite("a_b"));
    }
}
