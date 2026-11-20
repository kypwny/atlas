//! `atlas --exportmap SRC.ff --to iw3|iw4|iw5|codol OUT_DIR [--name MAP] [--signed] [--game-folder FOLDER] [--no-const] [--dry-run] [--force]`:
//! converts a map zone to the target game and writes a package under OUT_DIR in the folder layout that game loads custom maps from,
//! ready to be copied into a game folder. Nothing is ever written into a game folder; `--game-folder` is only read, for the archive
//! key and container the Call of Duty Online format needs (and the key of a Call of Duty Online source).
//!
//! The zone is converted with the engine of `convert_cli` (hub and spoke through stock IW4), or copied unchanged when it is already
//! in the target's format. A `SRC_load.ff` beside the source is converted and packaged too. Every rule below carries a basis,
//! printed with the plan:
//!
//! * VERIFIED: checked against files on this machine: the target game's own folder (stock maps, an installed custom map, the
//!   arena file the client ships), or the game's executable (path templates found as strings, and the loader code that uses them).
//!   Where an offset is named it is the virtual address in that build.
//! * ASSUMED: community practice, or the same engine family's behaviour, not checked against this game's files.
//!
//! Per target (details in [`target_for`]):
//!
//! * Call of Duty 4 (`iw3`): `usermaps/MAP/MAP.ff` + `MAP_load.ff` [+ `MAP.iwd`, not made here]. VERIFIED: `iw3mp.exe` builds
//!   `%s\usermaps\%s\%s.ff` (0x489f96) and, when a usermap exists, sets `fs_game` to `usermaps/MAP` (0x48ba90) so that the folder
//!   is loaded like a mod; a missing `_load` zone is only a warning (0x48aa47); the folder of the installed `mp_phoenix` has
//!   exactly these files; `devmap` is a command of the executable.
//! * Modern Warfare 2 (`iw4`), IW4x layout: the same folder and names. VERIFIED from the strings of `iw4x.dll`:
//!   `usermaps\{}\{}.ff`, `_load.ff`, `{}.iwd`, `usermaps/{}/{}.arena`, "Looking for custom maps...", "Discovered custom map {}",
//!   `devmap {}`. The `.arena` file is written (VERIFIED format: the stock `mp/basemaps.arena` of the client's `iw4x_00.iwd`);
//!   that the client accepts an entry with only `map`, `longname` and `gametype` is ASSUMED. The converter writes 32-bit (x86)
//!   zones. A stock x86 `iw4mp.exe` without IW4x loading the same folder is ASSUMED (it has the same `usermaps` / `_load` /
//!   `%s\%s%s.ff` strings; its loader was not read); a 64-bit `iw4mp.exe` (the 2026 Steam build) reads 64-bit zones and cannot
//!   take these.
//! * Modern Warfare 3 (`iw5`, the 64-bit build the converter reads): the same folder ASSUMED by analogy; VERIFIED only that
//!   `iw5mp.exe` has the strings `%s_load`, `%s\%s%s.ff`, `_load`, `usermaps` and `.arena`. No arena file is written.
//! * Call of Duty Online (`codol`, QA 3.3.3.3, version 423; the converter writes nothing else): `client_shipRetail/chinese/MAP.ff`
//!   + `MAP_load.ff` + `MAP_const.ff` next to the stock zones (VERIFIED: that is where the game's own maps are, and the executable
//!   names `%s_const`, `%s.ff`, `_load`, `_const`, `devmap`). The loader's zone list for a map asks for `MAP_const.ff` (3.19 loader at
//!   0x49eaf1, QA loader at 0x49fe30), and a zone file that is missing makes the loader raise `WIN_ZONE_ERROR` unless its name starts
//!   with `patch` or `ez_` or contains `_load` (QA 0x4a0430, read, not run), so the zone is written ([`super::const_zone`]).
//!   It holds raw files only: the netcode baselines `mp/constbaselines/bl_pc_MAP_MODE.bin` of the modes in the row of the stock
//!   table `mp/constBaselinesTable.csv`, then a marker raw file `MAP_const`. VERIFIED on all 65 stock const zones: Atlas lays
//!   each out byte-identical (inflated image) from its own raw files, and generates the stock `mp_con_spring_const` (the marker
//!   alone) from nothing. A baseline is a snapshot of a running server and is not derivable; none is needed: the executable
//!   reads a baseline only for a map with a row in that table (0x584880), and a custom map has none. The generated zone is
//!   therefore the marker alone; a QA zone exported with a `SRC_const.ff` beside it brings that file unchanged. `--no-const`
//!   leaves the zone out. Whether the client accepts a marker-only zone for a map is UNKNOWN (nothing is run; the stock map
//!   `mp_con_spring` ships that way). A 3.19 (461) folder cannot load QA-layout zones.
//!
//! Not done: the game's own map list (`mapsTable`, localized names and, for MW3, arenas live inside zones, not in plain files),
//! images and sounds (IWI files in the `.iwd`; `--packiwd` makes one), and renaming (map names are inside the zone: `.d3dbsp`
//! assets, scripts, the `MAP_load` marker). Nothing here starts a game.

