#!/usr/bin/env python3
"""Reads the type table (TPI stream) of an MSVC program database (MSF 7.00) and writes structures and enums.

usage: pdb_types.py FILE.pdb [-r REGEX] [--json OUT.json] [--c OUT.h]

Only the records the zone layouts need are decoded: structures, classes and unions with their data members (offset, type,
bit fields), enums with their constants, pointers, arrays, modifiers and the primitive types. Everything is read from the
file itself; the output has the size and member offsets the compiler used.
"""
import json
import re
import struct
import sys

PRIM = {
    0x03: ("void", 0), 0x10: ("char", 1), 0x20: ("unsigned char", 1), 0x68: ("int8_t", 1), 0x69: ("uint8_t", 1),
    0x70: ("char", 1), 0x71: ("wchar_t", 2), 0x72: ("short", 2), 0x73: ("unsigned short", 2), 0x74: ("int", 4),
    0x75: ("unsigned int", 4), 0x76: ("int64_t", 8), 0x77: ("uint64_t", 8), 0x11: ("short", 2), 0x21: ("unsigned short", 2),
    0x12: ("long", 4), 0x22: ("unsigned long", 4), 0x13: ("int64_t", 8), 0x23: ("uint64_t", 8), 0x40: ("float", 4),
    0x41: ("double", 8), 0x30: ("bool", 1), 0x31: ("bool16", 2), 0x32: ("bool32", 4),
}


def read_msf(path):
    data = open(path, "rb").read()
    if not data.startswith(b"Microsoft C/C++ MSF 7.00"):
        raise SystemExit("not an MSF 7.00 program database")
    bs, _fpm, _nb, dir_bytes, _u, map_addr = struct.unpack_from("<6I", data, 32)
    ndir = (dir_bytes + bs - 1) // bs
    dir_blocks = struct.unpack_from("<%dI" % ndir, data, map_addr * bs)
    d = b"".join(data[b * bs:(b + 1) * bs] for b in dir_blocks)[:dir_bytes]
    n = struct.unpack_from("<I", d, 0)[0]
    sizes = struct.unpack_from("<%dI" % n, d, 4)
    pos = 4 + 4 * n
    streams = []
    for s in sizes:
        if s == 0xFFFFFFFF:
            streams.append(b"")
            continue
        k = (s + bs - 1) // bs
        blocks = struct.unpack_from("<%dI" % k, d, pos)
        pos += 4 * k
        streams.append(b"".join(data[b * bs:(b + 1) * bs] for b in blocks)[:s])
    return streams


def numeric(b, o):
    v = struct.unpack_from("<H", b, o)[0]
    if v < 0x8000:
        return v, o + 2
    fmt = {0x8000: "<b", 0x8001: "<h", 0x8002: "<H", 0x8003: "<i", 0x8004: "<I", 0x8009: "<q", 0x800A: "<Q"}[v]
    return struct.unpack_from(fmt, b, o + 2)[0], o + 2 + struct.calcsize(fmt)


def cstr(b, o):
    e = b.index(b"\0", o)
    return b[o:e].decode("latin-1"), e + 1


