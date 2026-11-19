# Attribution

atlas is licensed GPL-3.0-only (see LICENSE).

- The idea and the feature set of the browsers (searchable thumbnail grid, per-channel
  and per-mip inspection, PNG export, grouping of duplicate versions) follow the WaW tool
  family by **phil81334** (https://github.com/phil81334, GPL-3.0). No code from those
  repositories has been copied yet; if any is, it is listed here with its revision.
- `src/wavelet.rs` is a port of `IwiWaveletDecoder.cpp` from **OpenAssetTools** by Laupetin
  and contributors (https://github.com/Laupetin/OpenAssetTools, GPL-3.0). The Huffman
  tables in `src/wavelet_tables.rs` are generated from that file by
  `scripts/gen_wavelet_tables.py`. The IWI header/flag/format layout in `src/iwi.rs` was
  checked against OpenAssetTools' `IwiTypes.h` and `IwiLoader.cpp` (revision
  7b24b776faf369480963b6fda44af9c131faf3c1) and against the files themselves.
- The streamed model container layout in `src/model.rs` comes from the project owner's own loader notes
  (`codol_stream_model.c`, `codol_xsurface.c` in the server repository) and was checked against the files.
- `src/zone/fastfile.rs` and `src/zone/replay*.rs` are Rust ports of `codol_fastfile.c` and `codol_load_blocks.c` from the
  project owner's own server code (codol-qa-server). The owner holds the copyright of both and licenses these ports under the
  GPL-3.0 like the rest of the program. Layouts and field offsets come from that code, which was derived from the game's own loaders.
- The IPAK container layout in `src/ipak.rs` (block header, command kinds) was first learned from reading
  **IwIUniversalTool** by Rastishka225 (https://github.com/Rastishka225/IwIUniversalTool, MIT, commit fcc8041) and then
  re-derived and checked against all 23,134 images in the installed Black Ops II IPAKs. No code was copied; the LZO1X
  decoder in `src/lzo.rs` is written from the public stream format. The Black Ops II IWI (version 27) header layout in
  `src/iwi.rs` was measured from those files.
- The model export in `src/export/` follows the conventions of **OpenAssetTools**' writers (`ObjWriting/XModel`, revision 4aa783b31f251d1201ede2a261d0bc188e4793c7;
  GPL-3.0): the (x, z, -y) axis change and reversed triangle order for glTF/OBJ, the XMODEL_EXPORT version 6 layout, and
  the rigid/blend skin-weight layout of the IW4 surface format (`XModelToCommonConverter`). The code is our own. SEModel was
  read from the reference dumps and checked byte for byte. The layout of XMODEL_EXPORT and the exporter feature set were also
  The animation decoding in `src/anim.rs` follows OpenAssetTools' `FlatXAnimReader.cpp` (which array each value comes from, the
  16-bit frame-number checkpoints) and the IW4 `XAnimParts` layout; quantisation scales and the SEAnim layout were checked
  against the reference dumps. The layout of XMODEL_EXPORT and the exporter feature set were also
  cross-checked against the strings of Tom_Bmx XmodelUtils (a closed Windows tool; only read, never run).
- The stock IW4 layouts in `src/zone/stock.rs` and the stock branches of the zone walkers, were worked out against real MW2 zones, using the IW4 struct definitions of **ZoneTool**
  (https://github.com/ZoneTool/zonetool, GPL-3.0, revision aa4475b, `src/IW4/Structs.hpp` and `src/CODO/Structs.hpp`) and the
  zone code descriptions of **OpenAssetTools** (`src/ZoneCode/Game/IW4/XAssets`, GPL-3.0) for sizes, offsets and load order. The
  code is our own; `tools/layout` only compiles ZoneTool's headers to read sizes and offsets out of them.
- `src/zone/schema_walk.rs`, `tools/gen_zone_schema.py` and `src/zone/schema/*.json` (zone walker for Black Ops II, Call of Duty 4,
  Modern Warfare 2 and 3): the structure definitions (`<game>_Assets.h`) and the zone loading rules (`<game>_Commands.txt`, `XAssets/*.txt`) are those of **OpenAssetTools**
  (Laupetin and contributors, https://github.com/Laupetin/OpenAssetTools, GPL-3.0). The schema file is generated from them (layout
  and rules in compact form, no source text), and the way the interpreter applies the rules (block stack, `Alloc`/`Load`
  semantics, reusable and insert pointers, dynamic arrays, member reordering, union conditions) follows the loaders that its
  zone code generator emits (`ZoneLoadTemplate.cpp`, `ZoneInputStream.cpp`). The code is our own.
- `src/zone/t6.rs`, `src/zone/t6_write.rs`, `src/zone/t6_rebuild.rs` (Black Ops II fastfile container, writer and editor): the
  container (Salsa20 with the per-stream SHA-1 hash chain, raw deflate, the 0x8000-byte chunk rules) was implemented from
  **OpenAssetTools**' `ZoneLoading`, `ZoneWriting` and `ZoneCommon/Zone/XChunk` (GPL-3.0); the constants are those it publishes.
  The code is our own.
- The 32-byte Salsa20 key in `src/zone/t6.rs` (`SALSA20_KEY`) that decrypts Black Ops II PC fastfiles is the constant published by
  **OpenAssetTools** (`ZoneConstants::SALSA20_KEY_TREYARCH_PC`, GPL-3.0). It is the same for every zone and is needed to read
  them at all; atlas uses it to decrypt and to re-encrypt the zones it writes, and does not verify or make Treyarch's RSA signatures.
- `src/zone/hq.rs`, `src/kapi.rs`, `src/oodle.rs` (Call of Duty HQ containers): the container facts that were taken from outside
  (the `IWffa100` block header idea, the `KAPI` entry table, loading the game's own Oodle DLL) come from the public design notes of
  **harmony** (https://github.com/g-rl/harmony, no licence stated, so none of its code was read or used); everything was
  re-measured on the installed files.
- `tools/blender_cast_check.py` runs Blender with dtzxporter's **Cast** add-on (MIT, https://github.com/dtzxporter/cast) headless; the
  Cast writer in `src/export/cast.rs` follows that project's format description and the add-on's property type ids.
- The 64-bit Modern Warfare 3 corrections in `tools/gen_zone_schema.py` (`FIXES`) were read from the game's own loader functions by
  static analysis of the installed executable (never run); the Modern Warfare 2 to CoDOL converter (`src/zone/convert*.rs`) uses
  field names from ZoneTool's and OpenAssetTools' IW4 and IW5 structures, and every CoDOL-side value was measured on real QA zones.
- `tools/ifs_helper` calls the game's own `IFS2.dll` by function position and calling convention, which come from Maurice Heumann's
  **ifs-tool** (https://github.com/momo5502/ifs-tool, MIT). The helper contains none of its code and ships without the DLL.
- The data tables (string tables, localize, raw files and weapon tables to CSV and JSON) follow what EthanC's **Hyde** (MIT, https://github.com/EthanC/Hyde)
compiles from **Jekyll** dumps; no code from either was used.
- Built with egui/eframe and wgpu (MIT/Apache-2.0), glam, bytemuck, pollster, rsa, aes, sha2, flate2, `image`, `zip`, `rfd`, `dirs`, `anyhow`.

The program contains no game assets. Files read from your installation remain the property
of their owners. Not affiliated with Tencent, Activision or Treyarch.
