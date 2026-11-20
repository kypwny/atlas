//! `atlas --convertzone SRC.ff --to iw3|iw4|iw5|codol [GAME_FOLDER] OUT.ff`: a zone of one game of the Infinity Ward family
//! (Call of Duty 4, Modern Warfare 2, Modern Warfare 3, Call of Duty Online) to another.
//!
//! Every conversion goes through stock IW4 (hub and spoke): a source is converted to IW4 and an IW4 zone to the target, so each
//! game needs two converters (to and from IW4) rather than one for every pair. Each hop's output is walked by the reader of its
//! own layout (the schema walker for IW3, IW4 and IW5, the QA walker for Call of Duty Online) and the conversion fails when
//! that walk is not exact, so a zone passed to the next hop, or written, always reads cleanly.

use super::convert_zone::{self, TypeStat};
use super::fastfile::{self, Family};
use super::schema_walk::{self, Game, ZoneImage};
use super::{Zone, u32_at};
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A game (layout) a zone can be in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    Iw3,
    Iw4,
    Iw5,
    /// Call of Duty Online; the version (334, 423 or 461) is kept next to the format.
    Codol,
    /// Future Warfare, Xbox 360 development build (big-endian, read only: a source of conversions, never a target).
    Nx1,
}

impl Fmt {
    pub fn parse(s: &str) -> Option<Fmt> {
        match s.to_ascii_lowercase().as_str() {
            "iw3" | "cod4" => Some(Fmt::Iw3),
            "iw4" | "mw2" => Some(Fmt::Iw4),
            "iw5" | "mw3" => Some(Fmt::Iw5),
            "codol" | "qa" | "codo" => Some(Fmt::Codol),
            "nx1" => Some(Fmt::Nx1),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Fmt::Iw3 => "iw3",
            Fmt::Iw4 => "iw4",
            Fmt::Iw5 => "iw5",
            Fmt::Codol => "codol",
            Fmt::Nx1 => "nx1",
        }
    }

    /// The schema walker's game, for the three layouts it reads.
    pub fn game(self) -> Option<Game> {
        match self {
            Fmt::Iw3 => Some(Game::Iw3),
            Fmt::Iw4 => Some(Game::Iw4),
            Fmt::Iw5 => Some(Game::Iw5),
            Fmt::Codol | Fmt::Nx1 => None,
        }
    }

    /// Fastfile version written for the format (Call of Duty Online: QA 3.3.3.3).
    fn version(self) -> u32 {
        match self {
            Fmt::Iw3 => 5,
            Fmt::Iw4 => 276,
            Fmt::Iw5 => 1,
            Fmt::Codol => fastfile::VERSION_QA,
            Fmt::Nx1 => 357,
        }
    }

    /// Name of asset type `ty` in this layout (Call of Duty Online numbers its types like IW4).
    pub fn type_name(self, ty: u32) -> String {
        if self == Fmt::Nx1 {
            return super::nx1::asset_type_name(ty).to_string();
        }
        self.game().unwrap_or(Game::Iw4).asset_type_name(ty)
    }
}

/// An inflated zone image with what is known about it.
pub struct Input {
    pub fmt: Fmt,
    pub version: u32,
    pub name: String,
    pub image: Vec<u8>,
}

impl Input {
    /// The image as the schema walker takes it (IW3, IW4, IW5 layouts).
    pub fn zone_image(&self) -> ZoneImage {
        if self.fmt == Fmt::Nx1 {
            let block_sizes = (0..super::nx1::BLOCKS).map(|i| u32::from_be_bytes(self.image[8 + 4 * i..12 + 4 * i].try_into().unwrap())).collect();
            return ZoneImage { name: self.name.clone(), image: self.image.clone(), block_sizes };
        }
        let n = self.fmt.game().map(|g| schema_block_count(g)).unwrap_or(8);
        let block_sizes = (0..n).map(|i| u32_at(&self.image, 8 + 4 * i)).collect();
        ZoneImage { name: self.name.clone(), image: self.image.clone(), block_sizes }
    }
}

fn schema_block_count(g: Game) -> usize {
    match g {
        Game::Iw3 => 9,
        Game::Iw5 => 9,
        _ => 8,
    }
}

/// The result of one hop.
#[derive(Default)]
pub struct HopOut {
    pub image: Vec<u8>,
    /// Per asset type of the hop's source: what became of the assets.
    pub stats: BTreeMap<u32, TypeStat>,
    /// References to assets that were left out (now null).
    pub nulled: usize,
    pub warnings: BTreeMap<String, usize>,
    /// Assets in the converted zone.
    pub assets: usize,
    /// Content hashes of the converted assets (only when the hop was asked for them).
    pub canon: Vec<super::convert_engine::CanonAsset>,
}

