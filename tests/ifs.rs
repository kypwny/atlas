//! The 3.19 `.ifs` archives, read through the helper and the game's own DLL. Needs `CODOL_TEST_319` (the installation
//! folder) and a built `tools/ifs_helper`; skipped otherwise.

use atlas::ifs::IfsStore;
use atlas::iwi::{self, Header};
use atlas::model::Model;
use std::path::PathBuf;

#[test]
fn archive_images_and_models_read_back_as_real_files() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let Some(store) = IfsStore::open(&root) else { panic!("the archives did not open: is tools/ifs_helper built, and is there an executable with the client key?") };
    assert!(store.archive_count() > 900 && store.entry_count() > 100_000);
    let images = store.image_providers();
    assert_eq!(images.len(), 2, "main and hires images");
    for p in &images {
        let list = p.list().unwrap();
        assert!(list.len() > 30_000, "{} lists {}", p.label(), list.len());
        // spread across the archives, including ones with files larger than the DLL's read buffer
        for l in list.iter().step_by(list.len() / 60) {
            let data = p.read(&l.key).unwrap_or_else(|e| panic!("{}: {e:#}", l.name));
            let h = Header::parse(&data).unwrap_or_else(|e| panic!("{}: {e:#}", l.name));
            assert_eq!(h.version, 9, "{}", l.name);
            if let Some(want) = iwi::expected_size(&h) {
                assert_eq!(want, data.len(), "{}", l.name);
            }
            iwi::decode_level(&data, &h, 0).unwrap_or_else(|e| panic!("{}: {e:#}", l.name));
        }
    }
    let models = store.model_providers();
    assert_eq!(models.len(), 1);
    let list = models[0].list().unwrap();
    assert!(list.len() > 15_000);
    for l in list.iter().step_by(list.len() / 60) {
        let m = Model::parse(&models[0].read(&l.key).unwrap()).unwrap_or_else(|e| panic!("{}: {e:#}", l.name));
        assert!(m.vertex_count() > 0 || m.surfaces.is_empty(), "{}", l.name);
    }
}

/// The scan is cached: a second open lists the same archives and images without opening any archive again.
#[test]
fn the_scan_is_cached_between_opens() {
    use std::time::Instant;
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let dir = std::env::temp_dir().join("atlas_ifs_cache_run");
    let _ = std::fs::remove_dir_all(&dir);
    // the variable is read when the store opens; this test is the only one in the file that sets it
    unsafe { std::env::set_var("ATLAS_CACHE", &dir) };
    let scan = || {
        let t = Instant::now();
        let store = IfsStore::open(&root).expect("archives");
        let opened = t.elapsed();
        let lists: Vec<usize> = store.image_providers().iter().map(|p| p.list().unwrap().len()).collect();
        let heads: Vec<Vec<u8>> = store.image_providers().iter().flat_map(|p| p.list().unwrap().into_iter().step_by(997).map(|l| p.read_head(&l.key, 32).unwrap()).collect::<Vec<_>>()).collect();
        (store.archive_count(), store.entry_count(), lists, heads, opened, t.elapsed())
    };
    let cold = scan();
    let warm = scan();
    println!("cold: open {:?}, with image lists {:?}; warm: open {:?}, with image lists {:?}", cold.4, cold.5, warm.4, warm.5);
    assert_eq!((cold.0, cold.1, &cold.2, &cold.3), (warm.0, warm.1, &warm.2, &warm.3), "the cached scan differs from the real one");
    assert!(warm.5 * 5 < cold.5, "the cached scan is not much faster");
    let _ = std::fs::remove_dir_all(&dir);
}