use super::convert_cli::{self, Fmt};
use super::schema_walk::{self, Game};
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Basis {
    Verified,
    Assumed,
}

impl Basis {
    fn tag(self) -> &'static str {
        match self {
            Basis::Verified => "VERIFIED",
            Basis::Assumed => "ASSUMED ",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub basis: Basis,
    pub text: String,
}

fn v(text: impl Into<String>) -> Rule {
    Rule { basis: Basis::Verified, text: text.into() }
}

fn a(text: impl Into<String>) -> Rule {
    Rule { basis: Basis::Assumed, text: text.into() }
}

/// Where a game wants a custom map and what goes with it.
#[derive(Clone, Debug)]
pub struct Target {
    pub fmt: Fmt,
    pub game: &'static str,
    /// Where the map's files go inside the package (forward slashes; `MAP` is replaced by the map's name).
    pub dir: &'static str,
    /// Whether the game needs `MAP.arena` next to the zone.
    pub arena: bool,
    /// Whether the game needs `MAP_const.ff` next to the zone.
    pub const_zone: bool,
    pub rules: Vec<Rule>,
    /// Console commands that start the map (`{}` is the map name) with their basis.
    pub start: Vec<(&'static str, Basis)>,
    /// What is left for the user (never written by this command).
    pub not_done: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Zone,
    Load,
    Const,
    Arena,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Zone => "zone",
            Role::Load => "load zone",
            Role::Const => "const zone",
            Role::Arena => "arena",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    /// Relative to the package folder, forward slashes.
    pub rel: String,
    pub role: Role,
}

/// A map name the game takes as a file name and a console argument: lower case letters, digits and underscores.
pub fn valid_map_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 63 || !name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_') {
        bail!("map name {name:?}: use 1 to 63 characters from a-z, 0-9 and _ (it is a folder and file name, and what the console takes)");
    }
    Ok(())
}

/// The files `map` needs in the package.
pub fn layout(target: &Target, map: &str, has_load: bool, has_const: bool) -> Vec<Planned> {
    let dir = target.dir.replace("MAP", map);
    let mut out = vec![Planned { rel: format!("{dir}/{map}.ff"), role: Role::Zone }];
    if has_load {
        out.push(Planned { rel: format!("{dir}/{map}_load.ff"), role: Role::Load });
    }
    if target.const_zone && has_const {
        out.push(Planned { rel: format!("{dir}/{map}_const.ff"), role: Role::Const });
    }
    if target.arena {
        out.push(Planned { rel: format!("{dir}/{map}.arena"), role: Role::Arena });
    }
    out
}

