//! Cross-checks our DDS export against the `image` crate's independent DDS
//! decoder. Runs only when CODOL_TEST_GAME points at a game folder.

use atlas::{convert, iwi, source};
use std::path::PathBuf;

#[test]
fn exported_dds_decodes_like_our_own_decoder() {
    let Some(root) = std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from) else {
        eprintln!("CODOL_TEST_GAME not set; skipping");
        return;
    };
    let cat = source::scan(source::discover(&root), &source::Progress::default());
    let mut checked = std::collections::BTreeMap::new();
    for e in &cat.entries {
        let Some(v) = e.variants.first() else { continue };
        let Some(h) = &v.header else { continue };
        let Some(fmt) = h.format() else { continue };
        if h.map_type() != iwi::MapType::Tex2D || h.width.max(h.height) > 512 {
            continue;
        }
        let n = checked.entry(fmt.name()).or_insert(0usize);
        if *n >= 4 {
            continue;
        }
        let data = cat.providers[v.provider].read(&v.key).unwrap();
        let dds = convert::iwi_to_dds(&data).unwrap();
        let theirs = match image::load_from_memory_with_format(&dds, image::ImageFormat::Dds) {
            Ok(i) => i.to_rgba8(),
            Err(err) => {
                eprintln!("note: image crate cannot read {} ({}): {err}", e.name, fmt.name());
                continue;
            }
        };
        let ours = iwi::decode_level(&data, h, 0).unwrap();
        assert_eq!((theirs.width(), theirs.height()), (ours.width, ours.height), "{}", e.name);
        // Expanded formats differ in how narrow formats map to RGB; compare block/true-colour formats strictly.
        if matches!(fmt, iwi::Format::Dxt1 | iwi::Format::Dxt3 | iwi::Format::Dxt5 | iwi::Format::Bgra | iwi::Format::Bgr) {
            let worst = theirs.as_raw().iter().zip(&ours.faces[0]).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
            assert!(worst <= 2, "{} ({}): max channel difference {worst}", e.name, fmt.name());
        }
        *n += 1;
    }
    eprintln!("checked: {checked:?}");
    assert!(checked.values().sum::<usize>() > 0, "no images were checked");
}

#[test]
fn dds_import_reproduces_the_original_iwi() {
    let Some(root) = std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from) else { return };
    let cat = source::scan(source::discover(&root), &source::Progress::default());
    let mut identical = 0;
    for e in &cat.entries {
        let Some(v) = e.variants.first() else { continue };
        let Some(h) = &v.header else { continue };
        let Some(fmt) = h.format() else { continue };
        if fmt.is_wavelet() || h.map_type() != iwi::MapType::Tex2D || h.width.max(h.height) > 256 || h.flags & 1 == 0 {
            continue;
        }
        let data = cat.providers[v.provider].read(&v.key).unwrap();
        // Only files whose size fields follow the common rule can be byte-identical.
        if iwi::read_raw(&data, h).and_then(|r| iwi::write(&r)).unwrap() != data {
            continue;
        }
        let dds = convert::iwi_to_dds(&data).unwrap();
        let opts = convert::Options { flags: Some(h.flags), ..Default::default() };
        let (back, _) = convert::dds_to_iwi(&dds, &opts).unwrap();
        assert_eq!(back, data, "{} ({})", e.name, fmt.name());
        identical += 1;
        if identical >= 300 {
            break;
        }
    }
    assert!(identical > 0);
    eprintln!("{identical} images went IWI -> DDS -> IWI unchanged");
}
