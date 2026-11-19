"""Compare the UVs of a streamed model with an SEModel export of the same model (ground truth).

usage: check_uv_vs_semodel.py <streamed model file> <model.semodel>

Reports which half of the packed texcoord word is U and whether V needs flipping, by matching the sets of
(u, v) pairs. The SEModel format is documented by SE-Tools (SEModel v1).
"""
import struct, sys
sys.path.insert(0, __file__.rsplit('\\', 1)[0].rsplit('/', 1)[0])
from probe_model import read_container, deref, u32, u16


def half(h):
    s = -1.0 if h & 0x8000 else 1.0
    e = (h >> 10) & 31
    f = h & 0x3FF
    if e == 0:
        return s * f * 2.0 ** -24
    return s * (1 + f / 1024) * 2.0 ** (e - 15)


def mine(path):
    b = open(path, 'rb').read()
    blocks, _ = read_container(b)
    root = blocks[3]
    cnt = u16(root, 8)
    surfs = deref(blocks, u32(root, 4), 84 * cnt)
    out = []
    for i in range(cnt):
        h = surfs[84 * i:84 * i + 84]
        vc = u16(h, 4)
        verts = deref(blocks, u32(h, 32), 32 * vc)
        uvs = []
        for v in range(vc):
            tc = struct.unpack_from('<I', verts, 32 * v + 20)[0]
            uvs.append((half(tc & 0xFFFF), half(tc >> 16)))
        out.append(uvs)
    return out


def semodel_uvs(path):
    b = open(path, 'rb').read()
    assert b[:7] == b'SEModel', 'not an SEModel'
    ver, hsz = struct.unpack_from('<HH', b, 7)
    flags = b[11]
    nb, nm, nmat = struct.unpack_from('<III', b, 12)
    pos = hsz
    # bone tags: null-terminated names
    for _ in range(nb):
        pos = b.index(b'\0', pos) + 1
    bone_flags = None
    if flags & 1:  # bones present
        # the bone data layout depends on the bone-data flags byte
        bone_flags = b[pos]; pos += 1
        for _ in range(nb):
            pos += 1  # padding
            pos += 4  # parent index
            if bone_flags & 1:  # global matrices
                pos += 12 + 16
            if bone_flags & 2:  # local matrices
                pos += 12 + 16
            if bone_flags & 4:  # scale
                pos += 12
    meshes = []
    for _ in range(nm):
        pos += 1  # mesh flags byte (unused here)
        uvsets, maxskin = b[pos], b[pos + 1]
        pos += 2
        vcnt, fcnt = struct.unpack_from('<II', b, pos)
        pos += 8
        pos += 12 * vcnt                       # positions
        uv = []
        for v in range(vcnt):
            uv.append(struct.unpack_from('<2f', b, pos + 8 * v * 1) if uvsets == 1 else None)
        pos += 8 * vcnt * uvsets               # uv sets
        pos += 12 * vcnt                       # normals
        pos += 4 * vcnt                        # colours
        pos += vcnt * maxskin * 6              # weights (u32 bone? + float) layout varies; approximate
        pos += fcnt * 3 * (1 if vcnt < 256 else 2 if vcnt < 65536 else 4)
        meshes.append(uv)
    return meshes


if __name__ == '__main__':
    a = mine(sys.argv[1])
    t = semodel_uvs(sys.argv[2])
    ref = sorted((round(u, 3), round(v, 3)) for m in t for uv in [m] for u, v in (uv if uv[0] is not None else []))
    print('streamed surfaces', [len(x) for x in a], 'semodel meshes', [len(x) for x in t])
    allmine = [uv for s in a for uv in s]
    for name, f in (('u=low v=high', lambda p: p), ('u=high v=low', lambda p: (p[1], p[0]))):
        for flip in (False, True):
            got = sorted((round(f(p)[0], 3), round(1 - f(p)[1] if flip else f(p)[1], 3)) for p in allmine)
            same = sum(1 for x, y in zip(got, ref) if x == y) / max(1, len(ref))
            print(f'{name:14s} flipV={flip!s:5s} -> {same:6.1%} of sorted UV pairs equal')
