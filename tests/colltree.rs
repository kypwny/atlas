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