type HopFn = fn(&Input) -> Result<HopOut>;

/// A hop run by the schema engine (`convert_engine`): the source zone is walked, its structures rebuilt in the target layout with
/// `rules`, and the target stream written by the loader's own rules.
pub fn engine_hop(from: Game, to: Game, rules: super::convert_engine::Rules, input: &Input) -> Result<HopOut> {
    use super::convert_engine as ce;
    let zi = input.zone_image();
    let out = ce::convert_zone(from, &zi, to, rules).map_err(|e| anyhow::anyhow!(e))?;
    let stats = out.stats.into_iter().map(|(ty, (converted, left_out))| (ty, TypeStat { converted, left_out })).collect();
    Ok(HopOut { image: out.image, stats, nulled: out.nulled, warnings: out.warnings, assets: out.assets, canon: out.canon })
}

fn same_game(input: &Input) -> Result<HopOut> {
    let g = input.fmt.game().context("not a schema game")?;
    engine_hop(g, g, Default::default(), input)
}

/// The converter from `from` to `to`, when there is one. Only hops to or from IW4 exist; anything else is composed.
fn hop(from: Fmt, to: Fmt) -> Option<HopFn> {
    match (from, to) {
        (a, b) if a == b && a != Fmt::Codol => Some(same_game),
        (Fmt::Iw4, Fmt::Codol) => Some(iw4_to_codol),
        (Fmt::Codol, Fmt::Iw4) => Some(super::convert_from_codol::codol_to_iw4),
        (Fmt::Iw3, Fmt::Iw4) => Some(super::convert_iw3::iw3_to_iw4),
        (Fmt::Iw4, Fmt::Iw3) => Some(super::convert_iw3::iw4_to_iw3),
        (Fmt::Iw5, Fmt::Iw4) => Some(super::convert_iw5::iw5_to_iw4),
        (Fmt::Iw4, Fmt::Iw5) => Some(super::convert_iw5::iw4_to_iw5),
        (Fmt::Nx1, Fmt::Iw4) => Some(super::convert_nx1::nx1_to_iw4),
        _ => None,
    }
}

fn iw4_to_codol(input: &Input) -> Result<HopOut> {
    let z = input.zone_image();
    let stock = super::convert_walk::ZoneImage { name: z.name, image: z.image, block_sizes: z.block_sizes };
    let c = convert_zone::convert_image(&stock)?;
    Ok(HopOut { image: c.image, stats: c.stats, nulled: c.nulled, warnings: c.warnings, assets: c.assets, canon: Vec::new() })
}

/// Opens a fastfile of any supported game: detects the game from its header, inflates it (Call of Duty Online needs the archive key).
pub fn open_input(path: &Path, key_folder: Option<&Path>) -> Result<Input> {
    if std::fs::File::open(path).and_then(|mut f| { use std::io::Read; let mut m = [0u8; 4]; f.read_exact(&mut m).map(|_| m) }).is_ok_and(|m| &m == b"NXff") {
        let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let raw = std::fs::read(path).with_context(|| path.display().to_string())?;
        let f = super::nx1::open(&raw).with_context(|| path.display().to_string())?;
        return Ok(Input { fmt: Fmt::Nx1, version: f.version, name, image: f.image });
    }
    let peek = fastfile::peek(path)?;
    if peek.big_endian {
        bail!("{}: a big-endian (console) fastfile; only PC zones are supported", path.display());
    }
    let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let raw = std::fs::read(path).with_context(|| path.display().to_string())?;
    let fmt = match (peek.family, peek.version) {
        (Family::Iw, 5) => Fmt::Iw3,
        (Family::Iw, 276) => Fmt::Iw4,
        (Family::Iw, 1) => Fmt::Iw5,
        (Family::Iw, fastfile::VERSION_C1 | fastfile::VERSION_QA | fastfile::VERSION_319) => Fmt::Codol,
        (Family::Treyarch, _) => bail!("{}: a Black Ops II zone (Treyarch engine); it is not convertible to or from the Infinity Ward layouts", path.display()),
        (_, v) => bail!("{}: fastfile version {v} ({}) is not a supported source", path.display(), fastfile::version_info(peek.family, v).build),
    };
    match fmt {
        Fmt::Codol => {
            let root = key_folder.context("a Call of Duty Online zone needs its game folder for the archive key (--game FOLDER)")?;
            let key = super::export::find_key(root)?;
            let o = fastfile::open_bytes(&raw, &key).with_context(|| path.display().to_string())?;
            Ok(Input { fmt, version: o.version, name, image: o.image })
        }
        _ => {
            let z = ZoneImage::open(fmt.game().unwrap(), &name, &raw)?;
            Ok(Input { fmt, version: peek.version, name, image: z.image })
        }
    }
}

