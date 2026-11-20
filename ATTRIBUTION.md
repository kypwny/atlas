# Attribution

atlas is GPL-3.0-only (see LICENSE). The program contains no game assets, and the files it reads from your installation remain the
property of their owners. Not affiliated with Tencent, Activision or Treyarch.

## Code ported or adapted

- **OpenAssetTools** (Laupetin and contributors, GPL-3.0, https://github.com/Laupetin/OpenAssetTools)
  - `src/wavelet.rs` is a port of `IwiWaveletDecoder.cpp`; `src/wavelet_tables.rs` is generated from it (`scripts/gen_wavelet_tables.py`).
  - The Black Ops II fastfile container (`src/zone/t6*.rs`) follows its zone loading and writing code, and `SALSA20_KEY` is the constant it publishes. atlas does not make Treyarch's RSA signatures.
  - The zone walker (`src/zone/schema_walk.rs`, `src/zone/schema/*.json`) uses its structure definitions and loading rules, in compact generated form.
  - The model, animation and IWI layouts in `src/export/`, `src/anim.rs` and `src/iwi.rs` were checked against its code. The writers are our own.
- **The project owner's own server code** (codol-qa-server): `src/zone/fastfile.rs`, `src/zone/replay*.rs` and `src/model.rs` are ports of it, licensed GPL-3.0 here.

## Formats and layouts learned from other projects (no code copied)

- **ZoneTool** (GPL-3.0, https://github.com/ZoneTool/zonetool): IW4 structure definitions behind `src/zone/stock.rs` and the stock zone walkers.
- **IwIUniversalTool** by Rastishka225 (MIT, https://github.com/Rastishka225/IwIUniversalTool): the IPAK container layout (`src/ipak.rs`), re-checked against the installed files.
- **harmony** by g-rl (https://github.com/g-rl/harmony, no licence stated, so only its public design notes were read):
  the Call of Duty HQ container facts (`src/zone/hq.rs`, `src/kapi.rs`, `src/oodle.rs`) and the idea and scope of the Sounds tab.
- **HashIndex** by ate47 (MIT, https://github.com/ate47/HashIndex): the `.wni` format and name hash (`src/hashindex.rs`). No index data is bundled.
- **ifs-tool** by Maurice Heumann (MIT, https://github.com/momo5502/ifs-tool): how to call the game's own `IFS2.dll` (`tools/ifs_helper`). The DLL is not shipped.
- **Cast** by dtzxporter (MIT, https://github.com/dtzxporter/cast): the Cast format description behind `src/export/cast.rs`; `tools/blender_cast_check.py` runs it headless.
- **Hyde** by EthanC and **Jekyll** (MIT): what the data tables export (`src/data.rs`).
- **Greyhound** (Scobalula) and **WraithXArchon** (DTZxPorter), both GPL-3.0: the loaded sound record and the WAV, FLAC and ADPCM header values used by `src/sound.rs`.
- **Black Ops II Sound Studio** by master131 (MIT): the Black Ops II sound bank layout (not read yet).
- **Saluki** by echo000 and dest1yo (https://github.com/echo000/saluki-releases): the feature set that the search words, image export options, material export, model units and bake, export marks and camera controls follow. Its README and guide were read only.
- **phil81334**'s WaW tool family (GPL-3.0): the idea and feature set of the image browser. No code copied.
- Tom_Bmx XmodelUtils (closed): its strings were read to cross-check the XMODEL_EXPORT layout. It was never run.

## Measured here

The Modern Warfare 3 corrections in `tools/gen_zone_schema.py` were read from the game's own loader functions by static analysis of the
installed executable (never run). Every CoDOL-side value in the zone converters was measured on real QA zones.

## Built with

egui/eframe, wgpu, rodio, glam, bytemuck, pollster, rsa, aes, sha2, flate2, `image`, `zip`, `rfd`, `dirs`, `anyhow` (MIT or Apache-2.0).
