//! Zone reader checks against a real game folder. Run with CODOL_TEST_GAME set; CODOL_TEST_INDEX (an index made by
//! an external C exporter) additionally requires byte-identical output zone by zone.

use atlas::zone::{Zone, export, fastfile::ArchiveKey};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;

fn game() -> Option<PathBuf> {
    std::env::var_os("CODOL_TEST_GAME").map(PathBuf::from)
}

fn zone_blocks(text: &str, skip_state: bool) -> BTreeMap<String, Vec<String>> {
    let mut d: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut cur = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("zone\t") {
            let name = rest.split('\t').next().unwrap().to_string();
            d.insert(name.clone(), Vec::new());
            cur = Some(name);
        } else if let Some(c) = &cur {
            // The C exporter predates the render-state and skeleton records; everything else must match line for line.
            if skip_state && (line.starts_with("state	") || line.starts_with("const	") || line.starts_with("skel	")) {
                continue;
            }
            d.get_mut(c).unwrap().push(line.to_string());
        }
    }
    d
}

#[test]
fn marketplace_zone_replays_completely() {
    let Some(root) = game() else { return };
    let key = export::find_key(&root).expect("archive key");
    let path = export::zone_files(&root).into_iter().find(|p| p.ends_with("common_marketplace_lite.ff")).expect("zone");
    let zone = Zone::open(&path, &key).unwrap();
    assert_eq!(zone.assets.len(), 8728);
    let mut out = String::new();
    let r = export::export_zone(&zone, "common_marketplace_lite.ff", &mut out);
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    assert_eq!((r.models, r.materials), (3119, 4018));
}

#[test]
fn all_zones_replay_like_the_reference() {
    let Some(root) = game() else { return };
    let progress = (AtomicUsize::new(0), AtomicUsize::new(0));
    let text = export::build_index(&root, &progress).unwrap();
    let mine = zone_blocks(&text, true);
    eprintln!("{} zones indexed", mine.len());
    assert!(mine.len() >= 200);
    // Render state: most materials must decode, and the classes must be plausible (mostly opaque).
    let index = atlas::modelindex::ModelIndex::parse(&text);
    let (mut with_state, mut total, mut opaque, mut blended) = (0, 0, 0, 0);
    for m in index.models.iter().flat_map(|m| m.materials.iter().flatten()) {
        let Some(mat) = index.material(m) else { continue };
        total += 1;
        if let Some(st) = mat.render {
            with_state += 1;
            if st.blended() {
                blended += 1;
            } else {
                opaque += 1;
            }
        }
    }
    eprintln!("{with_state} of {total} material slots have render state ({opaque} opaque, {blended} blended)");
    assert!(with_state * 100 >= total * 90, "too few materials decode a render state");
    assert!(opaque > blended * 3, "implausible class split");
    if let Some(reference) = std::env::var_os("CODOL_TEST_INDEX") {
        let reference = zone_blocks(&String::from_utf8_lossy(&std::fs::read(reference).unwrap()), true);
        let different: Vec<&String> = reference.keys().filter(|z| mine.get(*z) != reference.get(*z)).collect();
        assert!(different.is_empty(), "zones that differ from the C exporter: {different:?}");
    }
}

#[test]
fn wrong_key_is_rejected() {
    // The archive key must verify against its hash; a random file is not a key.
    let tmp = std::env::temp_dir().join("atlas_not_a_key.bin");
    std::fs::write(&tmp, vec![0u8; 5000]).unwrap();
    assert!(ArchiveKey::load(&tmp).is_err());
    let _ = std::fs::remove_file(tmp);
}

#[test]
fn survey_reads_the_fastfile_version_from_every_zone_header() {
    let Some(root) = game() else { return };
    let s = export::survey(&root);
    assert!(s.total > 0);
    // a QA 3.3.3.3 folder is all version 423; any other build reports its own version
    assert_eq!(s.by_version.iter().map(|(_, n)| n).sum::<usize>() + s.unreadable, s.total);
    for ((f, v), _) in &s.by_version {
        assert!(!atlas::zone::fastfile::version_info(*f, *v).build.contains("unknown"), "unrecognised fastfile version {v}");
    }
}

