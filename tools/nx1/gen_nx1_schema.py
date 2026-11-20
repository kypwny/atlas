#!/usr/bin/env python3
"""Builds src/zone/schema/nx1.json, the zone walker's rules for the Xbox 360 development build of Future Warfare (NX1).

The structure layouts (size and member offsets of every asset structure) are read from the build's program database with
`pdb_types.py`. The loading rules (counts, conditions, blocks, strings, reusable pointers) are `NX1_Rules.txt`, made by
`loader_rules.py` from the build's own loader functions, and `NX1_Commands.txt`, written by hand for what that tool cannot
translate. The rule language and the schema format are those of `gen_zone_schema.py` (OpenAssetTools' zone code generator).

usage: gen_nx1_schema.py PDB_FILE [OAT_SRC_DIR] [OUT.json]
"""
import importlib.util
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PDB = sys.argv[1]
OAT = sys.argv[2] if len(sys.argv) > 2 else r"D:\OpenAssetTools-src\src"
OUT = sys.argv[3] if len(sys.argv) > 3 else os.path.join(HERE, "..", "..", "src", "zone", "schema", "nx1.json")

# the generator for the other games is used as a library (its argument parsing runs at import time)
_argv = sys.argv
sys.argv = ["gen_zone_schema.py", "IW4", OAT, os.devnull]
spec = importlib.util.spec_from_file_location("gz", os.path.join(HERE, "..", "gen_zone_schema.py"))
gz = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gz)
sys.argv = _argv
sys.path.insert(0, HERE)
import pdb_types  # noqa: E402

# asset type number (the NX1 XAssetType enum) -> structure that holds it
ASSETS = {
    "PHYSPRESET": "PhysPreset", "PHYSCOLLMAP": "PhysCollmap", "XANIMPARTS": "XAnimParts", "XMODEL_SURFS": "XModelSurfs",
    "XMODEL": "XModel", "MATERIAL": "Material", "PIXELSHADER": "MaterialPixelShader", "TECHNIQUE_SET": "MaterialTechniqueSet",
    "IMAGE": "GfxImage", "SOUND": "SndAliasList", "SND_VOLUME_FALLOFF_CURVE": "SndVfCurve", "SND_LPF_CURVE": "SndLpfCurve",
    "LOADED_SOUND": "LoadedSound", "CLIPMAP_SP": "clipMap_t", "CLIPMAP_MP": "clipMap_t", "COMWORLD": "ComWorld",
    "GAMEWORLD_SP": "GameWorldSp", "GAMEWORLD_MP": "GameWorldMp", "MAP_ENTS": "MapEnts", "FXWORLD": "FxWorld",
    "GFXWORLD": "GfxWorld", "LIGHT_DEF": "GfxLightDef", "FONT": "Font_s", "MENULIST": "MenuList", "MENU": "menuDef_t",
    "LOCALIZE_ENTRY": "LocalizeEntry", "WEAPON": "WeaponCompleteDef", "SNDDRIVER_GLOBALS": "SndDriverGlobals",
    "FX": "FxEffectDef", "IMPACT_FX": "FxImpactTable", "SURFACE_FX": "SurfaceFxTable", "RAWFILE": "RawFile",
    "STRINGTABLE": "StringTable", "LEADERBOARD": "LeaderboardDef", "STRUCTURED_DATA_DEF": "StructuredDataDefSet",
    "TRACER": "TracerDef", "LASER": "LaserDef", "VEHICLE": "VehicleDef", "ADDON_MAP_ENTS": "AddonMapEnts",
    "SHOCKFILE": "ShellShockParams", "VOLUMESETTING": "AudioVolumeSetting", "REVERBPRESET": "RadVerbPreset", "FOG": "FogDef",
}
# names the Modern Warfare 2 rules use for structures NX1 calls differently
ALIASES = {"snd_alias_list_t": "SndAliasList"}
PRIM = {"char": "char", "unsigned char": "unsigned char", "int8_t": "char", "uint8_t": "unsigned char", "short": "short",
        "unsigned short": "unsigned short", "int": "int", "unsigned int": "unsigned int", "long": "int",
        "unsigned long": "unsigned int", "float": "float", "double": "double", "bool": "bool", "int64_t": "long long",
        "uint64_t": "unsigned long long", "wchar_t": "unsigned short", "bool16": "unsigned short", "bool32": "unsigned int",
        "void": "void"}