/// Walks a zone image with the reader of its layout; `Err` describes where the walk stops or what does not add up.
pub fn verify(fmt: Fmt, name: &str, image: &[u8]) -> Result<String> {
    match fmt.game() {
        Some(game) => {
            let n = schema_block_count(game);
            if image.len() < 8 + 4 * n {
                bail!("{} image too short ({} bytes)", fmt.name(), image.len());
            }
            let zone = ZoneImage { name: name.to_string(), image: image.to_vec(), block_sizes: (0..n).map(|i| u32_at(image, 8 + 4 * i)).collect() };
            let r = schema_walk::walk(game, &zone);
            if let Some(f) = &r.failure {
                bail!("the {} walker stops at asset {} (type {} {}) at {:#x}: {}", fmt.name(), f.asset_index, f.ty, game.asset_type_name(f.ty), f.offset, f.reason);
            }
            if !r.exact {
                bail!("the {} walker reads all {} assets but: {}", fmt.name(), r.assets_total, r.mismatches.join("; "));
            }
            Ok(format!("{} assets, exact ({} stream bytes, blocks {:?})", r.assets_walked, r.consumed, r.block_end))
        }
        None => {
            let zone = Zone::from_image(fastfile::VERSION_QA, image.to_vec())?;
            let mut r = super::replay::Replay::new(&zone).map_err(|f| anyhow::anyhow!("QA walker: zone prefix: {} at {}", f.what, f.at))?;
            r.enable_tracing();
            for i in 0..zone.assets.len() {
                r.replay_asset(&zone, i).map_err(|f| anyhow::anyhow!("the QA walker stops at asset {i} (type {}): {} at {:#x}", zone.assets[i].asset_type, f.what, f.at))?;
            }
            if r.block_positions().iter().zip(zone.block_sizes.iter()).enumerate().any(|(b, (p, s))| (b != 0 && *p as u32 != *s) || (b == 0 && *p as u32 + 16 != *s)) {
                bail!("the QA walker's block footprint {:?} differs from the header's {:?}", r.block_positions(), zone.block_sizes);
            }
            if !r.dangling_references().is_empty() {
                bail!("{} references dangle", r.dangling_references().len());
            }
            Ok(format!("{} assets, exact (blocks {:?})", zone.assets.len(), zone.block_sizes))
        }
    }
}