/// The 3.19 client's zones (version 461) replay with the QA layouts plus the two differences that matter for models: one
/// more technique slot in techsets and one more state-bits entry in materials. What still fails is the weapon family
/// (3.19's definition is 500 bytes longer) and a few rare types.
#[test]
fn version_461_zones_replay_with_materials() {
    let Some(root) = std::env::var_os("CODOL_TEST_319").map(PathBuf::from) else { return };
    let key = export::find_key(&root).unwrap();
    let (mut assets, mut failed, mut weapons_failed, mut materials, mut models, mut with_textures) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let mut text = String::new();
    for path in export::zone_files(&root) {
        let Ok(z) = Zone::open(&path, &key) else { continue };
        let r = export::export_zone(&z, "", &mut text);
        assets += r.assets;
        models += r.models;
        materials += r.materials;
        failed += r.failed.len();
        weapons_failed += r.failed.iter().filter(|f| matches!(f.1, 28 | 41 | 45)).count();
    }
    for l in text.lines() {
        with_textures += l.starts_with("tex\t") as usize;
    }
    println!("{assets} assets, {failed} failed ({weapons_failed} of them weapon-family), {models} models, {materials} materials, {with_textures} texture slots");
    assert!(assets > 90_000 && materials > 15_000 && models > 15_000, "{assets} assets, {materials} materials, {models} models");
    // images, models, materials, techsets: almost nothing may fail apart from the weapon family
    assert!((failed - weapons_failed) * 100 <= assets, "{} non-weapon failures of {assets}", failed - weapons_failed);
}

/// A map zone's asset list has the usual types, and the images it defines are installed (so they can be packed into an IWD).
#[test]
fn a_zones_asset_list_names_assets_and_its_images_are_installed() {
    use atlas::source;
    let Some(root) = game() else { return };
    let key = export::find_key(&root).unwrap();
    let path = export::zone_files(&root).into_iter().find(|p| p.ends_with("asym_addon_mp_nuked.ff")).expect("zone");
    let z = Zone::open(&path, &key).unwrap();
    let list = export::zone_asset_list(&z);
    let count = |ty: &str| list.iter().filter(|(t, _)| *t == ty).count();
    println!("{} assets: {} xmodel, {} material, {} image, {} techset", list.len(), count("xmodel"), count("material"), count("image"), count("techset"));
    assert!(count("xmodel") > 0 && count("material") > 0 && count("image") > 0);
    // no leading commas (those mark names defined elsewhere), no duplicates
    assert!(list.iter().all(|(_, n)| !n.starts_with(',')));
    let unique: std::collections::HashSet<_> = list.iter().collect();
    assert_eq!(unique.len(), list.len());
    let cat = source::scan(source::discover(&root), &source::Progress::default());
    let images: Vec<&String> = list.iter().filter(|(t, _)| *t == "image").map(|(_, n)| n).collect();
    let found = images.iter().filter(|n| cat.entries.binary_search_by(|e| e.name.as_str().cmp(n.to_ascii_lowercase().as_str())).is_ok()).count();
    println!("{found} of {} images installed", images.len());
    assert!(found * 10 >= images.len() * 8, "only {found} of {} images are installed", images.len());
}

/// A zone image sealed into a fastfile (signed with hash tables, or unsigned) opens again to the same image.
#[test]
fn sealed_fastfiles_open_to_the_same_zone_image() {
    use atlas::zone::fastfile;
    for (var, name) in [("CODOL_TEST_GAME", "common_mp_reload.ff"), ("CODOL_TEST_319", "ak47_mp.ff")] {
        let Some(root) = std::env::var_os(var).map(PathBuf::from) else { continue };
        let key = export::find_key(&root).unwrap();
        let path = export::zone_files(&root).into_iter().find(|p| p.ends_with(name)).expect("zone");
        let opened = fastfile::open(&path, &key).unwrap();
        assert!(opened.container.signed, "{name} is expected to be signed");
        // signed, as the original
        let again = fastfile::open_bytes(&fastfile::seal(&opened.image, opened.version, &opened.container).unwrap(), &key).unwrap();
        assert_eq!(again.image, opened.image, "{name}: signed round trip");
        // unsigned
        let mut c = opened.container.clone();
        c.signed = false;
        let plain = fastfile::open_bytes(&fastfile::seal(&opened.image, opened.version, &c).unwrap(), &key).unwrap();
        assert_eq!(plain.image, opened.image, "{name}: unsigned round trip");
        println!("{name}: {} byte image, signed and unsigned round trips ok", opened.image.len());
    }
}


