//! The collision brushes of a 3.19 map zone, rebuilt for the editor (`zone::mapsrc`), must be real convex solids: the hull of each
//! brush's planes has to fill the box the zone stores as the brush's bounds.

use atlas::zone::mapsrc::{self, Brush};
use atlas::zone::{Zone, export};
use std::path::PathBuf;

fn hull_aabb(b: &Brush) -> Option<([f64; 3], [f64; 3], usize)> {
    let p = &b.sides;
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    let mut count = 0usize;
    for i in 0..p.len() {
        for j in i + 1..p.len() {
            for k in j + 1..p.len() {
                let (a, bb, c) = (p[i].normal, p[j].normal, p[k].normal);
                let cross = |u: [f64; 3], v: [f64; 3]| [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                let dot = |u: [f64; 3], v: [f64; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
                let det = dot(a, cross(bb, c));
                if det.abs() < 1e-6 {
                    continue;
                }
                let (bc, ca, ab) = (cross(bb, c), cross(c, a), cross(a, bb));
                let x: Vec<f64> = (0..3).map(|n| (p[i].dist * bc[n] + p[j].dist * ca[n] + p[k].dist * ab[n]) / det).collect();
                let v = [x[0], x[1], x[2]];
                if p.iter().all(|s| dot(s.normal, v) <= s.dist + 0.05) {
                    count += 1;
                    for n in 0..3 {
                        lo[n] = lo[n].min(v[n]);
                        hi[n] = hi[n].max(v[n]);
                    }
                }
            }
        }
    }
    (count >= 4).then_some((lo, hi, count))
}

#[test]
fn brushes_of_a_319_map_are_closed_solids() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let maps: Vec<PathBuf> = export::zone_files(&root)
        .into_iter()
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            n.starts_with("mp_") && !n.ends_with("_load.ff") && !n.ends_with("_const.ff") && n != "mp_playlists.ff"
        })
        .collect();
    assert!(maps.len() > 30, "{} map zones", maps.len());
    for path in maps {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let name = name.as_str();
        let zone = Zone::open(&path, &key).unwrap();
        let m = mapsrc::read(&zone, name).unwrap();
        let mut all: Vec<&Brush> = m.world.iter().collect();
        all.extend(m.submodels.iter().flat_map(|s| s.1.iter()));
        let (mut ok, mut tight, mut degenerate) = (0usize, 0usize, 0usize);
        for b in &all {
            let Some((lo, hi, _)) = hull_aabb(b) else {
                degenerate += 1;
                continue;
            };
            ok += 1;
            // the first six sides are the axial ones: -x +x -y +y -z +z (axis-major, min then max)
            let box_lo = [-b.sides[0].dist, -b.sides[2].dist, -b.sides[4].dist];
            let box_hi = [b.sides[1].dist, b.sides[3].dist, b.sides[5].dist];
            if (0..3).all(|n| (lo[n] - box_lo[n]).abs() < 0.1 && (hi[n] - box_hi[n]).abs() < 0.1) {
                tight += 1;
            }
        }
        println!("{name}: {} brushes, {ok} solid, {tight} fill their bounds exactly, {degenerate} degenerate; {} entities, {} models, notes {:?}", all.len(), m.entities.len(), m.models.len(), m.report);
        assert!(degenerate * 50 < all.len(), "{degenerate} degenerate of {}", all.len());
        assert!(tight * 10 > ok * 9, "only {tight} of {ok} brushes fill their bounds");
        assert!(m.entities.len() > 100 && m.models.len() > 50, "{name}: {} entities, {} models", m.entities.len(), m.models.len());
        assert!(m.report.iter().all(|l| !l.contains("unresolved") && !l.contains("out of range")), "{name}: {:?}", m.report);
        // the angles written for a static model give its stored placement back (the engine's AnglesToAxis)
        let mut worst = 0.0f64;
        for sm in &m.models {
            let back = mapsrc::angles_to_axis(&mapsrc::axis_to_angles(&sm.axis));
            for r in 0..3 {
                for c in 0..3 {
                    worst = worst.max((back[r][c] - sm.axis[r][c]).abs());
                }
            }
        }
        assert!(worst < 2e-3, "{name}: a static model's angles are {worst} off its placement");
    }
}

/// Editing the entity text of a 3.19 map zone (nested in its collision map) and building the zone again must keep every brush and
/// static model, and carry the new text.
#[test]
fn edited_entities_of_a_319_map_survive_a_rebuild() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    for name in ["mp_bridge", "mp_afghan", "mp_battlecity"] {
        edit_round_trip(&root, &key, name);
    }
}

fn edit_round_trip(root: &std::path::Path, key: &atlas::zone::fastfile::ArchiveKey, name: &str) {
    let (key, root) = (key, root);
    let file = format!("{name}.ff");
    let path = export::zone_files(root).into_iter().find(|p| p.file_name().unwrap().to_string_lossy() == file).unwrap();
    let before = mapsrc::read(&Zone::open(&path, key).unwrap(), name).unwrap();

    let dir = std::env::temp_dir().join(format!("atlas_ents_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    atlas::zonefiles::unpack(&path, key, &dir).unwrap();
    let manifest = std::fs::read_to_string(dir.join("zone.tsv")).unwrap();
    let row = manifest.lines().skip(1).map(|l| l.split('\t').collect::<Vec<_>>()).find(|r| r[1] == "15" && !r[5].is_empty()).expect("a clip map with entity text");
    let text_file = dir.join(row[5]);
    let text = std::fs::read_to_string(&text_file).unwrap();
    let edited = format!("{text}\n{{\n\"classname\" \"script_origin\"\n\"targetname\" \"atlas_milestone3\"\n\"origin\" \"1 2 3\"\n}}\n");
    std::fs::write(&text_file, &edited).unwrap();

    let out = dir.join("rebuilt.ff");
    let rep = atlas::zonefiles::build(&dir, key, &out, false).unwrap();
    assert!(!rep.replaced.is_empty());
    let after_zone = Zone::open(&out, key).unwrap();
    let after = mapsrc::read(&after_zone, name).unwrap();
    assert_eq!(after.entities.len(), before.entities.len() + 1);
    assert_eq!(after.entities.last().unwrap().get("targetname"), Some("atlas_milestone3"));
    assert_eq!(after.world.len(), before.world.len());
    assert_eq!(after.submodels.len(), before.submodels.len());
    assert_eq!(after.models.len(), before.models.len());
    let same = |a: &Brush, b: &Brush| a.sides.len() == b.sides.len() && a.sides.iter().zip(&b.sides).all(|(x, y)| x.normal == y.normal && x.dist == y.dist && x.material == y.material);
    assert!(before.world.iter().zip(&after.world).all(|(a, b)| same(a, b)), "world brushes changed");
    println!("{name}: rebuilt {} bytes, {} references relocated, {} entities -> {}", rep.bytes, rep.relocated, before.entities.len(), after.entities.len());
    let _ = std::fs::remove_dir_all(&dir);
}
