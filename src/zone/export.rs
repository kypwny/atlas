//! Turns a replayed zone into model-index records (the format described in `modelindex`).

use super::replay::{INSERT, MatRec, Recorder, Replay};
use super::Zone;
use super::fastfile::Family;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt::Write;

fn dash(s: Option<&str>) -> &str {
    match s {
        Some(s) if !s.is_empty() => s,
        _ => "-",
    }
}

fn material_for<'a>(rec: &'a Recorder, start: i64, reference: u32) -> Option<&'a MatRec> {
    let start = if start >= 0 { start } else { via_alias(rec, 5, reference)? };
    rec.material_by_start.get(&(start as usize)).map(|&i| &rec.materials[i])
}

fn via_alias(rec: &Recorder, ty: u32, reference: u32) -> Option<i64> {
    if reference == 0 {
        return None;
    }
    rec.aliases.get(&(ty, reference)).map(|&s| s as i64)
}

/// Appends the zone's models, materials and textures to `out`.
pub fn write_zone(out: &mut String, zone_name: &str, version: u32, rec: &Recorder, strings: &[Option<String>]) {
    let _ = writeln!(out, "zone\t{zone_name}\t{version}");
    for m in &rec.models {
        let Some(name) = &m.name else { continue };
        let _ = writeln!(out, "model\t{name}\t{}\t{}\t{}", m.bones, m.roots, m.surfs);
        if let Some(sk) = &m.skel {
            // bone names are resolved here (they index the zone's own script string table)
            let names: Vec<String> = sk
                .names
                .chunks_exact(2)
                .map(|c| strings.get(u16::from_le_bytes([c[0], c[1]]) as usize).and_then(|s| s.clone()).unwrap_or_else(|| "-".into()))
                .collect();
            let hex = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02x}")).collect() };
            let _ = writeln!(out, "skel	{name}	{}	{}	{}	{}	{}", names.join(","), hex(&sk.parents), hex(&sk.quats), hex(&sk.trans), hex(&sk.base));
        }
        for j in 0..4 {
            let sn = if m.lod_start[j] >= 0 { rec.surfs.get(&(m.lod_start[j] as usize)).and_then(|s| s.as_deref()) } else { None };
            let sn = sn.or_else(|| via_alias(rec, 3, m.lod_ref[j]).and_then(|s| rec.surfs.get(&(s as usize)).and_then(|s| s.as_deref())));
            let hex: String = m.lod[j].iter().map(|b| format!("{b:02x}")).collect();
            let _ = writeln!(out, "lod\t{name}\t{j}\t{hex}\t{}", dash(sn));
        }
        for j in 0..m.mat_start.len() {
            let mt = material_for(rec, m.mat_start[j], m.mat_ref[j]);
            let _ = writeln!(out, "mat\t{name}\t{j}\t{}", dash(mt.and_then(|m| m.name.as_deref())));
        }
    }
    let mut done: HashSet<*const MatRec> = HashSet::new();
    for m in &rec.models {
        for j in 0..m.mat_start.len() {
            let Some(mt) = material_for(rec, m.mat_start[j], m.mat_ref[j]) else { continue };
            let Some(mname) = &mt.name else { continue };
            if !done.insert(mt as *const MatRec) {
                continue;
            }
            let _ = writeln!(out, "material\t{mname}\t{}", mt.textures.len());
            for (k, t) in mt.textures.iter().enumerate() {
                let image = if t.image_start >= 0 { rec.images.get(&(t.image_start as usize)).and_then(|s| s.as_deref()) } else { None };
                let image = image.or_else(|| via_alias(rec, 10, t.image_ref).and_then(|s| rec.images.get(&(s as usize)).and_then(|s| s.as_deref())));
                let _ = writeln!(out, "tex\t{mname}\t{k}\t{}\t{}\t{:08x}\t{}", t.semantic, t.sampler, t.hash, dash(image));
            }
            // Render state: technique-set name, the stored header and the state-bits table (decoded by the index reader).
            let techset = if mt.techset_start >= 0 { rec.techsets.get(&(mt.techset_start as usize)).and_then(|s| s.as_deref()) } else { None };
            let techset = techset.or_else(|| via_alias(rec, 9, mt.techset_ref).and_then(|s| rec.techsets.get(&(s as usize)).and_then(|s| s.as_deref())));
            let hex = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02x}")).collect() };
            let _ = writeln!(out, "state\t{mname}\t{}\t{}\t{}", dash(techset), hex(&mt.header), hex(&mt.state_bits));
            for c in mt.constants.chunks_exact(32) {
                let name: String = c[4..16].iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
                let f = |o: usize| f32::from_le_bytes([c[o], c[o + 1], c[o + 2], c[o + 3]]);
                let _ = writeln!(out, "const\t{mname}\t{name}\t{}\t{}\t{}\t{}", f(16), f(20), f(24), f(28));
            }
        }
    }
    let _ = INSERT;
}

