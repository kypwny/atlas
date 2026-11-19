//! Black Ops II zones as files and back (`--t6unzone`, `--t6buildzone`), after the CoDOL `--unzone` / `--buildzone` (see
//! `zonefiles`): a folder with a manifest and the content of the assets that can be edited, and a build that takes the edits
//! back into the zone with `t6_rebuild` (relocation included) and writes a fastfile.
//!
//! Layout of a folder: `zone.tsv` (one row per asset: index, type number and name, asset name, file, what the entry is),
//! `original.txt` (the path of the zone it came from; building starts from that zone), `assets/` (the files).
//! - raw files and script parse trees: their bytes (`assets/00012_<file name>`; a script parse tree is compiled script, `.bin`)
//! - localize entries: the text of the value (`.txt`)
//! - string tables: CSV (comma separated, a cell with a comma, quote or line break in double quotes)
//!
//! Building: an asset whose file differs from what the zone holds is replaced by the file's content; an asset whose file is
//! deleted is removed, and so is an asset whose row is deleted from `zone.tsv` (the way to remove an asset that has no file).
//! Everything else is kept as it is. The result is an unsigned zone (`TAffu100`; Treyarch's signature cannot be made) whose
//! cipher takes its name from the output file name, so name it the way the game will ask for it; a build with no edits can keep
//! the original signature (`keep_signature`, only when the output has the original's name): that copies the original file, since
//! the signature covers the hash table of the original's own deflate output, which a re-encoding does not always reproduce.
//!
//! What this has been checked against is the zone walker (the stream is used up exactly, every block ends at the header's size)
//! and the reader of `t6`; the game has not been run on a built zone.

use super::t6::{self, ASSET_TYPES};
use super::t6_rebuild::*;
use super::t6_write::{WriteOptions, write_fastfile};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

fn has_data(z: &T6Image, k: usize) -> bool {
    matches!(z.asset_pointer(k), FOLLOW | FOLLOW_SLOT)
}

fn type_name(ty: u32) -> &'static str {
    ASSET_TYPES.get(ty as usize).copied().unwrap_or("?")
}

/// A file name for an asset: its index and the last part of its name, with characters that file names cannot hold replaced.
fn file_name_for(k: usize, name: &str, extension: Option<&str>) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let base: String = base.chars().map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c }).collect();
    let base = if base.is_empty() { "asset".to_string() } else { base };
    match extension {
        Some(e) => format!("assets/{k:05}_{base}.{e}"),
        None => format!("assets/{k:05}_{base}"),
    }
}

// ---------------------------------------------------------------------------------------------------------------------- CSV
/// A string table as CSV bytes.
pub fn csv_write(columns: usize, cells: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    if columns == 0 {
        return out;
    }
    for row in cells.chunks(columns) {
        for (i, c) in row.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            if c.iter().any(|&b| matches!(b, b',' | b'"' | b'\n' | b'\r')) {
                out.push(b'"');
                for &b in c {
                    if b == b'"' {
                        out.push(b'"');
                    }
                    out.push(b);
                }
                out.push(b'"');
            } else {
                out.extend_from_slice(c);
            }
        }
        out.push(b'\n');
    }
    out
}