/// The layout rules of one target game.
pub fn target_for(fmt: Fmt) -> Result<Target> {
    let mut t = Target { fmt, game: "", dir: "usermaps/MAP", arena: false, const_zone: false, rules: Vec::new(), start: Vec::new(), not_done: Vec::new() };
    let iwd_note = "images and sounds the map reads from IWD files are not part of the zone: pack them into usermaps/MAP/MAP.iwd (atlas --packiwd)".to_string();
    match fmt {
        Fmt::Iw3 => {
            t.game = "Call of Duty 4";
            t.rules.push(v("zones of a custom map go to <game>\\usermaps\\MAP\\MAP.ff: iw3mp.exe builds '%s\\usermaps\\%s\\%s.ff' (0x489f96); the folder of the installed map mp_phoenix has MAP.ff, MAP_load.ff and MAP.iwd"));
            t.rules.push(v("when usermaps\\MAP\\MAP.ff exists the game sets fs_game to usermaps/MAP (0x48ba90), so the IWD beside the zone is loaded like a mod"));
            t.rules.push(v("MAP_load.ff is the loading-screen zone; a missing one only logs a warning (0x48aa47), so it is packaged when the source has one beside it"));
            t.start.push(("devmap {}", Basis::Verified));
            t.not_done.push(iwd_note);
        }
        Fmt::Iw4 => {
            t.game = "Modern Warfare 2 (IW4x layout, 32-bit zones)";
            t.arena = true;
            t.rules.push(v("zones of a custom map go to <game>\\usermaps\\MAP\\MAP.ff, with MAP_load.ff and MAP.iwd beside it: iw4x.dll has 'usermaps\\{}\\{}.ff', '_load.ff', '{}.iwd' and scans for them ('Looking for custom maps...', 'Discovered custom map {}')"));
            t.rules.push(v("the client reads usermaps/MAP/MAP.arena (string 'usermaps/{}/{}.arena'); the format is that of the stock mp/basemaps.arena in iw4x_00.iwd (map, longname, gametype, ...)"));
            t.rules.push(a("the client accepts an arena entry with only map, longname and gametype, and shows longname as written (it is not a localized string reference)"));
            t.rules.push(a("a stock x86 iw4mp.exe without IW4x loads the same folder: it has the strings '%s\\%s%s.ff', '_load' and 'usermaps', its loader was not read. A 64-bit iw4mp.exe (2026 Steam build) reads 64-bit zones and cannot take these"));
            t.start.push(("devmap {}", Basis::Verified));
            t.start.push(("map {}", Basis::Assumed));
            t.not_done.push(iwd_note);
        }
        Fmt::Iw5 => {
            t.game = "Modern Warfare 3 (64-bit zones)";
            t.rules.push(a("usermaps\\MAP\\MAP.ff + MAP_load.ff, by analogy with IW3 and IW4x; VERIFIED only that iw5mp.exe has the strings '%s_load', '%s\\%s%s.ff', '_load', 'usermaps' and '.arena' (its loader was not read)"));
            t.rules.push(a("no arena file is written (the MW3 client's way to list a custom map was not checked)"));
            t.rules.push(a("the converter writes the 64-bit MW3 layout; an x86 iw5mp.exe cannot take it"));
            t.start.push(("map {}", Basis::Assumed));
            t.not_done.push(iwd_note);
        }
        Fmt::Codol => {
            t.game = "Call of Duty Online (QA 3.3.3.3, fastfile version 423)";
            t.dir = "client_shipRetail/chinese";
            t.const_zone = true;
            t.rules.push(v("zones go to <game>\\client_shipRetail\\<language>\\MAP.ff (+ MAP_load.ff): every stock map of the QA and 3.19 folders is there, in the 'chinese' folder, and the executable has '%s.ff', '_load', '_const' and 'devmap'"));
            t.rules.push(v("the loader's zone list for a map is MAP_const, MAP (+ patch_MAP) (3.19 build 1649370 at 0x49eaf1; QA at 0x49fe30 builds '%s_const' the same way); a zone file that is missing raises WIN_ZONE_ERROR unless its name starts with 'patch' or 'ez_' or contains '_load' (QA 0x4a0430, read from the code, not run), so MAP_const.ff is written"));
            t.rules.push(v("a const zone holds raw files only: mp/constbaselines/bl_pc_MAP_MODE.bin for each mode in the map's row of the stock table mp/constBaselinesTable.csv, then a marker raw file MAP_const; all 65 stock const zones of the QA folder are laid out byte-identical (inflated image) by atlas from their own raw files, and the stock mp_con_spring_const (the marker alone) is generated byte-identical from nothing"));
            t.rules.push(v("the executable reads a baseline file only for a map that has a row in mp/constBaselinesTable.csv (QA 0x584880, 0x5849f0) and takes zero baselines when the row, the file or its 'IWCB000' header is missing: a custom map has no row, so the marker-only zone atlas writes carries everything the game would read; baselines themselves are snapshots of a running server and cannot be derived"));
            t.rules.push(a("the client accepts a marker-only MAP_const.ff for a map of its own: the stock mp_con_spring ships that way, but no game was started here, and the loader's other users of the zone list were not read"));
            t.rules.push(a("the unsigned zone is accepted (the container is taken from the folder given with --game-folder, whose zones must be QA 3.3.3.3; --signed keeps that folder's signing); a 3.19 (461) folder cannot load a QA-layout zone"));
            t.start.push(("devmap {}", Basis::Verified));
            t.not_done.push("netcode baselines (mp/constbaselines/*.bin) are not made: they are server snapshots; the generated MAP_const.ff has the marker only (a QA SRC_const.ff beside the source is packaged unchanged instead)".into());
            t.not_done.push("the game's map list (mapsTable, localized names) is inside zones and is not touched; start the map from the console".into());
        }
        Fmt::Nx1 => bail!("nx1 is a source only; export to iw3, iw4, iw5 or codol"),
    }
    Ok(t)
}