/// Outcome of replaying one zone.
pub struct ZoneReport {
    pub assets: usize,
    pub failed: Vec<(usize, u32, String)>,
    pub models: usize,
    pub materials: usize,
}

/// Replays every asset of `zone` and appends its index records to `out`. Stops at the first asset that fails.
pub fn export_zone(zone: &Zone, zone_name: &str, out: &mut String) -> ZoneReport {
    let mut report = ZoneReport { assets: zone.assets.len(), failed: Vec::new(), models: 0, materials: 0 };
    let mut replay = match Replay::new(zone) {
        Ok(r) => r,
        Err(f) => {
            report.failed.push((usize::MAX, 0, format!("{} at {:#x}", f.what, f.at)));
            return report;
        }
    };
    // Like the C harness: a failed asset is reported and the walk resumes at the next directory entry.
    for i in 0..zone.assets.len() {
        if let Err(f) = replay.replay_asset(zone, i) {
            report.failed.push((i, zone.assets[i].asset_type, format!("{} at {:#x}", f.what, f.at)));
        }
    }
    report.models = replay.rec.models.len();
    report.materials = replay.rec.materials.len();
    write_zone(out, zone_name, zone.version, &replay.rec, &zone.script_strings());
    report
}

/// Appends the zone's animations as `anim` lines: name, header, bone names, notifies, then the flat data arrays (hex).
pub fn write_anims(out: &mut String, rec: &Recorder, strings: &[Option<String>], filter: Option<&str>) {
    let hex = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02x}")).collect() };
    let script = |i: u16| strings.get(i as usize).and_then(|s| s.clone()).unwrap_or_else(|| "-".into());
    for a in &rec.anims {
        let Some(name) = &a.name else { continue };
        if filter.is_some_and(|f| !name.contains(f)) {
            continue;
        }
        let delta_col = |d: &Option<super::replay::DeltaRec>| match d {
            Some(d) => format!("{}:{}:{}:{}:{}", hex(&d.header), hex(&d.constant), hex(&d.frames), hex(&d.indices), hex(&d.data)),
            None => "-".to_string(),
        };
        let names: Vec<String> = a.names.chunks_exact(2).map(|c| script(u16::from_le_bytes([c[0], c[1]]))).collect();
        let notifies: Vec<String> = a.notify.chunks_exact(8).map(|c| format!("{}:{}", script(u16::from_le_bytes([c[0], c[1]])), f32::from_le_bytes([c[4], c[5], c[6], c[7]]))).collect();
        let _ = writeln!(
            out,
            "anim	{name}	{}	{}	{}	{}	{}	{}	{}	{}	{}\t{}\t{}\t{}",
            hex(&a.header),
            names.join(","),
            notifies.join(","),
            hex(&a.data_byte),
            hex(&a.data_short),
            hex(&a.data_int),
            hex(&a.random_short),
            hex(&a.random_byte),
            hex(&a.indices),
            delta_col(&a.delta[0]),
            delta_col(&a.delta[1]),
            delta_col(&a.delta[2])
        );
    }
}

