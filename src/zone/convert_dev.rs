//! Development aids for the stock-to-QA converter: dumps of real QA structures that are only seen through the walker.

use super::fastfile::ArchiveKey;
use super::replay::{DataRec, Replay};
use super::{u32_at, Zone};
use anyhow::{Context, Result};
use std::path::Path;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// `atlas --convertzone qadump GAME_FOLDER ZONE KIND [N]`: KIND is `model` (headers and lod records), `xsurf` (the XSurface records
/// behind the first N model-surface headers), `header:<kind>` (any recorded header kind).
pub fn qadump(args: &[String]) -> Result<()> {
    let root = Path::new(args.first().context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let zone = Zone::open(Path::new(args.get(1).context("zone")?), &key)?;
    let kind = args.get(2).context("kind")?.as_str();
    let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);
    let mut r = Replay::new(&zone).map_err(|f| anyhow::anyhow!("{} at {}", f.what, f.at))?;
    for i in 0..zone.assets.len() {
        r.replay_asset(&zone, i).map_err(|f| anyhow::anyhow!("asset {i}: {} at {:#x}", f.what, f.at))?;
    }
    match kind {
        "xsurf" => {
            let mut shown = 0;
            let mut starts: Vec<(&usize, &Option<String>)> = r.rec.surfs.iter().collect();
            starts.sort();
            for (&h, name) in starts {
                let hdr = &zone.data[h..h + 48];
                let count = u16::from_le_bytes([hdr[8], hdr[9]]) as usize;
                let mut at = h + 48;
                if u32_at(hdr, 0) == u32::MAX {
                    at += zone.data[at..].iter().position(|&c| c == 0).unwrap_or(0) + 1;
                }
                println!("surfs {:?} header {}", name, hex(hdr));
                if u32_at(hdr, 4) != 0 {
                    for j in 0..count {
                        println!("   xsurface {j}: {}", hex(&zone.data[at + 84 * j..at + 84 * (j + 1)]));
                    }
                }
                shown += 1;
                if shown >= n {
                    break;
                }
            }
        }
        k => {
            let want = k.strip_prefix("header:").unwrap_or(k);
            let mut shown = 0;
            for d in &r.rec.data {
                if let DataRec::Header { kind, name, bytes } = d {
                    if *kind == want {
                        println!("{:?}: {}", name, hex(bytes));
                        shown += 1;
                        if shown >= n {
                            break;
                        }
                    }
                }
            }
            if want == "model" {
                for m in r.rec.models.iter().take(n) {
                    println!("model {:?} lods:", m.name);
                    for l in &m.lod {
                        println!("   {}", hex(l));
                    }
                }
            }
        }
    }
    Ok(())
}

/// `atlas --convertzone stockmodel ZONE [N]`: part bits and lod records of the first N stock models.
pub fn stockmodel(args: &[String]) -> Result<()> {
    use super::convert_zone::Src;
    use super::convert_walk::{walk_with, Game, View, ZoneImage};
    let raw = std::fs::read(args.first().context("zone")?)?;
    let want = args.get(1).filter(|a| a.parse::<usize>().is_err()).cloned();
    let n: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(if want.is_some() { usize::MAX } else { 5 });
    let zone = ZoneImage::open(Game::Iw4, "x", &raw)?;
    let mut shown = 0usize;
    let mut hook = |v: &View, name: &str, hdr: u32, _id: u32| {
        if name != "XModel" || shown >= n {
            return;
        }
        let s = Src { v, hdr };
        if let Some(w) = &want {
            if s.cstr(0).map(|b| b != w.as_bytes()).unwrap_or(true) {
                return;
            }
        }
        shown += 1;
        println!("model {:?} bones {} roots {} surfs {} flags {} noScale {}", s.cstr(0).map(|b| String::from_utf8_lossy(&b).into_owned()), s.u8(4), s.u8(5), s.u8(6), s.u8(243), hex(&s.bytes(12, 24)));
        for i in 0..4u32 {
            let l = s.sub(64 + 44 * i);
            println!("  lod {i}: {}", hex(&l.bytes(0, 44)));
            if l.has(8) {
                let m = s.at(l.u32(8));
                println!("    surfs hdr {}", hex(&m.bytes(0, 36)));
                if m.has(4) {
                    for j in 0..m.u16(8) as u32 {
                        println!("    xsurface {j}: {}", hex(&s.at(m.u32(4) + 64 * j).bytes(0, 64)));
                    }
                }
            }
        }
    };
    walk_with(Game::Iw4, &zone, Some(&mut hook));
    Ok(())
}

