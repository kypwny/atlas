//! Black Ops II animations (`zone::t6_anims`) and their Cast export. Needs `CODOL_TEST_T6=<game folder>` (the one that has
//! `zone\all`); skipped without it. `CODOL_TEST_T6_ALL=1` also decodes every zone.

mod common;

use atlas::anim::{Animation, Rotation, Translation};
use atlas::export::t6::{T6Options, export_zones_with};
use atlas::zone::t6_anims::read_anims_file;
use atlas::zone::t6_models::read_models_file;
use common::*;
use std::path::PathBuf;

fn zone(name: &str) -> Option<PathBuf> {
    let p = std::env::var_os("CODOL_TEST_T6").map(PathBuf::from)?.join("zone").join("all").join(name);
    p.exists().then_some(p)
}

/// Frame numbers rise, stay inside the animation, and every rotation is (nearly) a unit quaternion.
fn check_anim(name: &str, a: &Animation) {
    // a name with a leading comma is a stub that only names an animation another zone holds
    if name.starts_with(',') {
        assert!(a.tracks.is_empty(), "{name}: stub with tracks");
        return;
    }
    assert!(a.frame_rate >= 1.0 && a.frame_rate <= 120.0, "{name}: {} fps", a.frame_rate);
    for (n, t) in a.tracks.iter().enumerate() {
        assert!(!t.name.is_empty(), "{name}: track {n} has no bone name");
        if let Rotation::Keys { at, q } = &t.rot {
            assert_eq!(at.len(), q.len(), "{name}/{}", t.name);
            assert!(at.windows(2).all(|w| w[0] < w[1]), "{name}/{}: rotation frames {at:?}", t.name);
            assert!(at.iter().all(|&f| f <= a.num_frames), "{name}/{}: frame past the end ({} frames)", t.name, a.num_frames);
            for k in q {
                let l = k.iter().map(|c| c * c).sum::<f32>().sqrt();
                // 16-bit quantisation of a unit quaternion; the look-around (additive) poses may be a little off
                assert!((l - 1.0).abs() < 0.05, "{name}/{}: rotation key length {l}", t.name);
            }
        }
        if let Translation::Keys { at, v } = &t.trans {
            assert_eq!(at.len(), v.len());
            assert!(at.windows(2).all(|w| w[0] < w[1]), "{name}/{}: translation frames {at:?}", t.name);
            assert!(at.iter().all(|&f| f <= a.num_frames));
            assert!(v.iter().flatten().all(|c| c.is_finite() && c.abs() < 1.0e6), "{name}/{}: translation {v:?}", t.name);
        }
    }
    for (n, time) in &a.notifies {
        // (a few notetracks have an empty name: script string 0)
        assert!((0.0..=1.0001).contains(time), "{name}: notetrack {n} at {time}");
    }
}

#[test]
fn common_mp_animations_decode_with_plausible_keys() {
    let Some(path) = zone("common_mp.ff") else { return };
    let (anims, report) = read_anims_file(&path).unwrap();
    assert!(report.exact);
    assert!(anims.len() > 4000, "{} animations", anims.len());
    let mut by_type = std::collections::BTreeMap::<u8, usize>::new();
    for a in &anims {
        let an = a.anim.as_ref().unwrap_or_else(|e| panic!("{}: {e}", a.name));
        check_anim(&a.name, an);
        *by_type.entry(an.asset_type).or_default() += 1;
    }
    println!("asset types: {by_type:?}");
    // known entries: a loop of 384 frames at 30 fps, a death with root motion, an additive look pose
    let find = |n: &str| anims.iter().find(|a| a.name == n).and_then(|a| a.anim.as_ref().ok()).unwrap_or_else(|| panic!("{n} missing"));
    let alert = find("pb_stand_alert");
    assert_eq!((alert.num_frames, alert.frame_rate as u32, alert.looped), (383, 30, true));
    assert!(alert.tracks.len() > 60 && alert.notifies.len() == 1);
    assert!(find("pb_stand_death_headchest_topple").delta.is_some());
    assert_eq!(find("german_shepherd_look_up").asset_type, 6);
}

#[test]
fn translations_are_offsets_from_the_bind_pose() {
    use glam::{Quat, Vec3};
    let (Some(az), Some(mz)) = (zone("common_mp.ff"), zone("faction_fbi_mp.ff")) else { return };
    let (anims, _) = read_anims_file(&az).unwrap();
    let (models, _) = read_models_file(&mz).unwrap();
    let md = models.models.iter().find(|m| m.name == "c_usa_mp_fbi_assault_fb").unwrap();
    let alert = anims.iter().find(|a| a.name == "pb_stand_alert").unwrap().anim.as_ref().unwrap();
    // forward kinematics at several frames: the model's bind pose with the animation's rotations in place of the bone's own and
    // its translations added to the bind translations
    for frame in [0.0f32, 50.0, 150.0, 300.0] {
        let n = md.bones.len();
        let (mut wp, mut wr) = (vec![Vec3::ZERO; n], vec![Quat::IDENTITY; n]);
        for (i, b) in md.bones.iter().enumerate() {
            let t = alert.tracks.iter().find(|t| t.name == b.name);
            let off = t.and_then(|t| t.translation_at(frame)).map(Vec3::from).unwrap_or(Vec3::ZERO);
            let rot = t.and_then(|t| t.rotation_at(frame)).map(Quat::from_array);
            if b.parent < 0 {
                wp[i] = Vec3::from(b.global.0) + off;
                wr[i] = rot.unwrap_or(Quat::from_array(b.global.1));
            } else {
                let p = b.parent as usize;
                wp[i] = wp[p] + wr[p] * (Vec3::from(b.local.0) + off);
                wr[i] = wr[p] * rot.unwrap_or(Quat::from_array(b.local.1));
            }
        }
        let z = |name: &str| wp[md.bones.iter().position(|b| b.name == name).unwrap()].z;
        // feet on the ground (the bind ankle is 4.8 inches up), pelvis and head above it at a person's height
        assert!((0.0..12.0).contains(&z("j_ankle_le")) && (0.0..12.0).contains(&z("j_ankle_ri")), "frame {frame}: ankles at {} / {}", z("j_ankle_le"), z("j_ankle_ri"));
        assert!((20.0..42.0).contains(&z("j_mainroot")), "frame {frame}: pelvis at {}", z("j_mainroot"));
        assert!((45.0..70.0).contains(&z("j_head")), "frame {frame}: head at {}", z("j_head"));
    }
}

