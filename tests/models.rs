//! Parses every streamed model of a game folder. Runs only when CODOL_TEST_GAME is set.

use atlas::{model, modelsource};
use std::path::PathBuf;

#[test]
fn every_streamed_model_parses() {
    let Some(root) = std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from) else {
        eprintln!("CODOL_TEST_GAME not set; skipping");
        return;
    };
    let cat = modelsource::scan(modelsource::discover(&root));
    assert!(!cat.entries.is_empty(), "no models found under {}", root.display());
    let mut failures = Vec::new();
    let mut mismatched_names = 0;
    for e in &cat.entries {
        match cat.providers[e.provider].read(&e.key).and_then(|b| model::Model::parse(&b)) {
            Ok(m) => {
                if m.name != e.name {
                    mismatched_names += 1;
                }
                for s in &m.surfaces {
                    // Normals should be close to unit length (they are packed in 3 bytes).
                    for v in s.vertices.iter().step_by(37) {
                        let len = (v.normal[0].powi(2) + v.normal[1].powi(2) + v.normal[2].powi(2)).sqrt();
                        assert!((0.9..1.1).contains(&len), "{}: normal length {len}", e.name);
                    }
                }
            }
            Err(err) => failures.push(format!("{}: {err:#}", e.name)),
        }
    }
    eprintln!("{} models parsed, {} failures, {} files whose stored name differs from the file name", cat.entries.len(), failures.len(), mismatched_names);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Needs CODOL_TEST_GAME and CODOL_TEST_INDEX (a model index made by `atlas --buildindex`).
#[test]
fn model_index_covers_the_streamed_models() {
    let (Some(root), Some(index_path)) = (std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from), std::env::var_os("CODOL_TEST_INDEX").map(PathBuf::from)) else {
        eprintln!("CODOL_TEST_GAME / CODOL_TEST_INDEX not set; skipping");
        return;
    };
    use atlas::{modelindex::ModelIndex, source};
    let index = ModelIndex::load(&index_path).unwrap();
    let models = modelsource::scan(modelsource::discover(&root));
    let images = source::scan(source::discover(&root), &source::Progress::default());
    let (mut with_lod, mut surfaces, mut with_color, mut color_installed) = (0usize, 0usize, 0usize, 0usize);
    for e in &models.entries {
        let m = model::Model::parse(&models.providers[e.provider].read(&e.key).unwrap()).unwrap();
        if index.find_file(&e.name).is_none() {
            continue;
        }
        with_lod += 1;
        for name in index.surface_images(&e.name, m.surfaces.len()) {
            surfaces += 1;
            if let Some(n) = name {
                with_color += 1;
                if images.entries.binary_search_by(|x| x.name.as_str().cmp(n.to_ascii_lowercase().as_str())).is_ok() {
                    color_installed += 1;
                }
            }
        }
    }
    eprintln!(
        "{} of {} model files are in the index; {} of {} surfaces have a colour map, {} of those exist as installed images",
        with_lod,
        models.entries.len(),
        with_color,
        surfaces,
        color_installed
    );
    assert!(with_lod * 100 >= models.entries.len() * 99, "index covers too few model files");
    assert!(with_color * 100 >= surfaces * 90, "too few surfaces resolve to a colour map");
    assert!(color_installed * 100 >= with_color * 90, "too few colour maps exist as images");
}
