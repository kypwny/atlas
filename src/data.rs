//! Game data as tables. The zones carry more than geometry: string tables (weapon stats, perks, class setups ...),
//! localised strings (display names), raw files (scripts, configs, menus). This module collects them from every zone of a
//! game folder, CoDOL or stock Modern Warfare 2, and writes them as CSV / JSON / files, the way a data compiler for a game
//! database would need them (the job EthanC's Hyde does for modern Call of Duty from Jekyll dumps).

use crate::zone::export::{find_key, zone_files};
use crate::zone::replay::{DataRec, Replay};
use crate::zone::{Zone, fastfile, stock};
use anyhow::{Context, Result, bail};
use flate2::read::ZlibDecoder;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// One string table, as it is in the game: a grid of text cells.
#[derive(Debug, Clone)]
pub struct Table {
    pub name: String,
    pub zone: String,
    pub columns: usize,
    pub rows: usize,
    pub cells: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RawFile {
    pub name: String,
    pub zone: String,
    pub bytes: Vec<u8>,
}

/// The weapon fields the tables know: IW4 name, type and offsets in each game's record (from `tools/data/weapon_fields.tsv`, made by
/// aligning IW4, QA and 3.19 records weapon by weapon: see docs).
const WEAPON_FIELDS: &str = include_str!("../tools/data/weapon_fields.tsv");

struct FieldDef {
    name: String,
    ty: String,
    /// Offset in the record of each layout: IW4 (1,784 bytes), QA (3,624), 3.19 (4,124).
    offsets: [Option<usize>; 3],
}

fn field_defs() -> Vec<FieldDef> {
    WEAPON_FIELDS
        .lines()
        .skip(1)
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            if c.len() < 7 {
                return None;
            }
            let o = |s: &str| s.parse::<usize>().ok();
            Some(FieldDef { name: c[1].to_string(), ty: c[2].to_string(), offsets: [o(c[0]), o(c[3]), o(c[5])] })
        })
        .collect()
}

/// A scalar read out of a record as text: `None` for pointers, arrays and offsets outside the record.
fn field_value(record: &[u8], offset: usize, ty: &str) -> Option<String> {
    if ty.ends_with('*') || ty.contains('[') {
        return None;
    }
    let at = |n: usize| record.get(offset..offset + n);
    let ty = ty.trim();
    if ty == "float" {
        let v = f32::from_le_bytes(at(4)?.try_into().ok()?);
        return Some(if v.is_finite() { format!("{v}") } else { String::new() });
    }
    if ty == "bool" || ty == "char" || ty == "unsigned char" {
        return Some(at(1)?[0].to_string());
    }
    if ty.contains("__int16") || ty == "short" || ty == "unsigned short" || ty == "uint16_t" {
        return Some(u16::from_le_bytes(at(2)?.try_into().ok()?).to_string());
    }
    // int, unsigned int, enums, bit fields' storage words
    Some(i32::from_le_bytes(at(4)?.try_into().ok()?).to_string())
}

/// The weapons of a game as two CSV tables: every field with a known place in the record, and a short summary of the stats that
/// matter. Which column a record layout uses is told by its size.
/// The weapons of a game as tables, one pair per record layout present (IW4, QA, 3.19; which one a record is told by its size):
/// every field with a known place in the record, and a short summary of the stats that matter. The first row is the header.
fn weapon_grids(weapons: &BTreeMap<String, Weapon>, localize: &BTreeMap<String, (String, String)>) -> Vec<(&'static str, Table, Table)> {
    const SUMMARY: [&str; 17] = [
        "weaponClass", "weapType", "inventoryType", "fireType", "iClipSize", "iFireTime", "iReloadTime", "iReloadEmptyTime", "iAdsTransInTime", "fAdsZoomFov", "playerDamage", "damage", "minDamage", "iStartAmmo", "iMaxAmmo", "moveSpeedScale", "iRaiseTime",
    ];
    const TAGS: [&str; 3] = ["iw4", "qa", "3.19"];
    let defs = field_defs();
    let mut out = Vec::new();
    for (kind, tag) in TAGS.iter().enumerate() {
        let layout = |len: usize| len == [1784usize, 3624, 4124][kind];
        let group: Vec<&Weapon> = weapons.values().filter(|w| layout(w.record.len())).collect();
        let Some(first) = group.first() else { continue };
        let columns: Vec<&FieldDef> = defs.iter().filter(|d| d.offsets[kind].is_some_and(|o| field_value(&first.record, o, &d.ty).is_some())).collect();
        let summary: Vec<&FieldDef> = columns.iter().copied().filter(|d| SUMMARY.contains(&d.name.as_str())).collect();
        let build = |cols: &[&FieldDef], name: &str| {
            let width = 4 + cols.len();
            let mut cells: Vec<String> = ["name", "display_key", "display_text", "zone"].iter().map(|s| s.to_string()).collect();
            cells.extend(cols.iter().map(|d| d.name.clone()));
            for w in &group {
                let text = localize.get(&w.display).map(|(t, _)| t.clone()).unwrap_or_default();
                cells.extend([w.name.clone(), w.display.clone(), text, w.zone.clone()]);
                cells.extend(cols.iter().map(|d| field_value(&w.record, d.offsets[kind].unwrap(), &d.ty).unwrap_or_default()));
            }
            Table { name: name.to_string(), zone: String::new(), columns: width, rows: 1 + group.len(), cells }
        };
        out.push((*tag, build(&columns, &format!("weapons ({tag})")), build(&summary, &format!("weapons summary ({tag})"))));
    }
    out
}

