//! Effects of CoD Online zones: read, resolved across zones and played. Needs `CODOL_TEST_319` (a 3.19 game folder); `CODOL_TEST_QA`
//! (a QA 3.3.3.3 one) adds the same checks on QA.
use atlas::fx::sim::{Frame, Player};
use atlas::fx::{ElemType, Library, Visual};
use atlas::zone::export;
use glam::Vec3;
use std::path::PathBuf;
use std::sync::Arc;

fn find(root: &std::path::Path, name: &str) -> PathBuf {
    export::zone_files(root).into_iter().find(|p| p.file_name().unwrap().to_string_lossy() == name).unwrap()
}

fn library(root: &std::path::Path, zones: &[&str]) -> Library {
    let key = export::find_key(root).unwrap();
    let mut lib = Library::default();
    for z in zones {
        lib.read_zone(&find(root, z), &key).unwrap();
    }
    lib
}

fn check(root: &std::path::Path) {
    let lib = library(root, &["mp_crash_tropical.ff", "common_mp.ff"]);
    assert!(lib.effects.len() > 500, "{} effects", lib.effects.len());

    // a prop's destruction: sprites with materials common_mp resolves, models whose geometry the map zone holds
    let fx = lib.get("props/plant_medium_destroy").expect("props/plant_medium_destroy");
    assert_eq!((fx.looping, fx.one_shot, fx.elems.len()), (5, 8, 13));
    let first = &fx.elems[0];
    assert_eq!(first.elem_type, ElemType::Billboard);
    assert!(matches!(&first.visuals[0], Visual::Material { name, textures, .. } if name == "gfx_dirtchunks" && textures.color.is_some()));
    // looping elements spawn `count` times at `interval`: the effect's looping life is interval * (count - 1)
    let looping = &fx.elems[1];
    assert_eq!(looping.spawn, [30, 8]);
    assert_eq!(fx.looping_life_msec, 30 * 7);
    let models = lib.models_for("props/plant_medium_destroy");
    assert!(!models.is_empty());
    let in_zone = models.iter().filter(|m| lib.models.contains_key(*m)).count();
    assert!(in_zone >= 5, "{in_zone} of {} models found in the zones", models.len());
    for m in models.iter().filter_map(|m| lib.models.get(m)) {
        assert!(m.model.vertex_count() > 0 && m.textures.len() == m.model.surfaces.len());
    }

    // playing it: elements appear, the one-shots die, the debris comes to rest on the ground plane
    let lib = Arc::new(lib);
    let mut p = Player::new(lib.clone());
    p.spawn_named("props/plant_medium_destroy", Frame::pointing(Vec3::ZERO, Vec3::X));
    p.update(16.0);
    assert!(p.particle_count() > 10);
    for _ in 0..(4000 / 16) {
        p.update(16.0);
    }
    let (eye, right, up) = (Vec3::new(300.0, 0.0, 100.0), Vec3::Y, Vec3::Z);
    let list = p.draw(eye, right, up);
    assert!(!list.models.is_empty());
    for m in &list.models {
        assert!(m.origin.z >= -0.01, "{} below the ground at {:?}", m.name, m.origin);
    }
    for _ in 0..(20000 / 16) {
        p.update(16.0);
    }
    // what is left are the long-lived ground splats the debris started on impact
    let left = p.alive();
    assert!(left.iter().all(|(name, _, _)| name != "props/plant_medium_destroy"), "still alive after 24 s: {left:?}");
    assert!(left.iter().any(|(name, _, _)| name.ends_with("ground_splat")), "no impact effect started: {left:?}");

    // an explosion with runners and child effects: every effect it starts is found
    let mut p = Player::new(lib.clone());
    assert!(p.spawn_named("explosions/grenadeexp_dirt_1", Frame::pointing(Vec3::ZERO, Vec3::Z)));
    for _ in 0..60 {
        p.update(16.0);
    }
    let list = p.draw(eye, right, up);
    assert!(list.batches.iter().map(|b| b.vertices.len()).sum::<usize>() > 100);
    assert!(p.missing.is_empty(), "not found: {:?}", p.missing);
}

#[test]
fn effects_read_and_play_319() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    check(&root);
}

#[test]
fn effects_read_and_play_qa() {
    let Some(root) = std::env::var_os("CODOL_TEST_QA").map(PathBuf::from) else { return };
    check(&root);
}
