//! The zones' own leaf-brush trees must find every brush for queries that touch it (this validates the reading of the tree
//! and the engine's walk), and a tree built from the same boxes must do the same.
use atlas::zone::colltree::{self, Tree};
use atlas::zone::{Zone, export};
use std::path::PathBuf;

#[test]
fn zone_trees_and_built_trees_find_every_brush() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let mut maps = 0;
    let (mut zone_nodes, mut built_nodes) = (0usize, 0usize);
    for path in export::zone_files(&root) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("mp_") || name.ends_with("_load.ff") || name.ends_with("_const.ff") || name == "mp_playlists.ff" {
            continue;
        }
        let z = Zone::open(&path, &key).unwrap();
        let t = colltree::read(&z).unwrap();
        // the zone's own tree
        for (mi, (node, members)) in t.roots.iter().enumerate().skip(1) {
            if members.is_empty() {
                continue;
            }
            let missed = t.tree.check(*node, &t.boxes, members, 8.0, 3);
            assert!(missed.is_empty(), "{name}: the zone's own tree misses brushes of model {mi}: {missed:?}");
        }
        // the world is reached through the BSP leaves. A brush that some leaf tree lists must be found by the walk from that
        // leaf's root with its own box (this checks the reading of the tree and the engine's traversal); brushes that no tree
        // lists at all are leftovers of the map compiler the engine never tests
        let world = &t.roots[0].1;
        let (mut listed, mut found) = (vec![false; t.boxes.len()], vec![false; t.boxes.len()]);
        for &root in &t.leaf_roots {
            let mut mine = Vec::new();
            t.tree_brushes(root, &mut mine);
            for b in mine {
                listed[b as usize] = true;
                if t.tree.query(root, &t.boxes[b as usize]).contains(&b) {
                    found[b as usize] = true;
                }
            }
        }
        let unwalkable: Vec<u16> = world.iter().copied().filter(|&b| listed[b as usize] && !found[b as usize]).take(4).collect();
        assert!(unwalkable.is_empty(), "{name}: brushes listed in a leaf tree but not found by walking it: {unwalkable:?}");
        let unlisted = world.iter().filter(|&&b| !listed[b as usize] && t.contents[b as usize] != 0).count();
        if unlisted > 0 {
            println!("{name}: {unlisted} of {} world brushes are in no leaf tree", world.len());
        }
        assert!(unlisted * 500 <= world.len(), "{name}: {unlisted} brushes are in no leaf tree");
        // a tree built over the world's brushes
        let built = Tree::build(&t.boxes, &t.contents, world, 4);
        let missed = built.check(0, &t.boxes, world, 8.0, 3);
        assert!(missed.is_empty(), "{name}: the built tree misses {missed:?}");
        zone_nodes += t.tree.nodes.len();
        built_nodes += built.nodes.len();
        maps += 1;
    }
    println!("{maps} maps: the zones' trees hold {zone_nodes} nodes in all, the built ones {built_nodes}");
    assert!(maps > 30);
}

/// The zones' own BSPs, read and walked like `CM_BoxLeafnums`: a brush must be listed by the tree of at least one leaf its box
/// overlaps. If the child order or the plane side were read wrongly, this would fail on nearly every brush. Then a BSP built over the
/// same brushes must list every brush in every leaf its box overlaps.
#[test]
fn the_zones_bsp_reaches_their_brushes_and_a_built_bsp_does_too() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let mut maps = 0;
    for path in export::zone_files(&root) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("mp_") || name.ends_with("_load.ff") || name.ends_with("_const.ff") || name == "mp_playlists.ff" {
            continue;
        }
        let z = Zone::open(&path, &key).unwrap();
        let t = colltree::read(&z).unwrap();
        let world = &t.roots[0].1;
        // brushes listed under each leaf's tree
        let mut listed: Vec<std::collections::HashSet<u16>> = Vec::with_capacity(t.leaf_root_of.len());
        for &r in &t.leaf_root_of {
            let mut v = Vec::new();
            if r != 0 {
                t.tree_brushes(r, &mut v);
            }
            listed.push(v.into_iter().collect());
        }
        let (mut checked, mut missed) = (0usize, 0usize);
        for &b in world {
            if t.contents[b as usize] == 0 || !listed.iter().any(|l| l.contains(&b)) {
                continue;
            }
            checked += 1;
            let bx = t.boxes[b as usize];
            if !t.bsp.leaves_in(&bx).iter().any(|&l| listed.get(l).map_or(false, |s| s.contains(&b))) {
                missed += 1;
            }
        }
        assert!(missed * 200 <= checked, "{name}: {missed} of {checked} brushes are not listed by any leaf their box overlaps (BSP read wrongly?)");

        // a built BSP over the world brushes
        let bsp = colltree::Bsp::build(&t.boxes, world, 16);
        assert!(bsp.nodes.len() <= 32767 && bsp.leaves.len() <= 32768, "{name}: {} nodes, {} leaves", bsp.nodes.len(), bsp.leaves.len());
        for &b in world {
            let bx = t.boxes[b as usize];
            for l in bsp.leaves_in(&bx) {
                assert!(bsp.leaves[l].members.contains(&b), "{name}: brush {b} overlaps leaf {l} but is not listed there");
            }
            let c: [f32; 3] = std::array::from_fn(|k| (bx.lo[k] + bx.hi[k]) * 0.5);
            assert!(bsp.leaves[bsp.leaf_of(c)].members.contains(&b), "{name}: brush {b}'s centre is in a leaf that does not list it");
        }
        println!("{name}: zone BSP {} nodes / {} leaves, built {} nodes / {} leaves for {} brushes", t.bsp.nodes.len(), t.bsp.leaves.len(), bsp.nodes.len(), bsp.leaves.len(), world.len());
        maps += 1;
    }
    assert!(maps > 30);
}