/// A weapon definition as stored in a zone.
#[derive(Debug, Clone)]
pub struct Weapon {
    pub name: String,
    /// The display-name key (looked up in the localised strings).
    pub display: String,
    pub record: Vec<u8>,
    pub zone: String,
}

/// Everything found in a game folder, each name kept once (the first zone that has it).
#[derive(Debug, Default)]
pub struct GameData {
    pub tables: BTreeMap<String, Table>,
    /// key -> (text, zone)
    pub localize: BTreeMap<String, (String, String)>,
    pub rawfiles: BTreeMap<String, RawFile>,
    /// (kind, name) -> header bytes of models, materials, technique sets, images and surfaces, for comparing layouts between games.
    pub headers: BTreeMap<(String, String), Vec<u8>>,
    /// internal name -> weapon; see `DataRec::Weapon` for the record layouts.
    pub weapons: BTreeMap<String, Weapon>,
    pub zones_read: usize,
    pub zones_failed: usize,
}

impl Table {
    pub fn cell(&self, row: usize, column: usize) -> &str {
        self.cells.get(row * self.columns + column).map(String::as_str).unwrap_or("")
    }

    /// RFC 4180 CSV.
    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        for r in 0..self.rows {
            for c in 0..self.columns {
                if c > 0 {
                    out.push(',');
                }
                let v = self.cell(r, c);
                if v.contains([',', '"', '\n', '\r']) {
                    out.push('"');
                    out.push_str(&v.replace('"', "\"\""));
                    out.push('"');
                } else {
                    out.push_str(v);
                }
            }
            out.push('\n');
        }
        out
    }
}

fn add(data: &mut GameData, zone: &str, recs: Vec<DataRec>) {
    for r in recs {
        match r {
            DataRec::StringTable { name, columns, rows, cells } => {
                let Some(name) = name else { continue };
                let cells = cells.into_iter().map(|c| c.unwrap_or_default()).collect();
                data.tables.entry(name.to_ascii_lowercase()).or_insert(Table { name, zone: zone.to_string(), columns, rows, cells });
            }
            DataRec::Localize { key, text } => {
                if let Some(key) = key {
                    data.localize.entry(key).or_insert((text.unwrap_or_default(), zone.to_string()));
                }
            }
            DataRec::Header { kind, name, bytes } => {
                if let Some(name) = name {
                    data.headers.entry((kind.to_string(), name)).or_insert(bytes);
                }
            }
            DataRec::Weapon { name, display, record } => {
                if let Some(name) = name {
                    data.weapons.entry(name.clone()).or_insert(Weapon { name, display: display.unwrap_or_default(), record, zone: zone.to_string() });
                }
            }
            DataRec::RawFile { name, compressed, length, bytes } => {
                let Some(name) = name else { continue };
                let bytes = if compressed {
                    let mut out = Vec::with_capacity(length);
                    if ZlibDecoder::new(&bytes[..]).read_to_end(&mut out).is_err() {
                        continue;
                    }
                    out
                } else {
                    // stored with a trailing NUL
                    bytes[..length.min(bytes.len())].to_vec()
                };
                data.rawfiles.entry(name.to_ascii_lowercase()).or_insert(RawFile { name, zone: zone.to_string(), bytes });
            }
        }
    }
}

/// The data records of one CoDOL zone.
pub fn zone_data(zone: &Zone) -> Vec<DataRec> {
    let Ok(mut replay) = Replay::new(zone) else { return Vec::new() };
    for i in 0..zone.assets.len() {
        let _ = replay.replay_asset(zone, i);
        // the 3.19 weapon layout is not walked; its record is cut out by the directory instead
        let e = zone.assets[i];
        if zone.version == fastfile::VERSION_319 && e.asset_type == 28 && e.reference >= 0xffff_fffe && e.extent >= WEAPON_319 {
            let blob = &zone.data[e.start..e.start + e.extent];
            let first = crate::zone::u32_at(blob, 0);
            // the inline strings follow the record in field order: internal name, then display name
            let mut at = WEAPON_319;
            let mut read = |word: u32| -> Option<String> {
                if word == u32::MAX {
                    let n = blob[at..].iter().position(|&c| c == 0)?;
                    let s = String::from_utf8_lossy(&blob[at..at + n]).into_owned();
                    at += n + 1;
                    Some(s)
                } else {
                    replay.pool_string(word)
                }
            };
            let name = read(first);
            let display = read(crate::zone::u32_at(blob, 4));
            replay.rec.data.push(DataRec::Weapon { name, display, record: blob[..WEAPON_319].to_vec() });
        }
    }
    std::mem::take(&mut replay.rec.data)
}