// ---------------------------------------------------------------------------------------------------------------- the zone

/// What the walker reads out of a zone's assets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    /// X of every `maps/mp/X.d3dbsp` asset.
    pub maps: BTreeSet<String>,
    /// Names of the raw files.
    pub raws: BTreeSet<String>,
}

/// The map and raw file names of an IW3, IW4 or IW5 zone image; `None` for layouts the schema walker does not read.
pub fn zone_names(fmt: Fmt, input_name: &str, image: &[u8], block_sizes: Vec<u32>) -> Option<Names> {
    let game: Game = fmt.game()?;
    let zi = schema_walk::ZoneImage { name: input_name.to_string(), image: image.to_vec(), block_sizes };
    let r = schema_walk::walk(game, &zi);
    let mut n = Names::default();
    for asset in r.assets.iter().filter(|x| !x.reference) {
        if let Some(m) = asset.name.strip_prefix("maps/mp/").and_then(|s| s.strip_suffix(".d3dbsp")) {
            n.maps.insert(m.to_string());
        }
        if game.asset_type_name(asset.ty) == "rawfile" {
            n.raws.insert(asset.name.clone());
        }
    }
    Some(n)
}

fn names_of_input(i: &convert_cli::Input) -> Option<Names> {
    let z = i.zone_image();
    zone_names(i.fmt, &i.name, &i.image, z.block_sizes)
}

/// A zone ready to be written.
pub struct Prepared {
    pub bytes: Vec<u8>,
    /// What the user is told about the route (one line per hop).
    pub route: Vec<String>,
    /// The target reader's verdict on the zone.
    pub verified: String,
    /// Names read from the zone in the target layout, or from the source for Call of Duty Online targets.
    pub names: Option<Names>,
}

/// Reads `src` and makes the bytes of the fastfile the target game takes. `game_folder` is only read.
pub fn prepare(src: &Path, game_folder: Option<&Path>, target: Fmt, signed: bool) -> Result<Prepared> {
    let input = convert_cli::open_input(src, game_folder)?;
    let mut route = vec![format!("{}: {} zone (version {}), {} bytes inflated", src.display(), input.fmt.name(), input.version, input.image.len())];
    let source_names = names_of_input(&input);
    if input.fmt == target && (target != Fmt::Codol || input.version == super::fastfile::VERSION_QA) {
        let verified = convert_cli::verify(target, &input.name, &input.image).context("the source zone does not read cleanly")?;
        route.push(format!("already a {} zone: copied unchanged", target.name()));
        return Ok(Prepared { bytes: std::fs::read(src)?, route, verified, names: source_names });
    }
    if input.fmt == Fmt::Codol && target == Fmt::Codol {
        bail!("no converter between Call of Duty Online builds (the zone is version {}, the target takes {})", input.version, super::fastfile::VERSION_QA);
    }
    if target == Fmt::Codol && game_folder.is_none() {
        bail!("exporting to Call of Duty Online needs --game-folder (a QA 3.3.3.3 folder, read only, for the archive key and container)");
    }
    let (done, steps) = convert_cli::convert(input, target)?;
    for s in &steps {
        route.push(format!("{} -> {}: {} assets out, {} references to left-out assets set to null", s.from.name(), s.to.name(), s.out.assets, s.out.nulled));
        for (w, n) in &s.out.warnings {
            route.push(format!("  warning: {w} (x{n})"));
        }
    }
    let verified = steps.last().map(|s| s.verified.clone()).unwrap_or_default();
    let names = if target == Fmt::Codol { source_names } else { names_of_input(&done) };
    let bytes = convert_cli::encode_fastfile(target, game_folder, &done.image, signed)?;
    Ok(Prepared { bytes, route, verified, names })
}

