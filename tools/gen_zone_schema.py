#!/usr/bin/env python3
"""Builds src/zone/t6_schema.json for the T6 zone walker (src/zone/t6_walk.rs).

Reads the Black Ops II structure definitions (T6_Assets.h) and the zone loading rules (T6_Commands.txt and XAssets/*.txt) of
OpenAssetTools (GPL-3.0, https://github.com/Laupetin/OpenAssetTools) and writes one compact JSON file: every struct and union
with its x86 layout (size, alignment, member offsets, declaration modifiers) and the loading rules attached to the members
(strings, counts, conditions, blocks, reusable pointers, alignment, ordering).  The layout and the rule semantics follow the
zone code generator of that project; this script is an independent re-implementation of the parts that are needed.

usage: gen_zone_schema.py [T6|IW3|IW4|IW5] [OAT_SRC_DIR] [OUT.json] [--x64]  (--x64: 8-byte pointers, for the 64-bit builds)
"""
import json
import os
import re
import sys

ARGS = [a for a in sys.argv[1:] if not a.startswith("--")]
X64 = "--x64" in sys.argv
GAME = (ARGS[0] if len(ARGS) > 0 else "T6").upper()
OAT = ARGS[1] if len(ARGS) > 1 else r"D:\OpenAssetTools-src\src"
OUT = ARGS[2] if len(ARGS) > 2 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src", "zone", "schema", GAME.lower() + ".json")
HDR = os.path.join(OAT, "Common", "Game", GAME, GAME + "_Assets.h")
GAME_H = os.path.join(OAT, "Common", "Game", GAME, GAME + ".h")
CMD_DIR = os.path.join(OAT, "ZoneCode", "Game", GAME)
LOADER = os.path.join(OAT, "ZoneLoading", "Game", GAME, "ContentLoader" + GAME + ".cpp")
CONSTANTS = os.path.join(OAT, "ZoneCommon", "Game", GAME, "ZoneConstants" + GAME + ".h")
POINTER = 8 if X64 else 4
DEFAULT_PACK = 8
ENUMS = {}
# Deviations of a build from the structure definitions: (game, 64-bit) -> structure -> operations on its member order
FIXES = {
    # The 64-bit Modern Warfare 3 build. Sources: the structure definitions (32-bit retail), and the engine's own Load functions
    # (iw5mp.exe, read statically) for every deviation below: sizes, offsets of the pointers and the counts it loads with.
    ("IW5", True): {
        # ZoneTool's retail MW3 structure: memUsage, bool bad, physPreset, physCollmap, one value; OAT has a pointer (invHighMipRadius)
        # in the place of `bad`, which carries no data in the stream
        "XModel": [("swap", "memUsage", "invHighMipRadius"), ("never", "invHighMipRadius")],
        # the engine loads 0x80 bytes: there is no subMaterials pointer
        "Material": [("remove", "subMaterials")],
        # channel maps are {count, pointer to 8-byte entries} (12 bytes, 4-byte packing), the entries follow the speaker map
        "MSSChannelMap": [("members", "unsigned char speakerCount; unsigned char pad[3]; MSSSpeakerLevels* speakers;", 4), ("cmd", "set count speakers speakerCount;")],
        "MSSSpeakerLevels": [("members", "int speaker; float level;")],
        # the sound data header is wave-format-like data (format, channels, rate, ...), 56 bytes, the length at +24, the data pointer at +48
        "MssSound": [("members", "unsigned short format; unsigned short channels; unsigned int sampleRate; unsigned int byteRate; unsigned short blockAlign; unsigned short bits; unsigned int unknown16; unsigned int unknown20; unsigned int data_len; unsigned char pad[20]; char* data;"), ("cmd", "set count data data_len;")],
        # water: three float arrays of N * M (the engine loads 0x60 bytes, the image pointer at +88; OAT has two arrays, the first of complex values)
        "water_t": [("members", "WaterWritable writable; float* H0; float* wTerm; float* H1; int M; int N; float Lx; float Lz; float gravity; float windvel; float winddir[2]; float amplitude; float codeConstant[4]; GfxImage* image;"), ("cmd", "set count H1 N * M;")],
        # glass piece dynamics are 48 bytes in the engine (OAT: 36)
        "FxGlassPieceDynamics": [("append", "float unknown[3];")],
        # the engine loads 0x200 bytes (OAT: 488): 24 more bytes of unused data at the end
        "clipMap_t": [("resize", "padding", 44)],
    },
}

# ---------------------------------------------------------------------------------------------------------------- header
BASE = {  # name -> (size, scalar kind)
    "float": (4, "f32"), "double": (8, "f64"), "bool": (1, "u8"), "char": (1, "i8"), "unsigned char": (1, "u8"), "signed char": (1, "i8"),
    "short": (2, "i16"), "unsigned short": (2, "u16"), "int": (4, "i32"), "unsigned int": (4, "u32"), "long": (4, "i32"),
    "unsigned long": (4, "u32"), "long long": (8, "i64"), "unsigned long long": (8, "u64"), "void": (0, "u8"),
    "uint8_t": (1, "u8"), "int8_t": (1, "i8"), "uint16_t": (2, "u16"), "int16_t": (2, "i16"), "uint32_t": (4, "u32"),
    "int32_t": (4, "i32"), "uint64_t": (8, "u64"), "int64_t": (8, "i64"),
}


