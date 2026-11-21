//! Writing an edited .map back into a 3.19 zone: static models move / turn, entities change and grow, and what is not supported is refused.
use atlas::zone::{Zone, export, mapsrc, mapwrite};
use std::path::PathBuf;

fn find(root: &std::path::Path, name: &str) -> PathBuf {
    export::zone_files(root).into_iter().find(|p| p.file_name().unwrap().to_string_lossy() == name).unwrap()
}

/// The text span of the static model entity with this index.
fn find_entity(text: &str, idx: usize) -> (usize, usize) {
    let start = text.find(&format!("\"atlas_static\" \"{idx}\"")).unwrap();
    let s = text[..start].rfind("{\n").unwrap();
    let e = start + text[start..].find("\n}\n").unwrap() + 3;
    (s, e)
}

fn block(m: &mapsrc::StaticModel, idx: usize, origin: [f64; 3], angles: [f64; 3], scale: f64) -> String {
    format!(
        "{{\n\"classname\" \"misc_model\"\n\"atlas_static\" \"{idx}\"\n\"model\" \"{}\"\n\"origin\" \"{} {} {}\"\n\"angles\" \"{} {} {}\"\n\"modelscale\" \"{}\"\n}}\n",
        m.name, origin[0], origin[1], origin[2], angles[0], angles[1], angles[2], scale
    )
}

