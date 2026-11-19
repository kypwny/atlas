import re, sys
import os
ZT = os.environ.get('ZONETOOL_SRC', 'zonetool/src')  # the src folder of a ZoneTool checkout
WANT = sys.argv[1].split(',')


def members(text, name):
    m = re.search(r'^\t\tstruct ' + name + r'\s*$', text, re.M)
    if not m:
        return []
    i = text.index('{', m.end())
    depth = 0
    j = i
    out = []
    cur = ''
    while True:
        c = text[j]
        if c == '{':
            depth += 1
            if depth > 1:
                cur += c
        elif c == '}':
            depth -= 1
            if depth == 0:
                break
            cur += c
        elif c == ';' and depth == 1:
            out.append(cur.strip())
            cur = ''
        elif depth >= 1:
            cur += c
        j += 1
    names = []
    for stmt in out:
        stmt = re.sub(r'//[^\n]*', '', stmt)
        stmt = re.sub(r'/\*.*?\*/', '', stmt, flags=re.S).strip()
        if not stmt or '{' in stmt or stmt.startswith('union') or ':' in stmt.split('=')[0].split('//')[0] and not '::' in stmt:
            continue
        mm = re.search(r'(\w+)\s*(\[[^\]]*\])*\s*$', stmt)
        if mm:
            names.append(mm.group(1))
    return names


def gen(ns, path, out):
    t = open(path, encoding='utf8', errors='replace').read()
    lines = ['typedef unsigned long DWORD;', '#include <cstdint>', '#include <cstddef>', '#include <cstring>', '#include <cstdio>', '#include <string>', '#include <vector>', '#include <map>', '#include <memory>']
    if ns == 'CODO':
        lines.append('namespace ZoneTool{namespace IW5{struct GfxImageFileHeader{char tag[3];char version;uint8_t flags;short dimensions[3];int fileSizeForPicmip[4];};}}')
    lines.append('#include "%s"' % path)
    lines.append('int main(){')
    for s in WANT:
        for mem in members(t, s):
            lines.append('printf("%s %s %%d\\n",(int)offsetof(ZoneTool::%s::%s,%s));' % (s, mem, ns, s, mem))
    lines.append('return 0;}')
    open(out, 'w').write('\n'.join(lines))


gen('IW4', ZT + '/IW4/Structs.hpp', 'off_iw4.cpp')