/// Turns a recorded animation into decodable raw arrays, resolving script strings through the zone's table.
pub fn raw_anim(a: &super::replay::AnimRec, strings: &[Option<String>]) -> Option<crate::anim::RawAnim> {
    let script = |i: u16| strings.get(i as usize).and_then(|s| s.clone()).unwrap_or_else(|| "-".into());
    let le16 = |b: &[u8]| -> Vec<i16> { b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect() };
    Some(crate::anim::RawAnim {
        name: a.name.clone()?,
        header: a.header.clone(),
        bone_names: a.names.chunks_exact(2).map(|c| script(u16::from_le_bytes([c[0], c[1]]))).collect(),
        notifies: a.notify.chunks_exact(8).map(|c| (script(u16::from_le_bytes([c[0], c[1]])), f32::from_le_bytes([c[4], c[5], c[6], c[7]]))).collect(),
        data_byte: a.data_byte.clone(),
        data_short: le16(&a.data_short),
        data_int: a.data_int.chunks_exact(4).map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect(),
        random_short: le16(&a.random_short),
        random_byte: a.random_byte.clone(),
        indices: a.indices.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect(),
        delta: [0usize, 1, 2].map(|i| a.delta[i].as_ref().map(|d| crate::anim::RawDelta { header: d.header.clone(), constant: d.constant.clone(), frames: d.frames.clone(), indices: d.indices.clone(), data: d.data.clone() })),
    })
}

/// Replays a zone and returns its animations (raw, not yet decoded).
pub fn zone_anims(zone: &Zone) -> Vec<crate::anim::RawAnim> {
    let Ok(mut replay) = Replay::new(zone) else { return Vec::new() };
    for i in 0..zone.assets.len() {
        let _ = replay.replay_asset(zone, i);
    }
    let strings = zone.script_strings();
    replay.rec.anims.iter().filter_map(|a| raw_anim(a, &strings)).collect()
}

/// The assets a zone defines, by name, as (zone builder type, name): models, materials, images, technique sets, model surfaces and
/// animations (the types whose names the replay records), in the order the zone stores them, each once.
pub fn zone_asset_list(zone: &Zone) -> Vec<(&'static str, String)> {
    let Ok(mut replay) = Replay::new(zone) else { return Vec::new() };
    for i in 0..zone.assets.len() {
        let _ = replay.replay_asset(zone, i);
    }
    let rec = &replay.rec;
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut seen: HashSet<(&'static str, String)> = HashSet::new();
    let mut add = |ty: &'static str, name: &str| {
        let name = name.trim_start_matches(',');
        if !name.is_empty() && seen.insert((ty, name.to_string())) {
            out.push((ty, name.to_string()));
        }
    };
    let sorted = |m: &HashMap<usize, Option<String>>| -> Vec<String> {
        let mut v: Vec<(&usize, &Option<String>)> = m.iter().collect();
        v.sort_by_key(|(k, _)| **k);
        v.into_iter().filter_map(|(_, n)| n.clone()).collect()
    };
    for m in &rec.models {
        if let Some(n) = &m.name {
            add("xmodel", n);
        }
    }
    for n in sorted(&rec.surfs) {
        add("xmodelsurfs", &n);
    }
    for m in &rec.materials {
        if let Some(n) = &m.name {
            add("material", n);
        }
    }
    for n in sorted(&rec.techsets) {
        add("techset", &n);
    }
    for n in sorted(&rec.images) {
        add("image", &n);
    }
    for a in &rec.anims {
        if let Some(n) = &a.name {
            add("xanim", n);
        }
    }
    out
}

/// Replays one zone and appends its animations to `out`.
pub fn export_zone_anims(zone: &Zone, out: &mut String, filter: Option<&str>) -> usize {
    let Ok(mut replay) = Replay::new(zone) else { return 0 };
    for i in 0..zone.assets.len() {
        let _ = replay.replay_asset(zone, i);
    }
    write_anims(out, &replay.rec, &zone.script_strings(), filter);
    replay.rec.anims.len()
}