def preprocess(text):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    defined = {"__zonecodegenerator"}
    stack = []
    active = True
    out = []
    for line in text.split("\n"):
        line = re.sub(r"//.*", "", line)
        s = line.strip()
        if s.startswith("#"):
            m = re.match(r"#\s*(\w+)\s*(.*)", s)
            d, rest = m.group(1), m.group(2).strip()
            if d == "ifndef":
                stack.append((active, rest not in defined))
                active = active and rest not in defined
            elif d == "ifdef":
                stack.append((active, rest in defined))
                active = active and rest in defined
            elif d == "else":
                parent, cond = stack[-1]
                stack[-1] = (parent, not cond)
                active = parent and not cond
            elif d == "endif":
                parent, _ = stack.pop()
                active = parent
            elif d == "pragma" and active:
                m2 = re.match(r"pack\s*\(\s*(push|pop)\s*(?:,\s*(\d+))?\s*\)", rest)
                if m2:
                    out.append(" __pack_push %s ; " % m2.group(2) if m2.group(1) == "push" else " __pack_pop ; ")
            continue
        if active:
            out.append(line)
    text = "\n".join(out)
    text = re.sub(r"\bvolatile\b", " ", text)
    if X64 and "--keepalign" not in sys.argv:
        # the 32 variants of the alignment macros only apply to 32-bit builds
        text = re.sub(r"\b(?:type_align|tdef_align|gcc_align)32\(\s*\d+\s*\)", " ", text)
    text = re.sub(r"\b(?:type_align|tdef_align|gcc_align)(?:32|64)?\(\s*(\d+)\s*\)", r" __align \1 ", text)
    return text


TOKEN = re.compile(r"[A-Za-z_]\w*|0[xX][0-9a-fA-F]+|\d+|::|<<|>>|<=|>=|==|!=|&&|\|\||[{}()\[\];,*:=<>+\-/%&^|!~.]")


class Def:
    def __init__(self, kind, name, **kw):
        self.kind = kind  # base struct union enum typedef
        self.name = name
        self.members = []
        self.size = 0
        self.align = 0
        self.forced = False
        self.pack = DEFAULT_PACK
        self.anon = False
        self.parent = None  # enum parent type name / typedef decl
        self.decl = None
        self.calc = False
        self.__dict__.update(kw)


class Decl:
    def __init__(self, base, mods):
        self.base = base  # Def
        self.mods = mods  # name-outward: ("a", n) / ("p",)
        self.bits = None
        self.size = 0
        self.align = 0
        self.forced = False


class Member:
    def __init__(self, name, decl):
        self.name = name
        self.decl = decl
        self.align_override = None
        self.offset = 0