/// CSV bytes as rows of cells (`\n` and `\r\n` end a row; a final line break does not start another row).
pub fn csv_read(data: &[u8]) -> Vec<Vec<Vec<u8>>> {
    let mut rows: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut row: Vec<Vec<u8>> = Vec::new();
    let mut cell: Vec<u8> = Vec::new();
    let mut quoted = false;
    let mut at_row_start = true;
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        if quoted {
            if c == b'"' {
                if data.get(i + 1) == Some(&b'"') {
                    cell.push(b'"');
                    i += 1;
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
        } else {
            match c {
                b'"' if cell.is_empty() => {
                    quoted = true;
                    at_row_start = false;
                }
                b',' => {
                    row.push(std::mem::take(&mut cell));
                    at_row_start = false;
                }
                b'\r' if data.get(i + 1) == Some(&b'\n') => {}
                b'\n' => {
                    row.push(std::mem::take(&mut cell));
                    rows.push(std::mem::take(&mut row));
                    at_row_start = true;
                }
                _ => {
                    cell.push(c);
                    at_row_start = false;
                }
            }
        }
        i += 1;
    }
    if !at_row_start || !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

/// The rows as a rectangle (short rows are padded with empty cells): (columns, rows, cells row by row).
fn rectangle(rows: Vec<Vec<Vec<u8>>>) -> (usize, usize, Vec<Vec<u8>>) {
    let columns = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let n = rows.len();
    let mut cells = Vec::with_capacity(columns * n);
    for mut r in rows {
        r.resize(columns, Vec::new());
        cells.extend(r);
    }
    (columns, n, cells)
}

// ------------------------------------------------------------------------------------------------------------------- unpack
/// What an asset becomes as a file: (relative path, bytes).
fn export(z: &T6Image, k: usize) -> Result<Option<(String, Vec<u8>)>> {
    if !has_data(z, k) {
        return Ok(None);
    }
    let name = z.asset_name(k);
    Ok(match z.asset_type(k) {
        RAWFILE => Some((file_name_for(k, name, None), z.rawfile(k)?)),
        SCRIPT_PARSE_TREE => Some((file_name_for(k, name, Some("bin")), z.rawfile(k)?)),
        LOCALIZE => {
            let (n, v) = z.localize(k)?;
            Some((file_name_for(k, &n, Some("txt")), v.into_bytes()))
        }
        STRINGTABLE => {
            let t = z.stringtable(k)?;
            let bytes = csv_write(t.columns, &t.texts().into_iter().map(String::into_bytes).collect::<Vec<_>>());
            Some((file_name_for(k, name, None), bytes))
        }
        _ => None,
    })
}

/// Writes the zone at `zone_path` into the folder `out`; returns the number of assets and of files.
pub fn unpack(zone_path: &Path, out: &Path) -> Result<(usize, usize)> {
    let raw = std::fs::read(zone_path).with_context(|| zone_path.display().to_string())?;
    let stem = zone_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let zone = t6::open_named(&raw, Some(&stem))?;
    let img = T6Image::from_zone(&zone)?;
    std::fs::create_dir_all(out.join("assets"))?;
    let mut manifest = String::from("index\ttype\ttype_name\tname\tfile\tdata\n");
    let mut files = 0;
    for k in 0..img.asset_count() {
        let ty = img.asset_type(k);
        let mut file = String::new();
        if let Some((rel, bytes)) = export(&img, k)? {
            std::fs::write(out.join(&rel), bytes)?;
            file = rel;
            files += 1;
        }
        let data = if has_data(&img, k) { "data" } else { "reference" };
        manifest.push_str(&format!("{k}\t{ty}\t{}\t{}\t{file}\t{data}\n", type_name(ty), img.asset_name(k).replace(['\t', '\n', '\r'], " ")));
    }
    std::fs::write(out.join("zone.tsv"), manifest)?;
    let full = std::fs::canonicalize(zone_path).unwrap_or_else(|_| zone_path.to_path_buf());
    std::fs::write(out.join("original.txt"), full.to_string_lossy().trim_start_matches(r"\\?\").as_bytes())?;
    Ok((img.asset_count(), files))
}

// -------------------------------------------------------------------------------------------------------------------- build
#[derive(Debug, Default)]
pub struct BuildReport {
    pub replaced: Vec<usize>,
    pub removed: Vec<usize>,
    pub signed: bool,
    pub stats: RebuildStats,
    pub bytes: usize,
}

/// The edit a changed file makes, or `None` when the file holds what the zone holds.
fn edit_for(z: &T6Image, k: usize, content: &[u8]) -> Result<Option<NewAsset>> {
    match z.asset_type(k) {
        RAWFILE | SCRIPT_PARSE_TREE => {
            if z.rawfile(k)? == content {
                return Ok(None);
            }
            Ok(Some(z.set_rawfile(k, content)?))
        }
        LOCALIZE => {
            let (_, old) = z.localize(k)?;
            if old.as_bytes() == content {
                return Ok(None);
            }
            let text = std::str::from_utf8(content).context("a localize text has to be UTF-8")?;
            Ok(Some(z.set_localize(k, text)?))
        }
        STRINGTABLE => {
            let t = z.stringtable(k)?;
            let (columns, rows, mut cells) = rectangle(csv_read(content));
            if columns == t.columns && rows == t.rows {
                // a cell that reads the way the old one is shown stays byte for byte what it was
                let mut changed = false;
                for (new, old) in cells.iter_mut().zip(&t.cells) {
                    let old = old.clone().unwrap_or_default();
                    if *new == String::from_utf8_lossy(&old).as_bytes() {
                        *new = old;
                    } else {
                        changed = true;
                    }
                }
                if !changed {
                    return Ok(None);
                }
            }
            Ok(Some(z.set_stringtable(k, columns, rows, &cells)?))
        }
        _ => bail!("asset {k}: a file for a {} cannot be applied", type_name(z.asset_type(k))),
    }
}

/// Builds the zone described by the folder `dir` into the fastfile `out`.
pub fn build(dir: &Path, out: &Path, keep_signature: bool) -> Result<BuildReport> {
    let original = std::fs::read_to_string(dir.join("original.txt")).context("original.txt")?;
    let original = PathBuf::from(original.trim());
    let raw = std::fs::read(&original).with_context(|| original.display().to_string())?;
    let stem = original.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let zone = t6::open_named(&raw, Some(&stem))?;
    let img = T6Image::from_zone(&zone)?;
    let manifest = std::fs::read_to_string(dir.join("zone.tsv")).context("zone.tsv")?;
    let mut listed = vec![false; img.asset_count()];
    let mut edits = Vec::new();
    let mut report = BuildReport::default();
    for line in manifest.lines().skip(1).filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 5 {
            bail!("bad manifest row {line:?}");
        }
        let k: usize = f[0].parse().with_context(|| format!("bad index in {line:?}"))?;
        if k >= img.asset_count() {
            bail!("the manifest names asset {k}, the zone has {}", img.asset_count());
        }
        if f[1].parse::<u32>().ok() != Some(img.asset_type(k)) {
            bail!("asset {k} is not of type {} in the original zone: the folder belongs to another zone", f[1]);
        }
        listed[k] = true;
        if f[4].is_empty() {
            continue;
        }
        match std::fs::read(dir.join(f[4])) {
            Ok(content) => {
                if let Some(na) = edit_for(&img, k, &content).with_context(|| format!("{}", f[4]))? {
                    edits.push(Edit::Replace(k, na));
                    report.replaced.push(k);
                }
            }
            Err(_) => {
                edits.push(Edit::Remove(k));
                report.removed.push(k);
            }
        }
    }
    for k in (0..img.asset_count()).filter(|&k| !listed[k]) {
        edits.push(Edit::Remove(k));
        report.removed.push(k);
    }
    let name = out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if edits.is_empty() && keep_signature && !zone.auth_header.is_empty() && name == zone.name {
        // nothing changed: the original file itself, whose signature covers its own deflate output
        report.signed = true;
        report.bytes = raw.len();
        std::fs::write(out, &raw)?;
        return Ok(report);
    }
    let (image, opts) = if edits.is_empty() {
        (zone.image.clone(), WriteOptions { chunk_sizes: Some(zone.chunk_sizes.clone()), ..WriteOptions::default() })
    } else {
        let r = rebuild(&img, edits)?;
        report.stats = r.stats.clone();
        (r.zone.image().to_vec(), WriteOptions::default())
    };
    let bytes = write_fastfile(&name, &image, &opts)?;
    report.bytes = bytes.len();
    std::fs::write(out, bytes)?;
    Ok(report)
}

// --------------------------------------------------------------------------------------------------------------- commands
/// `atlas --t6unzone ZONE.ff OUT_DIR`
pub fn command_unzone(args: &[String]) -> Result<()> {
    let (Some(zone), Some(out)) = (args.first(), args.get(1)) else { bail!("usage: atlas --t6unzone ZONE.ff OUT_DIR") };
    let (assets, files) = unpack(Path::new(zone), Path::new(out))?;
    println!("{assets} assets of {zone}: {files} files written to {out} (raw files, script parse trees, localize entries, string tables)");
    Ok(())
}

/// `atlas --t6buildzone DIR OUT.ff [--keep-signature]`
pub fn command_buildzone(args: &[String]) -> Result<()> {
    let (Some(dir), Some(out)) = (args.first(), args.get(1)) else { bail!("usage: atlas --t6buildzone DIR OUT.ff [--keep-signature]") };
    let r = build(Path::new(dir), Path::new(out), args.iter().any(|a| a == "--keep-signature"))?;
    println!("{out}: {} bytes, {}; replaced {:?}, removed {:?}", r.bytes, if r.signed { "signed (original signature)" } else { "unsigned (TAffu100)" }, r.replaced, r.removed);
    if r.stats.new_len > 0 {
        println!(
            "image {} -> {} bytes; {} offset pointers rewritten, {} dropped with their data; the rebuilt zone walks exactly",
            r.stats.old_len, r.stats.new_len, r.stats.pointers_moved, r.stats.pointers_dropped
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_round_trips_quotes_commas_and_line_breaks() {
        let cells: Vec<Vec<u8>> = ["a", "b,c", "say \"hi\"", "two\nlines", "", "x"].iter().map(|s| s.as_bytes().to_vec()).collect();
        let text = csv_write(3, &cells);
        assert_eq!(text, b"a,\"b,c\",\"say \"\"hi\"\"\"\n\"two\nlines\",,x\n");
        let (c, r, back) = rectangle(csv_read(&text));
        assert_eq!((c, r), (3, 2));
        assert_eq!(back, cells);
        // CRLF, no final line break, a short row
        let (c, r, back) = rectangle(csv_read(b"a,b\r\nc"));
        assert_eq!((c, r, back), (2, 2, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), Vec::new()]));
        assert!(csv_read(b"").is_empty());
    }
}
