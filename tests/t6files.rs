//! `--t6unzone` / `--t6buildzone` (`zone::t6_files`): a zone as files and back. Needs `CODOL_TEST_T6=<game folder>`; skipped
//! without it. The checks are structural (the walker and the reader of `t6` accept the result); the game was not run.

use atlas::zone::t6::{self};
use atlas::zone::t6_files::{build, unpack};
use atlas::zone::t6_rebuild::*;
use std::path::{Path, PathBuf};

fn zone() -> Option<PathBuf> {
    let root = std::env::var_os("CODOL_TEST_T6").map(PathBuf::from)?.join("zone");
    let p = root.join("japanese").join("ja_patch.ff");
    p.exists().then_some(p)
}

fn open_built(p: &Path) -> T6Image {
    let raw = std::fs::read(p).unwrap();
    let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
    let z = t6::open_named(&raw, Some(&stem)).unwrap();
    T6Image::from_zone(&z).unwrap()
}

fn files_of(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir.join("assets")).unwrap().flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with(suffix)).collect();
    v.sort();
    v
}

#[test]
fn a_zone_unpacks_and_builds_back_and_edited_files_change_the_zone() {
    let Some(zone_path) = zone() else { return };
    let tmp = std::env::temp_dir().join("atlas_t6files_test");
    let _ = std::fs::remove_dir_all(&tmp);
    let dir = tmp.join("unpacked");
    let (assets, files) = unpack(&zone_path, &dir).unwrap();
    assert!(assets > 1000 && files > 100, "{assets} assets, {files} files");

    // untouched: the same image (unsigned container; the name is the output's)
    let same = tmp.join("ja_patch.ff");
    let r = build(&dir, &same, false).unwrap();
    assert!(r.replaced.is_empty() && r.removed.is_empty() && !r.signed, "{r:?}");
    let original = open_built(&zone_path);
    let back = open_built(&same);
    assert!(back.image() == original.image(), "an untouched unpack builds back to a different image");
    // ... and with the original signature when the name is the original's
    let signed = tmp.join("signed").join("ja_patch.ff");
    std::fs::create_dir_all(signed.parent().unwrap()).unwrap();
    let r = build(&dir, &signed, true).unwrap();
    assert!(r.signed);
    let sz = t6::open(&std::fs::read(&signed).unwrap()).unwrap();
    assert!(sz.image == original.image());

    // edits: a raw file gets longer, a CSV cell changes, a localize text changes, another localize entry is deleted
    let manifest = std::fs::read_to_string(dir.join("zone.tsv")).unwrap();
    let row = |ty: &str| manifest.lines().skip(1).map(|l| l.split('\t').collect::<Vec<_>>()).filter(|f| f[1] == ty && !f[4].is_empty()).map(|f| (f[0].parse::<usize>().unwrap(), f[3].to_string(), f[4].to_string())).collect::<Vec<_>>();
    let raws = row("41");
    let tables = row("42");
    let locs = row("24");
    assert!(!raws.is_empty() && !tables.is_empty() && locs.len() > 1);
    let (raw_k, raw_name, raw_file) = raws[0].clone();
    let (del_k, _, del_file) = locs[1].clone();
    let (tab_k, _, tab_file) = tables.iter().find(|t| std::fs::read(dir.join(&t.2)).map(|b| b.len() > 100).unwrap_or(false)).cloned().unwrap();
    let (loc_k, loc_name, loc_file) = locs[0].clone();

    let mut raw_text = std::fs::read(dir.join(&raw_file)).unwrap();
    raw_text.extend_from_slice(b"\n// edited through --t6buildzone\n");
    std::fs::write(dir.join(&raw_file), &raw_text).unwrap();
    std::fs::remove_file(dir.join(&del_file)).unwrap();
    let mut csv = std::fs::read(dir.join(&tab_file)).unwrap();
    let first_comma = csv.iter().position(|&b| b == b',').unwrap();
    csv.splice(first_comma..first_comma, b"_atlas_edit".iter().copied());
    std::fs::write(dir.join(&tab_file), &csv).unwrap();
    std::fs::write(dir.join(&loc_file), "edited localize text").unwrap();

    let out = tmp.join("edited").join("ja_patch.ff");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    let r = build(&dir, &out, false).unwrap();
    assert_eq!(r.removed, vec![del_k], "{r:?}");
    assert_eq!(r.replaced.len(), 3, "{r:?}");
    assert!(r.replaced.contains(&raw_k) && r.replaced.contains(&tab_k) && r.replaced.contains(&loc_k));
    eprintln!("{r:?}");

    let edited = open_built(&out);
    assert_eq!(edited.asset_count(), original.asset_count() - 1);
    assert!(edited.report.exact);
    let k = edited.find(RAWFILE, &raw_name).unwrap();
    assert_eq!(edited.rawfile(k).unwrap(), raw_text);
    let k = edited.find(LOCALIZE, &loc_name).unwrap();
    assert_eq!(edited.localize(k).unwrap().1, "edited localize text");
    let t = edited.stringtable(edited.find(STRINGTABLE, original.asset_name(tab_k)).unwrap()).unwrap();
    assert!(t.texts().iter().any(|c| c.contains("_atlas_edit")));
    assert!(t.consistent());

    // a file that is not valid for the asset, and a folder of another zone, are refused
    std::fs::write(dir.join(&loc_file), [0xFFu8, 0xFE, 0x41]).unwrap();
    assert!(build(&dir, &tmp.join("bad.ff"), false).is_err());
    let _ = std::fs::remove_dir_all(&tmp);
}
