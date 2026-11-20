//! The `MAP_const.ff` zone of Call of Duty Online (QA 3.3.3.3, version 423), which the loader asks for beside `MAP.ff`.
//!
//! What it is (read from the 65 stock `*_const.ff` zones of the QA folder, and the QA executable):
//!
//! * A zone of raw files only (asset type 36), no script strings. The raw files are the map's netcode baselines,
//!   `mp/constbaselines/bl_pc_<zone>_<mode>.bin`, one per mode, then one marker raw file named `<zone>_const` last.
//!   `<zone>` is the zone's own name (`mp_afghan`, `coop_evac_mp_suburbs`, `zm_sumpf_sh`).
//! * Which modes a zone has is the row of the stock string table `mp/constBaselinesTable.csv` (an asset of the common
//!   zones) for that zone name, in the row's order. A zone with no row, or an empty row (`mp_con_spring`), has the marker only.
//! * A baseline file is `IWCB000\x01`, then one 272-byte entity state (the netcode's baseline record) for each entity the
//!   server holds when the match starts, entity number first, zlib-compressed. It is a snapshot of a running server's game
//!   scripts for that map and mode, so it cannot be derived from the map's zone. The marker is empty, except in
//!   `mp_hunted_spring` and `mp_suburbs`, where it holds the zone builder's log ("failed loading 'mp/constbaselines/...'
//!   of type 'rawfile'"): those two zones were built without their baselines.
//! * The executable (`sub_584880` / `sub_5849F0` in the QA build) looks the zone up in that table first; only when a row
//!   exists does it ask for the baseline raw file, and a missing or malformed one leaves the baseline count at zero (the
//!   netcode then deltas from zero). A custom map has no row, so it never reads a baseline; its `_const` zone is the marker
//!   alone, which is also what the stock `mp_con_spring_const` is.
//!
//! The image layout (checked on all 65 zones): the 0x28-byte header (size, 0, block sizes: block 0 = 0x20, block 3 = the sum
//! of the asset extents, the rest 0), then string count 0, 0, the asset count, 0xffffffff, a directory of (36, offset,
//! 0xffffffff, extent) and, per asset, the 16-byte raw file header (-1, compressed length, length, -1), the NUL-terminated
//! name, and the data (`compressed` bytes, or `length + 1` when the file is not compressed).

use super::{Zone, u32_at};
use anyhow::{Result, bail};

pub const RAWFILE: u32 = 36;

/// One raw file of the zone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawFile {
    pub name: String,
    /// Length of the zlib stream in `data`, 0 when the file is stored plain.
    pub compressed: u32,
    /// Length of the file's own bytes.
    pub length: u32,
    /// What follows the name in the zone.
    pub data: Vec<u8>,
}

impl RawFile {
    /// The marker: a raw file with no content.
    pub fn marker(zone: &str) -> RawFile {
        RawFile { name: marker_name(zone), compressed: 0, length: 0, data: vec![0] }
    }
}

pub fn marker_name(zone: &str) -> String {
    format!("{zone}_const")
}

pub fn baseline_name(zone: &str, mode: &str) -> String {
    format!("mp/constbaselines/bl_pc_{zone}_{mode}.bin")
}

/// The modes of `zone` in `mp/constBaselinesTable.csv` (the row's non-empty cells, in order); `None` when the table has no row for it.
pub fn table_row(csv: &str, zone: &str) -> Option<Vec<String>> {
    csv.lines().find_map(|l| {
        let mut cells = l.trim_end_matches('\r').split(',');
        (cells.next()? == zone).then(|| cells.filter(|c| !c.is_empty()).map(str::to_string).collect())
    })
}

/// The zone name a `_const` zone file is for (`mp_afghan_const` -> `mp_afghan`).
pub fn zone_of(file_stem: &str) -> Option<&str> {
    file_stem.strip_suffix("_const").filter(|z| !z.is_empty())
}

