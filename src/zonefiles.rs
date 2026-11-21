//! Unpacking a CoDOL zone into files and building it again: the `dumpzone` / `buildzone` workflow of ZoneTool, for the assets
//! this program can place (`zone::writer`). Every asset becomes `assets/<index>_<type>.bin`; map entities and raw files also
//! become plain text next to it, and an edited text file replaces its asset when the zone is built again. Assets whose file
//! is deleted are removed from the zone, and the references behind them are relocated.
//!
//! Layout of a folder: `zone.tsv` (the manifest), `assets/`, and `original.txt` (the path of the zone it came from: building
//! needs that zone for the old addresses, and its container for the key).

use crate::zone::fastfile::{self, ArchiveKey};
use crate::zone::writer::ZoneBuilder;
use crate::zone::Zone;
use anyhow::{Context, Result, bail};
use std::path::Path;

const MAP_ENTS: u32 = 19;
const ADDON_MAP_ENTS: u32 = 42;
const RAW_FILE: u32 = 36;

/// Where the entity text sits in a map-entities blob: (offset, length including the NUL), and the offset of the length field.
pub(crate) fn entity_text(blob: &[u8]) -> Option<(usize, usize)> {
    if blob.len() < 44 {
        return None;
    }
    let word = |o: usize| u32::from_le_bytes(blob[o..o + 4].try_into().unwrap());
    // 36-byte records (co-op), 44 (stock, stages) and 52 (this QA build): the name's first characters are never -1 at +36
    let header = if word(36) == u32::MAX { 52 } else { 36 };
    if word(4) == 0 || word(8) == 0 {
        return None;
    }
    let name = if word(0) == u32::MAX { blob[header..].iter().position(|&c| c == 0)? + 1 } else { 0 };
    let at = header + name;
    let n = word(8) as usize;
    (at + n <= blob.len() && n > 0).then_some((at, n))
}

/// Replaces the entity text of a map-entities blob.
fn with_entity_text(blob: &[u8], text: &str) -> Option<Vec<u8>> {
    let (at, n) = entity_text(blob)?;
    let mut bytes = text.as_bytes().to_vec();
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    bytes.push(0);
    let mut out = blob[..at].to_vec();
    out.extend_from_slice(&bytes);
    out.extend_from_slice(&blob[at + n..]);
    out[8..12].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    Some(out)
}

/// The text of a raw file blob (name, then the stored bytes when they are not compressed).
fn raw_text(blob: &[u8]) -> Option<(usize, usize, String)> {
    if blob.len() < 16 {
        return None;
    }
    let word = |o: usize| u32::from_le_bytes(blob[o..o + 4].try_into().unwrap());
    let (compressed, len) = (word(4), word(8) as usize);
    if compressed != 0 || word(12) == 0 {
        return None;
    }
    let name = if word(0) == u32::MAX { blob[16..].iter().position(|&c| c == 0)? + 1 } else { 0 };
    let at = 16 + name;
    (at + len < blob.len() + 1 && len > 0).then(|| (at, len, String::from_utf8_lossy(&blob[at..at + len]).into_owned()))
}

fn with_raw_text(blob: &[u8], text: &str) -> Option<Vec<u8>> {
    let (at, len, _) = raw_text(blob)?;
    let mut out = blob[..at].to_vec();
    out.extend_from_slice(text.as_bytes());
    out.push(0);
    // the stored length does not count the NUL; whatever followed the old text (nothing, for a raw file) stays
    out.extend_from_slice(&blob[(at + len + 1).min(blob.len())..]);
    out[8..12].copy_from_slice(&(text.len() as u32).to_le_bytes());
    Some(out)
}

/// Entity text nested in another asset (3.19 and QA keep the map's entities inside the collision map): for each such asset index,
/// the offsets from the asset's start of the nested header and of the text, and the text's length including the NUL.
pub fn nested_entities(zone: &Zone) -> std::collections::HashMap<usize, (usize, usize, usize)> {
    let mut out = std::collections::HashMap::new();
    let Ok(mut r) = crate::zone::replay::Replay::new(zone) else { return out };
    for i in 0..zone.assets.len() {
        let _ = r.replay_asset(zone, i);
    }
    for &(hdr, at, n) in &r.rec.map_ents {
        let found = zone.assets.iter().position(|a| hdr >= a.start && hdr < a.start + a.extent);
        if let Some(i) = found {
            let a = &zone.assets[i];
            if !matches!(a.asset_type, MAP_ENTS | ADDON_MAP_ENTS) && n > 1 && at + n <= a.start + a.extent {
                out.insert(i, (hdr - a.start, at - a.start, n));
            }
        }
    }
    out
}

