#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::{Context, Result, bail};
use atlas::{app, convert, iwd, iwi, pose, model, modelindex, modelsource, modeltex, source, viewer, zone};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const USAGE: &str = "\
atlas - the Call of Duty asset workbench

usage:
  atlas [GAME_FOLDER]                     open the browser
  atlas --selftest GAME_FOLDER            decode every image, report problems
  atlas --map2zone GAME_FOLDER ZONE.ff EDITED.map OUT.ff   writes the edits of a .map made by --zone2map into the zone: entities and static model placement (added / deleted static models are refused; --rebuild-bsp builds the BSP again and drops terrain collision)
  atlas --unzone GAME_FOLDER ZONE.ff OUT_DIR   every asset of a CoDOL zone as a file (map entities and raw files also as text)
  atlas --buildzone GAME_FOLDER DIR OUT.ff [--signed]   builds the zone again from those files: edited text replaces its asset, a deleted file removes it
  atlas --exportdata GAME_FOLDER OUT_DIR  string tables (CSV), localised strings (CSV, JSON) and raw files from every zone
  atlas --png GAME_FOLDER NAME OUT.png [MIP]  decode one image (face 0) to PNG
  atlas --list GAME_FOLDER                list sources and image counts
  atlas --audit GAME_FOLDER [FMT]        format histogram, header vs file length (FMT: list that format)
  atlas --packiwd OUT.iwd INPUT...        pack folders (paths relative to each), files, or archives into an IWD; .dds become images/*.iwi
  atlas --verifyiwd FILE.iwd              read every entry back (CRC) and check the .iwi headers
  atlas --ipak FILE.ipak [INDEX OUT.iwi]   check a Black Ops II IPAK, or extract entry INDEX
  atlas --exportmodel GAME_FOLDER NAME OUT_DIR --index MODEL_INDEX.tsv [--format glb,obj,xe,se,sa,cast] [--unit inches|cm|m] [--scale S] [--bake] [--no-images] [--anim NAME,PREFIX*]
                                                      export a model (an xmodel name, or a streamed file name) with its skeleton, skin weights,
                                                      materials and textures; --format picks glTF (glb), OBJ+MTL, XMODEL_EXPORT (xe), SEModel (se), SEAnim (sa); --anim adds animations (to the glb and as xanim/*.seanim)
  atlas --buildanims GAME_FOLDER OUT.tsv [NAME_FILTER]   replay the zones and write the animation index (anim lines)
  atlas --animcheck GAME_FOLDER           replay the zones and decode every animation, report failures
  atlas --animframe GAME_FOLDER MODEL ANIMS.tsv ANIM FRAME[,FRAME...] OUT.png --index MODEL_INDEX.tsv [--yaw D] [--pitch D] [--size WxH]
                                                      pose a model with an animation (matched by bone name), skin it on the CPU and render the frames
  atlas --dds2iwi IN.dds OUT.iwi [--usage color|normal|ui|clamp|raw] [--flags HEX] [--genmips]
  atlas --iwi2dds IN.iwi OUT.dds          (wavelet images are decompressed)
  atlas --modelcheck GAME_FOLDER          parse every streamed model, report problems
  atlas --rendermap GAME_FOLDER ZONE.ff OUT.png [--layers all|none|brush,static,..] [--world] [--size WxH] [--shading lit|color|normals|surfaces] [--yaw D] [--pitch D] [--zoom F] [--wire]
                                          the editor layers of a map (collision brushes, static model boxes, entity markers), rendered without a window
  atlas --rendermodel GAME_FOLDER NAME OUT.png [--size WxH] [--shading lit|normals|uv|uvcolor|color|surfaces]
                [--cull none|back|front] [--wire] [--yaw DEG] [--pitch DEG] [--no-normal] [--no-spec] [--index MODEL_INDEX.tsv]
                                                      render a model without a window (textured when an index is given)
  atlas --zoneinfo KEY ZONE.ff...         open fastfiles with the archive key (a c1_keys.bin or the game exe), summarise
  atlas --zoneindex KEY OUT.tsv ZONE.ff...  replay zones and write the model index (replaces tools/zone_export)
  atlas --buildindex GAME_FOLDER OUT.tsv  find the key and zones in a game folder and write the model index
  atlas --dumpshaders KEY OUT_DIR ZONE.ff...  write every inline shader program as a file
  atlas --roundtrip GAME_FOLDER           read and rewrite every image, compare with the originals
  atlas --convertzone SRC.ff --to iw3|iw4|iw5|codol [GAME_FOLDER] OUT.ff   convert a zone between games of the Infinity Ward family through stock IW4 (`check` instead of OUT.ff writes nothing)
  atlas --convertzone MW2_ZONE.ff GAME_FOLDER OUT.ff   MW2 to Call of Duty Online (the original form)
  atlas --exportmap SRC.ff --to iw3|iw4|iw5|codol OUT_DIR [--name MAP] [--signed] [--game-folder FOLDER] [--no-const] [--dry-run] [--force]
                                                      convert a map zone to the game and write a package under OUT_DIR in the layout that game loads custom maps from,
                                                      ready to copy into the game folder (nothing is written there): usermaps/MAP/MAP.ff + MAP_load.ff (from beside SRC)
                                                      for CoD4, MW2 (IW4x, plus MAP.arena) and MW3; client_shipRetail/chinese/ for Call of Duty Online QA 3.3.3.3
                                                      (--game-folder, read only: the archive key and container; MAP_const.ff too: the marker raw file alone, which is all the
                                                      game reads for a map outside its stock table mp/constBaselinesTable.csv, or a QA SRC_const.ff beside SRC unchanged;
                                                      --no-const leaves it out). Every layout rule is printed as VERIFIED (checked against
                                                      the game's files and executable) or ASSUMED (community practice): CoD4 and IW4x are verified, MW3 and stock x86 MW2
                                                      assumed, and for CoDOL that the client accepts a marker-only const zone (stock mp_con_spring is one; no game is run). The name stays the
                                                      zone's (renaming is refused), the map list (mapsTable) and IWD images are not made, no game is started. --dry-run
                                                      writes nothing; existing files in OUT_DIR need --force; the written zones are read back with the target's reader.
                                                      Start the map with devmap MAP (or map MAP)
  atlas --zonewalk iw3|iw4|iw5|t6|nx1 FILE|FOLDER [-v] [--dump DIR]   walk the assets of Call of Duty 4, MW2, MW3 or Black Ops II zones (--t6walk = t6)
  atlas --nx1info FILE.ff...   the container of Future Warfare (NX1) Xbox 360 fastfiles and their assets by type
  atlas --exportsound GAME_FOLDER|ZONE.ff OUT_DIR [--filter TEXT]   the loaded sounds of Call of Duty 4, Modern Warfare 2 and 3 zones as WAV (search words: length:>2 rate:44100 !name)
  atlas --t6export ZONE.ff|FOLDER OUT_DIR [--format cast|obj] [--lod N] [--name FILTER]   export every xmodel of a Black Ops II zone (LOD 0 by default)
  atlas --t6unzone ZONE.ff OUT_DIR        a Black Ops II zone as files: raw files, script trees, localize entries, string tables (CSV)
  atlas --t6buildzone DIR OUT.ff [--keep-signature]   builds the zone from those files: changed files replace their asset, deleted files and rows remove it;
                                                      unsigned (the name of OUT is the cipher name), the stream is checked with the walker
  atlas --hqinfo FILE|FOLDER [-v] [--strings] [--deep] [--sample N] [--dump DIR] [--oodle DLL] [--hashes FILE.wni]   open Call of Duty HQ (cod26) .ff fastfiles and .xpak/.xsub packages offline, one line per file and a survey
  atlas --hash STRING... [--hashes FILE.wni] [--names LIST.txt] [--keys FILE|FOLDER]   hashes of a name (FNV-1a variants of the cod26 asset names); with a WNI index (ate47 HashIndex) also the name lookup
";

/// The program is built for the Windows GUI subsystem, so starting it from Explorer opens only its window. A command-line run
/// (any argument) attaches to the console of the terminal that started it, so the commands still print there.
#[cfg(windows)]
fn attach_parent_console() {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(which: u32) -> isize;
        fn SetStdHandle(which: u32, handle: isize) -> i32;
        fn CreateFileW(name: *const u16, access: u32, share: u32, security: *const c_void, disposition: u32, flags: u32, template: isize) -> isize;
    }
    unsafe {
        if AttachConsole(u32::MAX) == 0 {
            return; // started without a console (a pipe or a file is already wired up, or there is nowhere to print)
        }
        let conout: Vec<u16> = "CONOUT$ ".encode_utf16().collect();
        for which in [(-11i32) as u32, (-12i32) as u32] {
            let h = GetStdHandle(which);
            if h == 0 || h == -1 {
                let console = CreateFileW(conout.as_ptr(), 0xC000_0000, 3, std::ptr::null(), 3, 0, 0);
                if console != -1 {
                    SetStdHandle(which, console);
                }
            }
        }
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(windows)]
    if !args.is_empty() {
        attach_parent_console();
    }
    match args.first().map(String::as_str) {
        Some("-h" | "--help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some("--selftest") => selftest(path_arg(&args, 1)?),
        Some("--list") => list(path_arg(&args, 1)?),
        Some("--packiwd") => packiwd(&args[1..]),
        Some("--verifyiwd") => verifyiwd(path_arg(&args, 1)?),
        Some("--ipak") => ipak_cmd(&args[1..]),
        Some("--exportmodel") => exportmodel(&args[1..]),
        Some("--buildanims") => {
            let (root, out) = (path_arg(&args, 1)?, path_arg(&args, 2)?);
            let progress = (AtomicUsize::new(0), AtomicUsize::new(0));
            let t = Instant::now();
            let text = zone::export::build_anim_index(&root, &progress, args.get(3).map(String::as_str))?;
            std::fs::write(&out, &text)?;
            println!("{} zones read, {} animations, {} bytes -> {} [{:.1}s]", progress.0.load(Ordering::Relaxed), text.lines().filter(|l| l.starts_with("anim	")).count(), text.len(), out.display(), t.elapsed().as_secs_f32());
            Ok(())
        }
        Some("--animcheck") => {
            let root = path_arg(&args, 1)?;
            let key = zone::export::find_key(&root)?;
            let (mut total, mut ok, mut errors) = (0usize, 0usize, BTreeMap::<String, usize>::new());
            let t = Instant::now();
            for path in zone::export::zone_files(&root) {
                let Ok(z) = zone::Zone::open(&path, &key) else { continue };
                for a in zone::export::zone_anims(&z) {
                    total += 1;
                    match a.decode() {
                        Ok(_) => ok += 1,
                        Err(e) => *errors.entry(format!("{e:#}").chars().take(70).collect()).or_default() += 1,
                    }
                }
            }
            println!("{total} animations, {ok} decoded, {} failed [{:.1}s]", total - ok, t.elapsed().as_secs_f32());
            for (e, n) in errors.iter().take(15) {
                println!("{n:>6}  {e}");
            }
            Ok(())
        }
        Some("--animframe") => animframe(&args[1..]),
        Some("--replayreport") => {
            let root = path_arg(&args, 1)?;
            let key = zone::export::find_key(&root)?;
            let (mut assets, mut failed_total) = (0usize, 0usize);
            let mut by: BTreeMap<(u32, String), usize> = BTreeMap::new();
            let mut sample: BTreeMap<(u32, String), String> = BTreeMap::new();
            let (mut models, mut materials) = (0usize, 0usize);
            let t = Instant::now();
            for path in zone::export::zone_files(&root) {
                let Ok(z) = zone::Zone::open(&path, &key) else { continue };
                let mut sink = String::new();
                let r = zone::export::export_zone(&z, "", &mut sink);
                assets += r.assets;
                models += r.models;
                materials += r.materials;
                failed_total += r.failed.len();
                for (idx, ty, what) in r.failed {
                    // the message without its position
                    let msg = what.split(" at ").next().unwrap_or("").to_string();
                    sample.entry((ty, msg.clone())).or_insert_with(|| format!("{} asset #{idx} {what}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
                    *by.entry((ty, msg)).or_default() += 1;
                }
            }
            println!("{assets} assets, {failed_total} failed, {models} models, {materials} materials [{:.1}s]", t.elapsed().as_secs_f32());
            let mut rows: Vec<_> = by.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            for ((ty, msg), n) in rows.iter().take(25) {
                println!("{n:>6}  type {ty:>2}  {msg}   e.g. {}", sample.get(&(*ty, msg.clone())).cloned().unwrap_or_default());
            }
            Ok(())
        }
        Some("--zoneassets") => {
            // development aid: --zoneassets GAME ZONE_FILE_NAME TYPE [COUNT [BYTES]]
            let root = path_arg(&args, 1)?;
            let (zname, ty) = (args.get(2).context("zone name")?, args.get(3).context("asset type")?.parse::<u32>()?);
            let count = args.get(4).and_then(|c| c.parse::<usize>().ok()).unwrap_or(5);
            let bytes = args.get(5).and_then(|c| c.parse::<usize>().ok()).unwrap_or(96);
            let key = zone::export::find_key(&root)?;
            let path = zone::export::zone_files(&root).into_iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(zname))).context("no such zone")?;
            let z = zone::Zone::open(&path, &key)?;
            println!("{}: version {}, {} assets", zname, z.version, z.assets.len());
            for (i, a) in z.assets.iter().enumerate().filter(|(_, a)| a.asset_type == ty).take(count) {
                let d = &z.data[a.start..a.start + a.extent.min(bytes)];
                println!("#{i} extent {} ref {:#x}: {}", a.extent, a.reference, d.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
            }
            Ok(())
        }
        Some("--dumpweapons") => {
            // development aid: atlas --dumpweapons GAME_FOLDER OUT.tsv: every weapon record (name, zone, size, bytes as hex)
            let (root, out) = (path_arg(&args, 1)?, path_arg(&args, 2)?);
            let data = atlas::data::collect(&root, None)?;
            let mut text = String::new();
            for (name, w) in &data.weapons {
                let hex: String = w.record.iter().map(|b| format!("{b:02x}")).collect();
                text.push_str(&format!("{}\t{name}\t{}\t{hex}\n", w.zone, w.record.len()));
            }
            std::fs::write(&out, text)?;
            println!("{} weapons -> {}", data.weapons.len(), out.display());
            Ok(())
        }
        Some("--unzone") => {
            // atlas --unzone GAME_FOLDER ZONE.ff OUT_DIR: every asset of a CoDOL zone as a file; map entities and raw files as text too
            let (root, zone_name, out) = (path_arg(&args, 1)?, args.get(2).context("zone file")?, path_arg(&args, 3)?);
            let key = zone::export::find_key(&root)?;
            let path = zone::export::zone_files(&root).into_iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(zone_name))).context("no such zone")?;
            let n = atlas::zonefiles::unpack(&path, &key, &out)?;
            println!("{n} assets of {zone_name} written to {}", out.display());
            Ok(())
        }
        Some("--map2zone") => {
            // atlas --map2zone GAME_FOLDER ZONE.ff EDITED.map OUT.ff: writes the edits of a .map (entities, static model placement) into the zone
            let (root, zone_name, map, out) = (path_arg(&args, 1)?, args.get(2).context("zone file")?, path_arg(&args, 3)?, path_arg(&args, 4)?);
            let key = zone::export::find_key(&root)?;
            let path = zone::export::zone_files(&root).into_iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(zone_name))).context("no such zone")?;
            let text = std::fs::read_to_string(&map).with_context(|| format!("reading {}", map.display()))?;
            let opts = zone::clipwrite::Options { rebuild_bsp: args.iter().any(|a| a == "--rebuild-bsp") };
            let plan = zone::mapwrite::apply_with(&path, &key, &text, &out, opts)?;
            println!(
                "{}: {} static models changed ({} with collision), entities {}; written to {}",
                zone_name,
                plan.models_changed,
                plan.collision_changed,
                if plan.entities_changed { "changed" } else { "unchanged" },
                out.display()
            );
            for n in &plan.notes {
                println!("note: {n}");
            }
            Ok(())
        }
        Some("--zone2map") => {
            // atlas --zone2map GAME_FOLDER ZONE.ff OUT.map: the collision brushes, brush entities, static models and entities of a CoDOL map as a Radiant .map
            let (root, zone_name, out) = (path_arg(&args, 1)?, args.get(2).context("zone file")?, path_arg(&args, 3)?);
            let key = zone::export::find_key(&root)?;
            let path = zone::export::zone_files(&root).into_iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(zone_name))).context("no such zone")?;
            let zone = zone::Zone::open(&path, &key)?;
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let m = zone::mapsrc::read(&zone, &stem)?;
            std::fs::write(&out, zone::mapsrc::to_map(&m))?;
            println!(
                "{stem}: {} entities, {} world brushes, {} brush models ({} brushes), {} static models -> {}",
                m.entities.len(),
                m.world.len(),
                m.submodels.len(),
                m.submodels.iter().map(|s| s.1.len()).sum::<usize>(),
                m.models.len(),
                out.display()
            );
            for line in &m.report {
                println!("note: {line}");
            }
            Ok(())
        }
        Some("--buildzone") => {
            // atlas --buildzone GAME_FOLDER UNPACKED_DIR OUT.ff [--signed]: builds the zone again from the files (edited text, deleted assets)
            let (root, dir, out) = (path_arg(&args, 1)?, path_arg(&args, 2)?, path_arg(&args, 3)?);
            let key = zone::export::find_key(&root)?;
            let r = atlas::zonefiles::build(&dir, &key, &out, args.iter().any(|a| a == "--signed"))?;
            println!("{}: {} bytes; replaced {:?}, removed {:?}, {} references relocated", out.display(), r.bytes, r.replaced, r.removed, r.relocated);
            Ok(())
        }
        Some("--dumpheaders") => {
            // development aid: atlas --dumpheaders GAME_FOLDER OUT.tsv: kind, name, size, header bytes (hex) of materials, models, ...
            let (root, out) = (path_arg(&args, 1)?, path_arg(&args, 2)?);
            let data = atlas::data::collect(&root, None)?;
            let mut text = String::new();
            for ((kind, name), bytes) in &data.headers {
                let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                text.push_str(&format!("{kind}\t{name}\t{}\t{hex}\n", bytes.len()));
            }
            std::fs::write(&out, text)?;
            println!("{} headers -> {}", data.headers.len(), out.display());
            Ok(())
        }
        Some("--exportdata") => {
            // atlas --exportdata GAME_FOLDER OUT_DIR: string tables as CSV, localised strings as CSV and JSON, raw files as files
            let (root, out) = (path_arg(&args, 1)?, path_arg(&args, 2)?);
            let t = Instant::now();
            let data = atlas::data::collect(&root, None)?;
            data.write(&out)?;
            println!(
                "{} zones read ({} failed): {} string tables, {} localised strings, {} raw files -> {} ({:.1}s)",
                data.zones_read,
                data.zones_failed,
                data.tables.len(),
                data.localize.len(),
                data.rawfiles.len(),
                out.display(),
                t.elapsed().as_secs_f32()
            );
            Ok(())
        }
        Some("--exportsound") => {
            // atlas --exportsound GAME_FOLDER|ZONE.ff OUT_DIR [--filter TEXT]: the loaded sounds of Call of Duty 4, Modern Warfare 2 and Modern Warfare 3 zones as WAV
            let (input, out) = (path_arg(&args, 1)?, path_arg(&args, 2)?);
            let filter = args.iter().position(|a| a == "--filter").and_then(|i| args.get(i + 1)).cloned().unwrap_or_default();
            let t = Instant::now();
            let files = if input.is_file() { vec![input.clone()] } else { zone::export::survey_files(&input) };
            let q = atlas::query::Query::parse(&filter, atlas::sound::PROPERTIES);
            let (mut zones, mut written, mut skipped) = (0usize, 0usize, 0usize);
            for f in files {
                if zone::fastfile::peek(&f).ok().and_then(|p| atlas::sound::game_of(&p)).is_none() {
                    continue;
                }
                match atlas::sound::read_zone(&f) {
                    Ok(list) => {
                        zones += 1;
                        let keep: Vec<&atlas::sound::Sound> = list.iter().filter(|s| q.matches(&s.name.to_lowercase(), |p| atlas::sound::property(s, p))).collect();
                        skipped += list.len() - keep.len();
                        written += atlas::sound::write_all(&keep, &out)?;
                    }
                    Err(e) => eprintln!("{}: {e:#}", f.display()),
                }
            }
            println!("{zones} zones, {written} sounds -> {} ({skipped} filtered out, {:.1}s)", out.display(), t.elapsed().as_secs_f32());
            Ok(())
        }
        Some("--t6info") => {
            // development aid: atlas --t6info ZONE.ff: opens a Black Ops II PC fastfile and lists its assets by type
            let path = path_arg(&args, 1)?;
            let z = zone::t6::open(&std::fs::read(&path).with_context(|| path.display().to_string())?)?;
            println!("{}: name {:?}, {} image bytes, size {:#x}, external {:#x}, blocks {:x?}", path.display(), z.name, z.image.len(), z.size, z.external_size, z.block_sizes);
            let w = |o: usize| u32::from_le_bytes(z.image[o..o + 4].try_into().unwrap());
            println!("  list: strings {} ({:#x}), depends {} ({:#x}), assets {} ({:#x})", w(40), w(44), w(48), w(52), w(56), w(60));
            let list = z.asset_list()?;
            println!("  depends {:?}; first strings {:?}; data starts at {:#x}", list.depends, list.strings.iter().take(4).collect::<Vec<_>>(), list.data_start);
            let mut by_type = std::collections::BTreeMap::<u32, usize>::new();
            for (t, _) in &list.assets {
                *by_type.entry(*t).or_default() += 1;
            }
            for (t, n) in by_type {
                println!("  type {t:2} {:18} {n}", zone::t6::ASSET_TYPES.get(t as usize).copied().unwrap_or("?"));
            }
            Ok(())
        }
        Some("--t6unzone") => zone::t6_files::command_unzone(&args[1..]),
        Some("--t6buildzone") => zone::t6_files::command_buildzone(&args[1..]),
        Some("--t6walk") => zone::schema_walk::command(Some(zone::schema_walk::Game::T6), &args[1..]),
        Some("--nx1info") => zone::nx1::command(&args[1..]),
        Some("--zonewalk") if args.get(1).is_some_and(|g| g.eq_ignore_ascii_case("nx1")) => zone::nx1_walk::command(Some(zone::nx1_walk::Game::Nx1), &args[2..]),
        Some("--zonewalk") => zone::schema_walk::command(None, &args[1..]),
        Some("--t6export") => atlas::export::t6::command(&args[1..]),
        Some("--hqinfo") => zone::hq::command(&args[1..]),
        Some("--hash") => atlas::hashindex::command(&args[1..]),
        Some("--convertzone") => zone::convert_cli::command(&args[1..]),
        Some("--exportmap") => zone::export_map::command(&args[1..]),
        Some("--iw4probe") => {
            // development aid: --iw4probe STOCK.ff [LAYOUT]: walk a stock IW4 zone with a CoDOL layout (334 or 423)
            let path = path_arg(&args, 1)?;
            let layout = args.get(2).and_then(|c| c.parse().ok()).unwrap_or(334);
            let (version, image) = zone::stock::open(&std::fs::read(&path).with_context(|| path.display().to_string())?)?;
            let p = zone::stock::probe(&image, layout)?;
            println!("{}: stock version {version}, image {} bytes, {} strings, {} assets, blocks {:?}", path.display(), image.len(), p.string_count, p.list.len(), p.block_sizes);
            for (t, (ok, total)) in &p.by_type {
                println!("  type {t:2}: {ok:5} of {total:5} walked");
            }
            println!("  block peaks {:?} (header {:?}); block 3 short by {}, -2 pointer words read: {}", p.peaks, p.block_sizes, p.block_sizes[3] as i64 - p.peaks[3] as i64, p.insert_words);
            for (i, t, start, end) in p.recent.iter().rev().take(6).rev() {
                println!("  last walked: asset {i} type {t} {start:#x}..{end:#x}");
            }
            match p.failure {
                Some((i, t, what, at)) => println!("layout {layout}: stopped at asset {i} (type {t}) that starts at {:#x}: {what} at {at:#x}", p.reached),
                None => println!("layout {layout}: every asset walked, stream reached {:#x} of {:#x}", p.reached, image.len()),
            }
            Ok(())
        }
        Some("--blocksizes") => {
            // development aid: do the header's block sizes equal how far the walk fills each block?
            let root = path_arg(&args, 1)?;
            let key = zone::export::find_key(&root)?;
            let (mut zones, mut equal, mut shown) = (0usize, 0usize, 0usize);
            for path in zone::export::zone_files(&root) {
                let Ok(z) = zone::Zone::open(&path, &key) else { continue };
                let Ok(mut r) = zone::replay::Replay::new(&z) else { continue };
                let mut failed = false;
                for i in 0..z.assets.len() {
                    failed |= r.replay_asset(&z, i).is_err();
                }
                if failed {
                    continue;
                }
                zones += 1;
                let pos = r.block_positions();
                let same = (0..8).all(|b| (pos[b] + if b == 0 { 16 } else { 0 }) as u32 == z.block_sizes[b]);
                equal += same as usize;
                if !r.dangling_references().is_empty() && shown < 6 {
                    shown += 1;
                    println!("{}: {} dangling references, first {:?}", path.display(), r.dangling_references().len(), r.dangling_references()[0]);
                }
                if !same && shown < 4 {
                    shown += 1;
                    println!("{}: header {:?} walk {:?}", path.display(), z.block_sizes, pos);
                }
            }
            println!("{zones} zones replayed without failure, {equal} have block sizes equal to the walk");
            Ok(())
        }
        Some("--audit") => audit(path_arg(&args, 1)?, args.get(2).and_then(|c| c.parse().ok())),
        Some("--dds2iwi") => dds2iwi(&args[1..]),
        Some("--iwi2dds") => {
            let (input, out) = (path_arg(&args, 1)?, path_arg(&args, 2)?);
            let dds = convert::iwi_to_dds(&std::fs::read(&input).with_context(|| input.display().to_string())?)?;
            std::fs::write(&out, &dds)?;
            println!("{} -> {} ({} bytes)", input.display(), out.display(), dds.len());
            Ok(())
        }
        Some("--rendermodel") => rendermodel(&args[1..]),
        Some("--rendermap") => rendermap(&args[1..]),
        Some("--modelcheck") => modelcheck(path_arg(&args, 1)?),
        Some("--zoneinfo") => zoneinfo(&args[1..]),
        Some("--zoneindex") => zoneindex(&args[1..]),
        Some("--dumpshaders") => dumpshaders(&args[1..]),
        Some("--buildindex") => {
            let (root, out) = (path_arg(&args, 1)?, path_arg(&args, 2)?);
            let progress = (AtomicUsize::new(0), AtomicUsize::new(0));
            let t = Instant::now();
            let text = zone::export::build_index(&root, &progress)?;
            std::fs::write(&out, &text)?;
            println!("{} zones read, {} lines -> {} [{:.1}s]", progress.0.load(Ordering::Relaxed), text.lines().count(), out.display(), t.elapsed().as_secs_f32());
            Ok(())
        }
        Some("--roundtrip") => roundtrip(path_arg(&args, 1)?),
        Some("--png") => {
            let (Some(name), Some(out)) = (args.get(2), args.get(3)) else { bail!("{USAGE}") };
            png(path_arg(&args, 1)?, name, PathBuf::from(out), args.get(4).and_then(|l| l.parse().ok()).unwrap_or(0))
        }
        Some(p) if !p.starts_with('-') => app::run(Some(PathBuf::from(p))),
        None => app::run(None),
        Some(other) => bail!("unknown option {other}\n{USAGE}"),
    }
}

fn path_arg(args: &[String], i: usize) -> Result<PathBuf> {
    args.get(i).map(PathBuf::from).with_context(|| USAGE.to_string())
}

fn build(root: &PathBuf) -> source::Catalog {
    let providers = source::discover(root);
    let progress = source::Progress::default();
    source::scan(providers, &progress)
}

fn list(root: PathBuf) -> Result<()> {
    let t = Instant::now();
    let cat = build(&root);
    for (i, p) in cat.providers.iter().enumerate() {
        let n = cat.entries.iter().flat_map(|e| &e.variants).filter(|v| v.provider == i).count();
        println!("{:>8}  {}", n, p.label());
    }
    for (v, n) in cat.versions() {
        println!("{:>8}  {} - {}", n, iwi::engine(v).label(v), iwi::engine(v).engine);
    }
    let z = zone::export::survey(&root);
    for ((f, v), n) in &z.by_version {
        println!("{n:>8}  zones, fastfile v{v} - {}", zone::fastfile::version_info(*f, *v).build);
    }
    if z.unreadable > 0 {
        println!("{:>8}  .ff files that are not fastfiles", z.unreadable);
    }
    println!("{} distinct names in {:.1}s", cat.entries.len(), t.elapsed().as_secs_f32());
    Ok(())
}

/// Header and size consistency over every installed image: format histogram, and for the
/// raw / block formats whether the file length equals the length the header implies.
fn audit(root: PathBuf, list_format: Option<u8>) -> Result<()> {
    use std::collections::BTreeMap;
    let cat = build(&root);
    let mut hist: BTreeMap<(u8, u8), usize> = BTreeMap::new();
    let (mut checked, mut bad) = (0usize, 0usize);
    let mut shown = 0;
    for e in &cat.entries {
        for v in &e.variants {
            let Some(h) = &v.header else { continue };
            *hist.entry((h.version, h.format)).or_default() += 1;
            if list_format == Some(h.format) {
                println!("fmt {} {} {}x{} flags {:#x}", h.format, e.name, h.width, h.height, h.flags);
            }
            let Some(want) = iwi::expected_size(h) else { continue };
            checked += 1;
            if want as u64 != v.size {
                bad += 1;
                if shown < 15 {
                    shown += 1;
                    println!("size mismatch {}: {}x{} fmt {} flags {:#x} want {} have {}", e.name, h.width, h.height, h.format, h.flags, want, v.size);
                }
            }
        }
    }
    for ((ver, fmt), n) in hist {
        println!("v{ver} format {fmt:>2} {:<14} {n}", iwi::Format::from_code(fmt).map_or("?", |f| f.name()));
    }
    println!("{checked} raw/block files checked, {bad} with a length that differs from the header's");
    Ok(())
}

fn packiwd(args: &[String]) -> Result<()> {
    let Some(out) = args.first() else { bail!("{USAGE}") };
    let opts = iwd::AddOptions { dds: Some(convert::Options { usage: "color".into(), flags: None, genmips: true }) };
    let mut entries = Vec::new();
    for a in &args[1..] {
        let p = PathBuf::from(a);
        if p.is_dir() {
            entries.extend(iwd::add_dir(&p, &opts)?);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("iwd")) {
            entries.extend(iwd::load(&p)?);
        } else {
            entries.extend(iwd::add_files(&[p], &opts)?);
        }
    }
    let t = Instant::now();
    let s = iwd::write(&entries, std::path::Path::new(out), iwd::WriteOptions::default())?;
    let v = iwd::verify(std::path::Path::new(out))?;
    println!("{out}: {} files ({} bytes), {} duplicate path(s) replaced; read back {} files, {} images, {} header problems [{:.1}s]", s.files, s.bytes, s.replaced_duplicates, v.files, v.images, v.bad_images.len(), t.elapsed().as_secs_f32());
    for b in v.bad_images.iter().take(10) {
        println!("  {b}");
    }
    Ok(())
}

fn verifyiwd(path: PathBuf) -> Result<()> {
    let v = iwd::verify(&path)?;
    println!("{}: {} files, {} images, {} header problems", path.display(), v.files, v.images, v.bad_images.len());
    for b in v.bad_images.iter().take(20) {
        println!("  {b}");
    }
    Ok(())
}

fn ipak_cmd(args: &[String]) -> Result<()> {
    let path = PathBuf::from(args.first().context("usage: --ipak FILE.ipak [INDEX OUT.iwi]")?);
    let ipak = atlas::ipak::Ipak::open(&path)?;
    if let (Some(i), Some(out)) = (args.get(1), args.get(2)) {
        let e = ipak.entries.get(i.parse::<usize>()?).context("no such entry")?;
        let data = ipak.read(e)?;
        std::fs::write(out, &data)?;
        println!("{:08x} ({} stored bytes) -> {} ({} bytes)", e.name_hash, e.size, out, data.len());
        return Ok(());
    }
    let t = Instant::now();
    let (mut ok, mut bad, mut versions) = (0usize, 0usize, BTreeMap::new());
    for (i, e) in ipak.entries.iter().enumerate() {
        match ipak.read(e) {
            Ok(d) if d.len() > 3 && &d[..3] == b"IWi" => {
                ok += 1;
                *versions.entry(d[3]).or_insert(0usize) += 1;
            }
            Ok(d) => {
                bad += 1;
                if bad <= 5 {
                    println!("entry {i} ({:08x}): {} bytes, does not start with IWi", e.name_hash, d.len());
                }
            }
            Err(err) => {
                bad += 1;
                if bad <= 5 {
                    println!("entry {i} ({:08x}): {err:#}", e.name_hash);
                }
            }
        }
    }
    println!("{}: {} entries, {ok} read as IWI, {bad} failed [{:.1}s]", path.display(), ipak.entries.len(), t.elapsed().as_secs_f32());
    for (v, n) in versions {
        println!("{n:>8}  {}", iwi::engine(v).label(v));
    }
    Ok(())
}

fn exportmodel(args: &[String]) -> Result<()> {
    use atlas::export::{self, ExportModel, Formats, Options};
    let (root, name, out) = (PathBuf::from(args.first().context("usage: --exportmodel GAME NAME OUT_DIR --index IDX")?), args.get(1).context("model name missing")?, PathBuf::from(args.get(2).context("output folder missing")?));
    let mut opts = Options::default();
    let mut index_path = None;
    let mut anim_names: Vec<String> = Vec::new();
    let mut it = args[3..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--index" => index_path = it.next().map(PathBuf::from),
            "--scale" => opts.scale = it.next().context("--scale needs a number")?.parse()?,
            "--unit" => opts.unit = export::Unit::from_code(it.next().context("--unit needs inches, cm or m")?).context("unknown unit (inches, cm, m)")?,
            "--bake" => opts.bake = true,
            "--no-images" => opts.images = false,
            "--anim" => anim_names = it.next().context("--anim needs names")?.split(',').map(str::to_string).collect(),
            "--format" => {
                let list = it.next().context("--format needs a list")?;
                opts.formats = Formats { glb: false, obj: false, xmodel_export: false, semodel: false, cast: false, seanim: false };
                for f in list.split(',') {
                    match f.trim() {
                        "glb" | "gltf" => opts.formats.glb = true,
                        "obj" => opts.formats.obj = true,
                        "xe" | "xmodel_export" => opts.formats.xmodel_export = true,
                        "se" | "semodel" => opts.formats.semodel = true,
                        "cast" => opts.formats.cast = true,
                        "sa" | "seanim" => opts.formats.seanim = true,
                        other => bail!("unknown format {other} (glb, obj, xe, se, sa, cast)"),
                    }
                }
            }
            other => bail!("unknown option {other}"),
        }
    }
    let index = index_path.map(|p| modelindex::ModelIndex::load(&p)).transpose()?;
    // an xmodel name resolves through the index to its first LOD's streamed file; anything else is taken as a file name
    let file = index
        .as_ref()
        .and_then(|i| i.models.iter().find(|m| m.name.eq_ignore_ascii_case(name)))
        .and_then(|m| m.lods.first().and_then(|l| l.file.clone()))
        .map(|f| f.trim_start_matches(',').to_string())
        .unwrap_or_else(|| name.clone());
    // loose folders, archives and 3.19's .ifs alike: the model catalogue knows where the file is
    let models = modelsource::scan(modelsource::discover(&root));
    let entry = models.entries.iter().find(|e| e.name.eq_ignore_ascii_case(&file)).with_context(|| format!("no streamed model file named {file}"))?;
    let model = model::Model::parse(&models.providers[entry.provider].read(&entry.key)?)?;
    let em = ExportModel::from_streamed(&model, index.as_ref(), &file);
    let catalog = build(&root);
    let t = Instant::now();
    // animations are found by replaying the zones: exact names, or a prefix ending in *
    let mut anims: Vec<atlas::anim::Animation> = Vec::new();
    if !anim_names.is_empty() {
        let (found, problems) = export::find_animations(&root, &anim_names)?;
        for p in problems {
            eprintln!("{p}");
        }
        anims = found;
        println!("{} animation(s) found for {:?}", anims.len(), anim_names);
    }
    let (files, missing) = export::export(&em, &out, Some(&catalog), &opts, &anims)?;
    println!("{}: {} parts, {} bones, {} materials -> {} files [{:.1}s]", em.name, em.parts.len(), em.bones.len(), em.materials.len(), files.len(), t.elapsed().as_secs_f32());
    for f in &files {
        println!("  {}", f.display());
    }
    if !missing.is_empty() {
        println!("  images not found: {}", missing.join(", "));
    }
    Ok(())
}

fn animframe(args: &[String]) -> Result<()> {
    use atlas::anim::RawAnim;
    use std::io::{BufRead, BufReader};
    let (Some(root), Some(name), Some(anims), Some(anim_name), Some(frames), Some(out)) = (args.first(), args.get(1), args.get(2), args.get(3), args.get(4), args.get(5)) else { bail!("{USAGE}") };
    let (mut index_path, mut size, mut yaw, mut pitch) = (None, (640u32, 640u32), None::<f32>, None::<f32>);
    let mut i = 6;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--index" => index_path = Some(PathBuf::from(v)),
            "--yaw" => yaw = Some(v.parse::<f32>()?.to_radians()),
            "--pitch" => pitch = Some(v.parse::<f32>()?.to_radians()),
            "--size" => {
                let (w, h) = v.split_once('x').context("--size WxH")?;
                size = (w.parse()?, h.parse()?);
            }
            o => bail!("unknown option {o}"),
        }
        i += 2;
    }
    let index = modelindex::ModelIndex::load(&index_path.context("--index is required")?)?;
    let m = index.models.iter().find(|m| m.name.eq_ignore_ascii_case(name)).with_context(|| format!("no model {name} in the index"))?;
    let skel = m.skeleton.as_ref().context("the index has no skeleton for this model")?;
    let file = m.lods.first().and_then(|l| l.file.clone()).context("model has no LOD file")?;
    let file = file.trim_start_matches(',').to_string();
    let model = model::Model::parse(&std::fs::read(PathBuf::from(root).join("main").join("models").join(&file))?)?;
    let raw = BufReader::new(std::fs::File::open(anims)?)
        .lines()
        .map_while(Result::ok)
        .find_map(|l| RawAnim::from_line(&l).filter(|a| a.name == *anim_name))
        .with_context(|| format!("animation {anim_name} not in {anims}"))?;
    let anim = raw.decode()?;
    let covered = skel.names.iter().filter(|n| anim.tracks.iter().any(|t| &t.name == *n)).count();
    println!("{}: {} bones (anim drives {covered} of the model's {}), {} frames at {} fps", anim.name, anim.tracks.len(), skel.names.len(), anim.num_frames as u32 + 1, anim.frame_rate);
    let bind = pose::bind_pose(skel);
    let mut v = viewer::Viewer::headless()?;
    let list: Vec<f32> = frames.split(',').filter_map(|f| f.parse().ok()).collect();
    let skinned: Vec<model::Model> = list.iter().map(|&f| pose::skin(&model, &bind, &pose::pose(skel, &anim, f))).collect();
    // one camera for all frames: fit the union of the posed bounds
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for m in &skinned {
        if let Some((a, b)) = m.bounds() {
            for k in 0..3 {
                lo[k] = lo[k].min(a[k]);
                hi[k] = hi[k].max(b[k]);
            }
        }
    }
    let mut cam = viewer::Camera::default();
    cam.fit(lo, hi);
    if let Some(y) = yaw {
        cam.yaw = y;
    }
    if let Some(p) = pitch {
        cam.pitch = p;
    }
    let none = vec![modelindex::SurfaceTextures::default(); model.surfaces.len()];
    let states: Vec<Option<modelindex::MaterialState>> = vec![None; model.surfaces.len()];
    let env: Vec<Option<[f32; 4]>> = vec![None; model.surfaces.len()];
    v.set_features(false, false);
    let mut sheet: Vec<Vec<u8>> = Vec::new();
    for m in &skinned {
        v.set_model(Some(m), &none, &states, &env);
        v.render(size, &cam, viewer::Shading::Normals, viewer::Cull::None, false, &[]);
        let (w, h, px) = v.read_back().context("read-back failed")?;
        assert_eq!((w, h), size);
        sheet.push(px);
    }
    // frames side by side in one image
    let (w, h) = size;
    let mut img = vec![0u8; (w as usize * list.len()) * h as usize * 4];
    for (k, px) in sheet.iter().enumerate() {
        for y in 0..h as usize {
            let dst = (y * w as usize * list.len() + k * w as usize) * 4;
            img[dst..dst + w as usize * 4].copy_from_slice(&px[y * w as usize * 4..(y + 1) * w as usize * 4]);
        }
    }
    image::save_buffer(out, &img, w * list.len() as u32, h, image::ColorType::Rgba8)?;
    println!("{} frames -> {out}", list.len());
    Ok(())
}

fn png(root: PathBuf, name: &str, out: PathBuf, level: usize) -> Result<()> {
    let cat = build(&root);
    let entry = cat.entries.iter().find(|e| e.name == name.to_ascii_lowercase()).with_context(|| format!("no image named {name}"))?;
    let v = &entry.variants[0];
    let header = v.header.clone().with_context(|| format!("unreadable header: {:?}", v.error))?;
    let data = cat.providers[v.provider].read(&v.key)?;
    let lvl = iwi::decode_level(&data, &header, level)?;
    image::save_buffer(&out, &lvl.faces[0], lvl.width, lvl.height, image::ColorType::Rgba8)?;
    println!("{}: {}x{} {} -> {}", name, lvl.width, lvl.height, header.format().map_or("?", |f| f.name()), out.display());
    Ok(())
}

fn selftest(root: PathBuf) -> Result<()> {
    let t = Instant::now();
    let cat = Arc::new(build(&root));
    println!("scanned {} names in {:.1}s", cat.entries.len(), t.elapsed().as_secs_f32());

    let bad_header = AtomicUsize::new(0);
    let size_mismatch = AtomicUsize::new(0);
    let decode_fail = AtomicUsize::new(0);
    let ok = AtomicUsize::new(0);
    let by_format: std::sync::Mutex<BTreeMap<&'static str, usize>> = Default::default();
    let problems: std::sync::Mutex<Vec<String>> = Default::default();

    let jobs: Vec<(usize, usize)> = cat.entries.iter().enumerate().flat_map(|(ei, e)| (0..e.variants.len()).map(move |vi| (ei, vi))).collect();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let chunk = jobs.len().div_ceil(threads).max(1);
    let t = Instant::now();
    std::thread::scope(|s| {
        for part in jobs.chunks(chunk) {
            let cat = Arc::clone(&cat);
            let (bad_header, size_mismatch, decode_fail, ok, by_format, problems) = (&bad_header, &size_mismatch, &decode_fail, &ok, &by_format, &problems);
            s.spawn(move || {
                for &(ei, vi) in part {
                    let e = &cat.entries[ei];
                    let v = &e.variants[vi];
                    let note = |msg: String| {
                        let mut p = problems.lock().unwrap();
                        if p.len() < 40 {
                            p.push(format!("{} [{}]: {msg}", e.name, cat.providers[v.provider].label()));
                        }
                    };
                    let Some(h) = &v.header else {
                        bad_header.fetch_add(1, Ordering::Relaxed);
                        note(format!("header: {}", v.error.clone().unwrap_or_default()));
                        continue;
                    };
                    let data = match cat.providers[v.provider].read(&v.key) {
                        Ok(d) => d,
                        Err(err) => {
                            decode_fail.fetch_add(1, Ordering::Relaxed);
                            note(format!("read: {err:#}"));
                            continue;
                        }
                    };
                    // compare with the file as read (an archive's listed size is not always the file's size)
                    if let Some(want) = iwi::expected_size(h) {
                        if want != data.len() {
                            size_mismatch.fetch_add(1, Ordering::Relaxed);
                            note(format!("size {} but header implies {want}", data.len()));
                        }
                    }
                    let last = h.mip_count() - 1;
                    let result = iwi::decode_level(&data, h, 0).and_then(|_| iwi::decode_level(&data, h, last));
                    match result {
                        Ok(_) => {
                            ok.fetch_add(1, Ordering::Relaxed);
                            *by_format.lock().unwrap().entry(h.format().map_or("?", |f| f.name())).or_default() += 1;
                        }
                        Err(err) => {
                            decode_fail.fetch_add(1, Ordering::Relaxed);
                            note(format!("{} {}x{}: {err:#}", h.format().map_or("?", |f| f.name()), h.width, h.height));
                        }
                    }
                }
            });
        }
    });
    println!("decoded in {:.1}s", t.elapsed().as_secs_f32());
    println!(
        "ok {}  bad header {}  size mismatch {}  decode failed {}",
        ok.load(Ordering::Relaxed),
        bad_header.load(Ordering::Relaxed),
        size_mismatch.load(Ordering::Relaxed),
        decode_fail.load(Ordering::Relaxed)
    );
    for (f, n) in by_format.lock().unwrap().iter() {
        println!("  {f:<14} {n}");
    }
    for p in problems.lock().unwrap().iter() {
        println!("  ! {p}");
    }
    Ok(())
}

fn dds2iwi(args: &[String]) -> Result<()> {
    let (Some(input), Some(out)) = (args.first(), args.get(1)) else { bail!("{USAGE}") };
    let mut opts = convert::Options::default();
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--usage" => {
                i += 1;
                opts.usage = args.get(i).context("--usage needs a value")?.clone();
            }
            "--flags" => {
                i += 1;
                opts.flags = Some(convert::parse_flags(args.get(i).context("--flags needs a value")?)?);
            }
            "--genmips" => opts.genmips = true,
            other => bail!("unknown option {other}
{USAGE}"),
        }
        i += 1;
    }
    let (iwi_bytes, summary) = convert::dds_to_iwi(&std::fs::read(input).with_context(|| input.clone())?, &opts)?;
    std::fs::write(out, &iwi_bytes)?;
    println!("{input}: {summary} -> {out}");
    Ok(())
}

/// Reads every plain-format image into stored form, writes it back and compares.
fn roundtrip(root: PathBuf) -> Result<()> {
    let cat = build(&root);
    let (mut same, mut sizes_only, mut other, mut skipped, mut failed) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let mut notes: Vec<String> = Vec::new();
    for e in &cat.entries {
        for v in &e.variants {
            let Some(h) = &v.header else { continue };
            if h.format().is_none_or(|f| f.is_wavelet()) {
                skipped += 1;
                continue;
            }
            let data = cat.providers[v.provider].read(&v.key)?;
            let rewritten = iwi::read_raw(&data, h).and_then(|r| iwi::write(&r));
            match rewritten {
                Ok(out) if out == data => same += 1,
                Ok(out) if out.len() == data.len() && out[..16] == data[..16] && out[32..] == data[32..] => {
                    sizes_only += 1;
                    if notes.len() < 5 {
                        notes.push(format!("{}: picmip sizes {:?} vs original {:?}", e.name, &out[16..32], &data[16..32]));
                    }
                }
                Ok(_) => {
                    other += 1;
                    if notes.len() < 20 {
                        notes.push(format!("{}: differs ({} bytes vs {})", e.name, 0, data.len()));
                    }
                }
                Err(err) => {
                    failed += 1;
                    if notes.len() < 20 {
                        notes.push(format!("{}: {err:#}", e.name));
                    }
                }
            }
        }
    }
    println!("identical {same}  only picmip-size fields differ {sizes_only}  other differences {other}  write failed {failed}  (wavelet skipped {skipped})");
    for n in notes {
        println!("  ! {n}");
    }
    Ok(())
}

fn modelcheck(root: PathBuf) -> Result<()> {
    let t = Instant::now();
    let cat = modelsource::scan(modelsource::discover(&root));
    println!("{} model files in {} sources", cat.entries.len(), cat.providers.len());
    let (mut ok, mut empty, mut failed) = (0usize, 0usize, 0usize);
    let (mut surfaces, mut verts, mut tris) = (0usize, 0usize, 0usize);
    let mut blended = 0usize;
    let mut notes = Vec::new();
    for e in &cat.entries {
        let parsed = cat.providers[e.provider].read(&e.key).and_then(|b| model::Model::parse(&b));
        match parsed {
            Ok(m) => {
                if m.name != e.name {
                    notes.push(format!("{}: contains the name {:?}", e.name, m.name));
                }
                if m.surfaces.is_empty() {
                    empty += 1;
                }
                surfaces += m.surfaces.len();
                verts += m.vertex_count();
                tris += m.triangle_count();
                blended += m.surfaces.iter().filter(|s| s.blend_counts.iter().any(|&c| c > 0)).count();
                ok += 1;
            }
            Err(err) => {
                failed += 1;
                if notes.len() < 25 {
                    notes.push(format!("{}: {err:#}", e.name));
                }
            }
        }
    }
    println!("parsed {ok} ({empty} without surfaces), failed {failed}; {surfaces} surfaces, {verts} vertices, {tris} triangles, {blended} surfaces with blend weights; {:.1}s", t.elapsed().as_secs_f32());
    for n in notes.iter().take(25) {
        println!("  ! {n}");
    }
    Ok(())
}

fn rendermodel(args: &[String]) -> Result<()> {
    let (Some(root), Some(name), Some(out)) = (args.first(), args.get(1), args.get(2)) else { bail!("{USAGE}") };
    let (mut size, mut shading, mut cull, mut wire) = ((960u32, 720u32), viewer::Shading::Lit, viewer::Cull::Material, false);
    let mut cam = viewer::Camera::default();
    let mut index_path: Option<PathBuf> = None;
    let (mut no_normal, mut no_spec) = (false, false);
    let mut zoom = 1.0f32;
    let mut shading_given = false;
    let (mut yaw, mut pitch): (Option<f32>, Option<f32>) = (None, None);
    let mut i = 3;
    while i < args.len() {
        let option = args[i].clone();
        let value = |i: &mut usize| -> Result<String> {
            *i += 1;
            args.get(*i).cloned().context("option needs a value")
        };
        match option.as_str() {
            "--size" => {
                let v = value(&mut i)?;
                let (w, h) = v.split_once('x').context("--size WxH")?;
                size = (w.parse()?, h.parse()?);
            }
            "--index" => index_path = Some(PathBuf::from(value(&mut i)?)),
            "--shading" => {
                shading_given = true;
                shading = match value(&mut i)?.as_str() {
                    "lit" => viewer::Shading::Lit,
                    "normals" => viewer::Shading::Normals,
                    "uv" => viewer::Shading::UvChecker,
                    "uvcolor" => viewer::Shading::UvColor,
                    "color" => viewer::Shading::VertexColor,
                    "surfaces" => viewer::Shading::Surfaces,
                    "textured" => viewer::Shading::Textured,
                    o => bail!("unknown shading {o}"),
                }
            }
            "--cull" => {
                cull = match value(&mut i)?.as_str() {
                    "none" => viewer::Cull::None,
                    "back" => viewer::Cull::Back,
                    "front" => viewer::Cull::Front,
                    "material" => viewer::Cull::Material,
                    o => bail!("unknown cull {o}"),
                }
            }
            "--wire" => wire = true,
            "--no-normal" => no_normal = true,
            "--zoom" => zoom = value(&mut i)?.parse::<f32>()?,
            "--no-spec" => no_spec = true,
            "--yaw" => yaw = Some(value(&mut i)?.parse::<f32>()?.to_radians()),
            "--pitch" => pitch = Some(value(&mut i)?.parse::<f32>()?.to_radians()),
            o => bail!("unknown option {o}"),
        }
        i += 1;
    }
    let cat = modelsource::scan(modelsource::discover(&PathBuf::from(root)));
    let e = cat.entries.iter().find(|e| e.name.eq_ignore_ascii_case(name)).with_context(|| format!("no model named {name}"))?;
    let m = model::Model::parse(&cat.providers[e.provider].read(&e.key)?)?;
    let (lo, hi) = m.bounds().context("model has no vertices")?;
    cam.fit(lo, hi);
    if let Some(y) = yaw {
        cam.yaw = y;
    }
    if let Some(p) = pitch {
        cam.pitch = p;
    }
    cam.dist /= zoom;
    let mut v = viewer::Viewer::headless()?;
    let mut surface_tex: Vec<modelindex::SurfaceTextures> = vec![Default::default(); m.surfaces.len()];
    let mut surface_states: Vec<Option<modelindex::MaterialState>> = vec![None; m.surfaces.len()];
    let mut surface_env: Vec<Option<[f32; 4]>> = vec![None; m.surfaces.len()];
    if let Some(path) = &index_path {
        let index = modelindex::ModelIndex::load(path)?;
        surface_tex = index.surface_textures(&e.name, m.surfaces.len());
        surface_states = index.surface_states(&e.name, m.surfaces.len());
        surface_env = index.surface_env(&e.name, m.surfaces.len());
        let images = source::scan(source::discover(&PathBuf::from(root)), &source::Progress::default());
        for (k, t) in surface_tex.iter().enumerate() {
            if t.color.is_none() {
                eprintln!("surface {k}: no material in the index");
            }
            for (label, name) in [("colour", &t.color), ("normal", &t.normal), ("spec", &t.spec)] {
                let Some(name) = name else { continue };
                match modeltex::load_image(&images, name) {
                    Ok(img) => {
                        eprintln!("surface {k}: {label} {name} ({}x{}, {} mips)", img.mips[0].width, img.mips[0].height, img.mips.len());
                        v.upload_texture(name, &img);
                    }
                    Err(err) => eprintln!("surface {k}: {label} {name}: {err:#}"),
                }
            }
        }
        if !shading_given {
            shading = viewer::Shading::Textured;
        }
    }
    v.set_features(!no_normal, !no_spec);
    v.set_model(Some(&m), &surface_tex, &surface_states, &surface_env);
    v.render(size, &cam, shading, cull, wire, &[]);
    let (w, h, px) = v.read_back().context("read-back failed")?;
    image::save_buffer(out, &px, w, h, image::ColorType::Rgba8)?;
    println!("{}: {} surfaces, {} vertices, {} triangles, bounds {:?}..{:?} -> {out}", m.name, m.surfaces.len(), m.vertex_count(), m.triangle_count(), lo, hi);
    Ok(())
}

/// `atlas --rendermap GAME_FOLDER ZONE.ff OUT.png [--layers all|none|a,b] [--world] [--size WxH] [--shading ..] [--yaw D] [--pitch D] [--zoom F] [--wire]`:
/// the editor layers of a map (collision brushes, static model boxes, entity markers) rendered without a window. `--layers` takes
/// substrings of the layer names (`brush`, `static`, `spawns`, ..); the world surfaces are drawn only with `--world`.
fn rendermap(args: &[String]) -> Result<()> {
    let (Some(root), Some(zone_name), Some(out)) = (args.first(), args.get(1), args.get(2)) else { bail!("{USAGE}") };
    let (mut size, mut shading, mut wire, mut world_on) = ((1280u32, 800u32), viewer::Shading::Lit, false, false);
    let (mut layers_arg, mut yaw, mut pitch, mut zoom) = ("all".to_string(), None::<f32>, None::<f32>, 1.0f32);
    let mut i = 3;
    while i < args.len() {
        let option = args[i].clone();
        let mut value = || -> Result<String> {
            i += 1;
            args.get(i).cloned().context("option needs a value")
        };
        match option.as_str() {
            "--size" => {
                let v = value()?;
                let (w, h) = v.split_once('x').context("--size WxH")?;
                size = (w.parse()?, h.parse()?);
            }
            "--layers" => layers_arg = value()?,
            "--world" => world_on = true,
            "--wire" => wire = true,
            "--zoom" => zoom = value()?.parse()?,
            "--yaw" => yaw = Some(value()?.parse::<f32>()?.to_radians()),
            "--pitch" => pitch = Some(value()?.parse::<f32>()?.to_radians()),
            "--shading" => {
                shading = match value()?.as_str() {
                    "lit" => viewer::Shading::Lit,
                    "color" => viewer::Shading::VertexColor,
                    "normals" => viewer::Shading::Normals,
                    "surfaces" => viewer::Shading::Surfaces,
                    "textured" => viewer::Shading::Textured,
                    o => bail!("unknown shading {o}"),
                }
            }
            o => bail!("unknown option {o}"),
        }
        i += 1;
    }
    let root = PathBuf::from(root);
    let key = zone::export::find_key(&root)?;
    let path = zone::export::zone_files(&root).into_iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(zone_name))).context("no such zone")?;
    let z = zone::Zone::open(&path, &key)?;
    let src = zone::mapsrc::read(&z, zone_name)?;
    let mut model = zone::world::model_of_zone(&z).map(|m| m.0).unwrap_or(model::Model { name: zone_name.clone(), surfaces: Vec::new() });
    let world_surfaces = model.surfaces.len();
    let layers = zone::mapscene::layers(&src);
    let mut visible = vec![world_on; world_surfaces];
    for l in layers {
        let on = match layers_arg.as_str() {
            "all" => true,
            "none" => false,
            list => list.split(',').any(|t| l.name.contains(t)),
        };
        println!("{:<28} {:>8} vertices {:>8} triangles{}", l.name, l.surface.vertices.len(), l.surface.indices.len() / 3, if on { "" } else { "  (hidden)" });
        visible.push(on);
        model.surfaces.push(l.surface);
    }
    // frame the middle 70% of what is shown (props far outside the play area would otherwise push the camera away)
    let mut axes: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for (s, _) in model.surfaces.iter().zip(&visible).filter(|(_, v)| **v) {
        for v in s.vertices.iter().step_by(7) {
            for a in 0..3 {
                axes[a].push(v.pos[a]);
            }
        }
    }
    if axes[0].is_empty() {
        bail!("nothing to draw: no layer matches {layers_arg:?}");
    }
    let (mut lo, mut hi) = ([0f32; 3], [0f32; 3]);
    for a in 0..3 {
        axes[a].sort_by(|x, y| x.total_cmp(y));
        let n = axes[a].len();
        let (a0, a1) = (axes[a][n * 15 / 100], axes[a][(n * 85 / 100).min(n - 1)]);
        let pad = (a1 - a0) * 0.25;
        lo[a] = a0 - pad;
        hi[a] = a1 + pad;
    }
    let mut cam = viewer::Camera::default();
    println!("framing {lo:?} .. {hi:?}");
    cam.fit(lo, hi);
    if let Some(y) = yaw {
        cam.yaw = y;
    }
    if let Some(p) = pitch {
        cam.pitch = p;
    }
    cam.dist /= zoom;
    let mut v = viewer::Viewer::headless()?;
    let tex: Vec<modelindex::SurfaceTextures> = vec![Default::default(); model.surfaces.len()];
    v.set_model(Some(&model), &tex, &[], &[]);
    v.render(size, &cam, shading, viewer::Cull::None, wire, &visible);
    let (w, h, px) = v.read_back().context("read-back failed")?;
    image::save_buffer(out, &px, w, h, image::ColorType::Rgba8)?;
    println!("{zone_name}: {} entities, {} world brushes, {} brush models, {} static models -> {out}", src.entities.len(), src.world.len(), src.submodels.len(), src.models.len());
    Ok(())
}

fn zoneinfo(args: &[String]) -> Result<()> {
    let (Some(key), files) = (args.first(), &args[1.min(args.len())..]) else { bail!("{USAGE}") };
    let key = zone::fastfile::ArchiveKey::load(&PathBuf::from(key))?;
    let mut by_type: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    let t = Instant::now();
    for f in files {
        match zone::Zone::open(&PathBuf::from(f), &key) {
            Ok(z) => {
                let mut types: BTreeMap<u32, usize> = BTreeMap::new();
                for a in &z.assets {
                    *types.entry(a.asset_type).or_default() += 1;
                    let e = by_type.entry(a.asset_type).or_default();
                    e.0 += 1;
                    e.1 += a.extent;
                }
                println!("{f}: v{} {} bytes, {} strings, {} assets {:?}", z.version, z.data.len(), z.string_count, z.assets.len(), types);
            }
            Err(e) => println!("{f}: {e:#}"),
        }
    }
    println!("total by type (count, bytes): {by_type:?}  [{:.1}s]", t.elapsed().as_secs_f32());
    Ok(())
}

fn zoneindex(args: &[String]) -> Result<()> {
    let (Some(key), Some(out_path)) = (args.first(), args.get(1)) else { bail!("{USAGE}") };
    let key = zone::fastfile::ArchiveKey::load(&PathBuf::from(key))?;
    let mut out = String::from("#codol-zone-export 1
");
    let t = Instant::now();
    let mut ok = 0;
    for f in &args[2..] {
        let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| f.clone());
        let z = match zone::Zone::open(&PathBuf::from(f), &key) {
            Ok(z) => z,
            Err(e) => {
                println!("{name}: {e:#}");
                continue;
            }
        };
        let r = zone::export::export_zone(&z, &name, &mut out);
        if r.failed.is_empty() {
            ok += 1;
        }
        println!("{name}: {} assets, {} models, {} materials{}", r.assets, r.models, r.materials, r.failed.iter().map(|(i, t, w)| format!("  FAILED asset {i} (type {t}): {w}")).collect::<String>());
    }
    std::fs::write(out_path, out)?;
    println!("{ok} of {} zones replayed completely [{:.1}s]", args.len() - 2, t.elapsed().as_secs_f32());
    Ok(())
}

fn dumpshaders(args: &[String]) -> Result<()> {
    let (Some(key), Some(dir)) = (args.first(), args.get(1)) else { bail!("{USAGE}") };
    let key = zone::fastfile::ArchiveKey::load(&PathBuf::from(key))?;
    std::fs::create_dir_all(dir)?;
    let mut seen = std::collections::HashSet::new();
    let mut written = 0;
    for f in &args[2..] {
        let z = zone::Zone::open(&PathBuf::from(f), &key)?;
        let mut replay = zone::replay::Replay::new(&z).map_err(|e| anyhow::anyhow!("{} at {:#x}", e.what, e.at))?;
        for i in 0..z.assets.len() {
            let _ = replay.replay_asset(&z, i);
        }
        for (name, bytes) in &replay.rec.shaders {
            let name = name.clone().unwrap_or_else(|| "unnamed".into()).replace([',', '/', '\\', ':', '*', '?'], "_");
            if seen.insert((name.clone(), bytes.len())) {
                std::fs::write(PathBuf::from(dir).join(format!("{name}.bin")), bytes)?;
                written += 1;
            }
        }
    }
    println!("{written} shader programs written to {dir}");
    Ok(())
}