/// Lays the raw files out as a zone image.
pub fn image(files: &[RawFile]) -> Vec<u8> {
    let bodies: Vec<Vec<u8>> = files
        .iter()
        .map(|f| {
            let mut b = Vec::with_capacity(16 + f.name.len() + 1 + f.data.len());
            for w in [u32::MAX, f.compressed, f.length, u32::MAX] {
                b.extend_from_slice(&w.to_le_bytes());
            }
            b.extend_from_slice(f.name.as_bytes());
            b.push(0);
            b.extend_from_slice(&f.data);
            b
        })
        .collect();
    let extents: u32 = bodies.iter().map(|b| b.len() as u32).sum();
    let mut out = vec![0u8; 8];
    for s in [0x20u32, 0, 0, extents, 0, 0, 0, 0] {
        out.extend_from_slice(&s.to_le_bytes());
    }
    for w in [0u32, 0, files.len() as u32, u32::MAX] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    let mut offset = 16 * files.len() as u32 + 0x10;
    for b in &bodies {
        for w in [RAWFILE, offset, u32::MAX, b.len() as u32] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        offset += b.len() as u32;
    }
    for b in &bodies {
        out.extend_from_slice(b);
    }
    let size = (out.len() - 0x28) as u32;
    out[0..4].copy_from_slice(&size.to_le_bytes());
    out
}

/// The zone image of a map that has no baselines: the marker alone (what the stock `mp_con_spring_const` is).
pub fn marker_only(zone: &str) -> Vec<u8> {
    image(&[RawFile::marker(zone)])
}

/// Reads the raw files of a zone image that holds nothing else.
pub fn parse(image: &[u8]) -> Result<Vec<RawFile>> {
    let z = Zone::from_image(super::fastfile::VERSION_QA, image.to_vec())?;
    if z.string_count != 0 {
        bail!("a const zone has no script strings, this one has {}", z.string_count);
    }
    let mut out = Vec::new();
    for (i, a) in z.assets.iter().enumerate() {
        if a.asset_type != RAWFILE || a.reference != u32::MAX {
            bail!("asset {i}: type {} reference {:#x}, a const zone holds inline raw files only", a.asset_type, a.reference);
        }
        let b = &z.data[a.start..a.start + a.extent];
        if b.len() < 17 || u32_at(b, 0) != u32::MAX || u32_at(b, 12) != u32::MAX {
            bail!("asset {i}: not a raw file header");
        }
        let (compressed, length) = (u32_at(b, 4), u32_at(b, 8));
        let Some(n) = b[16..].iter().position(|&c| c == 0) else { bail!("asset {i}: unterminated name") };
        let data = b[16 + n + 1..].to_vec();
        let want = if compressed > 0 { compressed as usize } else { length as usize + 1 };
        if data.len() != want {
            bail!("asset {i}: {} data bytes, the header says {want}", data.len());
        }
        out.push(RawFile { name: String::from_utf8_lossy(&b[16..16 + n]).into_owned(), compressed, length, data });
    }
    Ok(out)
}