/// Size of the 3.19 weapon record.
pub const WEAPON_319: usize = 4124;

/// Collects the data of every zone under a game folder (CoDOL zones through the archive key, stock IW4 zones as they are).
pub fn collect(root: &Path, progress: Option<&(std::sync::atomic::AtomicUsize, std::sync::atomic::AtomicUsize)>) -> Result<GameData> {
    use std::sync::atomic::Ordering;
    let zones = zone_files(root);
    if zones.is_empty() {
        bail!("no fastfiles found under {}", root.display());
    }
    if let Some(p) = progress {
        p.1.store(zones.len(), Ordering::Relaxed);
    }
    let key = find_key(root).ok();
    let mut data = GameData::default();
    for path in &zones {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let read = (|| -> Result<Vec<DataRec>> {
            let peek = fastfile::peek(path)?;
            if peek.version == stock::VERSION_IW4_PC && !peek.big_endian {
                let (_, image) = stock::open(&std::fs::read(path)?)?;
                // the stock layouts are CoDOL 334's plus the IW4 ones; a zone that stops early still gives what came before
                return Ok(stock::probe_data(&image).unwrap_or_default());
            }
            let key = key.as_ref().context("no archive key")?;
            Ok(zone_data(&Zone::open(path, key)?))
        })();
        match read {
            Ok(recs) => {
                data.zones_read += 1;
                add(&mut data, &name, recs);
            }
            Err(_) => data.zones_failed += 1,
        }
        if let Some(p) = progress {
            p.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(data)
}

/// A game-relative path made safe to write under a folder: no drive, no `..`, forward slashes.
pub fn safe_path(name: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for c in Path::new(&name.replace('\\', "/")).components() {
        if let Component::Normal(p) = c {
            out.push(p);
        }
    }
    if out.as_os_str().is_empty() { PathBuf::from("unnamed") } else { out }
}

fn json_string(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

impl GameData {
    /// The weapon tables (full and summary) for each record layout found.
    pub fn weapon_grids(&self) -> Vec<(&'static str, Table, Table)> {
        weapon_grids(&self.weapons, &self.localize.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }

    /// Writes `stringtables/<name>` (CSV), `rawfiles/<name>`, `localize.csv`, `localize.json` and `index.json` under `out`.
    pub fn write(&self, out: &Path) -> Result<()> {
        for t in self.tables.values() {
            let mut p = out.join("stringtables").join(safe_path(&t.name));
            if p.extension().is_none() {
                p.set_extension("csv");
            }
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::write(&p, t.to_csv())?;
        }
        for f in self.rawfiles.values() {
            let p = out.join("rawfiles").join(safe_path(&f.name));
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::write(&p, &f.bytes)?;
        }
        let mut csv = String::from("key,text,zone\n");
        let mut json = String::from("{\n");
        for (i, (k, (text, zone))) in self.localize.iter().enumerate() {
            let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
            csv.push_str(&format!("{},{},{}\n", q(k), q(text), q(zone)));
            json.push_str(&format!("  {}: {}{}\n", json_string(k), json_string(text), if i + 1 < self.localize.len() { "," } else { "" }));
        }
        json.push('}');
        std::fs::create_dir_all(out)?;
        let grids = self.weapon_grids();
        for (tag, full, short) in &grids {
            // one layout per game folder in practice; a folder with several gets a suffix
            let suffix = if grids.len() > 1 { format!("_{}", tag.replace('.', "")) } else { String::new() };
            std::fs::write(out.join(format!("weapons{suffix}.csv")), full.to_csv())?;
            std::fs::write(out.join(format!("weapons_summary{suffix}.csv")), short.to_csv())?;
        }
        std::fs::write(out.join("localize.csv"), csv)?;
        std::fs::write(out.join("localize.json"), json)?;
        let index = format!(
            "{{\n  \"zones_read\": {},\n  \"zones_failed\": {},\n  \"stringtables\": {},\n  \"localized_strings\": {},\n  \"rawfiles\": {}\n}}\n",
            self.zones_read,
            self.zones_failed,
            self.tables.len(),
            self.localize.len(),
            self.rawfiles.len()
        );
        std::fs::write(out.join("index.json"), index)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes_what_needs_it() {
        let t = Table { name: "t".into(), zone: "z".into(), columns: 2, rows: 2, cells: vec!["a".into(), "b,c".into(), "d\"e".into(), "".into()] };
        assert_eq!(t.to_csv(), "a,\"b,c\"\n\"d\"\"e\",\n");
    }

    #[test]
    fn paths_cannot_leave_the_output_folder() {
        assert_eq!(safe_path("../../x\\y.csv"), PathBuf::from("x/y.csv"));
        assert_eq!(safe_path("C:\\a\\b"), PathBuf::from("a/b"));
        assert_eq!(safe_path(""), PathBuf::from("unnamed"));
    }
}