/// Replays every zone of a game folder and returns the animation index text (`anim` lines).
pub fn build_anim_index(root: &Path, progress: &(AtomicUsize, AtomicUsize), filter: Option<&str>) -> Result<String> {
    let key = find_key(root)?;
    let zones = zone_files(root);
    if zones.is_empty() {
        bail!("no fastfiles found under {}", root.display());
    }
    progress.1.store(zones.len(), Ordering::Relaxed);
    let mut out = String::from("#codol-anim-export 1
");
    for path in &zones {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Ok(zone) = Zone::open(path, &key) {
            let _ = writeln!(out, "zone	{name}	{}", zone.version);
            export_zone_anims(&zone, &mut out, filter);
        }
        progress.0.fetch_add(1, Ordering::Relaxed);
    }
    Ok(out)
}

use super::fastfile::ArchiveKey;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Finds the archive key in the game folder: a `c1_keys.bin`, or any executable that contains it.
pub fn find_key(root: &Path) -> Result<ArchiveKey> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir in [root.to_path_buf(), root.join("..")] {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            if name == "c1_keys.bin" || name.ends_with(".exe") {
                candidates.push(p);
            }
        }
    }
    // Smaller executables first: the key sits in the game's own client, not in the bundled helpers.
    candidates.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(u64::MAX));
    for c in &candidates {
        if let Ok(k) = ArchiveKey::load(c) {
            return Ok(k);
        }
    }
    bail!("no executable or c1_keys.bin with the archive key was found in {} (looked at {} files)", root.display(), candidates.len())
}

/// Every `.ff` under `client_shipRetail/<language>/`, plus any directly in the folder.
pub fn zone_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut scan = |dir: &Path| {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ff")) {
                    out.push(p);
                }
            }
        }
    };
    scan(root);
    let ship = root.join("client_shipRetail");
    if let Ok(rd) = std::fs::read_dir(&ship) {
        let mut langs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        langs.sort();
        for l in langs {
            scan(&l);
        }
    }
    out.sort();
    out
}

/// Fastfile versions found in a game folder's zones (headers only, so no key is needed).
#[derive(Clone, Debug, Default)]
pub struct ZoneSurvey {
    pub total: usize,
    /// ((family, version), zones), most common first.
    pub by_version: Vec<((Family, u32), usize)>,
    /// Files that are not fastfiles of a known family.
    pub unreadable: usize,
}

/// Every `.ff` a survey looks at: the CoDOL layout (`zone_files`) plus Treyarch's `zone/<language>/` folders.
pub fn survey_files(root: &Path) -> Vec<PathBuf> {
    let mut out = zone_files(root);
    let zone = root.join("zone");
    let mut dirs = vec![zone.clone()];
    if let Ok(rd) = std::fs::read_dir(&zone) {
        let mut subs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subs.sort();
        dirs.extend(subs);
    }
    for d in dirs {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ff")) && !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

pub fn survey(root: &Path) -> ZoneSurvey {
    let files = survey_files(root);
    let mut counts: BTreeMap<(Family, u32), usize> = BTreeMap::new();
    let mut unreadable = 0;
    for f in &files {
        match super::fastfile::peek(f) {
            Ok(p) => *counts.entry((p.family, p.version)).or_default() += 1,
            Err(_) => unreadable += 1,
        }
    }
    let mut by_version: Vec<_> = counts.into_iter().collect();
    by_version.sort_by(|a, b| b.1.cmp(&a.1));
    ZoneSurvey { total: files.len(), by_version, unreadable }
}

/// Replays every zone of a game folder and returns the model index text. `progress` is (zones done, zones total).
pub fn build_index(root: &Path, progress: &(AtomicUsize, AtomicUsize)) -> Result<String> {
    let key = find_key(root)?;
    let zones = zone_files(root);
    if zones.is_empty() {
        bail!("no fastfiles found under {}", root.display());
    }
    progress.1.store(zones.len(), Ordering::Relaxed);
    let mut out = String::from("#codol-zone-export 1\n");
    for path in &zones {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // A zone that will not open (for example another game version's) is skipped, not fatal.
        if let Ok(zone) = Zone::open(path, &key) {
            export_zone(&zone, &name, &mut out);
        }
        progress.0.fetch_add(1, Ordering::Relaxed);
    }
    if !out.contains("\nmodel\t") {
        bail!("no models were found in {} zones", zones.len());
    }
    Ok(out)
}
