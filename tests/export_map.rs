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