/// The `MAP_const.ff` of a Call of Duty Online package: the `SRC_const.ff` beside a QA source, copied unchanged after its raw files
/// are read back, else the marker-only zone sealed like the map zone. `game_folder` is only read.
pub fn prepare_const(map: &str, beside: &Path, game_folder: Option<&Path>, target: Fmt, signed: bool) -> Result<Prepared> {
    if beside.is_file() {
        let input = convert_cli::open_input(beside, game_folder)?;
        if input.fmt == Fmt::Codol && input.version == super::fastfile::VERSION_QA {
            let files = super::const_zone::parse(&input.image).with_context(|| format!("{} is not a const zone", beside.display()))?;
            if let Err(e) = super::const_zone::check(map, &files, None) {
                bail!("{}: not the const zone of {map}: {e}", beside.display());
            }
            let baselines = files.len() - 1;
            return Ok(Prepared {
                bytes: std::fs::read(beside)?,
                route: vec![format!("{}: QA const zone of {map} ({baselines} baseline files + marker), copied unchanged", beside.display())],
                verified: String::new(),
                names: None,
            });
        }
    }
    let folder = game_folder.context("the const zone is sealed like the map zone and needs --game-folder")?;
    let image = super::const_zone::marker_only(map);
    let verified = convert_cli::verify(target, &format!("{map}_const"), &image).context("the generated const zone does not read cleanly")?;
    let bytes = convert_cli::encode_fastfile(target, Some(folder), &image, signed)?;
    Ok(Prepared {
        bytes,
        route: vec![format!("generated: the marker raw file {map}_const alone, no baselines ({} bytes inflated); {verified}", image.len())],
        verified,
        names: None,
    })
}

/// The map name: what the zone holds, checked against `--name` and the file name.
pub fn resolve_map(stem: &str, requested: Option<&str>, names: Option<&Names>) -> Result<(String, Vec<String>)> {
    let mut notes = Vec::new();
    let map = match names {
        Some(n) => {
            let Some(first) = n.maps.iter().next() else {
                bail!("the zone has no maps/mp/*.d3dbsp asset: it is not a multiplayer map zone");
            };
            if n.maps.len() > 1 {
                bail!("the zone holds more than one map ({}); pass a zone with one", n.maps.iter().cloned().collect::<Vec<_>>().join(", "));
            }
            if let Some(r) = requested.filter(|r| *r != first) {
                bail!(
                    "--name {r}: the zone's assets are named for {first} (maps/mp/{first}.d3dbsp, its scripts and the {first}_load marker); renaming would mean rewriting names inside the zone, which atlas does not do. Export it as {first}"
                );
            }
            if stem != first {
                notes.push(format!("the file is called {stem} but the zone holds {first}; exporting as {first}"));
            }
            first.clone()
        }
        None => {
            let m = requested.unwrap_or(stem);
            if m != stem {
                bail!("--name {m}: the zone's map names cannot be read here (Call of Duty Online source), and the file is called {stem}; rename the file or use --name {stem}");
            }
            notes.push("the map name inside the zone was not checked (this layout has no reader for it): taken from the file name".into());
            m.to_string()
        }
    };
    valid_map_name(&map)?;
    Ok((map, notes))
}

/// The `.arena` entry written for an IW4x map. Keys and the gametype list are those of the first entry of the stock
/// `mp/basemaps.arena` (iw4x_00.iwd); `longname` is the plain map name.
pub fn arena_text(map: &str) -> String {
    format!("{{\n    map          \"{map}\"\n    longname     \"{map}\"\n    gametype     \"dm dom sd sab war koth oneflag arena dd vip ctf gtnw oitc gg ss conf cranked infected tdef\"\n}}\n")
}

