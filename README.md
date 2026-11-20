<div align="center">

# atlas

Open Call of Duty fastfiles, export models and maps to [Cast](https://github.com/dtzxporter/cast) for Blender, and edit zones.

![the Monkey King amusement park map in the Maps tab](docs/images/preview.png)

![an AK-47 viewmodel in the Models tab](docs/images/preview-model.png)

**Models, animations and maps export to [Cast](https://github.com/dtzxporter/cast) and import into Blender.**

[What it does](#what-it-does) · [Tested games](#tested-games) · [Build](#build) · [Usage](#usage) · [Licence](#licence)

</div>

## What it does

- Browse images (IWI), materials, sounds, models, animations and maps.
- Export images as PNG, TGA, TIFF or DDS, with normal maps repaired for OpenGL or DirectX.
- Play and export the loaded sounds of Call of Duty 4, Modern Warfare 2 and Modern Warfare 3 zones as WAV, no game running.
- Export models and animations to Cast for Blender, or to glTF, OBJ, SEModel and SEAnim, in the unit you pick, with animations baked to a key per frame if you want.
- Dump string tables, localize strings and raw files to CSV and JSON.
- Unpack a zone to files, edit them, build it again (Call of Duty Online, Black Ops II).
- Convert zones between Call of Duty 4, Modern Warfare 2, Modern Warfare 3 and Call of Duty Online (`--convertzone SRC.ff --to GAME`).
- Check whether a zone reads cleanly (CoD4, MW2, MW3, Black Ops II).
- Pack IWDs, convert DDS to IWI.

> [!NOTE]
> Nothing atlas converts or writes has been tried in a game. The Black Ops II zones it writes are unsigned and the game may refuse them.

## Tested games

- Call of Duty Online, all builds
- Black Ops II
- Modern Warfare 2
- Call of Duty 4
- Modern Warfare 3 (the 64-bit build)
- Future Warfare, Xbox 360 development build (NX1), zones only
- Call of Duty HQ (`cod26`), containers only, no decoding

## Build

Windows: download the zip from [Releases](https://github.com/kypwny/atlas/releases) and run `atlas.exe`. To build it yourself:

Prerequisites: Rust 1.95 or newer, from [rustup.rs](https://rustup.rs). On Windows pick the MSVC toolchain, which needs the Visual Studio Build Tools (the C++ workload).

```bash
cargo build --release
```

Run `atlas` and drop a game folder on the window, or `atlas GAME_FOLDER`. A zone folder like `zone/all` works too.

Call of Duty Online 3.19 `.ifs` archives also need the helper next to the program:

```bash
rustup target add i686-pc-windows-msvc
cd tools/ifs_helper && cargo build --release --target i686-pc-windows-msvc
```

> [!WARNING]
> Call of Duty HQ needs `oo2core_8_win64.dll` from the game's own folder. atlas doesn't ship it.

## Usage

### Export to Cast and import into Blender

1. In Blender 3.6 or newer, install the Cast add-on from [dtzxporter/cast](https://github.com/dtzxporter/cast/releases): Edit > Preferences > Add-ons > Install from Disk, then enable **Cast Support**.
2. In atlas: open the game folder, Models tab, select the model, tick **Cast**. For animations, type their names (or `prefix*`) into **animations**. Export.
3. In Blender: File > Import > Cast (.cast) and pick `xmodel/<name>.cast`. You get the armature, meshes, materials and the animations as actions. Textures are PNGs in `images/`.

Command line:

```text
atlas --buildindex GAME_FOLDER index.tsv
atlas --exportmodel GAME_FOLDER viewmodel_ak47 OUT_DIR --index index.tsv --format cast --anim "viewmodel_ak47_*"
```

`--unit inches|cm|m` and `--scale S` pick the unit of the positions, `--bake` keys every frame. In the Models tab the same are next to the formats.

Black Ops II writes each animation as its own `xanim/<name>.cast`. Import the model first, then the animations with the armature selected:

```text
atlas --t6export ZONE.ff OUT_DIR --format cast --anims "pb_stand_*" --images --ipak IPAK_DIR
```

### Sounds

Sounds tab, pick a zone, click a sound to play it. Space plays or stops, Up and Down audition, E exports, Ctrl+C copies the name. Search takes `length:>2`, `rate:44100`, `channels:2`, `!word`.

```text
atlas --exportsound GAME_FOLDER|ZONE.ff OUT_DIR [--filter "length:>1 !amb"]
```

Sounds come from the zone files, not from a running game. Streamed sounds (`.iwd`) and Black Ops II sound banks are not read yet. PCM and Microsoft/IMA ADPCM play and export as WAV; other codecs export as the bare data.

### Look at a map

Maps tab, pick a zone, View map. Drag rotates, right-drag pans, wheel zooms, Shift for fine. The Lights box shows the sun and the omni, spot and hero lights.

### Edit a raw file or string table

```text
atlas --unzone GAME_FOLDER ZONE.ff OUT_DIR
# edit the files in OUT_DIR; delete the ones you want removed
atlas --buildzone GAME_FOLDER OUT_DIR NEW.ff
```

Black Ops II: `--t6unzone ZONE.ff OUT_DIR` and `--t6buildzone OUT_DIR NEW.ff`. If another asset points into one you changed, it names both and stops.

### Dump the tables

```text
atlas --exportdata GAME_FOLDER OUT_DIR
```

### MW2 map to Call of Duty Online

```text
atlas --convertzone MW2.ff GAME_FOLDER OUT.ff
```

`--convertzone check MW2.ff` converts and runs the reader over the result.

### Export a map

```text
atlas --exportmap SRC.ff --to iw3|iw4|iw5|codol OUT_DIR [--dry-run]
```

Converts the zone and writes a folder to copy into the game: `usermaps/MAP` for Call of Duty 4, IW4x and Modern Warfare 3, `client_shipRetail/chinese` for Call of Duty Online QA (zone, `_load` and a generated `_const` zone with the marker file only, no netcode baselines; `--no-const` skips it). It prints which layout rules are verified and which assumed. Nothing is written into a game folder or tried in a game.

### Check a zone

```text
atlas --zonewalk iw3|iw4|iw5|t6 FILE_OR_FOLDER
```

Prints `exact`, or the asset, offset and reason where it stopped.

`atlas --help` lists every command.

## Licence

GPL-3.0, see [LICENSE](LICENSE). Credits in [ATTRIBUTION.md](ATTRIBUTION.md). No game assets included. Not affiliated with Tencent, Activision or Treyarch.