class HeaderParser:
    def __init__(self, text):
        self.t = TOKEN.findall(preprocess(text))
        self.p = 0
        self.defs = {}
        self.enum_vals = {}
        self.pack_stack = []
        self.anon_n = 0
        for n, (sz, _) in BASE.items():
            self.defs[n] = Def("base", n, size=sz, align=sz)

    def peek(self, o=0):
        return self.t[self.p + o] if self.p + o < len(self.t) else None

    def next(self):
        v = self.t[self.p]
        self.p += 1
        return v

    def expect(self, v):
        n = self.next()
        if n != v:
            raise SyntaxError("expected %r got %r near %s" % (v, n, " ".join(self.t[self.p - 8:self.p + 4])))

    def cur_pack(self):
        return self.pack_stack[-1] if self.pack_stack else DEFAULT_PACK

    # integer constant expression (array sizes, enum values)
    def const_expr(self, stop):
        toks = []
        depth = 0
        while True:
            v = self.peek()
            if depth == 0 and v in stop:
                break
            if v == "(":
                depth += 1
            elif v == ")":
                depth -= 1
            toks.append(self.next())
        expr = " ".join(str(self.enum_vals.get(x, x)) for x in toks)
        expr = re.sub(r"\b0[xX][0-9a-fA-F]+\b", lambda m: str(int(m.group(0), 16)), expr)
        return int(eval(expr, {"__builtins__": {}}, {}))  # trusted input (the header)

    def parse(self):
        while self.p < len(self.t):
            self.top()
        return self.defs

    def top(self):
        v = self.peek()
        if v == "__pack_push":
            self.next()
            n = self.next()
            self.pack_stack.append(int(n) if n != ";" else DEFAULT_PACK)
            if n != ";":
                self.expect(";")
            return
        if v == "__pack_pop":
            self.next()
            self.expect(";")
            self.pack_stack.pop()
            return
        if v == ";":
            self.next()
            return
        if v == "typedef":
            return self.typedef()
        if v in ("struct", "union"):
            return self.struct_top()
        if v == "enum":
            return self.enum_def()
        raise SyntaxError("unexpected %r near %s" % (v, " ".join(self.t[self.p - 5:self.p + 5])))

    def enum_def(self):
        self.expect("enum")
        name = self.next()
        parent = "int"
        if self.peek() == ":":
            self.next()
            parent = self.type_name()
        self.expect("{")
        nxt = 0
        while self.peek() != "}":
            n = self.next()
            if self.peek() == "=":
                self.next()
                nxt = self.const_expr((",", "}"))
            self.enum_vals[n] = nxt
            nxt += 1
            if self.peek() == ",":
                self.next()
        self.expect("}")
        self.expect(";")
        e = Def("enum", name, parent=parent)
        e.size, e.align = BASE[parent][0], BASE[parent][0]
        self.defs[name] = e

    def type_name(self):
        words = []
        while self.peek() in ("unsigned", "signed", "char", "short", "int", "long", "const"):
            w = self.next()
            if w != "const":
                words.append(w)
        if words:
            if words == ["signed", "char"]:
                return "signed char"
            if "unsigned" in words:
                words.remove("unsigned")
                return "unsigned " + " ".join(words or ["int"])
            return " ".join(words)
        return self.next()

    def typedef(self):
        self.expect("typedef")
        align = None
        if self.peek() == "__align":
            self.next()
            align = int(self.next())
        if self.peek() == "const":
            self.next()
        tn = self.type_name()
        if tn in ("struct", "union"):
            tn = self.next()
        base = self.defs.get(tn) or self.fwd(tn)
        mods, name, _ = self.declarator()
        self.expect(";")
        d = Def("typedef", name)
        d.decl = Decl(base, mods)
        d.align_override = align
        self.defs[name] = d

    def fwd(self, name):
        d = Def("fwd", name)
        self.defs[name] = d
        return d

    def declarator(self):
        """Returns (mods name-outward, name, bits)."""
        nptr = 0
        while self.peek() in ("*", "const"):
            if self.next() == "*":
                nptr += 1
        if self.peek() == "(":
            self.next()
            inner, name, _ = self.declarator()
            self.expect(")")
            mods = inner
        else:
            name = self.next() if self.peek() not in (";", ",", ":") else ""
            mods = []
        while self.peek() == "[":
            self.next()
            n = self.const_expr(("]",))
            self.expect("]")
            mods.append(("a", n))
        mods = mods + [("p",)] * nptr
        bits = None
        if self.peek() == ":":
            self.next()
            bits = int(self.next())
        return mods, name, bits

    def struct_top(self):
        kw = self.next()
        align = None
        if self.peek() == "__align":
            self.next()
            align = int(self.next())
        name = self.next()
        if self.peek() == ";":  # forward declaration
            self.next()
            if name not in self.defs:
                self.fwd(name)
            return
        d = self.struct_body(kw, name, align)
        self.expect(";")
        return d

    def struct_body(self, kw, name, align):
        self.expect("{")
        d = self.defs.get(name)
        if d is None or d.kind != "fwd":
            d = Def(kw, name)
            self.defs[name] = d
        d.kind = kw
        d.pack = self.cur_pack()
        if align:
            d.align, d.forced = align, True
        while self.peek() != "}":
            self.member(d)
        self.expect("}")
        return d

    def member(self, owner):
        malign = None
        if self.peek() == "__align":
            self.next()
            malign = int(self.next())
        if self.peek() == "const":
            self.next()
        kw = self.peek()
        base = None
        if kw in ("struct", "union"):
            self.next()
            salign = None
            if self.peek() == "__align":
                self.next()
                salign = int(self.next())
            if self.peek() == "{":
                self.anon_n += 1
                base = self.struct_body(kw, "$anon%d" % self.anon_n, salign)
                base.anon = True
            else:
                nm = self.next()
                if self.peek() == "{":
                    base = self.struct_body(kw, nm, salign)
                else:
                    base = self.defs.get(nm) or self.fwd(nm)
        elif kw == "enum":
            raise SyntaxError("enum member")
        else:
            tn = self.type_name()
            base = self.defs.get(tn) or self.fwd(tn)
        while True:
            if self.peek() == ";":  # anonymous member without declarator
                self.next()
                m = Member("", Decl(base, []))
                m.align_override = malign
                owner.members.append(m)
                return
            mods, name, bits = self.declarator()
            decl = Decl(base, mods)
            decl.bits = bits
            m = Member(name, decl)
            m.align_override = malign
            owner.members.append(m)
            if self.peek() == ",":
                self.next()
                continue
            self.expect(";")
            return


# ---------------------------------------------------------------------------------------------------------------- layout
def align_up(v, a):
    return (v + a - 1) // a * a if a > 0 else v


def resolve(d):
    while d.kind == "typedef":
        d = d.decl.base
    return d


def def_calc(d):
    if d.kind in ("base", "enum"):
        return
    if d.kind == "fwd":
        raise RuntimeError("unresolved type " + d.name)
    if d.calc:
        return
    if d.kind == "typedef":
        decl_calc(d.decl)
        d.size = d.decl.size
        if d.align_override:
            d.align, d.forced = d.align_override, True
        else:
            d.align, d.forced = d.decl.align, d.decl.forced
        d.calc = True
        return
    d.calc = True
    for m in d.members:
        decl_calc(m.decl)
    if not d.forced:
        d.align = max([(m.align_override or m.decl.align) for m in d.members] or [0])
    if d.kind == "struct":
        size = 0
        bit = 0
        for m in d.members:
            if m.decl.bits:
                m.offset = size + bit // 8
                bit += m.decl.bits
            else:
                if bit > 0:
                    bit = align_up(bit, 8)
                    size += bit // 8
                    bit = 0
                malign = m.align_override or m.decl.align
                force = bool(m.align_override) or m.decl.forced
                size = align_up(size, malign if force else min(malign, d.pack))
                m.offset = size
                size += m.decl.size
        if bit > 0:
            size += align_up(bit, 8) // 8
        d.size = align_up(size, d.align)
    else:
        size = 0
        for m in d.members:
            m.offset = 0
            size = max(size, m.decl.size)
        d.size = align_up(size, d.align)


