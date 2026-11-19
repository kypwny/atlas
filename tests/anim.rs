//! Animation decoding against real zone data. `CODOL_TEST_ANIMS` = an animation index made by
//! `atlas --buildanims`; skipped without it.

use atlas::anim::{RawAnim, Rotation, Translation};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

#[test]
fn every_recorded_animation_decodes_with_all_data_consumed_and_unit_quaternions() {
    let Some(path) = std::env::var_os("CODOL_TEST_ANIMS").map(PathBuf::from) else { return };
    let (mut n, mut failed, mut worst_norm) = (0usize, Vec::<String>::new(), (1.0f32, 1.0f32));
    let (mut keyed_rot, mut const_rot, mut none_rot, mut keyed_trans, mut const_trans, mut none_trans) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut max_trans = 0.0f32;
    for line in BufReader::new(std::fs::File::open(path).unwrap()).lines().map_while(Result::ok) {
        let Some(raw) = RawAnim::from_line(&line) else { continue };
        n += 1;
        let anim = match raw.decode() {
            Ok(a) => a,
            Err(e) => {
                if failed.len() < 6 {
                    failed.push(format!("{}: {e:#}", raw.name));
                }
                continue;
            }
        };
        for t in &anim.tracks {
            match &t.rot {
                Rotation::None => none_rot += 1,
                Rotation::Keys { at, q } => {
                    if at.len() > 1 { keyed_rot += 1 } else { const_rot += 1 }
                    assert!(at.windows(2).all(|w| w[0] < w[1]), "{}: frame numbers not increasing in {}", raw.name, t.name);
                    for k in q {
                        let norm = (k[0] * k[0] + k[1] * k[1] + k[2] * k[2] + k[3] * k[3]).sqrt();
                        worst_norm.0 = worst_norm.0.min(norm);
                        worst_norm.1 = worst_norm.1.max(norm);
                    }
                }
            }
            match &t.trans {
                Translation::None => none_trans += 1,
                Translation::Keys { at, v } => {
                    if at.len() > 1 { keyed_trans += 1 } else { const_trans += 1 }
                    assert!(at.windows(2).all(|w| w[0] < w[1]), "{}: translation frames not increasing in {}", raw.name, t.name);
                    for k in v {
                        assert!(k.iter().all(|x| x.is_finite()), "{}: non-finite translation in {}", raw.name, t.name);
                        max_trans = max_trans.max(k.iter().fold(0.0f32, |m, x| m.max(x.abs())));
                    }
                }
            }
        }
    }
    println!("{n} animations; rotations keyed {keyed_rot} constant {const_rot} none {none_rot}; translations keyed {keyed_trans} constant {const_trans} none {none_trans}; quaternion norm {worst_norm:?}; largest translation {max_trans}");
    assert!(n > 5, "only {n} animations");
    assert!(failed.is_empty(), "{} failed, for example {failed:#?}", failed.len());
    assert!(worst_norm.0 > 0.9 && worst_norm.1 < 1.1, "quaternion norms {worst_norm:?}");
}

/// The reference SEAnim dumps (`CODOL_TEST_SEANIMS`) read and write back byte for byte.
#[test]
fn every_reference_seanim_reads_and_writes_back_identically() {
    use atlas::export::seanim::SeAnim;
    let Some(dir) = std::env::var_os("CODOL_TEST_SEANIMS").map(PathBuf::from) else { return };
    let (mut n, mut bad) = (0usize, Vec::<String>::new());
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x == "seanim") {
            continue;
        }
        let bytes = std::fs::read(&p).unwrap();
        let a = SeAnim::read(&bytes).unwrap_or_else(|err| panic!("{}: {err:#}", p.display()));
        if a.write() != bytes && bad.len() < 8 {
            bad.push(p.file_name().unwrap().to_string_lossy().into_owned());
        }
        n += 1;
    }
    assert!(n > 1000, "only {n} files");
    assert!(bad.is_empty(), "round trip differs for {bad:?}");
}

