//! Terrain collision read from every 3.19 map zone and checked against its own aabb trees.
use atlas::zone::{Zone, export, terrain};
use std::path::PathBuf;

#[test]
fn terrain_triangles_lie_in_their_tree_boxes() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let mut maps = 0;
    for path in export::zone_files(&root) {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.starts_with("mp_") || name.ends_with("_load.ff") || name.ends_with("_const.ff") || name == "mp_playlists.ff" {
            continue;
        }
        let z = Zone::open(&path, &key).unwrap();
        let t = terrain::read(&z).unwrap();
        let parts = t.reachable_partitions();
        let (mut tris, mut outside, mut broken) = (0usize, 0usize, 0usize);
        for &(p, node) in &parts {
            let n = &t.nodes[node];
            let Some(list) = t.partition_triangles(p) else {
                broken += 1;
                continue;
            };
            for tri in list {
                tris += 1;
                let inside = tri.iter().all(|v| (0..3).all(|k| (v[k] - n.origin[k]).abs() <= n.half[k] + 0.5));
                if !inside {
                    outside += 1;
                }
            }
        }
        println!("{name}: {} verts, {} triangles in {} partitions ({} of {} reachable from the BSP), {} nodes; {} outside their box, {} unreadable", t.verts.len(), t.tris.len(), t.partitions.len(), parts.len(), t.partitions.len(), t.nodes.len(), outside, broken);
        assert_eq!(broken, 0, "{name}: partitions that point outside the vertex or triangle arrays");
        assert!(outside * 100 <= tris.max(1), "{name}: {outside} of {tris} triangles lie outside the box of their aabb tree node");
        maps += 1;
    }
    assert!(maps > 30);
}
