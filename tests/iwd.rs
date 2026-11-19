//! IWD packing against real files. Needs `CODOL_TEST_GAME` (a game folder with `main/images`); skipped otherwise.

use atlas::iwd::{self, AddOptions, WriteOptions};
use atlas::source::{IwdArchive, Provider};
use std::path::PathBuf;

#[test]
fn packed_images_read_back_identically_through_the_browser_provider() {
    let Some(game) = std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from) else { return };
    let images = game.join("main").join("images");
    if !images.is_dir() {
        return;
    }
    let mut entries = iwd::add_dir(&images, &AddOptions::default()).unwrap();
    assert!(entries.iter().all(|e| e.path.starts_with("images/")), "a folder named images is prefixed");
    entries.truncate(400);
    let out = std::env::temp_dir().join(format!("codol-iwd-real-{}.iwd", std::process::id()));
    let s = iwd::write(&entries, &out, WriteOptions::default()).unwrap();
    assert_eq!(s.files, 400);
    let v = iwd::verify(&out).unwrap();
    assert_eq!((v.files, v.images, v.bad_images.len()), (400, 400, 0));

    // the browser's own archive reader sees exactly the same bytes as the loose files
    let provider = IwdArchive::open(&out).unwrap();
    let listing = provider.list().unwrap();
    assert_eq!(listing.len(), 400);
    for l in &listing {
        let loose = std::fs::read(images.join(format!("{}.iwi", l.name))).unwrap();
        assert_eq!(provider.read(&l.key).unwrap(), loose, "{}", l.name);
    }
    drop(provider);

    // re-pack from the archive itself and in place: the entries are copied, not recompressed
    let again = iwd::load(&out).unwrap();
    let before = std::fs::read(&out).unwrap();
    iwd::write(&again, &out, WriteOptions::default()).unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), before);
    let _ = std::fs::remove_file(&out);
}
