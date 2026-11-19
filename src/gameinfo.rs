//! What a game folder is, from its files: the zones' fastfile versions, the images' IWI versions, and whether what is needed to
//! read it (for 3.19: the IFS helper, the DLL it knows and the client key) is in place.

use crate::iwi;
use crate::zone::export;
use crate::zone::fastfile::version_info;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Note {
    pub ok: bool,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct GameInfo {
    pub root: PathBuf,
    /// Best guess at the game and build, for headings.
    pub name: String,
    pub zones: Vec<String>,
    pub images: Vec<String>,
    pub notes: Vec<Note>,
}

fn ok(text: impl Into<String>) -> Note {
    Note { ok: true, text: text.into() }
}

fn problem(text: impl Into<String>) -> Note {
    Note { ok: false, text: text.into() }
}

/// The IWI version of the first image found loose in the folder or in an IWD, if any.
fn sample_image_version(root: &Path) -> Option<u8> {
    for sub in ["main", "hires", "raw", ""] {
        let dir = if sub.is_empty() { root.to_path_buf() } else { root.join(sub) };
        let images = if dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("images")) { dir.clone() } else { dir.join("images") };
        if let Ok(rd) = std::fs::read_dir(&images) {
            for e in rd.flatten() {
                if e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("iwi")) {
                    let mut head = vec![0u8; 4];
                    if std::fs::File::open(e.path()).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok() && &head[..3] == b"IWi" {
                        return Some(head[3]);
                    }
                }
            }
        }
    }
    None
}

/// Looks at a folder. With `deep`, also checks the things that cost time: for an installation with `.ifs` archives, whether the
/// client key can be found in an executable.
pub fn detect(root: &Path, deep: bool) -> GameInfo {
    let mut info = GameInfo { root: root.to_path_buf(), ..Default::default() };
    let survey = export::survey(root);
    for ((family, version), n) in &survey.by_version {
        info.zones.push(format!("{n} zones, fastfile v{version}: {}", version_info(*family, *version).build));
    }
    if let Some(((family, version), _)) = survey.by_version.first() {
        info.name = version_info(*family, *version).build.to_string();
    }
    if let Some(v) = sample_image_version(root) {
        let e = iwi::engine(v);
        info.images.push(e.label(v));
        if info.name.is_empty() {
            info.name = e.game.to_string();
        }
    }
    if info.name.is_empty() {
        info.name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string());
    }
    let ifs_dir = root.join("IIPS").join("IIPSDownload");
    let dll = root.join("IIPS").join("IFS2.dll");
    if ifs_dir.is_dir() && dll.is_file() {
        let archives = std::fs::read_dir(&ifs_dir).map(|rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("ifs"))).count()).unwrap_or(0);
        info.notes.push(ok(format!("{archives} .ifs archives (the images, models and zones of this build live in them)")));
        match crate::ifs::helper_path() {
            Some(p) => info.notes.push(ok(format!("IFS helper: {}", p.display()))),
            None => info.notes.push(problem("IFS helper not built: run cargo build --release --target i686-pc-windows-msvc in tools/ifs_helper")),
        }
        match crate::ifs::dll_is_known(&dll) {
            Some(true) => info.notes.push(ok("IFS2.dll is a build the helper knows")),
            Some(false) => info.notes.push(problem("IFS2.dll is not a build the helper knows (it checks the file's SHA-256)")),
            None => info.notes.push(problem("IFS2.dll could not be read")),
        }
        if deep {
            match crate::ifs::LooseKey::find(root) {
                Some(_) => info.notes.push(ok("client key found in an executable of this folder")),
                None => info.notes.push(problem("no executable in this folder carries the client key")),
            }
        }
    }
    if info.zones.is_empty() && info.images.is_empty() && info.notes.is_empty() {
        info.notes.push(problem("no zones, images or archives found here (is this the game folder?)"));
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_with_a_version_9_image_is_named_after_the_game() {
        let dir = std::env::temp_dir().join(format!("codol-gameinfo-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("main").join("images")).unwrap();
        let mut iwi = b"IWi\x09".to_vec();
        iwi.extend(vec![0u8; 28]);
        std::fs::write(dir.join("main").join("images").join("x.iwi"), iwi).unwrap();
        let g = detect(&dir, false);
        assert_eq!(g.name, "Call of Duty Online");
        assert_eq!(g.images, vec!["Call of Duty Online (IWI v9)".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_folder_says_so() {
        let dir = std::env::temp_dir().join(format!("codol-gameinfo-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let g = detect(&dir, false);
        assert!(g.notes.iter().any(|n| !n.ok && n.text.contains("no zones")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