def decl_calc(decl):
    if any(m[0] == "p" for m in decl.mods):
        decl.align, decl.forced = POINTER, False
    else:
        def_calc(decl.base)
        decl.align, decl.forced = decl.base.align, decl.base.forced
    if not decl.mods:
        def_calc(decl.base)
        decl.size = decl.base.size
        return
    cur = 0
    if decl.mods[-1][0] != "p":
        def_calc(decl.base)
        cur = decl.base.size
    for m in reversed(decl.mods):
        cur = POINTER if m[0] == "p" else cur * m[1]
    decl.size = cur


# ---------------------------------------------------------------------------------------------------------------- commands
class MInfo:
    """A member plus its loading rules (the generator's MemberInformation)."""

    def __init__(self, owner, m):
        self.owner = owner
        self.m = m
        self.name = m.name
        self.type = None  # SInfo of the resolved struct/union type
        self.is_string = False
        self.is_script = False
        self.reusable = False
        self.cond = None
        self.block = None
        self.alloc_align = None
        # modifier rules: list of dicts parallel to decl.mods
        self.mods = []
        for md in m.decl.mods:
            if md[0] == "a":
                self.mods.append({"t": "a", "n": md[1], "dsize": None, "dcount": None})
            else:
                self.mods.append({"t": "p", "count": None, "by_idx": []})
        self.leaf = False
        self.ignore = False


class SInfo:
    def __init__(self, d, sid):
        self.d = d
        self.sid = sid
        self.name = d.name
        self.members = [MInfo(self, m) for m in d.members]
        self.block = None
        self.asset = ""
        self.alloc_align = None
        self.leaf = False


class Cmd:
    def __init__(self, defs):
        self.defs = defs
        self.infos = {}
        self.by_def = {}
        n = 0
        for name, d in defs.items():
            if d.kind in ("struct", "union") and id(d) not in self.by_def:
                si = SInfo(d, n)
                n += 1
                self.infos[name] = si
                self.by_def[id(d)] = si
        # second pass: member types
        for si in self.by_def.values():
            for mi in si.members:
                b = resolve(mi.m.decl.base)
                if b.kind in ("struct", "union"):
                    mi.type = self.by_def[id(b)]
        self.enum_members = {}
        for n, d in defs.items():
            pass
        self.in_use = None
        self.blocks = []

    # name -> chain
    def extract(self, name, start, si, chain):
        parts = name.split("::")
        cur = si
        for i, p in enumerate(parts):
            if cur is None:
                return False
            found = None
            for mi in cur.members:
                if mi.name == p:
                    found = mi
                    break
            if found is None:
                return False
            chain.append(found)
            cur = found.type
        return True

    def typename_and_members(self, name):
        """GetTypenameAndMembersFromTypename -> (root SInfo, chain) or None."""
        if self.in_use is not None:
            chain = []
            if self.extract(name, 0, self.in_use, chain):
                return self.in_use, chain
        parts = name.split("::")
        for i in range(1, len(parts) + 1):
            tn = "::".join(parts[:i])
            d = self.defs.get(tn)
            if d is not None and d.kind in ("struct", "union"):
                si = self.by_def[id(d)]
                chain = []
                if i == len(parts):
                    return si, chain
                if self.extract("::".join(parts[i:]), 0, si, chain):
                    return si, chain
                return None
        return None


def lex_cmd(text):
    text = re.sub(r"/\*.*?\*/", " ", text, flags=re.S)
    text = re.sub(r"//[^\n]*", "", text)
    return re.findall(r'"[^"]*"|[A-Za-z_]\w*|0[xX][0-9a-fA-F]+|\d+|::|<<|>>|<=|>=|==|!=|&&|\|\||\.\.\.|[{}()\[\];,*:=<>+\-/%&^|!~.]', text)


def read_cmd(path, base_dir):
    text = open(path, encoding="utf-8").read()
    toks = []
    for line_toks in [text]:
        pass
    # handle #include by line first
    out = []
    for line in text.split("\n"):
        m = re.match(r'\s*#include\s+"([^"]+)"', line)
        if m:
            out.extend(read_cmd(os.path.join(base_dir, m.group(1)), base_dir))
        else:
            out.extend(lex_cmd(line))
    return out


PREC = {"*": 1, "/": 1, "%": 1, "+": 2, "-": 2, "<<": 3, ">>": 3, "<": 4, "<=": 4, ">": 4, ">=": 4, "==": 5, "!=": 5, "&": 6, "^": 7, "|": 8, "&&": 9,
        "||": 10}


