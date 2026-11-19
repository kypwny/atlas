"""Probe a CoDOL streamed model file (main/models/<name>). Usage: probe_model.py FILE"""
import struct, sys

def u32(b, o): return struct.unpack_from('<I', b, o)[0]
def u16(b, o): return struct.unpack_from('<H', b, o)[0]

def read_container(b):
    assert u32(b, 0) == 1 and u32(b, 4) == 0xe9c9c447, "not a streamed model"
    d = [struct.unpack_from('<5I', b, 8 + 20 * i) for i in range(4)]
    blocks = [bytearray(b[o:o + s]) for (s, o, st, c, _) in d[:3]]
    fix = d[3]
    tbl = b[fix[1]:fix[1] + fix[0]]
    for i in range(3):
        s, o, st, c, _ = d[i]
        for j in range(c):
            f = u32(tbl, 4 * (st + j)); loc, tgt = f >> 3, f & 7
            off = u32(blocks[i], loc)
            struct.pack_into('<I', blocks[i], loc, ((3, 6, 7)[tgt] << 28) + off + 1)
    return {3: blocks[0], 6: blocks[1], 7: blocks[2]}, d

def deref(blocks, ref, n):
    ref -= 1; blk = ref >> 28; off = ref & 0xfffffff
    return blocks[blk][off:off + n]

if __name__ == '__main__':
    b = open(sys.argv[1], 'rb').read()
    blocks, d = read_container(b)
    root = blocks[3]
    name = deref(blocks, u32(root, 0), 80).split(b'\0')[0].decode()
    cnt = u16(root, 8)
    print('name', name, 'surfaces', cnt, 'block sizes', [len(blocks[k]) for k in (3, 6, 7)])
    surfs = deref(blocks, u32(root, 4), 84 * cnt)
    for i in range(cnt):
        h = surfs[84 * i:84 * i + 84]
        vc, tc = u16(h, 4), u16(h, 6)
        vinfo = struct.unpack_from('<4H', h, 20)
        print(f' surf{i}: hdr0..4={h[:4].hex()} verts={vc} tris={tc} baseTri={u16(h,8)} baseVert={u16(h,10)} vinfo={vinfo} rigid={u32(h,40)}')
        verts = deref(blocks, u32(h, 32), 32 * vc)
        tris = deref(blocks, u32(h, 16), 6 * tc)
        xs = [struct.unpack_from('<3f', verts, 32 * v) for v in range(vc)]
        mn = [min(p[k] for p in xs) for k in range(3)]; mx = [max(p[k] for p in xs) for k in range(3)]
        print('   bounds', [round(v, 2) for v in mn], [round(v, 2) for v in mx])
        idx = struct.unpack_from('<%dH' % (3 * tc), tris)
        print('   max index', max(idx), '<', vc)
        v0 = verts[:32]
        f = struct.unpack_from('<4f4I', v0)
        print('   v0', [round(x, 3) for x in f[:4]], [hex(x) for x in f[4:]])
        rl = deref(blocks, u32(h, 44), 12 * u32(h, 40)) if u32(h, 40) else b''
        for r in range(u32(h, 40)):
            print('   rigid', struct.unpack_from('<4H', rl, 12 * r), hex(u32(rl, 12 * r + 8)))