// ---------------------------------------------------------------------------------------------------------------- command

struct Opts {
    src: PathBuf,
    out: PathBuf,
    to: Fmt,
    name: Option<String>,
    game_folder: Option<PathBuf>,
    dry_run: bool,
    force: bool,
    signed: bool,
    no_const: bool,
}

fn usage() -> anyhow::Error {
    anyhow::anyhow!("usage: atlas --exportmap SRC.ff --to iw3|iw4|iw5|codol OUT_DIR [--name MAP] [--signed] [--game-folder FOLDER] [--no-const] [--dry-run] [--force]")
}

fn parse(args: &[String]) -> Result<Opts> {
    let (mut name, mut to, mut game_folder) = (None, None, None);
    let (mut dry_run, mut force, mut signed, mut no_const) = (false, false, false, false);
    let mut positional = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" | "--to" | "--game-folder" => {
                let flag = args[i].clone();
                i += 1;
                let val = args.get(i).with_context(|| format!("{flag} needs a value"))?;
                match flag.as_str() {
                    "--name" => name = Some(val.clone()),
                    "--game-folder" => game_folder = Some(PathBuf::from(val)),
                    _ => to = Some(Fmt::parse(val).with_context(|| format!("unknown game {val:?} (iw3, iw4, iw5 or codol)"))?),
                }
            }
            "--dry-run" => dry_run = true,
            "--force" => force = true,
            "--signed" => signed = true,
            "--no-const" => no_const = true,
            x if x.starts_with("--") => bail!("unknown option {x}"),
            _ => positional.push(PathBuf::from(&args[i])),
        }
        i += 1;
    }
    let [src, out] = <[PathBuf; 2]>::try_from(positional).map_err(|_| usage())?;
    Ok(Opts { src, out, to: to.ok_or_else(usage)?, name, game_folder, dry_run, force, signed, no_const })
}

fn join(root: &Path, rel: &str) -> PathBuf {
    root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR))
}