/// `atlas --convertzone qastats GAME_FOLDER ZONE`: value histograms of the QA fields that have no stock counterpart.
pub fn qastats(args: &[String]) -> Result<()> {
    use std::collections::BTreeMap;
    let root = Path::new(args.first().context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let zone = Zone::open(Path::new(args.get(1).context("zone")?), &key)?;
    let mut r = Replay::new(&zone).map_err(|f| anyhow::anyhow!("{} at {}", f.what, f.at))?;
    for i in 0..zone.assets.len() {
        r.replay_asset(&zone, i).map_err(|f| anyhow::anyhow!("asset {i}: {} at {:#x}", f.what, f.at))?;
    }
    let mut h: BTreeMap<String, BTreeMap<u32, usize>> = BTreeMap::new();
    let mut add = |k: &str, v: u32| *h.entry(k.to_string()).or_default().entry(v).or_default() += 1;
    for (&s, _) in r.rec.surfs.iter() {
        let hdr = &zone.data[s..s + 48];
        add("surfs+44", u32_at(hdr, 44));
        add("surfs+4", u32_at(hdr, 4));
        let count = u16::from_le_bytes([hdr[8], hdr[9]]) as usize;
        let mut at = s + 48;
        if u32_at(hdr, 0) == u32::MAX {
            at += zone.data[at..].iter().position(|&c| c == 0).unwrap_or(0) + 1;
        }
        if u32_at(hdr, 4) != 0 {
            for j in 0..count {
                let x = &zone.data[at + 84 * j..at + 84 * (j + 1)];
                add("xs+2", u16::from_le_bytes([x[2], x[3]]) as u32);
                add("xs+8", u32_at(x, 8));
                add("xs+12", u32_at(x, 12));
                add("xs+16", u32_at(x, 16));
                add("xs+20", u32_at(x, 20));
                add("xs+24", u32_at(x, 24));
                add("xs+28", u32_at(x, 28));
                add("xs+32", u32_at(x, 32));
                add("xs+36", u32_at(x, 36));
                add("xs+44", u32_at(x, 44));
                add("xs+48", u32_at(x, 48));
                add("xs+0", u32_at(x, 0));
            }
        }
    }
    for m in &r.rec.models {
        for (i, l) in m.lod.iter().enumerate() {
            add(&format!("lod{}+12 hashlike", i.min(1)), if u32_at(l, 12) == 0 { 0 } else if u32_at(l, 12) >= 0xffff_fffe { u32_at(l, 12) } else { 1 });
            add(&format!("lod{}+48", i.min(1)), u32_at(l, 48));
            add(&format!("lod{}+52", i.min(1)), u32_at(l, 52));
        }
    }
    for (k, m) in h {
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        println!("{k:22} {} distinct; {}", v.len(), v.iter().take(6).map(|(a, n)| format!("{a:#x} x{n}")).collect::<Vec<_>>().join("  "));
    }
    Ok(())
}

/// `atlas --convertzone qalodhash GAME_FOLDER ZONE`: per lod: model name, surfaces name, the word at lod+12.
pub fn qalodhash(args: &[String]) -> Result<()> {
    let root = Path::new(args.first().context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let zone = Zone::open(Path::new(args.get(1).context("zone")?), &key)?;
    let mut r = Replay::new(&zone).map_err(|f| anyhow::anyhow!("{} at {}", f.what, f.at))?;
    for i in 0..zone.assets.len() {
        r.replay_asset(&zone, i).map_err(|f| anyhow::anyhow!("asset {i}: {} at {:#x}", f.what, f.at))?;
    }
    for m in &r.rec.models {
        for i in 0..4 {
            let s = m.lod_start[i];
            let sn = if s >= 0 { r.rec.surfs.get(&(s as usize)).cloned().flatten() } else { None };
            println!("{}\t{}\t{:?}\t{:08x}", m.name.clone().unwrap_or_default(), i, sn, u32_at(&m.lod[i], 12));
        }
    }
    Ok(())
}

/// `atlas --convertzone qaraw GAME_FOLDER ZONE TYPE [N] [BYTES]`: the first bytes of the first N directory entries of a type.
pub fn qaraw(args: &[String]) -> Result<()> {
    let root = Path::new(args.first().context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let zone = Zone::open(Path::new(args.get(1).context("zone")?), &key)?;
    let ty: u32 = args.get(2).context("type")?.parse()?;
    let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);
    let bytes: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(64);
    let mut shown = 0;
    for (i, a) in zone.assets.iter().enumerate() {
        if a.asset_type == ty {
            let end = (a.start + bytes).min(a.start + a.extent);
            println!("#{i} ref {:#x} extent {}: {}", a.reference, a.extent, hex(&zone.data[a.start..end]));
            shown += 1;
            if shown >= n {
                break;
            }
        }
    }
    Ok(())
}

/// `atlas --convertzone qapreset GAME_FOLDER ZONE...`: word histograms of the physics presets that end a model (no collision map
/// after them), found from the end of the model's extent.
pub fn qapreset(args: &[String]) -> Result<()> {
    use std::collections::BTreeMap;
    let root = Path::new(args.first().context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let mut h: BTreeMap<usize, BTreeMap<u32, usize>> = BTreeMap::new();
    let mut seen = 0usize;
    for z in &args[1..] {
        let zone = Zone::open(Path::new(z), &key)?;
        for a in zone.assets.iter().filter(|a| a.asset_type == 4) {
            let d = &zone.data[a.start..a.start + a.extent];
            if d.len() < 360 + 68 || u32_at(d, 0x160) != u32::MAX - 1 || u32_at(d, 0x164) != 0 {
                continue;
            }
            // header (68) + name\0 + prefix\0 up to the end
            let end = d.len();
            for p in (360..end - 68).rev() {
                if u32_at(d, p) != u32::MAX {
                    continue;
                }
                let mut q = p + 68;
                let Some(n) = d[q..].iter().position(|&c| c == 0) else { continue };
                q += n + 1;
                let has_prefix = u32_at(d, p + 28) == u32::MAX;
                if has_prefix {
                    let Some(n) = d[q..].iter().position(|&c| c == 0) else { continue };
                    q += n + 1;
                }
                if q == end {
                    seen += 1;
                    for w in 1..17 {
                        *h.entry(4 * w).or_default().entry(u32_at(d, p + 4 * w)).or_default() += 1;
                    }
                    break;
                }
            }
        }
    }
    println!("{seen} presets");
    for (k, m) in h {
        let mut v: Vec<_> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        println!("+{k:2} {} distinct; {}", v.len(), v.iter().take(6).map(|(a, n)| format!("{a:#x} x{n}")).collect::<Vec<_>>().join("  "));
    }
    Ok(())
}

/// `atlas --convertzone qamodelrefs GAME_FOLDER ZONE...`: how many models carry a physics preset / collision map pointer, by value.
pub fn qamodelrefs(args: &[String]) -> Result<()> {
    use std::collections::BTreeMap;
    let root = Path::new(args.first().context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let mut h: BTreeMap<(u32, u32), usize> = BTreeMap::new();
    for z in &args[1..] {
        let zone = Zone::open(Path::new(z), &key)?;
        for a in zone.assets.iter().filter(|a| a.asset_type == 4) {
            let d = &zone.data[a.start..a.start + a.extent];
            if d.len() >= 360 {
                *h.entry((u32_at(d, 0x160), u32_at(d, 0x164))).or_default() += 1;
            }
        }
    }
    for ((a, b), n) in h {
        println!("preset {a:#x} collmap {b:#x}: {n}");
    }
    Ok(())
}

/// `atlas --convertzone stocksurfs ZONE`: are model-surface records shared between stock models? (lods naming the same id)
pub fn stocksurfs(args: &[String]) -> Result<()> {
    use super::convert_zone::Src;
    use super::convert_walk::{walk_with, Game, View, ZoneImage};
    use std::collections::HashMap;
    let raw = std::fs::read(args.first().context("zone")?)?;
    let zone = ZoneImage::open(Game::Iw4, "x", &raw)?;
    let mut names: HashMap<u32, String> = HashMap::new();
    let mut users: HashMap<u32, Vec<(String, u32)>> = HashMap::new();
    let mut lods = 0usize;
    let mut hook = |v: &View, name: &str, hdr: u32, id: u32| {
        let s = Src { v, hdr };
        if name == "XModelSurfs" {
            names.insert(id, s.cstr(0).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default());
        }
        if name != "XModel" {
            return;
        }
        let model = s.cstr(0).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        for i in 0..4u32 {
            let l = s.sub(64 + 44 * i);
            if l.has(8) {
                lods += 1;
                if let Some(id) = l.asset(8) {
                    users.entry(id).or_default().push((model.clone(), i));
                }
            }
        }
    };
    walk_with(Game::Iw4, &zone, Some(&mut hook));
    let shared: Vec<_> = users.iter().filter(|(_, v)| v.len() > 1).collect();
    println!("{lods} lod surfaces, {} records, {} used by several lods", names.len(), shared.len());
    for (id, v) in shared.iter().take(8) {
        println!("  {:?} {:?}", names.get(id), v);
    }
    Ok(())
}

/// `atlas --convertzone cmp STOCK_ZONE GAME_FOLDER QA_ZONE TYPE HEADER_BYTES NAME_OFFSET`: converts the stock zone and compares the
/// header (first HEADER_BYTES) of every converted asset of TYPE with the asset of the same name in the QA zone, word by word
/// (words holding an address are treated as equal). Prints, per word offset, how often it differs, plus whether the whole stream
/// has the same length.
pub fn cmp(args: &[String]) -> Result<()> {
    use super::convert_walk::{Game, ZoneImage};
    use std::collections::BTreeMap;
    let raw = std::fs::read(args.first().context("stock zone")?)?;
    let stock = ZoneImage::open(Game::Iw4, "x", &raw)?;
    let root = Path::new(args.get(1).context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let zone = Zone::open(Path::new(args.get(2).context("qa zone")?), &key)?;
    let ty: u32 = args.get(3).context("type")?.parse()?;
    let hsize: usize = args.get(4).context("header bytes")?.parse()?;
    let name_off: usize = args.get(5).context("name offset")?.parse()?;
    let (frags, _) = super::convert_zone::convert_frags(&stock)?;
    let mut qa: std::collections::HashMap<Vec<u8>, &[u8]> = std::collections::HashMap::new();
    for a in zone.assets.iter().filter(|a| a.asset_type == ty) {
        let d = &zone.data[a.start..a.start + a.extent];
        if d.len() < hsize {
            continue;
        }
        let n = if u32_at(d, name_off) == u32::MAX {
            let r = &d[hsize..];
            r[..r.iter().position(|&c| c == 0).unwrap_or(0)].to_vec()
        } else {
            continue;
        };
        qa.entry(n).or_insert(d);
    }
    let norm = |w: u32| if w >= 0x1000_0000 && w < 0xffff_fffe { 0xAAAA_AAAA } else { w };
    let mut diffs: BTreeMap<usize, (usize, Vec<(u32, u32)>)> = BTreeMap::new();
    let (mut pairs, mut same_len) = (0usize, 0usize);
    for f in frags.iter().filter(|f| f.ty == ty) {
        let Some(d) = qa.get(f.name.as_bytes()) else { continue };
        pairs += 1;
        if d.len() == f.bytes.len() {
            same_len += 1;
        }
        for w in 0..hsize / 4 {
            let (a, b) = (norm(u32_at(&f.bytes, 4 * w)), norm(u32_at(d, 4 * w)));
            if a != b {
                let e = diffs.entry(4 * w).or_default();
                e.0 += 1;
                if e.1.len() < 3 {
                    e.1.push((a, b));
                }
            }
        }
    }
    if ty == 4 {
        // the converted surface records against the QA streams: where they differ from the first record with the same name
        let (mut exact, mut found, mut total) = (0usize, 0usize, 0usize);
        for blob in super::convert_model::all_surfs() {
            let name_end = 48 + blob[48..].iter().position(|&c| c == 0).unwrap_or(0);
            let name = &blob[48..name_end];
            if name.is_empty() {
                continue;
            }
            total += 1;
            // QA record: header, name string
            for a in zone.assets.iter().filter(|a| a.asset_type == 4) {
                let d = &zone.data[a.start..a.start + a.extent];
                let Some(p) = d.windows(name.len() + 1).position(|w| &w[..name.len()] == name && w[name.len()] == 0) else { continue };
                if p < 48 {
                    continue;
                }
                found += 1;
                let q = &d[p - 48..];
                let n = blob.len().min(q.len());
                let first = (0..n).find(|&i| blob[i] != q[i]);
                match first {
                    None if blob.len() <= q.len() => exact += 1,
                    f => {
                        if found - exact <= 6 {
                            println!("  surfs {:?}: first difference at {:?} (mine {} bytes)", String::from_utf8_lossy(name), f, blob.len());
                            if let Some(i) = f {
                                println!("     mine {}", hex(&blob[i.saturating_sub(8)..(i + 24).min(blob.len())]));
                                println!("     qa   {}", hex(&q[i.saturating_sub(8)..(i + 24).min(q.len())]));
                            }
                        }
                    }
                }
                break;
            }
        }
        println!("surfaces: {total} records, {found} found by name in the QA zone, {exact} byte-identical");
    }
    if ty == 30 {
        // element tables
        let mut ediff: BTreeMap<usize, (usize, Vec<(u32, u32)>)> = BTreeMap::new();
        let mut total_delta: BTreeMap<i64, usize> = BTreeMap::new();
        let (mut elems, mut tail_same, mut tail_diff) = (0usize, 0usize, 0usize);
        for f in frags.iter().filter(|f| f.ty == ty) {
            let Some(d) = qa.get(f.name.as_bytes()) else { continue };
            let count: usize = (0..3).map(|k| u32_at(&f.bytes, 16 + 4 * k) as usize).sum();
            let (s0, s1) = (32 + f.name.len() + 1, 32 + f.name.len() + 1);
            if count != (0..3).map(|k| u32_at(d, 16 + 4 * k) as usize).sum::<usize>() || f.bytes.len() < s0 + 260 * count || d.len() < s1 + 260 * count {
                continue;
            }
            for i in 0..count {
                elems += 1;
                for w in 0..65 {
                    let (a, b) = (norm(u32_at(&f.bytes, s0 + 260 * i + 4 * w)), norm(u32_at(d, s1 + 260 * i + 4 * w)));
                    if a != b {
                        let e = ediff.entry(4 * w).or_default();
                        e.0 += 1;
                        if e.1.len() < 3 {
                            e.1.push((a, b));
                        }
                    }
                }
            }
            {
                let mut vis_total = 0i64;
                for i in 0..count {
                    let m = &f.bytes[s0 + 260 * i..s0 + 260 * (i + 1)];
                    if u32_at(m, 184) != 0 {
                        vis_total += m[179] as i64 + 1;
                    }
                }
                let delta = u32_at(d, 8) as i64 - u32_at(&f.bytes, 8) as i64;
                *total_delta.entry(delta - (8 * count as i64 + 16 * vis_total)).or_default() += 1;
                if std::env::var("FXFIT").is_ok() {
                    println!("FIT {count} {vis_total} {delta} {}", u32_at(&f.bytes, 8));
                }
            }
            // the bytes behind the table (samples, visuals, strings)
            let (ta, tb) = (&f.bytes[s0 + 260 * count..], &d[s1 + 260 * count..]);
            if ta.len() == tb.len() && ta.iter().zip(tb.iter()).all(|(x, y)| x == y) {
                tail_same += 1;
            } else {
                tail_diff += 1;
                if tail_diff <= 3 {
                    let k = ta.iter().zip(tb.iter()).position(|(x, y)| x != y).unwrap_or(ta.len().min(tb.len()));
                    println!("  {}: behind the element table: mine {} bytes, QA {} bytes, first difference at {k}", f.name, ta.len(), tb.len());
                    println!("     mine {}", hex(&ta[k.saturating_sub(16)..(k + 48).min(ta.len())]));
                    println!("     qa   {}", hex(&tb[k.saturating_sub(16)..(k + 48).min(tb.len())]));
                }
            }
        }
        println!("{elems} elements compared; behind the tables {tail_same} identical, {tail_diff} different");
        println!("totalSize: actual delta minus (8 per element + 16 per visual sample): {:?}", total_delta);
        for (off, (n, ex)) in ediff {
            println!("  element +{off:3}: {n} differ   e.g. (mine, QA) {}", ex.iter().map(|(a, b)| format!("{a:08x}/{b:08x}")).collect::<Vec<_>>().join(" "));
        }
    }
    println!("{pairs} pairs by name, {same_len} with equal stream length");
    for (off, (n, ex)) in diffs {
        println!("  +{off:3}: {n} differ   e.g. (mine, QA) {}", ex.iter().map(|(a, b)| format!("{a:08x}/{b:08x}")).collect::<Vec<_>>().join(" "));
    }
    Ok(())
}

/// `atlas --convertzone fxsamples STOCK_ZONE GAME_FOLDER QA_ZONE [N]`: for the first N effects of the same name that have the same
/// element counts, prints the first element's visual-state samples (mine and QA) and the elements' flags and types.
pub fn fxsamples(args: &[String]) -> Result<()> {
    use super::convert_walk::{Game, ZoneImage};
    use std::collections::HashMap;
    let raw = std::fs::read(args.first().context("stock zone")?)?;
    let stock = ZoneImage::open(Game::Iw4, "x", &raw)?;
    let root = Path::new(args.get(1).context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let zone = Zone::open(Path::new(args.get(2).context("qa zone")?), &key)?;
    let limit: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);
    let (frags, _) = super::convert_zone::convert_frags(&stock)?;
    let mut qa: HashMap<Vec<u8>, &[u8]> = HashMap::new();
    for a in zone.assets.iter().filter(|a| a.asset_type == 30) {
        let d = &zone.data[a.start..a.start + a.extent];
        if u32_at(d, 0) == u32::MAX {
            let r = &d[32..];
            qa.entry(r[..r.iter().position(|&c| c == 0).unwrap_or(0)].to_vec()).or_insert(d);
        }
    }
    let mut shown = 0;
    let mut types: std::collections::BTreeMap<(u8, u8), usize> = std::collections::BTreeMap::new();
    let mut flag_pairs: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    let mut co: std::collections::BTreeMap<(u32, u32), usize> = std::collections::BTreeMap::new();
    let mut seen: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    let mut flag_examples: Vec<(u8, u32, u32)> = Vec::new();
    let mut extra: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for f in frags.iter().filter(|f| f.ty == 30) {
        let Some(d) = qa.get(f.name.as_bytes()) else { continue };
        let count: usize = (0..3).map(|k| u32_at(&f.bytes, 16 + 4 * k) as usize).sum();
        if count != (0..3).map(|k| u32_at(d, 16 + 4 * k) as usize).sum::<usize>() {
            continue;
        }
        let s0 = 32 + f.name.len() + 1;
        for i in 0..count {
            let (m, q) = (&f.bytes[s0 + 260 * i..s0 + 260 * (i + 1)], &d[s0 + 260 * i..s0 + 260 * (i + 1)]);
            *types.entry((m[176], q[176])).or_default() += 1;
            *flag_pairs.entry(u32_at(m, 0) ^ u32_at(q, 0)).or_default() += 1;
            for sb in 0..32 {
                for qb in 0..32 {
                    if (u32_at(m, 0) >> sb) & 1 == 1 && (u32_at(q, 0) >> qb) & 1 == 1 {
                        *co.entry((sb, qb)).or_default() += 1;
                    }
                }
                if (u32_at(m, 0) >> sb) & 1 == 1 {
                    *seen.entry(sb).or_default() += 1;
                }
            }
            if u32_at(m, 0) != u32_at(q, 0) && flag_examples.len() < 14 {
                flag_examples.push((m[176], u32_at(m, 0), u32_at(q, 0)));
            }
        }
        // the extra words of QA visual-state samples, by element type (words 6, 7, 14, 15 of each 64-byte sample)
        {
            let mut off = s0 + 260 * count;
            for i in 0..count {
                let m = &f.bytes[s0 + 260 * i..s0 + 260 * (i + 1)];
                let vel = if u32_at(m, 180) != 0 { m[178] as usize + 1 } else { 0 };
                let vis = if u32_at(m, 184) != 0 { m[179] as usize + 1 } else { 0 };
                off += 96 * vel;
                if off + 64 * vis > d.len() {
                    break;
                }
                for k in 0..vis {
                    let q = &d[off + 64 * k..off + 64 * (k + 1)];
                    let key = format!("type {:2} extra {:08x} {:08x} / {:08x} {:08x}", m[176], u32_at(q, 24), u32_at(q, 28), u32_at(q, 56), u32_at(q, 60));
                    *extra.entry(key).or_default() += 1;
                }
                off += 64 * vis;
                break; // only the first element: the visuals and strings that follow vary in size
            }
        }
        if shown < limit && count == 1 {
            let (m, q) = (&f.bytes[s0..s0 + 260], &d[s0..s0 + 260]);
            let vel = m[178] as usize + 1;
            let vis = m[179] as usize + 1;
            let off = s0 + 260 + 96 * vel;
            println!("{}: type {} vel {} vis {} visual {:?}", f.name, m[176], vel, vis, hex(&m[177..178]));
            for k in 0..vis {
                println!("   mine {}", hex(&f.bytes[off + 64 * k..off + 64 * (k + 1)]));
                println!("   qa   {}", hex(&d[off + 64 * k..off + 64 * (k + 1)]));
            }
            shown += 1;
        }
    }
    println!("element types (mine, QA): {types:?}");
    println!("flag xor: {:x?}", flag_pairs);
    for (k, n) in &extra {
        println!("  {k}: {n}");
    }
    for (sb, n) in &seen {
        let best: Vec<String> = (0..32u32).filter_map(|qb| co.get(&(*sb, qb)).filter(|&&c| c * 10 >= *n * 2).map(|c| format!("q{qb}:{c}"))).collect();
        println!("stock bit {sb:2} (set in {n}): {}", best.join(" "));
    }
    for (t, a, b) in flag_examples {
        println!("  stock type {t} flags {a:08x} -> QA {b:08x}");
    }
    Ok(())
}

/// `atlas --convertzone impactmask STOCK_ZONE`: per record of the stock impact table, which of the 35 effect references are set.
pub fn impactmask(args: &[String]) -> Result<()> {
    use super::convert_walk::{walk_with, Game, View, ZoneImage};
    let raw = std::fs::read(args.first().context("zone")?)?;
    let zone = ZoneImage::open(Game::Iw4, "x", &raw)?;
    let mut hook = |v: &View, name: &str, hdr: u32, _id: u32| {
        if name != "FxImpactTable" {
            return;
        }
        let s = super::convert_zone::Src { v, hdr };
        println!("table {:?}", s.cstr(0).map(|b| String::from_utf8_lossy(&b).into_owned()));
        let base = s.u32(4);
        for r in 0..15u32 {
            let row: String = (0..35u32).map(|k| if s.at(base + 140 * r + 4 * k).has(0) { '1' } else { '0' }).collect();
            println!("  {r:2} {row}");
        }
    };
    walk_with(Game::Iw4, &zone, Some(&mut hook));
    Ok(())
}

/// `atlas --convertzone qaclip GAME_FOLDER ZONE`: the block-3 allocations of the collision map asset, in order (address, length).
pub fn qaclip(args: &[String]) -> Result<()> {
    let root = Path::new(args.first().context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let zone = Zone::open(Path::new(args.get(1).context("zone")?), &key)?;
    let mut r = Replay::new(&zone).map_err(|f| anyhow::anyhow!("{} at {}", f.what, f.at))?;
    r.enable_tracing();
    let target = zone.assets.iter().position(|a| a.asset_type == 15).context("no collision map")?;
    for i in 0..zone.assets.len() {
        r.replay_asset(&zone, i).map_err(|f| anyhow::anyhow!("asset {i}: {} at {:#x}", f.what, f.at))?;
    }
    let a = zone.assets[target];
    println!("asset {target}: stream {:#x}, extent {}", a.start, a.extent);
    let allocs = r.block3_allocations();
    let info = r.block3_allocation_info();
    for (i, &(asset, address, length)) in allocs.iter().enumerate() {
        if asset == target {
            println!("  {address:08x} len {length:8} stream {:#x} read {}", info[i].0, info[i].1);
            if (length == 64 || length == 36 || length == 52) && info[i].1 {
                let w: Vec<String> = (0..(length / 4).min(16)).map(|k| format!("{:x}", u32_at(&zone.data, info[i].0 + 4 * k))).collect();
                println!("      words {}", w.join(" "));
            }
        }
    }
    Ok(())
}

/// `atlas --convertzone cmp1 STOCK_ZONE GAME_FOLDER QA_ZONE TYPE HEADER_BYTES`: like `cmp` for the first asset of the type in each
/// zone (for maps that exist in both games), printing every differing header word (addresses treated as equal).
pub fn cmp1(args: &[String]) -> Result<()> {
    use super::convert_walk::{Game, ZoneImage};
    let raw = std::fs::read(args.first().context("stock zone")?)?;
    let stock = ZoneImage::open(Game::Iw4, "x", &raw)?;
    let root = Path::new(args.get(1).context("game folder")?);
    let key: ArchiveKey = super::export::find_key(root)?;
    let zone = Zone::open(Path::new(args.get(2).context("qa zone")?), &key)?;
    let ty: u32 = args.get(3).context("type")?.parse()?;
    let hsize: usize = args.get(4).context("header bytes")?.parse()?;
    let (frags, _) = super::convert_zone::convert_frags(&stock)?;
    let f = frags.iter().find(|f| f.ty == ty).context("no converted asset of the type")?;
    let a = zone.assets.iter().find(|a| a.asset_type == ty).context("no QA asset of the type")?;
    let d = &zone.data[a.start..a.start + a.extent];
    let norm = |w: u32| if w >= 0x1000_0000 && w < 0xffff_fffe { 0xAAAA_AAAA } else { w };
    println!("mine {} bytes, QA {} bytes", f.bytes.len(), d.len());
    for w in 0..hsize / 4 {
        let (x, y) = (u32_at(&f.bytes, 4 * w), u32_at(d, 4 * w));
        if norm(x) != norm(y) {
            println!("  +{:4}: mine {x:08x} ({}) QA {y:08x} ({})", 4 * w, x, y);
        }
    }
    Ok(())
}