class SkipCmd(Exception):
    pass


REMOVED = set()


class CmdParser:
    def __init__(self, cmd, toks, enums):
        self.c = cmd
        self.t = toks
        self.p = 0
        self.enums = enums
        self.assets = {}
        self.cur_type = None  # for expression resolution

    def peek(self, o=0):
        return self.t[self.p + o] if self.p + o < len(self.t) else None

    def next(self):
        v = self.t[self.p]
        self.p += 1
        return v

    def expect(self, v):
        n = self.next()
        if n != v:
            raise SyntaxError("expected %r got %r near %s" % (v, n, " ".join(self.t[self.p - 8:self.p + 4])))

    def typename(self):
        n = self.next()
        while self.peek() == "::":
            self.next()
            n += "::" + self.next()
        return n

    # --- expressions -> AST
    def evaluation(self, cur_type):
        operands, ops = [], []
        while True:
            operands.append(self.operand(cur_type))
            if self.peek() in PREC:
                ops.append(self.next())
            else:
                break
        # precedence climbing over the flat list (left associative)
        def build(lo, hi):
            if lo == hi:
                return operands[lo]
            # split at the last operator with the highest precedence value (lowest binding)
            best = None
            for i in range(lo, hi):
                pr = PREC[ops[i]]
                if best is None or pr >= PREC[ops[best]]:
                    best = i
            return ["o", ops[best], build(lo, best), build(best + 1, hi)]
        return build(0, len(operands) - 1)

    def operand(self, cur_type):
        v = self.peek()
        if v == "!" or v == "(":
            neg = False
            if v == "!":
                self.next()
                neg = True
            self.expect("(")
            e = self.evaluation(cur_type)
            self.expect(")")
            return ["o", "==", e, ["n", 0]] if neg else e
        if re.match(r"0[xX]", v):
            self.next()
            return ["n", int(v, 16)]
        if v.isdigit():
            self.next()
            return ["n", int(v)]
        name = self.typename()
        idx = []
        while self.peek() == "[":
            self.next()
            idx.append(self.evaluation(cur_type))
            self.expect("]")
        if name in self.enums:
            return ["n", self.enums[name]]
        r = self.c.typename_and_members(name)
        if r is None and cur_type is not None and cur_type is not self.c.in_use:
            chain = []
            if self.c.extract(name, 0, cur_type, chain):
                r = (cur_type, chain)
        if r is None:
            if name.split("::")[0] in REMOVED or name.split("::")[-1] in REMOVED:
                raise SkipCmd()
            raise SyntaxError("unknown operand %s" % name)
        root, chain = r
        if not chain:
            return ["v", root.sid]  # the loader's current pointer to that structure
        return emit_dyn(root, chain, idx)

    # --- statements
    def run(self):
        while self.p < len(self.t):
            kw = self.next()
            if kw == "game" or kw == "wordsize":
                while self.next() != ";":
                    pass
            elif kw == "asset":
                st = self.next()
                an = self.next()
                self.expect(";")
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
                self.c.in_use = self.c.infos[n]
            elif kw == "reorder":
                self.reorder()
            elif kw == "set":
                start = self.p
                try:
                    self.set_cmd()
                except SkipCmd:
                    self.p = start
                    while self.next() != ";":
                        pass
            else:
                raise SyntaxError("unknown command %r near %s" % (kw, " ".join(self.t[self.p - 5:self.p + 5])))

    def lookup(self, name):
        r = self.c.typename_and_members(name)
        if r is None:
            if name.split("::")[-1] in REMOVED or name.split("::")[0] in REMOVED:
                raise SkipCmd()
            raise SyntaxError("unknown type " + name)
        return r

    def set_cmd(self):
        what = self.next()
        if what == "block":
            # set block [Type::member] BLOCK;
            if self.peek(1) != ";":
                name = self.typename()
                bname = self.next()
                root, chain = self.lookup(name)
            else:
                bname = self.next()
                root, chain = self.c.in_use, []
            self.expect(";")
            if chain:
                chain[-1].block = ENUMS[bname]
            else:
                root.block = ENUMS[bname]
        elif what in ("string", "scriptstring", "reusable"):
            name = self.typename()
            self.expect(";")
            root, chain = self.lookup(name)
            mi = chain[-1]
            if what == "string":
                mi.is_string = True
            elif what == "scriptstring":
                mi.is_script = True
            else:
                mi.reusable = True
        elif what == "count":
            resolve_n = 0
            while self.peek() == "*":
                self.next()
                resolve_n += 1
            name = self.typename()
            idxs = []
            while self.peek() == "[":
                self.next()
                t = self.next()
                idxs.append(int(t) if t.isdigit() else self.enums[t])
                self.expect("]")
            root, chain = self.lookup(name)
            ev = self.evaluation(root)
            self.expect(";")
            mi = chain[-1]
            ptr = None
            for md in mi.mods:
                if md["t"] == "p":
                    if resolve_n > 0:
                        resolve_n -= 1
                    else:
                        ptr = md
                        break
            if ptr is None:
                raise SyntaxError("no pointer modifier for count " + name)
            if not idxs:
                ptr["count"] = ev
            else:
                sizes = [md["n"] for md in mi.mods if md["t"] == "a"]
                if not ptr["by_idx"]:
                    need = 0
                    for s in sizes:
                        need = s if need == 0 else need * s
                    ptr["by_idx"] = [None] * need
                depth = [1] * len(sizes)
                cur = 1
                for i in range(len(sizes), 0, -1):
                    if i < len(sizes):
                        cur *= sizes[i]
                    depth[i - 1] = cur
                ci = 0
                for k, ix in enumerate(idxs):
                    if k >= len(depth):
                        break
                    ci += depth[k] * ix
                ptr["by_idx"][ci] = ev
        elif what == "condition":
            name = self.typename()
            root, chain = self.lookup(name)
            if self.peek() == "always":
                self.next()
                ev = ["n", 1]
            elif self.peek() == "never":
                self.next()
                ev = ["n", 0]
            else:
                ev = self.evaluation(root)
            self.expect(";")
            chain[-1].cond = ev
        elif what in ("arraysize", "arraycount"):
            name = self.typename()
            root, chain = self.lookup(name)
            ev = self.evaluation(root)
            self.expect(";")
            mi = chain[-1]
            for md in mi.mods:
                if md["t"] == "a":
                    md["dsize" if what == "arraysize" else "dcount"] = ev
                    break
        elif what == "allocalign":
            name = self.typename()
            root, chain = self.lookup(name)
            ev = self.evaluation(root)
            self.expect(";")
            if chain:
                chain[-1].alloc_align = ev
            else:
                root.alloc_align = ev
        elif what in ("action", "assetref"):
            while self.next() != ";":
                pass
        else:
            raise SyntaxError("unknown set " + what)

    def reorder(self):
        info = self.c.in_use
        if self.peek() != ":":
            n = self.typename()
            root, chain = self.lookup(n)
            info = chain[-1].type if chain else root
        self.expect(":")
        find_first = False
        if self.peek() == "..." or (self.peek() == "." and self.peek(1) == "."):
            if self.peek() == "...":
                self.next()
            else:
                self.next(); self.next(); self.next()
            find_first = True
        names = []
        while self.peek() != ";":
            names.append(self.next())
        self.expect(";")
        old = list(info.members)
        new = []
        first = None
        if find_first:
            first = names.pop(0)
        for n in names:
            for i, m in enumerate(old):
                if m.name == n:
                    new.append(old.pop(i))
                    break
            else:
                raise SyntaxError("reorder: no member " + n)
        res = []
        if find_first:
            while old:
                m = old.pop(0)
                res.append(m)
                if m.name == first:
                    break
        res.extend(new)
        res.extend(old)
        info.members = res