#[test]
fn an_edited_map_goes_back_into_the_zone() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    for name in ["mp_bridge.ff", "mp_afghan.ff"] {
        let path = find(&root, name);
        let zone = Zone::open(&path, &key).unwrap();
        let orig = mapsrc::read(&zone, name).unwrap();
        let map = mapsrc::to_map(&orig);
        let dir = std::env::temp_dir().join(format!("atlas_map2zone_{}_{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // 1. the map as exported matches the zone: nothing to write
        let parsed = mapsrc::parse_map(&map).unwrap();
        assert_eq!(parsed.entities.len(), orig.entities.len(), "{name}");
        assert_eq!(parsed.models.len(), orig.models.len(), "{name}");
        let plan = mapwrite::plan(&zone, &orig, &parsed).unwrap();
        assert_eq!((plan.models_changed, plan.entities_changed), (0, false), "{name}: an unedited map changed something: {:?}", plan.notes);

        // 2. edit the text: move model 0 by +100 in x, turn model 1 by 90 degrees of yaw and double its scale, change an
        //    entity key, add an entity
        let (o0, o1) = (orig.models[0].clone(), orig.models[1].clone());
        let mut edited = map.clone();
        let a1 = mapsrc::axis_to_angles(&o1.axis);
        let (s1, e1) = find_entity(&edited, 1);
        edited.replace_range(s1..e1, &block(&o1, 1, o1.origin, [a1[0], (a1[1] + 90.0) % 360.0, a1[2]], o1.scale * 2.0));
        let a0 = mapsrc::axis_to_angles(&o0.axis);
        let (s0, e0) = find_entity(&edited, 0);
        edited.replace_range(s0..e0, &block(&o0, 0, [o0.origin[0] + 100.0, o0.origin[1], o0.origin[2]], a0, o0.scale));
        edited = edited.replacen("\"classname\" \"worldspawn\"\n", "\"classname\" \"worldspawn\"\n\"atlas_test_key\" \"edited\"\n", 1);
        let tail = edited.rfind("// entity").unwrap();
        edited.insert_str(tail, "// entity new\n{\n\"classname\" \"script_origin\"\n\"targetname\" \"atlas_m7\"\n\"origin\" \"5 6 7\"\n}\n");

        let out = dir.join("out.ff");
        let plan = mapwrite::apply(&path, &key, &edited, &out).unwrap();
        assert_eq!(plan.models_changed, 2, "{name}");
        assert!(plan.entities_changed, "{name}");
        println!("{name}: {} models changed, {} with collision", plan.models_changed, plan.collision_changed);

        // 3. read the result back
        let after_zone = Zone::open(&out, &key).unwrap();
        let after = mapsrc::read(&after_zone, name).unwrap();
        assert_eq!(after.models.len(), orig.models.len(), "{name}");
        assert_eq!(after.entities.len(), orig.entities.len() + 1, "{name}");
        assert_eq!(after.entities[0].get("atlas_test_key"), Some("edited"), "{name}");
        assert_eq!(after.entities.last().unwrap().get("targetname"), Some("atlas_m7"), "{name}");
        let m0 = &after.models[0];
        assert!((m0.origin[0] - (o0.origin[0] + 100.0)).abs() < 0.01 && (m0.origin[1] - o0.origin[1]).abs() < 0.01, "{name}: model 0 {:?} vs {:?}", m0.origin, o0.origin);
        let m1 = &after.models[1];
        assert!((m1.scale - o1.scale * 2.0).abs() < 1e-3, "{name}: scale {}", m1.scale);
        let ang = mapsrc::axis_to_angles(&m1.axis);
        let want = (a1[1] + 90.0) % 360.0;
        let d = (ang[1] - want).rem_euclid(360.0);
        assert!(d.min(360.0 - d) < 0.05, "{name}: yaw {} vs {want}", ang[1]);
        // everything else is untouched
        for i in 2..orig.models.len() {
            assert_eq!(after.models[i].origin, orig.models[i].origin, "{name}: model {i} moved");
            assert_eq!(after.models[i].axis, orig.models[i].axis, "{name}: model {i} turned");
        }
        assert_eq!(after.world.len(), orig.world.len());
        assert_eq!(after.submodels.len(), orig.submodels.len());

        // 4. what is not supported is refused
        let (s, e) = find_entity(&map, 3);
        let mut deleted = map.clone();
        deleted.replace_range(s..e, "");
        let err = mapwrite::apply(&path, &key, &deleted, &dir.join("no.ff")).unwrap_err().to_string();
        assert!(err.contains("static model"), "{name}: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A model that has collision: its collision record moves with it (origin, inverse scaled axis, absolute bounds), the rest stays.
#[test]
fn collision_of_a_moved_static_model_moves_too() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let name = "mp_bridge.ff";
    let path = find(&root, name);
    let zone = Zone::open(&path, &key).unwrap();
    let orig = mapsrc::read(&zone, name).unwrap();
    let f = |z: &Zone, o: usize| f32::from_le_bytes(z.data[o..o + 4].try_into().unwrap());
    let (c0, cn) = orig.layout.clip_statics;
    // a model and the collision record at its origin
    let (mi, ci) = (0..orig.models.len())
        .find_map(|mi| {
            let (at, _) = orig.layout.draw[mi];
            let o = [f(&zone, at), f(&zone, at + 4), f(&zone, at + 8)];
            (0..cn).find(|&ci| [f(&zone, c0 + 76 * ci + 4), f(&zone, c0 + 76 * ci + 8), f(&zone, c0 + 76 * ci + 12)] == o).map(|ci| (mi, ci))
        })
        .expect("a static model with collision");
    let before_mid: Vec<f32> = (0..3).map(|k| f(&zone, c0 + 76 * ci + 52 + 4 * k)).collect();
    let m = orig.models[mi].clone();
    let mut map = mapsrc::to_map(&orig);
    let (s, e) = find_entity(&map, mi);
    map.replace_range(s..e, &block(&m, mi, [m.origin[0], m.origin[1] + 64.0, m.origin[2]], m.angles, m.scale));
    let out = std::env::temp_dir().join(format!("atlas_map2zone_coll_{}.ff", std::process::id()));
    let plan = mapwrite::apply(&path, &key, &map, &out).unwrap();
    assert_eq!((plan.models_changed, plan.collision_changed, plan.entities_changed), (1, 1, false));
    let after = Zone::open(&out, &key).unwrap();
    let a = mapsrc::read(&after, name).unwrap();
    assert_eq!(a.layout.clip_statics, orig.layout.clip_statics);
    let at = c0 + 76 * ci;
    assert!((f(&after, at + 8) as f64 - (m.origin[1] + 64.0)).abs() < 0.01, "collision origin y");
    assert!((f(&after, at + 4) as f64 - m.origin[0]).abs() < 0.01, "collision origin x");
    assert!((f(&after, at + 56) - (before_mid[1] + 64.0)).abs() < 0.01, "collision bounds follow");
    assert!((a.models[mi].origin[1] - (m.origin[1] + 64.0)).abs() < 0.01, "draw record");
    // an unchanged neighbour keeps its collision record
    let other = (ci + 1) % cn;
    for k in 0..19 {
        assert_eq!(f(&zone, c0 + 76 * other + 4 + 4 * k).to_bits(), f(&after, c0 + 76 * other + 4 + 4 * k).to_bits());
    }
    let _ = std::fs::remove_file(out);
}

fn brush_span(text: &str, idx: usize) -> (usize, usize) {
    let start = text.find(&format!("// brush {idx}\n{{\n")).unwrap();
    let end = start + text[start..].find("\n}\n").unwrap() + 3;
    (start, end)
}

/// Brushes deleted, added and changed in the .map go back into the collision map; the trees find them, the rest is untouched.
#[test]
fn brushes_can_be_deleted_added_and_changed() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    for name in ["mp_dr_cistern.ff", "mp_bridge.ff"] {
        let path = find(&root, name);
        let zone = Zone::open(&path, &key).unwrap();
        let orig = mapsrc::read(&zone, name).unwrap();
        let before_tree = atlas::zone::colltree::read(&zone).unwrap();
        let map = mapsrc::to_map(&orig);

        // delete world brush 5, move brush 7 by 24 units in x (a changed brush), and add a 64 unit cube at the origin
        let mut edited = map.clone();
        let (s7, e7) = brush_span(&edited, 7);
        let moved = {
            let block = &edited[s7..e7];
            let mut out = String::new();
            for line in block.lines() {
                if line.starts_with('(') {
                    let mut rebuilt = String::new();
                    let mut rest = line;
                    for _ in 0..3 {
                        let open = rest.find('(').unwrap();
                        let close = rest.find(')').unwrap();
                        let t: Vec<f64> = rest[open + 1..close].split_whitespace().map(|x| x.parse().unwrap()).collect();
                        rebuilt.push_str(&format!("( {} {} {} ) ", t[0] + 24.0, t[1], t[2]));
                        rest = &rest[close + 1..];
                    }
                    out.push_str(&rebuilt);
                    out.push_str(rest.trim_start());
                } else {
                    out.push_str(line);
                }
                out.push('\n');
            }
            out
        };
        edited.replace_range(s7..e7, &moved);
        let (s5, e5) = brush_span(&edited, 5);
        edited.replace_range(s5..e5, "");
        // a 64 unit cube, written by the exporter itself so that the plane winding is right
        let side = |n: [f64; 3], d: f64| mapsrc::Side { normal: n, dist: d, material: "caulk".into() };
        let cube_brush = mapsrc::Brush {
            sides: vec![side([-1.0, 0.0, 0.0], 0.0), side([1.0, 0.0, 0.0], 64.0), side([0.0, -1.0, 0.0], 0.0), side([0.0, 1.0, 0.0], 64.0), side([0.0, 0.0, -1.0], 0.0), side([0.0, 0.0, 1.0], 64.0)],
            contents: 1,
            id: mapsrc::NO_ID,
        };
        let cube_map = mapsrc::to_map(&mapsrc::MapSource { entities: vec![mapsrc::Entity { keys: vec![("classname".into(), "worldspawn".into())] }], world: vec![cube_brush], ..Default::default() });
        let cube_start = cube_map.find("// brush 0").unwrap();
        let cube = format!("{}
", cube_map[cube_start..].trim_end().trim_end_matches('}').trim_end()).replace("// brush 0", "// brush new");
        // into the world entity: before its closing brace
        let world_end = edited.find("\n}\n// entity 1\n").unwrap() + 1;
        edited.insert_str(world_end, &cube);

        let dir = std::env::temp_dir().join(format!("atlas_brushes_{}_{}", name, std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.ff");
        let plan = mapwrite::apply(&path, &key, &edited, &out).unwrap();
        println!("{name}: {:?}", plan.notes);
        assert!(plan.rebuilt.is_some(), "{name}");

        let after_zone = Zone::open(&out, &key).unwrap();
        let after = mapsrc::read(&after_zone, name).unwrap();
        // the old brush 5 is gone (contents 0), brush 7 became a new brush, one cube is new: world = orig - 2 + 2 + 1
        // deleted brushes stay in the arrays with contents 0; two went (brush 5, the old brush 7) and two came (the cube, the moved brush)
        let tot = |m: &mapsrc::MapSource| m.submodels.iter().map(|s| s.1.len()).sum::<usize>();
        println!("{name}: world {} -> {}, brush model brushes {} -> {}, models with brushes {} -> {}", orig.world.len(), after.world.len(), tot(&orig), tot(&after), orig.submodels.iter().filter(|s| !s.1.is_empty()).count(), after.submodels.iter().filter(|s| !s.1.is_empty()).count());
        assert_eq!(after.world.len(), orig.world.len() + 2, "{name}: world brush records");
        assert_eq!(after.world.iter().filter(|b| b.contents != 0).count(), orig.world.iter().filter(|b| b.contents != 0).count(), "{name}: brushes in play");
        let has = |m: &mapsrc::MapSource, f: &dyn Fn(&mapsrc::Brush) -> bool| m.world.iter().filter(|b| b.contents != 0).any(|b| f(b));
        // a cube: six planes at 0 and 64
        let is_cube = |b: &mapsrc::Brush| b.sides.len() >= 6 && b.sides.iter().filter(|s| (s.dist - 64.0).abs() < 0.1 || s.dist.abs() < 0.1).count() >= 6;
        assert!(has(&after, &is_cube), "{name}: the new cube is not there");
        // the moved brush: the same planes shifted by 24 in x, as a set (a rewritten brush keeps its non-axial planes and gets its
        // six axial ones from its bounds, so the order and the list can differ)
        let moved7 = &orig.world[7];
        let shifted: Vec<([f64; 3], f64)> = moved7.sides.iter().map(|y| (y.normal, y.dist + 24.0 * y.normal[0])).collect();
        let near = |x: &mapsrc::Side, n: &[f64; 3], d: f64| (0..3).all(|k| (x.normal[k] - n[k]).abs() < 0.01) && (x.dist - d).abs() < 0.2;
        if !has(&after, &|b| shifted.iter().all(|(n, d)| b.sides.iter().any(|x| near(x, n, *d))) && b.sides.iter().all(|x| shifted.iter().any(|(n, d)| near(x, n, *d)))) {
            println!("expected planes {:?}", shifted);
            for b in after.world.iter().filter(|b| b.contents != 0 && b.id as usize >= before_tree.boxes.len()) {
                println!("new brush {}: {:?}", b.id, b.sides.iter().map(|x| (x.normal, x.dist)).collect::<Vec<_>>());
            }
        }
        assert!(
            has(&after, &|b| shifted.iter().all(|(n, d)| b.sides.iter().any(|x| near(x, n, *d))) && b.sides.iter().all(|x| shifted.iter().any(|(n, d)| near(x, n, *d)))),
            "{name}: the moved brush is not there"
        );
        // brush models are untouched
        assert_eq!(after.submodels.iter().filter(|s| !s.1.is_empty()).count(), orig.submodels.iter().filter(|s| !s.1.is_empty()).count());
        // the trees are sound: every brush that is in play is listed by some leaf tree and found by walking it
        let t = atlas::zone::colltree::read(&after_zone).unwrap();
        let mut found = vec![false; t.boxes.len()];
        for &root in &t.leaf_roots {
            let mut mine = Vec::new();
            t.tree_brushes(root, &mut mine);
            for b in mine {
                if t.tree.query(root, &t.boxes[b as usize]).contains(&b) {
                    found[b as usize] = true;
                }
            }
        }
        let new_ids: Vec<usize> = after.world.iter().map(|b| b.id as usize).filter(|&i| i >= before_tree.boxes.len()).collect();
        assert_eq!(new_ids.len(), 2, "{name}: two new brushes expected");
        for id in &new_ids {
            assert!(found[*id], "{name}: new brush {id} is in no leaf tree");
        }
        assert_eq!(t.contents[orig.world[5].id as usize], 0, "{name}: the deleted brush still has contents");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
