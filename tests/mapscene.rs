//! The editor layers of every 3.19 map zone: every layer builds, indices stay in range, no coordinate is NaN, and the solid
//! collision, static model and entity layers are not empty.
use atlas::zone::{Zone, export, mapscene, mapsrc};
use std::path::PathBuf;

#[test]
fn every_map_has_drawable_editor_layers() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let mut maps = 0;
    for path in export::zone_files(&root) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("mp_") || name.ends_with("_load.ff") || name.ends_with("_const.ff") || name == "mp_playlists.ff" {
            continue;
        }
        let z = Zone::open(&path, &key).unwrap();
        let m = mapsrc::read(&z, &name).unwrap();
        let layers = mapscene::layers(&m);
        let by = |n: &str| layers.iter().find(|l| l.name == n).unwrap_or_else(|| panic!("{name}: no layer {n}"));
        for l in &layers {
            let s = &l.surface;
            assert!(s.indices.iter().all(|&i| (i as usize) < s.vertices.len()), "{name}: {} has an index out of range", l.name);
            assert!(s.indices.len() % 3 == 0, "{name}: {}", l.name);
            assert!(s.vertices.iter().all(|v| v.pos.iter().all(|c| c.is_finite())), "{name}: {} has a non-finite vertex", l.name);
        }
        assert!(!by("collision: world brushes").surface.indices.is_empty(), "{name}: no solid brushes drawn");
        assert!(!by("static models").surface.indices.is_empty(), "{name}: no static model boxes");
        assert!(layers.iter().filter(|l| l.name.starts_with("entities")).any(|l| !l.surface.indices.is_empty()), "{name}: no entity markers");
        maps += 1;
    }
    assert!(maps > 30, "{maps} maps");
}