class Builder:
    def __init__(self, types):
        self.t = types
        self.hp = gz.HeaderParser("")
        self.defs = self.hp.defs
        self.anon = 0
        self.names = {}  # pdb type name -> def name

    def prim(self, name):
        return self.defs[PRIM.get(name, "int")]

    def def_name(self, n):
        if n in self.names:
            return self.names[n]
        if "<" in n or "::" in n:
            self.anon += 1
            out = "$nx%d" % self.anon
        else:
            out = n
        self.names[n] = out
        return out

    def type_def(self, d):
        """Def for the innermost named type of a type tree."""
        k = d["k"]
        if k == "prim":
            return self.prim(d["n"])
        if k in ("struct", "union"):
            return self.struct_def(d["n"])
        if k == "enum":
            return self.enum_def(d["n"])
        if k in ("ptr", "arr", "bits"):
            return self.type_def(d["to"] if k == "ptr" else d["of"])
        return self.defs["void"]

    def enum_def(self, n):
        name = self.def_name(n)
        if name in self.defs:
            return self.defs[name]
        info = self.t.struct_info(n)
        parent = {1: "unsigned char", 2: "unsigned short"}.get(info["size"], "int")
        e = gz.Def("enum", name, parent=parent)
        e.size = e.align = info["size"]
        e.calc = True
        self.defs[name] = e
        return e

    def mods_of(self, d):
        """Declaration modifiers outermost first, as the header parser writes them."""
        mods = []
        while d["k"] in ("ptr", "arr", "bits"):
            if d["k"] == "ptr":
                if d["to"]["k"] == "func":
                    mods.append(("p",))
                    return mods
                mods.append(("p",))
                d = d["to"]
            elif d["k"] == "arr":
                mods.append(("a", d["n"]))
                d = d["of"]
            else:
                d = d["of"]
        return mods

    def struct_def(self, n):
        name = self.def_name(n)
        if name in self.defs:
            return self.defs[name]
        info = self.t.struct_info(n)
        d = gz.Def(info["kind"], name)
        d.calc = True
        d.size = info["size"]
        self.defs[name] = d
        align = 1
        for m in info["members"]:
            base = self.type_def(m["t"])
            mods = self.mods_of(m["t"])
            decl = gz.Decl(base, mods)
            bits = m["t"]["k"] == "bits"
            decl.size = m["sz"]
            if any(x[0] == "p" for x in mods):
                decl.align = 4
            else:
                a = base.align if base.kind != "base" else base.size
                decl.align = max(a, 1)
            if bits:
                decl.bits = m["t"]["len"]
            decl.forced = False
            mem = gz.Member(m["n"], decl)
            mem.offset = m["o"]
            d.members.append(mem)
            align = max(align, decl.align)
        d.align = min(align, 8)
        return d


def closure_defs(types):
    b = Builder(types)
    for name in sorted(set(ASSETS.values())):
        b.struct_def(name)
    # the other structures the rules mention
    for name in ("XModelSurfs", "XSurface", "WeaponDef"):
        if name in types.defs:
            b.struct_def(name)
    return b


class Tolerant(gz.CmdParser):
    """The Modern Warfare 2 statements that name something NX1 does not have are skipped."""

    skipped = []

    def run(self):
        while self.p < len(self.t):
            start = self.p
            kw = self.next()
            try:
                if kw in ("game", "wordsize"):
                    while self.next() != ";":
                        pass
                elif kw == "asset":
                    st = self.next()
                    an = self.next()
                    self.expect(";")
                    if st in self.c.infos:
                        self.c.infos[st].asset = an
                elif kw == "block":
                    kind = self.next()
                    name = self.next()
                    dflt = False
                    if self.peek() == "default":
                        self.next()
                        dflt = True
                    self.expect(";")
                    self.c.blocks.append((kind, name, dflt))
                elif kw == "use":
                    n = self.typename()
                    self.expect(";")
                    self.c.in_use = None
                    self.c.in_use = self.c.infos[n]
                elif kw == "reorder":
                    self.reorder()
                elif kw == "set":
                    self.set_cmd()
                else:
                    raise SyntaxError("unknown command %r" % kw)
            except (gz.SkipCmd, SyntaxError, KeyError, IndexError, ValueError, TypeError, AttributeError) as e:
                self.p = start
                while self.next() != ";":
                    pass
                Tolerant.skipped.append((" ".join(self.t[start:self.p]), str(e)[:80]))


