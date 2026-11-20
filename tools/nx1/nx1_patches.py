"""Corrections to the structure layouts that the loaders of the NX1 build need, shared by gen_nx1_schema.py and loader_rules.py."""

# Arrays of pointers that the loader loads element by element, interleaved with another member (the zone stores
# name 0, knots 0, name 1, knots 1): the array is replaced by one member per element.
SPLITS = {
    "WeaponDef": ["accuracyGraphName", "originalAccuracyGraphKnots"],
}


def split_members(name, info):
    """Applies SPLITS to the structure info (as `pdb_types.Types.struct_info` returns it)."""
    names = SPLITS.get(name)
    if not names or info.get("kind") == "enum":
        return info
    out = []
    for m in info["members"]:
        t = m["t"]
        if m["n"] in names and t["k"] == "arr" and t["of"]["k"] == "ptr":
            step = m["sz"] // t["n"]
            for i in range(t["n"]):
                out.append({"n": "%s%d" % (m["n"], i), "o": m["o"] + i * step, "t": t["of"], "sz": step})
        else:
            out.append(m)
    info = dict(info)
    info["members"] = out
    return info
