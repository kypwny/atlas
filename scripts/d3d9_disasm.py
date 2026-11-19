"""Minimal Direct3D 9 shader model 2/3 disassembler (enough to read the game's lit shaders).

usage: d3d9_disasm.py SHADER.bin
Prints the constant table names found in the CTAB comment and the instruction list.
"""
import struct, sys

OPS = {
    0: 'nop', 1: 'mov', 2: 'add', 3: 'sub', 4: 'mad', 5: 'mul', 6: 'rcp', 7: 'rsq', 8: 'dp3', 9: 'dp4', 10: 'min', 11: 'max',
    12: 'slt', 13: 'sge', 14: 'exp', 15: 'log', 16: 'lit', 17: 'dst', 18: 'lrp', 19: 'frc', 20: 'm4x4', 21: 'm4x3', 22: 'm3x4',
    23: 'm3x3', 24: 'm3x2', 25: 'call', 26: 'callnz', 27: 'loop', 28: 'ret', 29: 'endloop', 30: 'label', 31: 'dcl', 32: 'pow',
    33: 'crs', 34: 'sgn', 35: 'abs', 36: 'nrm', 37: 'sincos', 38: 'rep', 39: 'endrep', 40: 'if', 41: 'ifc', 42: 'else', 43: 'endif',
    44: 'break', 45: 'breakc', 46: 'mova', 47: 'defb', 48: 'defi', 66: 'texld', 65: 'texkill', 81: 'def', 82: 'dsx', 83: 'dsy', 84: 'cmp',
    88: 'cmp', 89: 'bem', 90: 'dp2add', 91: 'dsx', 92: 'dsy', 93: 'texldd', 95: 'texldl', 96: 'breakp',
}
# opcode numbers collide in older docs; the ones above match d3d9types.h for SM3
OPS[88] = 'cmp'
OPS[90] = 'dp2add'
OPS[82] = 'dsx'
OPS[83] = 'dsy'
REG = {0: 'r', 1: 'v', 2: 'c', 3: 'a', 4: 'rast', 5: 'oD', 6: 'o', 7: 'i', 8: 'oC', 9: 'oDepth', 10: 's', 11: 'x', 12: 'b', 13: 'x', 14: 'aL', 15: 'vPos/vFace', 17: 'p'}
COMP = 'xyzw'


def regname(t, vs):
    num = t & 0x7FF
    rtype = ((t >> 28) & 7) | ((t >> 8) & 0x18)
    name = REG.get(rtype, f'?{rtype}')
    if vs and rtype == 3:
        name = 'a'
    if not vs and rtype == 3:
        name = 't'
    if not vs and rtype == 6:
        name = 't'
    return f'{name}{num}', rtype


def dest(t, vs):
    name, _ = regname(t, vs)
    mask = (t >> 16) & 15
    m = ''.join(COMP[i] for i in range(4) if mask >> i & 1)
    mod = (t >> 20) & 15
    return name + ('.' + m if m and m != 'xyzw' else ''), ('_sat' if mod & 1 else '') + ('_pp' if mod & 2 else '')


def src(t, vs):
    name, rtype = regname(t, vs)
    sw = (t >> 16) & 255
    comps = ''.join(COMP[(sw >> (2 * i)) & 3] for i in range(4))
    # compact trailing repeats
    while len(comps) > 1 and comps[-1] == comps[-2]:
        comps = comps[:-1]
    s = name + ('' if comps == 'xyzw' else '.' + comps)
    mod = (t >> 24) & 15
    pre = {1: '-', 2: 'bias(', 3: '-bias(', 4: 'sign(', 5: '-sign(', 6: 'comp(', 7: 'x2(', 8: '-x2(', 11: '!'}.get(mod, '')
    if mod == 0 or mod in (1, 11):
        return pre + s
    if mod == 9:
        return 'abs(' + s + ')'
    if mod == 10:
        return '-abs(' + s + ')'
    return pre + s + ')'


def main(path):
    b = open(path, 'rb').read()
    ver = struct.unpack_from('<I', b, 0)[0]
    vs = (ver >> 16) == 0xFFFE
    print(f'{"vs" if vs else "ps"}_{(ver >> 8) & 255}_{ver & 255}')
    pos = 4
    while pos + 4 <= len(b):
        t = struct.unpack_from('<I', b, pos)[0]
        op = t & 0xFFFF
        if op == 0xFFFF:
            print('end')
            break
        if op == 0xFFFE:  # comment
            n = (t >> 16) & 0x7FFF
            body = b[pos + 4:pos + 4 + 4 * n]
            if body[:4] == b'CTAB':
                ct = body[4:]
                count, cinfo = struct.unpack_from("<II", ct, 12)
                sets = {0: "b", 1: "i", 2: "c", 3: "s"}
                items = []
                for i in range(count):
                    no, rset, ridx, rcount = struct.unpack_from("<IHHH", ct, cinfo + 20 * i)
                    nm = ct[no:ct.index(b"0", no)].decode("latin-1")
                    items.append((sets.get(rset, "?"), ridx, rcount, nm))
                print("// constants:", ", ".join(f"{nm}={k}{i}" + (f"[{c}]" if c > 1 else "") for k, i, c, nm in sorted(items)))
            pos += 4 + 4 * n
            continue
        n = (t >> 24) & 15
        toks = struct.unpack_from(f'<{n}I', b, pos + 4) if n else ()
        name = OPS.get(op, f'op{op}')
        if op == 31:  # dcl
            use = toks[0]
            d, _ = dest(toks[1], vs)
            kind = use & 31
            print(f'dcl_{ {0: "position", 1: "blendweight", 2: "blendindices", 3: "normal", 4: "psize", 5: "texcoord", 6: "tangent", 7: "binormal", 8: "tessfactor", 9: "positiont", 10: "color", 11: "fog", 12: "depth", 13: "sample"}.get(kind, kind) }{(use >> 16) & 15 if kind in (5, 10) else ""} {d}')
        elif op == 81:  # def
            d, _ = dest(toks[0], vs)
            print('def', d, ', '.join(f'{struct.unpack("<f", struct.pack("<I", x))[0]:g}' for x in toks[1:]))
        elif op in (47, 48):
            print(name, *toks)
        elif op == 66 or op == 95:  # texld / texldl
            d, m = dest(toks[0], vs)
            flag = (t >> 16) & 7
            sam = regname(toks[2], vs)[0]
            suffix = {1: 'p', 2: 'b'}.get(flag, '')
            print(f'{name}{suffix}{m} {d}, {src(toks[1], vs)}, {sam}')
        elif op in (40, 41, 42, 43, 27, 29, 25, 28, 30, 38, 39, 44, 45):
            print(name, ', '.join(src(x, vs) for x in toks))
        elif n >= 1:
            d, m = dest(toks[0], vs)
            print(f'{name}{m} {d}, ' + ', '.join(src(x, vs) for x in toks[1:]))
        else:
            print(name)
        pos += 4 + 4 * n


if __name__ == '__main__':
    main(sys.argv[1])