def emit_dyn(root, chain, idx):
    refs = []
    for mi in chain[:-1]:
        deref = any(md["t"] == "p" for md in mi.mods)
        refs.append([mi.m.offset, 1 if deref else 0])
    last = chain[-1] if chain else None
    if last is None:
        raise SyntaxError("operand without member")
    refs.append([last.m.offset, 0])
    b = resolve(last.m.decl.base)
    kind = BASE[b.name][1] if b.kind == "base" else ("i32" if b.kind == "enum" else "u32")
    if b.kind == "enum":
        kind = BASE[b.parent][1]
    dims = [md["n"] for md in last.mods if md["t"] == "a"]
    # element size of the member's declared base type (for array strides)
    esz = last.m.decl.base.size if last.m.decl.base.kind != "typedef" else last.m.decl.base.size
    if any(md["t"] == "p" for md in last.mods) and last.mods and last.mods[-1]["t"] == "p":
        esz = POINTER
    if idx and last.mods and last.mods[0]["t"] == "p":
        # pointer[index]: read the pointer, then index in elements of the pointed-to type
        refs[-1][1] = 1
        esz = last.m.decl.base.size
    return ["d", root.sid, refs, kind, dims, esz, idx]


# ---------------------------------------------------------------------------------------------------------------- analysis
def static_false(ev):
    return ev is not None and ev[0] == "n" and ev[1] == 0


def is_static(ev):
    return ev is not None and ev[0] == "n"