/// Entity text without the white space between its quoted strings (everything inside quotes is kept); braces stay on lines of
/// their own, because the game's tokenizer only splits unquoted text at white space: `{`, `"classname""x""origin""1 2 3"`, `}`.
pub fn minify_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quoted = false;
    for c in text.chars() {
        if c == '"' {
            quoted = !quoted;
            out.push(c);
        } else if quoted {
            out.push(c);
        } else if c == '{' {
            out.push_str("{\n");
        } else if c == '}' {
            out.push_str("\n}\n");
        } else if !c.is_whitespace() {
            out.push(c);
        }
    }
    out
}

/// The text made exactly `len` bytes long (without the NUL), so that nothing behind it in the zone moves: shorter text is
/// padded with line feeds, longer text is first written without the white space between tokens. `None` when it still does not fit.
pub fn fit_entities(text: &str, len: usize) -> Option<String> {
    let trimmed = text.trim_end_matches(|c: char| c == '\0' || c.is_whitespace());
    let mut t = if trimmed.len() <= len { trimmed.to_string() } else { minify_entities(trimmed) };
    if t.len() > len {
        return None;
    }
    while t.len() < len {
        t.push('\n');
    }
    Some(t)
}

/// Replaces the text nested at (`hdr`, `at`, `n`) of a blob and fixes the length in the nested header.
pub(crate) fn with_nested_text(blob: &[u8], (hdr, at, n): (usize, usize, usize), text: &str) -> Vec<u8> {
    let mut bytes = text.as_bytes().to_vec();
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    bytes.push(0);
    let mut out = blob[..at].to_vec();
    out.extend_from_slice(&bytes);
    out.extend_from_slice(&blob[at + n..]);
    out[hdr + 8..hdr + 12].copy_from_slice(&(bytes.len() as u32).to_le_bytes());
    out
}

/// Writes the zone at `zone_path` into `out`.
pub fn unpack(zone_path: &Path, key: &ArchiveKey, out: &Path) -> Result<usize> {
    let opened = fastfile::open(zone_path, key)?;
    let zone = Zone::from_image(opened.version, opened.image)?;
    std::fs::create_dir_all(out.join("assets"))?;
    let mut manifest = String::from("index\ttype\treference\textent\tfile\ttext\n");
    let nested = nested_entities(&zone);
    for (i, a) in zone.assets.iter().enumerate() {
        let blob = &zone.data[a.start..a.start + a.extent];
        let file = format!("assets/{i:05}_{}.bin", a.asset_type);
        std::fs::write(out.join(&file), blob)?;
        let mut text = String::new();
        if matches!(a.asset_type, MAP_ENTS | ADDON_MAP_ENTS) {
            if let Some((at, n)) = entity_text(blob) {
                text = format!("assets/{i:05}_{}.txt", a.asset_type);
                std::fs::write(out.join(&text), &blob[at..at + n - 1])?;
            }
        } else if a.asset_type == RAW_FILE {
            if let Some((_, _, t)) = raw_text(blob) {
                text = format!("assets/{i:05}_{}.txt", a.asset_type);
                std::fs::write(out.join(&text), t)?;
            }
        } else if let Some(&(_, at, n)) = nested.get(&i) {
            text = format!("assets/{i:05}_{}.txt", a.asset_type);
            std::fs::write(out.join(&text), &blob[at..at + n - 1])?;
        }
        manifest.push_str(&format!("{i}\t{}\t{:#x}\t{}\t{file}\t{text}\n", a.asset_type, a.reference, a.extent));
    }
    std::fs::write(out.join("zone.tsv"), manifest)?;
    std::fs::write(out.join("original.txt"), zone_path.to_string_lossy().as_bytes())?;
    Ok(zone.assets.len())
}

/// What building changed.
#[derive(Debug, Default)]
pub struct BuildReport {
    pub replaced: Vec<usize>,
    pub removed: Vec<usize>,
    pub relocated: usize,
    pub bytes: usize,
}