/// Wraps a zone image of IW3, IW4 or IW5 in an unsigned fastfile.
pub fn write_iw_fastfile(fmt: Fmt, image: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    let mut out = Vec::new();
    out.extend_from_slice(b"IWffu100");
    out.extend_from_slice(&fmt.version().to_le_bytes());
    // IW4 and IW5 headers carry nine more bytes (a timestamp and flags the loader does not check); IW3's has none
    if fmt != Fmt::Iw3 {
        out.extend_from_slice(&[0u8; 9]);
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(image)?;
    out.extend_from_slice(&z.finish()?);
    Ok(out)
}

/// The hops from `from` to `to`, in order.
pub fn route(from: Fmt, to: Fmt) -> Vec<(Fmt, Fmt)> {
    match (from, to) {
        (a, b) if a == b => vec![(a, b)],
        (Fmt::Iw4, b) => vec![(Fmt::Iw4, b)],
        (a, Fmt::Iw4) => vec![(a, Fmt::Iw4)],
        (a, b) => vec![(a, Fmt::Iw4), (Fmt::Iw4, b)],
    }
}

/// One finished hop with its verification.
pub struct Step {
    pub from: Fmt,
    pub to: Fmt,
    pub out: HopOut,
    pub verified: String,
}

/// Runs the hops from the input to `target`; each output is verified by the reader of its layout.
pub fn convert(input: Input, target: Fmt) -> Result<(Input, Vec<Step>)> {
    let mut cur = input;
    let mut steps = Vec::new();
    for (a, b) in route(cur.fmt, target) {
        let Some(f) = hop(a, b) else {
            bail!("no converter from {} to {} yet", a.name(), b.name());
        };
        let out = f(&cur).with_context(|| format!("{} -> {}", a.name(), b.name()))?;
        let verified = verify(b, &cur.name, &out.image).with_context(|| format!("the {} -> {} output does not read cleanly", a.name(), b.name()))?;
        cur = Input { fmt: b, version: b.version(), name: cur.name.clone(), image: out.image.clone() };
        steps.push(Step { from: a, to: b, out, verified });
    }
    Ok((cur, steps))
}

pub fn print_step(s: &Step) {
    println!("{} -> {}: {} assets out, {} references to left-out assets set to null", s.from.name(), s.to.name(), s.out.assets, s.out.nulled);
    for (w, n) in &s.out.warnings {
        println!("  warning: {w} (x{n})");
    }
    for (ty, st) in &s.out.stats {
        let out: usize = st.left_out.values().sum();
        print!("  type {ty:2} {:18} {:5} converted, {out:5} left out", s.from.type_name(*ty), st.converted);
        for (why, n) in &st.left_out {
            print!("  [{n} x {why}]");
        }
        println!();
    }
    println!("  checked by the {} reader: {}", s.to.name(), s.verified);
}

fn usage() -> anyhow::Error {
    anyhow::anyhow!(
        "usage: atlas --convertzone SRC.ff --to iw3|iw4|iw5|codol [GAME_FOLDER] OUT.ff [--signed] [--game FOLDER]\n       atlas --convertzone check SRC.ff --to iw3|iw4|iw5|codol [GAME_FOLDER]\n       atlas --convertzone MW2_ZONE.ff GAME_FOLDER OUT.ff [--signed]   (MW2 to Call of Duty Online)"
    )
}

/// `atlas --convertzone ...`: the unified form with `--to`, otherwise the original MW2-to-CoDOL commands and development aids.
pub fn command(args: &[String]) -> Result<()> {
    let Some(to_at) = args.iter().position(|a| a == "--to") else {
        return convert_zone::command(args);
    };
    let to = args.get(to_at + 1).context("--to needs a target game")?;
    let target = Fmt::parse(to).with_context(|| format!("unknown target {to:?} (iw3, iw4, iw5 or codol)"))?;
    let mut check = false;
    let mut batch = false;
    let mut signed = false;
    let mut game_folder: Option<PathBuf> = None;
    let mut positional: Vec<&String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--to" => i += 1,
            "--signed" => signed = true,
            "--game" => {
                i += 1;
                game_folder = Some(PathBuf::from(args.get(i).context("--game needs a folder")?));
            }
            "check" if i == 0 => check = true,
            "batch" if i == 0 => batch = true,
            a if a.starts_with("--") => bail!("unknown option {a}"),
            _ => positional.push(&args[i]),
        }
        i += 1;
    }
    let (src, rest) = positional.split_first().ok_or_else(usage)?;
    if batch {
        return run_batch(Path::new(src.as_str()), target, game_folder.or(rest.first().map(|f| PathBuf::from(f.as_str()))).as_deref());
    }
    // [GAME_FOLDER] OUT.ff
    let (folder_pos, out): (Option<PathBuf>, Option<PathBuf>) = match (check, rest) {
        (true, []) => (None, None),
        (true, [f]) => (Some(PathBuf::from(f)), None),
        (false, [o]) => (None, Some(PathBuf::from(o))),
        (false, [f, o]) => (Some(PathBuf::from(f)), Some(PathBuf::from(o))),
        _ => return Err(usage()),
    };
    let folder = game_folder.or(folder_pos);
    if target == Fmt::Codol && folder.is_none() {
        bail!("converting to Call of Duty Online needs GAME_FOLDER (its archive key and container)");
    }
    let input = open_input(Path::new(src.as_str()), folder.as_deref())?;
    println!("{}: {} zone (version {}), {} bytes inflated", src, input.fmt.name(), input.version, input.image.len());
    let (done, steps) = convert(input, target)?;
    for s in &steps {
        print_step(s);
    }
    if steps.is_empty() {
        bail!("nothing to convert");
    }
    if let Some(out) = out {
        let bytes = match target {
            Fmt::Codol => {
                let root = folder.as_deref().unwrap();
                let key = super::export::find_key(root)?;
                let mut container = convert_zone::template_container(root, &key)?;
                container.signed = signed && container.signed;
                let sealed = fastfile::seal(&done.image, fastfile::VERSION_QA, &container)?;
                // the sealed file must open again to the same image
                let again = fastfile::open_bytes(&sealed, &key).context("reopening the sealed file")?;
                if again.image != done.image {
                    bail!("the sealed file does not inflate to the converted image");
                }
                sealed
            }
            other => write_iw_fastfile(other, &done.image)?,
        };
        std::fs::write(&out, &bytes).with_context(|| out.display().to_string())?;
        println!("{}: {} bytes", out.display(), bytes.len());
    }
    Ok(())
}