pub fn command(args: &[String]) -> Result<()> {
    let o = parse(args)?;
    let t = target_for(o.to)?;
    let gf = o.game_folder.as_deref();
    let stem = o.src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if stem.ends_with("_load") || stem.ends_with("_const") {
        bail!("{}: give the map zone itself; its _load zone is packaged from the file beside it", o.src.display());
    }
    println!("target: {}", t.game);
    let main = prepare(&o.src, gf, t.fmt, o.signed)?;
    let (map, notes) = resolve_map(&stem, o.name.as_deref(), main.names.as_ref())?;

    // the loading-screen zone beside the source, when there is one
    let load_path = o.src.with_file_name(format!("{stem}_load.ff"));
    let load = if load_path.is_file() { Some(prepare(&load_path, gf, t.fmt, o.signed)?) } else { None };
    if let Some(l) = &load {
        if let Some(n) = &l.names {
            if !n.raws.contains(&format!("{map}_load")) {
                bail!("{}: its raw file {map}_load is missing; it is not the loading zone of {map}", load_path.display());
            }
        }
    }

    // the const zone: a QA source's own zone beside it, else the marker-only zone (Call of Duty Online only)
    let const_path = o.src.with_file_name(format!("{stem}_const.ff"));
    let konst = if t.const_zone && !o.no_const { Some(prepare_const(&map, &const_path, gf, t.fmt, o.signed)?) } else { None };

    let mut files: Vec<(Planned, Vec<u8>)> = Vec::new();
    for p in layout(&t, &map, load.is_some(), konst.is_some()) {
        let bytes = match p.role {
            Role::Zone => main.bytes.clone(),
            Role::Load => load.as_ref().map(|l| l.bytes.clone()).unwrap_or_default(),
            Role::Const => konst.as_ref().map(|k| k.bytes.clone()).unwrap_or_default(),
            Role::Arena => arena_text(&map).into_bytes(),
        };
        files.push((p, bytes));
    }

    println!("map: {map}");
    for l in &main.route {
        println!("  {l}");
    }
    match &load {
        Some(l) => {
            println!("  load zone {}:", load_path.display());
            for r in &l.route {
                println!("    {r}");
            }
        }
        None => println!("  no {stem}_load.ff beside the source: no loading-screen zone is packaged"),
    }
    if let Some(k) = &konst {
        println!("  const zone:");
        for r in &k.route {
            println!("    {r}");
        }
    } else if t.const_zone {
        println!("  --no-const: MAP_const.ff is left out (the game's zone list asks for it)");
    }
    for n in &notes {
        println!("  note: {n}");
    }
    println!("  checked by the {} reader: {}", t.fmt.name(), main.verified);
    println!("rules (VERIFIED = checked against files on this machine, ASSUMED = community practice or same-engine analogy):");
    for r in &t.rules {
        println!("  [{}] {}", r.basis.tag(), r.text);
    }
    println!("files:");
    for (p, b) in &files {
        println!("  {} ({} bytes, {})", join(&o.out, &p.rel).display(), b.len(), p.role.name());
    }
    for (cmd, basis) in &t.start {
        println!("start it with: {}   [{}]", cmd.replace("{}", &map), basis.tag().trim());
    }
    for n in &t.not_done {
        println!("not done: {}", n.replace("MAP", &map));
    }

    // never overwrite without --force
    let existing: Vec<String> = files.iter().map(|(p, _)| join(&o.out, &p.rel)).filter(|p| p.exists()).map(|p| p.display().to_string()).collect();
    if !existing.is_empty() && !o.force {
        bail!("already there: {}\nnothing written; --force overwrites them", existing.join(", "));
    }
    if o.dry_run {
        println!("--dry-run: nothing written");
        return Ok(());
    }

    let mut written = Vec::new();
    for (p, b) in &files {
        let path = join(&o.out, &p.rel);
        std::fs::create_dir_all(path.parent().unwrap()).with_context(|| path.display().to_string())?;
        std::fs::write(&path, b).with_context(|| path.display().to_string())?;
        println!("wrote {}", path.display());
        written.push((p.role, path));
    }

    // the written zones must read back clean with the target's reader
    for (role, path) in written.iter().filter(|(r, _)| *r != Role::Arena) {
        let again = convert_cli::open_input(path, gf).with_context(|| format!("reopening {} ({})", path.display(), role.name()))?;
        if again.fmt != t.fmt {
            bail!("{}: reads back as {} not {}", path.display(), again.fmt.name(), t.fmt.name());
        }
        let r = convert_cli::verify(again.fmt, &again.name, &again.image).with_context(|| format!("{} does not read cleanly", path.display()))?;
        println!("reopened {}: {r}", path.file_name().unwrap().to_string_lossy());
    }
    println!("copy the contents of {} into the {} folder (nothing was written there); then start the map with: {}", o.out.display(), t.game, t.start[0].0.replace("{}", &map));
    println!("The game has not been started; a clean read here does not show that the game accepts the map.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("atlas-exportmap-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn map_names_are_file_names() {
        assert!(valid_map_name("mp_rust").is_ok());
        for bad in ["", "Mp_Rust", "../x", "a b", "a\\b", &"a".repeat(64)] {
            assert!(valid_map_name(bad).is_err(), "{bad:?}");
        }
    }

    fn rels(t: &Target, load: bool) -> Vec<String> {
        layout(t, "mp_x", load, true).into_iter().map(|p| p.rel).collect()
    }

    #[test]
    fn cod4_and_mw3_go_to_usermaps() {
        let t = target_for(Fmt::Iw3).unwrap();
        assert_eq!(rels(&t, true), ["usermaps/mp_x/mp_x.ff", "usermaps/mp_x/mp_x_load.ff"]);
        assert_eq!(rels(&t, false), ["usermaps/mp_x/mp_x.ff"]);
        assert!(t.rules.iter().all(|r| r.basis == Basis::Verified));
        let m = target_for(Fmt::Iw5).unwrap();
        assert_eq!(rels(&m, true), ["usermaps/mp_x/mp_x.ff", "usermaps/mp_x/mp_x_load.ff"]);
        assert!(m.rules.iter().all(|r| r.basis == Basis::Assumed));
    }

    #[test]
    fn iw4x_gets_an_arena_file_and_says_stock_mw2_is_assumed() {
        let t = target_for(Fmt::Iw4).unwrap();
        assert_eq!(rels(&t, true).last().unwrap(), "usermaps/mp_x/mp_x.arena");
        assert!(t.rules.iter().any(|r| r.basis == Basis::Assumed && r.text.contains("stock x86 iw4mp.exe")));
    }

    #[test]
    fn codol_goes_to_the_stock_zone_folder_with_its_const_zone() {
        let t = target_for(Fmt::Codol).unwrap();
        assert_eq!(rels(&t, true), ["client_shipRetail/chinese/mp_x.ff", "client_shipRetail/chinese/mp_x_load.ff", "client_shipRetail/chinese/mp_x_const.ff"]);
        assert_eq!(layout(&t, "mp_x", false, false).len(), 1, "--no-const and no load zone: the map zone alone");
        assert!(t.rules.iter().any(|r| r.basis == Basis::Verified && r.text.contains("byte-identical")));
        assert!(t.rules.iter().any(|r| r.basis == Basis::Assumed && r.text.contains("marker-only")));
        assert!(t.not_done.iter().any(|n| n.contains("baselines")));
        // the other games have no const zone
        assert_eq!(rels(&target_for(Fmt::Iw4).unwrap(), false), ["usermaps/mp_x/mp_x.ff", "usermaps/mp_x/mp_x.arena"]);
        assert!(target_for(Fmt::Nx1).is_err());
    }

    fn names(maps: &[&str]) -> Names {
        Names { maps: maps.iter().map(|s| s.to_string()).collect(), raws: BTreeSet::new() }
    }

    #[test]
    fn the_map_name_comes_from_the_zone_and_renaming_is_refused() {
        assert_eq!(resolve_map("mp_x", None, Some(&names(&["mp_x"]))).unwrap().0, "mp_x");
        assert_eq!(resolve_map("copy_of_x", None, Some(&names(&["mp_x"]))).unwrap().1.len(), 1);
        assert!(resolve_map("mp_x", Some("mp_y"), Some(&names(&["mp_x"]))).unwrap_err().to_string().contains("renaming"));
        assert!(resolve_map("mp_x", None, Some(&names(&[]))).is_err());
        assert!(resolve_map("mp_x", None, Some(&names(&["a", "b"]))).is_err());
        assert!(resolve_map("mp_x", Some("mp_y"), None).is_err());
        assert_eq!(resolve_map("mp_x", Some("mp_x"), None).unwrap().0, "mp_x");
        assert!(resolve_map("Bad Name", None, None).is_err());
    }

    #[test]
    fn the_arena_entry_has_the_stock_shape() {
        let a = arena_text("mp_x");
        assert!(a.starts_with("{\n") && a.ends_with("}\n"));
        assert!(a.contains("map          \"mp_x\"") && a.contains("longname") && a.contains("gametype"));
    }

    #[test]
    fn the_command_line_needs_a_zone_a_target_and_a_folder() {
        assert!(parse(&args(&["a.ff", "out"])).is_err());
        assert!(parse(&args(&["a.ff", "--to", "iw4"])).is_err());
        assert!(parse(&args(&["a.ff", "--to", "t6", "out"])).is_err());
        assert!(parse(&args(&["a.ff", "--to", "iw4", "out", "--game", "x"])).is_err());
        let o = parse(&args(&["a.ff", "--to", "cod4", "out", "--dry-run", "--force", "--name", "mp_x", "--game-folder", "g"])).unwrap();
        assert!(o.dry_run && o.force && o.to == Fmt::Iw3 && o.name.as_deref() == Some("mp_x") && o.game_folder.is_some() && !o.no_const);
        assert!(parse(&args(&["a.ff", "--to", "codol", "out", "--no-const"])).unwrap().no_const);
    }

    #[test]
    fn a_load_zone_given_as_the_source_is_refused_before_anything_is_read() {
        let t = Tmp::new("loadsrc");
        let out = t.0.join("out");
        let e = command(&args(&[t.0.join("mp_x_load.ff").to_str().unwrap(), "--to", "iw4", out.to_str().unwrap()])).unwrap_err();
        assert!(e.to_string().contains("map zone itself"));
        assert!(!out.exists());
    }
}