def pdb_enums(types):
    """Every enumerator of the program database (the rules compare members with them)."""
    out = {}
    for name in types.defs:
        kind, _b = types.recs[types.defs[name]]
        if kind == 0x1507:
            for n, v in types.struct_info(name)["values"]:
                out.setdefault(n, v)
    return out


def main():
    types = pdb_types.Types(PDB)
    b = closure_defs(types)
    defs = b.defs
    for a, real in ALIASES.items():
        defs[a] = defs[real]
    # block names the rules use: the NX1 header sizes six blocks
    enum_vals = dict(pdb_enums(types))
    enum_vals.update({"XFILE_BLOCK_TEMP": 0, "XFILE_BLOCK_PHYSICAL": 1, "XFILE_BLOCK_RUNTIME": 2, "XFILE_BLOCK_VIRTUAL": 3,
                 "XFILE_BLOCK_LARGE": 4, "XFILE_BLOCK_RUNTIME_PHYSICAL": 5, "XFILE_BLOCK_VERTEX": 1, "XFILE_BLOCK_INDEX": 1})
    gz.ENUMS = enum_vals
    cmd = gz.Cmd(defs)
    for a, real in ALIASES.items():
        cmd.infos[a] = cmd.infos[real]
    toks = []
    for name in ("NX1_Rules.txt", "NX1_Commands.txt"):
        path = os.path.join(HERE, name)
        if os.path.exists(path):
            toks += gz.lex_cmd(open(path, encoding="utf-8").read())
    parser = Tolerant(cmd, toks, enum_vals)
    parser.run()
    if "-v" in sys.argv:
        for s, e in Tolerant.skipped:
            print("skipped:", s[:150], "|", e)
    print("skipped statements:", len(Tolerant.skipped))
    for key, struct in ASSETS.items():
        cmd.infos[struct].asset = "Asset" + struct
    infos = gz.compute_flags(cmd)
    out = gz.emit(cmd, infos, enum_vals)
    enum = sorted((v, n) for n, v in types.struct_info("XAssetType")["values"])
    asset_names = []
    for v, n in enum:
        if n == "ASSET_TYPE_COUNT":
            break
        asset_names.append(n)
    amap = {}
    for i, n in enumerate(asset_names):
        key = n.replace("ASSET_TYPE_", "")
        if key in ASSETS:
            amap[str(i)] = cmd.infos[ASSETS[key]].sid
    out["assets"] = amap
    out["skip"] = []
    out["asset_names"] = asset_names + ["ASSET_TYPE_COUNT"]
    out["blocks"] = [
        {"name": "XFILE_BLOCK_TEMP", "kind": 0, "default": 1}, {"name": "XFILE_BLOCK_PHYSICAL", "kind": 3, "default": 0},
        {"name": "XFILE_BLOCK_RUNTIME", "kind": 1, "default": 1}, {"name": "XFILE_BLOCK_VIRTUAL", "kind": 3, "default": 1},
        {"name": "XFILE_BLOCK_LARGE", "kind": 3, "default": 0}, {"name": "XFILE_BLOCK_RUNTIME_PHYSICAL", "kind": 1, "default": 0, "unchecked": 1}]
    out["bits"] = 4
    out["insert"] = 3
    out["game"] = "NX1"
    out["ptr"] = 4
    out["be"] = 1
    out["enums"] = {}
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        json.dump(out, f, separators=(",", ":"))
    print("structs:", len(out["structs"]), "bytes:", os.path.getsize(OUT))


if __name__ == "__main__":
    main()
