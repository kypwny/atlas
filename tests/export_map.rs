//! `atlas --exportmap` into a folder under the system temp directory. Needs the MW2 zones (`CODOL_TEST_MW2`, as in the conversion
//! tests) and does nothing without them. No game folder is involved.

use atlas::zone::export_map::command;
use atlas::zone::convert_cli::{open_input, verify};
use std::path::PathBuf;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn zone() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("CODOL_TEST_MW2")?).join("zone/iw4x/x86/english/mp_rust.ff");
    p.exists().then_some(p)
}

#[test]
fn a_stock_map_is_exported_for_each_game_and_reads_back() {
    let Some(src) = zone() else { return };
    let root = std::env::temp_dir().join(format!("atlas-exportmap-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (to, files) in [("iw4", 3usize), ("iw3", 2), ("iw5", 2)] {
        let out = root.join(to);
        let (s, o) = (src.to_str().unwrap(), out.to_str().unwrap());
        // a dry run writes nothing
        command(&args(&[s, "--to", to, o, "--dry-run"])).unwrap();
        assert!(!out.exists(), "{to}: dry run");
        command(&args(&[s, "--to", to, o])).unwrap();
        let dir = out.join("usermaps/mp_rust");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), files, "{to}: zone, load zone [, arena]");
        let again = open_input(&dir.join("mp_rust.ff"), None).unwrap();
        verify(again.fmt, &again.name, &again.image).unwrap();
        // never over existing files without --force
        assert!(command(&args(&[s, "--to", to, o])).is_err());
        command(&args(&[s, "--to", to, o, "--force"])).unwrap();
        // the zone holds mp_rust, so no other name
        assert!(command(&args(&[s, "--to", to, o, "--name", "mp_other", "--force"])).is_err());
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// Call of Duty Online needs `CODOL_TEST_QA` (a QA 3.3.3.3 game folder, only read) as well: the package gets a generated `MAP_const.ff`,
/// which `--no-const` leaves out, and the const zone reads back with the QA walker as the marker raw file alone.
#[test]
fn a_codol_package_has_a_generated_const_zone() {
    let (Some(src), Some(game)) = (zone(), std::env::var_os("CODOL_TEST_QA").map(PathBuf::from)) else { return };
    let root = std::env::temp_dir().join(format!("atlas-exportmap-const-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (s, g) = (src.to_str().unwrap(), game.to_str().unwrap());
    let with = root.join("with");
    command(&args(&[s, "--to", "codol", with.to_str().unwrap(), "--game-folder", g])).unwrap();
    let dir = with.join("client_shipRetail/chinese");
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 3, "zone, load zone, const zone");
    let konst = open_input(&dir.join("mp_rust_const.ff"), Some(&game)).unwrap();
    assert_eq!(atlas::zone::const_zone::parse(&konst.image).unwrap(), [atlas::zone::const_zone::RawFile::marker("mp_rust")]);
    assert_eq!(konst.image, atlas::zone::const_zone::marker_only("mp_rust"));
    verify(konst.fmt, &konst.name, &konst.image).unwrap();
    let without = root.join("without");
    command(&args(&[s, "--to", "codol", without.to_str().unwrap(), "--game-folder", g, "--no-const"])).unwrap();
    assert_eq!(std::fs::read_dir(without.join("client_shipRetail/chinese")).unwrap().count(), 2);
    let _ = std::fs::remove_dir_all(&root);
}