/// Taking a zone apart and putting it back together (new directory, recomputed block sizes, relocation pass) gives the
/// original image, byte for byte, for every zone of a build.
#[test]
fn zones_rebuild_to_the_identical_image() {
    use atlas::zone::writer::{ZoneBuilder, baseline_dangling};
    for var in ["CODOL_TEST_GAME", "CODOL_TEST_319"] {
        let Some(root) = std::env::var_os(var).map(PathBuf::from) else { continue };
        let key = export::find_key(&root).unwrap();
        let (mut done, mut skipped, mut sizes) = (0, Vec::new(), Vec::new());
        for path in export::zone_files(&root) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if std::env::var("CODOL_TEST_ZONE").is_ok_and(|only| only != name) {
                continue;
            }
            let Ok(z) = Zone::open(&path, &key) else { continue };
            let b = ZoneBuilder::from_zone(&z);
            match b.build(&z) {
                Ok(built) => {
                    if built.image != z.data {
                        // a difference limited to the header's block sizes means the zone's own sizes are not the walk's peaks
                        let body = built.image[0x28..] == z.data[0x28..] && built.image[..8] == z.data[..8];
                        let at = built.image.iter().zip(&z.data).position(|(a, b)| a != b);
                        assert!(body, "{name}: rebuilt image differs (lengths {} vs {}, first difference at {at:?})", built.image.len(), z.data.len());
                        sizes.push(format!("{name}: header {:?}, walk {:?}", z.block_sizes, built.block_sizes));
                    }
                    assert_eq!(built.relocated, 0, "{name}");
                    assert_eq!(built.dangling, baseline_dangling(&z).unwrap(), "{name}");
                    done += 1;
                }
                // zones with assets the replay does not support (weapon family on 461) cannot be walked, so not rebuilt
                Err(e) => skipped.push(format!("{name}: {e}")),
            }
        }
        println!("{var}: {done} zones rebuilt, {} with other block sizes in the header, {} skipped", sizes.len(), skipped.len());
        for s in sizes.iter().take(12) {
            println!("  {s}");
        }
        for s in skipped.iter().take(8) {
            println!("  {s}");
        }
        assert!(done > 0);
    }
}

/// Longer map entities in a zone: everything after them moves, and every reference still lands.
#[test]
fn replacing_map_entities_relocates_later_references() {
    use atlas::zone::writer::{ZoneBuilder, baseline_dangling};
    for var in ["CODOL_TEST_GAME", "CODOL_TEST_319"] {
        let Some(root) = std::env::var_os(var).map(PathBuf::from) else { continue };
        let key = export::find_key(&root).unwrap();
        let mut tried = 0;
        for path in export::zone_files(&root) {
            let Ok(z) = Zone::open(&path, &key) else { continue };
            let Some(index) = z.assets.iter().position(|a| a.asset_type == 42 && a.reference >= 0xffff_fffe && u32::from_le_bytes(z.data[a.start + 4..a.start + 8].try_into().unwrap()) == u32::MAX) else { continue };
            let e = z.assets[index];
            let blob = &z.data[e.start..e.start + e.extent];
            let (name_ptr, text_ptr, n) = (u32::from_le_bytes(blob[0..4].try_into().unwrap()), u32::from_le_bytes(blob[4..8].try_into().unwrap()), u32::from_le_bytes(blob[8..12].try_into().unwrap()) as usize);
            if text_ptr == 0 || n == 0 {
                continue;
            }
            let header = if blob[36..40] == [0xff; 4] { 52 } else { 36 };
            let name_len = if name_ptr == u32::MAX { blob[header..].iter().position(|&c| c == 0).unwrap() + 1 } else { 0 };
            let text_at = header + name_len;
            let extra: &[u8] = b"{
\"classname\" \"info_null\"
\"origin\" \"0 0 0\"
\"targetname\" \"ported_by_codol_assets\"
}
";
            let mut out = blob[..text_at].to_vec();
            out.extend_from_slice(&blob[text_at..text_at + n - 1]);
            out.extend_from_slice(extra);
            out.push(0);
            out.extend_from_slice(&blob[text_at + n..]);
            out[8..12].copy_from_slice(&((n + extra.len()) as u32).to_le_bytes());
            let mut b = ZoneBuilder::from_zone(&z);
            b.replace(index, out);
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let built = match b.build(&z) {
                Ok(b) => b,
                Err(err) => {
                    println!("{name}: {err}");
                    continue;
                }
            };
            let again = Zone::from_image(z.version, built.image.clone()).unwrap();
            assert_eq!(again.assets.len(), z.assets.len());
            assert!(again.assets[index].extent > z.assets[index].extent);
            assert_eq!(built.dangling, baseline_dangling(&z).unwrap(), "{name}");
            assert!(built.image.windows(21).any(|w| w == b"ported_by_codol_asset"), "{name}");
            println!("{name}: map entities {} -> {} bytes, {} references relocated", z.assets[index].extent, again.assets[index].extent, built.relocated);
            tried += 1;
            if tried >= 6 {
                break;
            }
        }
        assert!(tried > 0, "{var}: no map zone found with entities");
    }
}