/// Builds the zone described by the folder `dir` into the fastfile `out` (unsigned unless `signed`).
pub fn build(dir: &Path, key: &ArchiveKey, out: &Path, signed: bool) -> Result<BuildReport> {
    let original_path = std::fs::read_to_string(dir.join("original.txt")).context("original.txt")?;
    let opened = fastfile::open(Path::new(original_path.trim()), key)?;
    let original = Zone::from_image(opened.version, opened.image.clone())?;
    let mut builder = ZoneBuilder::from_zone(&original);
    let nested = nested_entities(&original);
    let manifest = std::fs::read_to_string(dir.join("zone.tsv")).context("zone.tsv")?;
    let mut report = BuildReport::default();
    // from the last row to the first, so removing one does not move the indexes of the next
    let rows: Vec<Vec<&str>> = manifest.lines().skip(1).map(|l| l.split('\t').collect()).collect();
    for row in rows.iter().rev() {
        if row.len() < 6 {
            bail!("bad manifest row {row:?}");
        }
        let index: usize = row[0].parse()?;
        let (file, text) = (dir.join(row[4]), row[5]);
        let ty: u32 = row[1].parse()?;
        let Ok(blob) = std::fs::read(&file) else {
            builder.remove(index);
            report.removed.push(index);
            continue;
        };
        let original_blob = &original.data[original.assets[index].start..original.assets[index].start + original.assets[index].extent];
        let mut new = blob.clone();
        if !text.is_empty() {
            if let Ok(t) = std::fs::read_to_string(dir.join(text)) {
                let nest = nested.get(&index).copied().filter(|&(_, at, n)| at + n <= blob.len());
                let (current, expected) = match ty {
                    MAP_ENTS | ADDON_MAP_ENTS => (entity_text(&blob).map(|(a, n)| String::from_utf8_lossy(&blob[a..a + n - 1]).into_owned()), true),
                    _ if nest.is_some() => (nest.map(|(_, a, n)| String::from_utf8_lossy(&blob[a..a + n - 1]).into_owned()), true),
                    _ => (raw_text(&blob).map(|(_, _, s)| s), true),
                };
                if expected && current.as_deref() != Some(t.as_str()) {
                    new = match (ty, nest) {
                        (MAP_ENTS | ADDON_MAP_ENTS, _) => with_entity_text(&blob, &t),
                        // nested text: it may change length, what lies behind it is relocated. `ATLAS_FIT_ENTS=1` keeps the old
                        // length instead (padding, or dropping the white space between quoted strings), so that nothing moves
                        (_, Some(p)) if std::env::var_os("ATLAS_FIT_ENTS").is_none() => Some(with_nested_text(&blob, p, &t)),
                        (_, Some(p)) => {
                            let fitted = fit_entities(&t, p.2 - 1).with_context(|| {
                                format!("{}: the entity text is {} bytes too long to fit the zone even without white space ({} bytes available)", row[5], minify_entities(&t).len().saturating_sub(p.2 - 1), p.2 - 1)
                            })?;
                            Some(with_nested_text(&blob, p, &fitted))
                        }
                        _ => with_raw_text(&blob, &t),
                    }
                    .with_context(|| format!("{}: this asset's text cannot be replaced", row[4]))?;
                }
            }
        }
        if new != original_blob {
            if nested.contains_key(&index) && !text.is_empty() {
                builder.replace_same_layout(index, new);
            } else {
                builder.replace(index, new);
            }
            report.replaced.push(index);
        }
    }
    let built = builder.build(&original)?;
    report.relocated = built.relocated;
    let mut container = opened.container.clone();
    container.signed = signed && container.signed;
    let bytes = fastfile::seal(&built.image, opened.version, &container)?;
    report.bytes = bytes.len();
    std::fs::write(out, bytes)?;
    report.replaced.reverse();
    report.removed.reverse();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_text_is_found_and_replaced() {
        // 36-byte header, inline name, text
        let mut blob = vec![0u8; 36];
        blob[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        blob[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        let text = b"{\n\"a\" \"b\"\n}\0";
        blob[8..12].copy_from_slice(&(text.len() as u32).to_le_bytes());
        blob.extend_from_slice(b"maps/x.mapents\0");
        blob.extend_from_slice(text);
        let (at, n) = entity_text(&blob).unwrap();
        assert_eq!(&blob[at..at + n - 1], b"{\n\"a\" \"b\"\n}");
        let new = with_entity_text(&blob, "{\n\"classname\" \"x\"\n}").unwrap();
        let (at, n) = entity_text(&new).unwrap();
        assert_eq!(&new[at..at + n - 1], b"{\n\"classname\" \"x\"\n}");
        assert_eq!(&new[36..36 + 15], b"maps/x.mapents\0");
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    #[test]
    fn entity_text_is_fitted_to_its_old_length() {
        let text = "{\n\"classname\" \"worldspawn\"\n\"a\" \"b c\"\n}\n{\n\"classname\" \"x\"\n}\n";
        // shorter or equal: kept, padded with line feeds
        let same = fit_entities(text, text.len() + 5).unwrap();
        assert_eq!(same.len(), text.len() + 5);
        assert!(same.starts_with(text.trim_end()));
        // longer than the room: white space between quoted strings goes, braces keep their own lines, quoted text is untouched
        let small = minify_entities(text);
        assert!(small.len() < text.len());
        assert!(small.starts_with("{\n\"classname\"\"worldspawn\"\"a\"\"b c\"\n}\n{\n\"classname\"\"x\"\n}"), "{small:?}");
        let tight = fit_entities(text, small.len() + 1).unwrap();
        assert_eq!(tight.len(), small.len() + 1);
        assert!(tight.starts_with(&small));
        assert!(fit_entities(text, 10).is_none());
    }
}