def compute_flags(cmd):
    infos = list(cmd.by_def.values())
    for si in infos:
        for mi in si.members:
            mi.ignore = static_false(mi.cond)

    def has_dyn_size(mi):
        return any(md["t"] == "a" and md["dsize"] is not None for md in mi.mods)

    memo = {}

    def struct_leaf(si, stack=()):
        if si.sid in memo:
            return memo[si.sid]
        if si.sid in stack:
            return True
        r = True
        for mi in si.members:
            if mi.ignore:
                continue
            if mi.is_string or mi.is_script:
                r = False
                break
            bad = False
            for md in mi.mods:
                if md["t"] == "p":
                    cnt = md["count"]
                    if cnt is None:
                        bad = True  # default count 1 (non-zero)
                    elif not (is_static(cnt) and cnt[1] == 0):
                        bad = True
            if bad or has_dyn_size(mi):
                r = False
                break
            if mi.type is not None and mi.type is not si and not struct_leaf(mi.type, stack + (si.sid,)):
                r = False
                break
        memo[si.sid] = r
        return r

    for si in infos:
        si.leaf = struct_leaf(si)
    for si in infos:
        for mi in si.members:
            lf = True
            if mi.is_string or mi.is_script:
                lf = False
            elif mi.type is not None and not mi.type.leaf:
                lf = False
            else:
                for md in mi.mods:
                    if md["t"] == "p":
                        cnt = md["count"]
                        if cnt is None or not (is_static(cnt) and cnt[1] == 0):
                            lf = False
                if has_dyn_size(mi):
                    lf = False
            mi.leaf = lf
    # union ordering
    for si in infos:
        if si.d.kind == "union":
            cands = [i for i, mi in enumerate(si.members) if mi.cond is None and not mi.leaf]
            if len(cands) == 1:
                mi = si.members.pop(cands[0])
                si.members.append(mi)
    # dynamic members
    dyn_memo = {}

    def struct_dyn(si):
        if si.sid in dyn_memo:
            return dyn_memo[si.sid]
        dyn_memo[si.sid] = None
        res = None
        for mi in si.members:
            if member_dyn(mi):
                res = mi
                break
        dyn_memo[si.sid] = res
        return res

    def member_dyn(mi):
        if has_dyn_size(mi):
            return True
        has_ptr = any(md["t"] == "p" for md in mi.mods)
        return (not has_ptr) and mi.type is not None and struct_dyn(mi.type) is not None

    for si in infos:
        si.dynm = struct_dyn(si)
    for si in infos:
        for mi in si.members:
            mi.dyn = member_dyn(mi)
    return infos


def emit(cmd, infos, enums):
    out = {"structs": [None] * len(infos), "assets": {}}
    for si in infos:
        has_dyn_member = si.dynm is not None
        members = []
        for mi in si.members:
            m = mi.m
            has_ptr = any(md["t"] == "p" for md in mi.mods)
            after_partial = mi.dyn or (si.d.kind == "union" and has_dyn_member)
            needs = (not mi.ignore) and (mi.is_string or has_ptr or (mi.type is not None and not mi.type.leaf) or after_partial)
            b = resolve(m.decl.base)
            kind = ""
            if b.kind == "base":
                kind = BASE[b.name][1]
            elif b.kind == "enum":
                kind = BASE[b.parent][1]
            mods = []
            for md in mi.mods:
                if md["t"] == "a":
                    mods.append(["a", md["n"], md["dsize"], md["dcount"]])
                else:
                    mods.append(["p", md["count"], md["by_idx"]])
            esize = m.decl.base.size if m.decl.base.kind != "fwd" else 0
            ealign = m.decl.base.align
            ma = m.align_override or m.decl.align
            if LAYOUT32 and si.name in LAYOUT32 and mi.name in LAYOUT32[si.name][1]:
                ma, ealign = LAYOUT32[si.name][1][mi.name]
            members.append({
                "n": m.name, "o": m.offset, "k": kind, "ty": mi.type.sid if mi.type is not None else -1, "mods": mods,
                "str": int(mi.is_string), "scr": int(mi.is_script), "re": int(mi.reusable), "c": mi.cond, "blk": mi.block,
                "aa": mi.alloc_align, "leaf": int(mi.leaf), "ign": int(mi.ignore), "dyn": int(mi.dyn), "ap": int(after_partial),
                "need": int(needs), "es": esize, "ea": ealign, "bits": m.decl.bits or 0, "ep": int(has_ptr),
                "ma": ma, "td": int(m.decl.base.kind == "typedef"), "tk": m.decl.base.name,
            })
        # union chain: used members (non-leaf, or all when the union has a dynamic member)
        out["structs"][si.sid] = {
            "n": si.name, "u": int(si.d.kind == "union"), "s": si.d.size, "a": LAYOUT32[si.name][0] if LAYOUT32 and si.name in LAYOUT32 else si.d.align, "leaf": int(si.leaf), "asset": si.asset,
            "blk": si.block, "aa": si.alloc_align, "dynoff": si.dynm.m.offset if si.dynm is not None else -1, "dyn": int(si.dynm is not None),
            "m": members,
        }
    return out


LAYOUT32 = None


def layout32():
    """Alignments of the 32-bit layout: the 64-bit builds allocate with the alignment constants of the 32-bit code."""
    global POINTER, DEFAULT_PACK, X64
    saved = (POINTER, DEFAULT_PACK, X64)
    POINTER, DEFAULT_PACK, X64 = 4, 8, False
    try:
        hp = HeaderParser(open(HDR, encoding="utf-8").read())
        defs = hp.parse()
        for d in list(defs.values()):
            def_calc(d)
        res = {}
        for n, d in defs.items():
            if d.kind in ("struct", "union"):
                res[n] = (d.align, {m.name: (m.align_override or m.decl.align, m.decl.base.align) for m in d.members if m.name})
        return res
    finally:
        POINTER, DEFAULT_PACK, X64 = saved


