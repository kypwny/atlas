"""Probe an inflated CoDOL zone image (see codol_qa_fastfile.py for making one). Usage: probe_zone.py ZONE"""
import struct, sys, collections
b = open(sys.argv[1], 'rb').read()
u32 = lambda o: struct.unpack_from('<I', b, o)[0]
print('size', len(b), 'size-0x28 ==', u32(0), 'blocks', [u32(8 + 4 * i) for i in range(8)])
nstr, nass = u32(0x28), u32(0x30)
pos = 0x38 + nstr * 4
for i in range(nstr):
    if u32(0x38 + 4 * i) == 0xFFFFFFFF:
        pos = b.index(b'\0', pos) + 1
table = pos
print('strings', nstr, 'assets', nass, 'table at', hex(table))
seq = []
for i in range(nass):
    t, off, _, ext = struct.unpack_from('<4I', b, table + 16 * i)
    seq.append((t, off + 0x28, ext))
runs = []
for t, off, ext in seq:
    if runs and runs[-1][0] == t: runs[-1][1] += 1; runs[-1][2] += ext
    else: runs.append([t, 1, ext])
print('type runs (type, count, bytes):', runs[:60], '... total runs', len(runs))
print('bytes by type', {t: sum(e for tt, _, e in seq if tt == t) for t in sorted({t for t, _, _ in seq})})