/// `atlas --convertzone batch FOLDER --to TARGET [--game FOLDER]`: converts every zone under FOLDER (writing nothing) and reports
/// which ones convert and read cleanly.
fn run_batch(root: &Path, target: Fmt, key_folder: Option<&Path>) -> Result<()> {
    let mut files = Vec::new();
    collect(root, &mut files);
    files.sort();
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut totals: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
    let mut warnings: BTreeMap<String, usize> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut source_fmt = None;
    let t0 = std::time::Instant::now();
    for f in &files {
        let label = f.strip_prefix(root).unwrap_or(f).to_string_lossy().replace('\\', "/");
        let t = std::time::Instant::now();
        let r = open_input(f, key_folder).and_then(|inp| {
            source_fmt = Some(inp.fmt);
            convert(inp, target)
        });
        match r {
            Ok((_, steps)) => {
                ok += 1;
                let first = &steps[0];
                for (w, n) in &first.out.warnings {
                    *warnings.entry(w.clone()).or_default() += n;
                }
                for st in first.out.stats.values() {
                    for (why, n) in &st.left_out {
                        *reasons.entry(why.clone()).or_default() += n;
                    }
                }
                let (c, l): (usize, usize) = first.out.stats.values().fold((0, 0), |a, s| (a.0 + s.converted, a.1 + s.left_out.values().sum::<usize>()));
                for (ty, st) in &first.out.stats {
                    let e = totals.entry(*ty).or_default();
                    e.0 += st.converted;
                    e.1 += st.left_out.values().sum::<usize>();
                }
                println!("{label}: ok, {c} assets converted, {l} left out, {} nulled references ({:.1}s)", steps.iter().map(|s| s.out.nulled).sum::<usize>(), t.elapsed().as_secs_f32());
            }
            Err(e) => {
                failed += 1;
                println!("{label}: FAILED: {}", format!("{e:#}").replace(char::from(10), " "));
            }
        }
    }
    println!("{} zones: {ok} converted and read cleanly, {failed} failed ({:.0}s)", files.len(), t0.elapsed().as_secs_f32());
    for (why, n) in &reasons {
        println!("  left out: {n} x {why}");
    }
    for (w, n) in warnings.iter().take(80) {
        println!("  warning: {w} (x{n})");
    }
    if let Some(sf) = source_fmt {
        for (ty, (c, l)) in &totals {
            println!("  type {ty:2} {:18} {c:6} converted, {l:6} left out", sf.type_name(*ty));
        }
    }
    Ok(())
}

fn collect(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                collect(&e.path(), out);
            }
        }
    } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ff")) {
        out.push(p.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_conversion_goes_through_iw4() {
        assert_eq!(route(Fmt::Iw4, Fmt::Codol), vec![(Fmt::Iw4, Fmt::Codol)]);
        assert_eq!(route(Fmt::Iw3, Fmt::Iw5), vec![(Fmt::Iw3, Fmt::Iw4), (Fmt::Iw4, Fmt::Iw5)]);
        assert_eq!(route(Fmt::Codol, Fmt::Iw3), vec![(Fmt::Codol, Fmt::Iw4), (Fmt::Iw4, Fmt::Iw3)]);
        assert_eq!(route(Fmt::Iw5, Fmt::Iw4), vec![(Fmt::Iw5, Fmt::Iw4)]);
    }

    #[test]
    fn games_are_named_by_either_name() {
        assert_eq!(Fmt::parse("cod4"), Some(Fmt::Iw3));
        assert_eq!(Fmt::parse("MW3"), Some(Fmt::Iw5));
        assert_eq!(Fmt::parse("qa"), Some(Fmt::Codol));
        assert_eq!(Fmt::parse("t6"), None);
    }

    #[test]
    fn every_hop_to_and_from_iw4_exists() {
        for f in [Fmt::Iw3, Fmt::Iw5, Fmt::Codol] {
            assert!(hop(Fmt::Iw4, f).is_some(), "iw4 -> {}", f.name());
        }
        for f in [Fmt::Iw3, Fmt::Iw5, Fmt::Codol] {
            assert!(hop(f, Fmt::Iw4).is_some(), "{} -> iw4", f.name());
        }
    }
}
