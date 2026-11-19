//! Unpacking a zone to files and building it again (`--unzone`, `--buildzone`). Needs `CODOL_TEST_GAME` (a QA build).

use atlas::zone::{Zone, export};
use std::path::PathBuf;

#[test]
fn edited_entities_and_a_deleted_raw_file_round_trip_through_files() {
    let Some(root) = std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let zone_path = export::zone_files(&root).into_iter().find(|p| p.ends_with("coop_evac_mp_ambush_sh.ff")).expect("zone");
    let dir = std::env::temp_dir().join("atlas_zonefiles_test");
    let _ = std::fs::remove_dir_all(&dir);
    let n = atlas::zonefiles::unpack(&zone_path, &key, &dir).unwrap();
    assert!(n > 100);

    // unchanged: the zone builds back to the same image
    let out = dir.join("same.ff");
    let r = atlas::zonefiles::build(&dir, &key, &out, false).unwrap();
    assert!(r.replaced.is_empty() && r.removed.is_empty() && r.relocated == 0, "{r:?}");
    let (a, b) = (Zone::open(&zone_path, &key).unwrap(), Zone::open(&out, &key).unwrap());
    assert!(a.data == b.data, "an untouched unpack builds back to a different image");

    // edit the entities (longer text: everything after them moves) and delete a raw file
    let text = std::fs::read_dir(dir.join("assets")).unwrap().flatten().map(|e| e.path()).find(|p| p.to_string_lossy().ends_with("_42.txt")).expect("map entities");
    let mut t = std::fs::read_to_string(&text).unwrap();
    t.push_str("\n{\n\"classname\" \"info_null\"\n\"targetname\" \"atlas_edit\"\n}\n");
    std::fs::write(&text, &t).unwrap();
    let raw = std::fs::read_dir(dir.join("assets")).unwrap().flatten().map(|e| e.path()).find(|p| p.to_string_lossy().ends_with("_36.bin")).expect("a raw file");
    std::fs::remove_file(&raw).unwrap();
    let out = dir.join("edited.ff");
    let r = atlas::zonefiles::build(&dir, &key, &out, false).unwrap();
    assert_eq!((r.replaced.len(), r.removed.len()), (1, 1), "{r:?}");
    assert!(r.relocated > 0);
    let edited = Zone::open(&out, &key).unwrap();
    assert_eq!(edited.assets.len(), a.assets.len() - 1);
    assert!(edited.data.windows(10).any(|w| w == b"atlas_edit"));
    let _ = std::fs::remove_dir_all(dir);
}
