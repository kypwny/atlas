import re
import os
ZT = os.environ.get('ZONETOOL_SRC', 'zonetool/src')  # the src folder of a ZoneTool checkout


def gen(ns, path, out):
    t = open(path, encoding='utf8', errors='replace').read()
    ind = '		' if ns == 'IW4' else '	'
    names = sorted(set(re.findall(r'^' + ind + r'(?:struct|union)\s+(\w+)\s*(?::[^{]*)?$', t, re.M)))
    names = [n for n in names if n != 'VecInternal']
    lines = ['typedef unsigned long DWORD;', '#include <cstdint>', '#include <cstddef>', '#include <cstring>', '#include <cstdio>', '#include <string>', '#include <vector>', '#include <map>', '#include <memory>']
    if ns == 'CODO':
        lines.append('namespace ZoneTool{namespace IW5{struct GfxImageFileHeader{char tag[3];char version;uint8_t flags;short dimensions[3];int fileSizeForPicmip[4];};}}')
    lines.append('#include "%s"' % path)
    lines.append('int main(){')
    for n in names:
        lines.append('printf("%s %%d\\n",(int)sizeof(ZoneTool::%s::%s));' % (n, ns, n))
    lines.append('return 0;}')
    open(out, 'w').write('\n'.join(lines))
    return names


gen('IW4', ZT + '/IW4/Structs.hpp', 'iw4.cpp')
gen('CODO', ZT + '/CODO/Structs.hpp', 'codo.cpp')