class Types:
    def __init__(self, path):
        tpi = read_msf(path)[2]
        _ver, hdr_size, self.ti_begin, ti_end = struct.unpack_from("<IIII", tpi, 0)
        self.recs = {}
        o, ti = hdr_size, self.ti_begin
        while ti < ti_end and o < len(tpi):
            ln, kind = struct.unpack_from("<HH", tpi, o)
            self.recs[ti] = (kind, tpi[o + 4:o + 2 + ln])
            o += 2 + ln
            ti += 1
        self.names = {}
        self.defs = {}  # name -> type index of the complete definition
        for t, (kind, b) in self.recs.items():
            if kind in (0x1504, 0x1505):
                name = cstr(b, numeric(b, 16)[1])[0]
            elif kind == 0x1506:
                name = cstr(b, numeric(b, 8)[1])[0]
            elif kind == 0x1507:
                name = cstr(b, 12)[0]
            else:
                continue
            self.names[t] = name
            if not struct.unpack_from("<H", b, 2)[0] & 0x80:  # not a forward reference
                self.defs.setdefault(name, t)

    def fields(self, fl):
        """Data members and enumerators of a field list; continuation records are followed."""
        res = []
        while fl:
            _kind, b = self.recs[fl]
            o, fl = 0, 0
            while o < len(b):
                sub = struct.unpack_from("<H", b, o)[0]
                if sub == 0x1404:  # continuation
                    fl = struct.unpack_from("<I", b, o + 4)[0]
                    break
                if sub == 0x1502:  # enumerator
                    val, o = numeric(b, o + 4)
                    name, o = cstr(b, o)
                    res.append(("enum", name, val))
                elif sub == 0x150D:  # data member
                    t = struct.unpack_from("<I", b, o + 4)[0]
                    off, o = numeric(b, o + 8)
                    name, o = cstr(b, o)
                    res.append(("member", name, off, t))
                elif sub == 0x1510:  # nested type
                    o = cstr(b, o + 8)[1]
                elif sub == 0x1400:  # base class
                    o = numeric(b, o + 8)[1]
                elif sub == 0x150E:  # static member
                    o = cstr(b, o + 8)[1]
                elif sub == 0x1409:  # virtual function table pointer
                    o += 8
                elif sub == 0x150F:  # overloaded method
                    o = cstr(b, o + 8)[1]
                elif sub == 0x1511:  # method
                    attr = struct.unpack_from("<H", b, o + 2)[0]
                    intro = (attr >> 2) & 7 in (4, 6)
                    o = cstr(b, o + 8 + (4 if intro else 0))[1]
                else:
                    raise SystemExit("unknown field record %#x" % sub)
                while o < len(b) and b[o] >= 0xF0:
                    o += 1
        return res

    def size_of(self, t):
        if t < 0x1000:
            return 4 if t & 0x700 else PRIM.get(t & 0xFF, ("", 4))[1]
        kind, b = self.recs[t]
        if kind == 0x1002:
            return 4
        if kind == 0x1001:
            return self.size_of(struct.unpack_from("<I", b, 0)[0])
        if kind == 0x1503:
            return numeric(b, 8)[0]
        if kind in (0x1504, 0x1505):
            return numeric(b, 16)[0] if not struct.unpack_from("<H", b, 2)[0] & 0x80 else self.size_of(self.defs.get(self.names[t], t)) if self.defs.get(self.names[t], t) != t else 0
        if kind == 0x1506:
            return numeric(b, 8)[0]
        if kind == 0x1507:
            return self.size_of(struct.unpack_from("<I", b, 4)[0])
        if kind == 0x1205:
            return self.size_of(struct.unpack_from("<I", b, 0)[0])
        return 0

    def desc(self, t):
        """A type as a small tree: prim / ptr / arr / struct / enum / bits."""
        if t < 0x1000:
            if t & 0x700:
                return {"k": "ptr", "to": self.desc(t & 0xFF)}
            n, s = PRIM.get(t & 0xFF, ("prim%x" % t, 4))
            return {"k": "prim", "n": n, "sz": s}
        kind, b = self.recs[t]
        if kind == 0x1002:
            return {"k": "ptr", "to": self.desc(struct.unpack_from("<I", b, 0)[0])}
        if kind == 0x1001:
            return self.desc(struct.unpack_from("<I", b, 0)[0])
        if kind == 0x1503:
            et = struct.unpack_from("<I", b, 0)[0]
            total = numeric(b, 8)[0]
            esz = self.size_of(et) or 1
            return {"k": "arr", "of": self.desc(et), "n": total // esz}
        if kind == 0x1205:
            bt, ln, pos = struct.unpack_from("<IBB", b, 0)
            return {"k": "bits", "of": self.desc(bt), "len": ln, "pos": pos}
        if kind in (0x1504, 0x1505, 0x1506):
            return {"k": "union" if kind == 0x1506 else "struct", "n": self.names[t]}
        if kind == 0x1507:
            return {"k": "enum", "n": self.names[t]}
        if kind == 0x1008:
            return {"k": "func"}
        return {"k": "other", "kind": kind}

    def struct_info(self, name):
        t = self.defs[name]
        kind, b = self.recs[t]
        if kind == 0x1507:
            fl = struct.unpack_from("<I", b, 8)[0]
            return {"kind": "enum", "size": self.size_of(t),
                    "values": [[f[1], f[2]] for f in self.fields(fl) if f[0] == "enum"]}
        fl = struct.unpack_from("<I", b, 4)[0]
        size = numeric(b, 16)[0] if kind != 0x1506 else numeric(b, 8)[0]
        return {"kind": "union" if kind == 0x1506 else "struct", "size": size,
                "members": [{"n": f[1], "o": f[2], "t": self.desc(f[3]), "sz": self.size_of(f[3])}
                            for f in self.fields(fl) if f[0] == "member"]}


def c_type(d, var=""):
    k = d["k"]
    if k == "prim":
        return d["n"] + (" " + var if var else "")
    if k in ("struct", "union", "enum"):
        return d["n"] + (" " + var if var else "")
    if k == "ptr":
        return c_type(d["to"], "*" + var)
    if k == "arr":
        return c_type(d["of"], "%s[%d]" % (var, d["n"]))
    if k == "bits":
        return c_type(d["of"], var) + " : %d" % d["len"]
    return "void *" + var


def main():
    args = sys.argv[1:]
    if not args:
        raise SystemExit(__doc__)
    path = args[0]
    pat = re.compile(args[args.index("-r") + 1]) if "-r" in args else re.compile(".")
    types = Types(path)
    out = {}
    for name in sorted(types.defs):
        if pat.search(name) and not name.startswith("<"):
            try:
                out[name] = types.struct_info(name)
            except (KeyError, struct.error, IndexError):
                pass
    if "--json" in args:
        with open(args[args.index("--json") + 1], "w", encoding="utf-8") as f:
            json.dump(out, f, indent=1)
    target = open(args[args.index("--c") + 1], "w", encoding="utf-8") if "--c" in args else (sys.stdout if "--json" not in args else None)
    if target:
        for name, info in out.items():
            if info["kind"] == "enum":
                target.write("enum %s { /* %d bytes */\n" % (name, info["size"]))
                for n, v in info["values"]:
                    target.write("    %s = %d,\n" % (n, v))
                target.write("};\n\n")
            else:
                target.write("%s %s { /* size 0x%x */\n" % (info["kind"], name, info["size"]))
                for m in info["members"]:
                    target.write("    /* +0x%04x */ %s;\n" % (m["o"], c_type(m["t"], m["n"])))
                target.write("};\n\n")


if __name__ == "__main__":
    main()