/// Decoded animations agree with the reference SEAnim of the same name: for every bone both have, where both keep the same
/// number of keys, the rotations (either sign) and the locations (the reference is in centimetres) are the same. The
/// reference dumps come from another CoDOL build than the QA zones, so some animations differ (extra or missing bones,
/// changed motion) and most, not all, agree.
#[test]
fn decoded_animations_agree_with_the_reference_seanims() {
    use atlas::export::seanim::{SeAnim, from_animation};
    use atlas::zone::{Zone, export};
    let (Some(game), Some(dir)) = (std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from), std::env::var_os("CODOL_TEST_SEANIMS").map(PathBuf::from)) else { return };
    let key = export::find_key(&game).unwrap();
    let mut seen = std::collections::HashSet::new();
    let (mut compared, mut same_frames, mut rot_same, mut rot_total, mut loc_same, mut loc_total) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for path in export::zone_files(&game) {
        let Ok(z) = Zone::open(&path, &key) else { continue };
        for raw in export::zone_anims(&z) {
            if !seen.insert(raw.name.clone()) {
                continue;
            }
            let anim = raw.decode().unwrap();
            let Ok(bytes) = std::fs::read(dir.join(format!("{}.seanim", raw.name))) else { continue };
            let r = SeAnim::read(&bytes).unwrap();
            compared += 1;
            same_frames += (r.frame_count == anim.num_frames as u32 + 1) as usize;
            // our own SEAnim of it reads back and has the frame count and bones of the animation
            let mine = SeAnim::read(&from_animation(&anim, None, 2.54).write()).unwrap();
            assert_eq!((mine.frame_count, mine.bones.len()), (anim.num_frames as u32 + 1, anim.tracks_with_delta().len()), "{}", raw.name);
            for t in &anim.tracks {
                let Some(b) = r.bones.iter().find(|b| b.name == t.name) else { continue };
                if let Rotation::Keys { at, q } = &t.rot {
                    if at.len() == b.rotation.len() {
                        rot_total += 1;
                        rot_same += (at.iter().zip(&b.rotation).all(|(a, (f, _))| *a as u32 == *f) && q.iter().zip(&b.rotation).all(|(a, (_, v))| (0..4).map(|i| a[i] * v[i]).sum::<f32>().abs() > 0.999)) as usize;
                    }
                }
                if let Translation::Keys { at, v } = &t.trans {
                    if at.len() == b.location.len() {
                        loc_total += 1;
                        loc_same += (at.iter().zip(&b.location).all(|(a, (f, _))| *a as u32 == *f) && v.iter().zip(&b.location).all(|(a, (_, w))| (0..3).all(|i| (a[i] * 2.54 - w[i]).abs() < 1e-2 * (1.0 + w[i].abs())))) as usize;
                    }
                }
            }
        }
    }
    println!("{compared} animations have a reference ({same_frames} with the same frame count); rotation tracks {rot_same}/{rot_total} agree, location tracks {loc_same}/{loc_total}");
    assert!(compared > 1000, "only {compared} compared");
    assert!(rot_same * 100 >= rot_total * 80, "rotations: {rot_same} of {rot_total}");
    assert!(loc_same * 100 >= loc_total * 80, "locations: {loc_same} of {loc_total}");
}

/// Delta (root motion) tracks: an animation has one exactly when its reference SEAnim has the delta type (3) and a
/// `tag_origin` bone, with the same frames, the same locations (centimetres in the reference) and the same rotations.
#[test]
fn delta_tracks_agree_with_the_reference_tag_origin() {
    use atlas::export::seanim::{SeAnim, from_animation};
    use atlas::zone::{Zone, export};
    let (Some(game), Some(dir)) = (std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from), std::env::var_os("CODOL_TEST_SEANIMS").map(PathBuf::from)) else { return };
    let key = export::find_key(&game).unwrap();
    let mut seen = std::collections::HashSet::new();
    let (mut with_delta, mut type3, mut checked, mut trans_same, mut rot_same, mut rot_checked) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    for path in export::zone_files(&game) {
        let Ok(z) = Zone::open(&path, &key) else { continue };
        for raw in export::zone_anims(&z) {
            if !seen.insert(raw.name.clone()) {
                continue;
            }
            let anim = raw.decode().unwrap();
            let Ok(bytes) = std::fs::read(dir.join(format!("{}.seanim", raw.name))) else { continue };
            let r = SeAnim::read(&bytes).unwrap();
            let Some(d) = &anim.delta else { continue };
            with_delta += 1;
            type3 += (r.anim_type == 3) as usize;
            assert_eq!(from_animation(&anim, None, 2.54).anim_type, 3, "{}", raw.name);
            let Some(b) = r.bones.iter().find(|b| b.name == "tag_origin") else { continue };
            checked += 1;
            if let Translation::Keys { at, v } = &d.trans {
                trans_same += (at.len() == b.location.len() && at.iter().zip(&b.location).all(|(a, (f, _))| *a as u32 == *f) && v.iter().zip(&b.location).all(|(a, (_, w))| (0..3).all(|i| (a[i] * 2.54 - w[i]).abs() < 1e-2 * (1.0 + w[i].abs())))) as usize;
            }
            if let Rotation::Keys { at, q } = &d.rot {
                rot_checked += 1;
                rot_same += (at.len() == b.rotation.len() && at.iter().zip(&b.rotation).all(|(a, (f, _))| *a as u32 == *f) && q.iter().zip(&b.rotation).all(|(a, (_, v))| (0..4).map(|i| a[i] * v[i]).sum::<f32>().abs() > 0.999)) as usize;
            }
        }
    }
    println!("{with_delta} animations with root motion ({type3} of them delta type in the reference); {checked} compared: translations {trans_same}/{checked}, rotations {rot_same}/{rot_checked}");
    assert!(with_delta > 500, "only {with_delta} animations with root motion");
    assert!(type3 * 100 >= with_delta * 98, "only {type3} of {with_delta} are delta type in the reference");
    assert!(trans_same * 100 >= checked * 90 && rot_same * 100 >= rot_checked * 90, "translations {trans_same}/{checked}, rotations {rot_same}/{rot_checked}");
}