/// Removing an asset nothing refers to shifts every later directory slot and block-3 address; the rebuilt zone is
/// consistent, and sealed into a fastfile it opens again to the same image.
#[test]
fn removing_an_asset_rebuilds_and_seals() {
    use atlas::zone::{fastfile, writer::{ZoneBuilder, baseline_dangling}};
    for var in ["CODOL_TEST_GAME", "CODOL_TEST_319"] {
        let Some(root) = std::env::var_os(var).map(PathBuf::from) else { continue };
        let key = export::find_key(&root).unwrap();
        let mut tried = 0;
        for path in export::zone_files(&root) {
            let Ok(opened) = fastfile::open(&path, &key) else { continue };
            let Ok(z) = Zone::from_image(opened.version, opened.image.clone()) else { continue };
            // a raw file in the first half of the zone, so later assets have to move
            let Some(index) = z.assets.iter().take(z.assets.len() / 2).position(|a| a.asset_type == 36 && a.reference >= 0xffff_fffe) else { continue };
            let mut b = ZoneBuilder::from_zone(&z);
            b.remove(index);
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let Ok(built) = b.build(&z) else { continue };
            let again = Zone::from_image(z.version, built.image.clone()).unwrap();
            assert_eq!(again.assets.len(), z.assets.len() - 1, "{name}");
            assert_eq!(built.dangling, baseline_dangling(&z).unwrap(), "{name}");
            for signed in [true, false] {
                let mut c = opened.container.clone();
                c.signed = signed && c.signed;
                let sealed = fastfile::seal(&built.image, z.version, &c).unwrap();
                assert!(fastfile::open_bytes(&sealed, &key).unwrap().image == built.image, "{name}: sealed round trip");
            }
            println!("{name}: removed asset {index} (rawfile): {} -> {} bytes, {} references relocated", z.data.len(), built.image.len(), built.relocated);
            tried += 1;
            if tried >= 4 {
                break;
            }
        }
        assert!(tried > 0, "{var}: no zone with a removable raw file");
    }
}

/// A stock Modern Warfare 2 (IW4) zone, `CODOL_TEST_MW2=<the x86 game folder>` (mp_rust.ff under zone/iw4x/x86/english): the
/// container opens (plain, no key), the header and asset list read like a CoDOL zone's, and the whole stream walks with the
/// 1.1.1.13 layouts plus the IW4 ones (world, collision, sounds, effects, ...): every asset, ending exactly at the end of the
/// image, with the memory blocks filled as the header says (block 3 is a few hundred bytes short on this map).
#[test]
fn stock_iw4_map_walks_completely() {
    use atlas::zone::stock;
    let Some(root) = std::env::var_os("CODOL_TEST_MW2").map(PathBuf::from) else { return };
    let path = root.join("zone/iw4x/x86/english/mp_rust.ff");
    if !path.exists() {
        return;
    }
    let (version, image) = stock::open(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(version, 276);
    let p = stock::probe(&image, 334).unwrap();
    assert_eq!(p.string_count, 329);
    assert_eq!(p.list.len(), 2582);
    assert_eq!(p.failure, None, "the walk stopped");
    assert_eq!(p.reached, image.len());
    for b in [0, 1, 2, 4, 5, 6, 7] {
        assert_eq!(p.peaks[b] as u32, p.block_sizes[b], "block {b}");
    }
    let short = p.block_sizes[3] as usize - p.peaks[3];
    assert!(short < 1_000, "block 3 is {short} bytes short");
    let walked: usize = p.by_type.values().map(|v| v.0).sum();
    assert_eq!(walked, 2582);
}
