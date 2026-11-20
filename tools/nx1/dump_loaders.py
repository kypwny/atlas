"""IDAPython script: decompiles the generated `Load_<Structure>` functions of the NX1 executable into one `<Structure>.c` file each,
the input of loader_rules.py.

The executable is the Xbox 360 image that belongs to the program database (converted to a PE file), opened in IDA with the
PowerPC decompiler. Run it inside IDA after setting the two paths below:
  MAP  the linker map of the same build (`?Load_Name@@YAX_N@Z  <address>` lines name the loaders)
  OUT  the output folder
"""
import os
import re

import ida_hexrays
import ida_name

MAP = r"6-nx1mp_fast_server.map"
OUT = r"loaders"

symbols = []
for line in open(MAP, errors="replace"):
    m = re.search(r"\?(\w+)@@\w+\s+([0-9a-f]{8}) f", line)
    if m:
        symbols.append((m.group(1), int(m.group(2), 16)))

os.makedirs(OUT, exist_ok=True)
# name every function after the map (the decompiler then prints the called loaders by name)
for name, ea in symbols:
    ida_name.set_name(ea, name, ida_name.SN_NOCHECK | ida_name.SN_NOWARN | ida_name.SN_FORCE)
for name, ea in symbols:
    mm = re.match(r"Load_(\w+)$", name)
    if not mm or re.search(r"(Array|Stream)$", mm.group(1)):
        continue
    try:
        text = str(ida_hexrays.decompile(ea))
    except Exception as e:  # functions the decompiler refuses are left out
        text = "ERR %s" % e
    with open(os.path.join(OUT, mm.group(1) + ".c"), "w") as f:
        f.write(text)