def main():
    global LAYOUT32
    if X64 and "--layout64" not in sys.argv:
        LAYOUT32 = layout32()
    text = open(HDR, encoding="utf-8").read()
    hp = HeaderParser(text)
    defs = hp.parse()
    for d in defs.values():
        if d.kind == "fwd":
            raise RuntimeError("forward declaration never defined: " + d.name)
    for sname, ops in FIXES.get((GAME, X64), {}).items():
        d = defs[sname]
        for op in ops:
            if op[0] == "remove":
                d.members = [m for m in d.members if m.name != op[1]]
                REMOVED.add(op[1])
            elif op[0] == "members":
                # replace all members: C declarations, parsed with the types of the header
                sub = HeaderParser("struct __tmp { %s };" % op[1])
                sub.defs = defs
                sub.enum_vals = hp.enum_vals
                sub.pack_stack = [op[2]] if len(op) > 2 else []
                sub.parse()
                tmp = defs.pop("__tmp")
                REMOVED.update(m.name for m in d.members if m.name)
                for m in tmp.members:
                    REMOVED.discard(m.name)
                d.members = tmp.members
                if len(op) > 2:
                    d.pack = op[2]
                    d.align, d.forced = min(op[2], 8), True  # a packed structure is aligned to its packing
            elif op[0] == "append":
                sub = HeaderParser("struct __tmp { %s };" % op[1])
                sub.defs = defs
                sub.enum_vals = hp.enum_vals
                sub.parse()
                d.members += defs.pop("__tmp").members
            elif op[0] == "pack":
                d.pack = op[1]
            elif op[0] == "resize":
                # the first array dimension of a member
                for m in d.members:
                    if m.name == op[1]:
                        m.decl.mods[0] = ("a", op[2])
            elif op[0] == "swap":
                names = [m.name for m in d.members]
                i, j = names.index(op[1]), names.index(op[2])
                d.members[i], d.members[j] = d.members[j], d.members[i]
            elif op[0] not in ("align", "never", "cmd", "resize", "append"):
                raise RuntimeError("unknown fix " + op[0])
    for d in list(defs.values()):
        def_calc(d)
    global ENUMS
    ENUMS = hp.enum_vals
    cmd = Cmd(defs)
    toks = read_cmd(os.path.join(CMD_DIR, GAME + "_Commands.txt"), CMD_DIR)
    for sname, ops in FIXES.get((GAME, X64), {}).items():
        for op in ops:
            if op[0] == "cmd":
                # extra loading rules for a structure that a fix gave new members
                toks = toks + lex_cmd("use %s; %s" % (sname, op[1]))
    CmdParser(cmd, toks, hp.enum_vals).run()
    for sname, ops in FIXES.get((GAME, X64), {}).items():
        for op in ops:
            if op[0] == "align":
                for mi in cmd.infos[sname].members:
                    if mi.name == op[1]:
                        mi.alloc_align = ["n", op[2]]
            elif op[0] == "never":
                for mi in cmd.infos[sname].members:
                    if mi.name == op[1]:
                        mi.cond = ["n", 0]
    infos = compute_flags(cmd)
    out = emit(cmd, infos, hp.enum_vals)
    # asset type numbers: the order of the asset type enum; the structure each one loads (from the content loader)
    gh = open(GAME_H, encoding="utf-8").read()
    names = None
    for m in re.finditer(r"enum\s+\w+\s*\{(.*?)\}", gh, re.S):
        body = re.sub(r"//[^\n]*", "", m.group(1))
        ids = [x.strip().split("=")[0].strip() for x in body.split(",") if x.strip()]
        if "ASSET_TYPE_COUNT" in ids:
            names = ids
            break
    ld = open(LOADER, encoding="utf-8").read()
    amap = {}
    skip = []
    for m in re.finditer(r"^\s*LOAD_ASSET\((\w+),\s*(\w+),", ld, re.M):
        amap[names.index(m.group(1))] = m.group(2)
    for m in re.finditer(r"^\s*SKIP_ASSET\((\w+)", ld, re.M):
        skip.append(names.index(m.group(1)))
    out["assets"] = {str(k): cmd.infos[v].sid for k, v in amap.items()}
    out["skip"] = skip
    out["asset_names"] = names
    blocks = []
    for kind, name, dflt in cmd.blocks:
        blocks.append({"name": name, "kind": {"temp": 0, "runtime": 1, "delay": 2, "normal": 3}[kind], "default": int(dflt), "index": hp.enum_vals[name]})
    blocks.sort(key=lambda b: b["index"])
    out["blocks"] = [{"name": b["name"], "kind": b["kind"], "default": b["default"]} for b in blocks]
    consts = open(CONSTANTS, encoding="utf-8").read()
    out["bits"] = int(re.search(r"OFFSET_BLOCK_BIT_COUNT\s*=\s*(\d+)", consts).group(1))
    out["insert"] = hp.enum_vals[re.search(r"INSERT_BLOCK\s*=\s*(\w+)", consts).group(1)]
    out["game"] = GAME
    out["ptr"] = POINTER
    out["enums"] = {}
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        json.dump(out, f, separators=(",", ":"))
    print("structs:", len(out["structs"]), "bytes:", os.path.getsize(OUT))
    # layout self-check against the header's static_asserts
    for name, want in (("MaterialTextureDefSamplerState", 1), ("GfxStateBitsLoadBits", 8), ("GfxStateBitsLoadBitsStructured", 8), ("SndAliasFlags", 8)):
        if name in defs:
            print(name, defs[name].size, "expected", want)


if __name__ == "__main__":
    main()