#[test]
fn exported_animation_casts_are_well_formed() {
    let Some(path) = zone("common_mp.ff") else { return };
    let out = std::env::temp_dir().join(format!("atlas_t6anim_{}", std::process::id()));
    let opts = T6Options { anims: Some("pb_stand_alert*".into()), no_models: true, ..Default::default() };
    let sum = export_zones_with(&path, &out, &opts).unwrap();
    assert_eq!(sum.anims, 3, "{:?}", sum.problems);
    let (anims, _) = read_anims_file(&path).unwrap();
    let a = anims.iter().find(|a| a.name == "pb_stand_alert").unwrap().anim.as_ref().unwrap();
    let bytes = std::fs::read(out.join("xanim").join("pb_stand_alert.cast")).unwrap();
    let seen = parse_cast(&bytes);
    assert_eq!((seen.count(ROOT), seen.count(ANIMATION), seen.count(MODEL)), (1, 1, 0));
    let anim_node = seen.nodes.iter().position(|n| n.0 == ANIMATION).unwrap();
    assert_eq!(seen.prop(anim_node, "fr").unwrap().f32s(), vec![30.0]);
    // one rotation curve per rotated bone and three location curves per translated bone
    let rot_bones = a.tracks.iter().filter(|t| matches!(t.rot, Rotation::Keys { .. })).count();
    let loc_bones = a.tracks.iter().filter(|t| matches!(t.trans, Translation::Keys { .. })).count();
    let curves: Vec<usize> = (0..seen.nodes.len()).filter(|&i| seen.nodes[i].0 == CURVE).collect();
    assert_eq!(curves.len(), rot_bones + 3 * loc_bones);
    let (mut rq, mut loc) = (0, 0);
    for &c in &curves {
        assert_eq!(seen.nodes[c].2, Some(anim_node), "curves hang off the animation");
        let (key, mode, bone) = (seen.prop(c, "kp").unwrap().string(), seen.prop(c, "m").unwrap().string(), seen.prop(c, "nn").unwrap().string());
        let (frames, values) = (seen.prop(c, "kb").unwrap(), seen.prop(c, "kv").unwrap());
        assert_eq!(frames.len, values.len, "{bone}/{key}: frames and values");
        let f = frames.u32s();
        assert!(f.windows(2).all(|w| w[0] < w[1]) && *f.last().unwrap() <= a.num_frames as u32, "{bone}/{key}: frames {f:?}");
        let track = a.tracks.iter().find(|t| t.name == bone).unwrap_or_else(|| panic!("curve for unknown bone {bone}"));
        if key == "rq" {
            rq += 1;
            assert_eq!((mode.as_str(), values.code.as_str()), ("absolute", "4v"));
            let q = values.f32s();
            assert!(q.chunks_exact(4).all(|c| (c.iter().map(|x| x * x).sum::<f32>().sqrt() - 1.0).abs() < 0.05));
            let Rotation::Keys { at, .. } = &track.rot else { panic!() };
            assert_eq!(f, at.iter().map(|&x| x as u32).collect::<Vec<_>>());
        } else {
            loc += 1;
            assert_eq!(mode, "relative", "{bone}/{key}: translations are offsets from the bind pose");
            assert!(["tx", "ty", "tz"].contains(&key.as_str()));
        }
    }
    assert_eq!((rq, loc), (rot_bones, 3 * loc_bones));
    // a location is in centimetres: an inch value times 2.54
    let t = a.tracks.iter().find(|t| t.name == "j_mainroot").unwrap();
    let Translation::Keys { v, .. } = &t.trans else { panic!() };
    let c = curves.iter().copied().find(|&c| seen.prop(c, "nn").unwrap().string() == "j_mainroot" && seen.prop(c, "kp").unwrap().string() == "tx").unwrap();
    assert!((seen.prop(c, "kv").unwrap().f32s()[0] - v[0][0] * 2.54).abs() < 1e-4);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn every_zone_animation_decodes_when_asked() {
    let Some(dir) = std::env::var_os("CODOL_TEST_T6").map(PathBuf::from).map(|d| d.join("zone")) else { return };
    if std::env::var_os("CODOL_TEST_T6_ALL").is_none() {
        return;
    }
    fn walk(d: &std::path::Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "ff") {
                out.push(p);
            }
        }
    }
    let mut files = Vec::new();
    walk(&dir, &mut files);
    let mut total = 0;
    for f in &files {
        let (anims, report) = read_anims_file(f).unwrap();
        assert!(report.exact, "{}", f.display());
        for a in &anims {
            let an = a.anim.as_ref().unwrap_or_else(|e| panic!("{} in {}: {e}", a.name, f.display()));
            check_anim(&a.name, an);
            total += 1;
        }
    }
    println!("{total} animations in {} zones", files.len());
    assert!(total > 10_000);
}