/// Whether the raw files follow the rule of a stock zone: the baselines of the zone's row of the table, in its order, then the
/// marker. `table` is the text of `mp/constBaselinesTable.csv` (without it only the shape is checked).
pub fn check(zone: &str, files: &[RawFile], table: Option<&str>) -> Result<(), String> {
    let Some((marker, baselines)) = files.split_last() else { return Err("no marker".into()) };
    if marker.name != marker_name(zone) {
        return Err(format!("last raw file is {:?}, not {:?}", marker.name, marker_name(zone)));
    }
    let names: Vec<&str> = baselines.iter().map(|f| f.name.as_str()).collect();
    if let Some(t) = table {
        let want: Vec<String> = table_row(t, zone).unwrap_or_default().iter().map(|m| baseline_name(zone, m)).collect();
        // a row whose baselines were not built leaves the marker alone (the zone builder logged it in the marker)
        if names != want && !names.is_empty() {
            return Err(format!("baselines {names:?}, the table row gives {want:?}"));
        }
    } else if names.iter().any(|n| !n.starts_with(&format!("mp/constbaselines/bl_pc_{zone}_"))) {
        return Err(format!("baselines {names:?} are not of {zone}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "mp_a,dd,dm,,\r\nmp_empty,,,,\ncoop_x_mp_a,coop,,,\n";

    fn baseline(zone: &str, mode: &str, n: usize) -> RawFile {
        RawFile { name: baseline_name(zone, mode), compressed: n as u32, length: 8 * n as u32, data: vec![7; n] }
    }

    #[test]
    fn the_table_row_gives_the_modes_in_order() {
        assert_eq!(table_row(TABLE, "mp_a").unwrap(), ["dd", "dm"]);
        assert!(table_row(TABLE, "mp_empty").unwrap().is_empty());
        assert_eq!(table_row(TABLE, "coop_x_mp_a").unwrap(), ["coop"]);
        assert!(table_row(TABLE, "mp_b").is_none());
        assert_eq!(zone_of("mp_a_const"), Some("mp_a"));
        assert_eq!(zone_of("mp_a"), None);
    }

    #[test]
    fn the_marker_only_zone_is_a_valid_one_raw_file_zone() {
        let img = marker_only("mp_custom");
        // 0x28 header + 16 prefix + 16 directory + 16 header + "mp_custom_const\0" + one byte
        assert_eq!(img.len(), 0x28 + 16 + 16 + 16 + 16 + 1);
        assert_eq!(&img[8..12], &0x20u32.to_le_bytes());
        assert_eq!(&img[20..24], &(16u32 + 16 + 1).to_le_bytes());
        let z = Zone::from_image(super::super::fastfile::VERSION_QA, img.clone()).unwrap();
        assert_eq!((z.assets.len(), z.assets[0].asset_type), (1, 36));
        let files = parse(&img).unwrap();
        assert_eq!(files, [RawFile::marker("mp_custom")]);
        // the QA walker reads it exactly, with the header's block sizes
        let r = super::super::convert_cli::verify(super::super::convert_cli::Fmt::Codol, "mp_custom_const", &img).unwrap();
        assert!(r.contains("exact"), "{r}");
    }

    #[test]
    fn baselines_come_before_the_marker_and_the_image_round_trips() {
        let files = vec![baseline("mp_a", "dd", 40), baseline("mp_a", "dm", 33), RawFile::marker("mp_a")];
        let img = image(&files);
        assert_eq!(parse(&img).unwrap(), files);
        assert_eq!(image(&parse(&img).unwrap()), img);
        assert!(super::super::convert_cli::verify(super::super::convert_cli::Fmt::Codol, "mp_a_const", &img).is_ok());
        assert_eq!(check("mp_a", &files, Some(TABLE)), Ok(()));
        assert_eq!(check("mp_a", &files, None), Ok(()));
    }

    #[test]
    fn the_rule_rejects_wrong_names_and_order() {
        let ok = vec![baseline("mp_a", "dd", 4), baseline("mp_a", "dm", 4), RawFile::marker("mp_a")];
        assert!(check("mp_a", &ok, Some(TABLE)).is_ok());
        let swapped = vec![ok[1].clone(), ok[0].clone(), ok[2].clone()];
        assert!(check("mp_a", &swapped, Some(TABLE)).unwrap_err().contains("table row"));
        assert!(check("mp_a", &ok[..2], Some(TABLE)).unwrap_err().contains("last raw file"));
        assert!(check("mp_b", &ok, Some(TABLE)).is_err());
        // no row and no baselines: the marker alone is the rule
        assert!(check("mp_custom", &[RawFile::marker("mp_custom")], Some(TABLE)).is_ok());
        assert!(check("mp_custom", &ok, Some(TABLE)).is_err());
    }

    #[test]
    fn foreign_assets_are_refused() {
        let mut img = marker_only("mp_a");
        img[0x38..0x3c].copy_from_slice(&5u32.to_le_bytes());
        assert!(parse(&img).is_err());
    }
}
