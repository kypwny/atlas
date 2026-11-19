import bpy, sys, addon_utils

paths = sys.argv[sys.argv.index("--") + 1:]
path = paths[0]
mods = [m.__name__ for m in addon_utils.modules() if "cast" in m.__name__.lower()]
print("CAST addon candidates:", mods)
ok = False
for name in mods:
    try:
        addon_utils.enable(name, default_set=False)
        ok = True
        break
    except Exception as e:
        print("enable failed", name, e)
print("addon enabled:", ok, "cast operator:", hasattr(bpy.ops.import_scene, "cast"))
bpy.ops.import_scene.cast(filepath=path)
objs = list(bpy.data.objects)
print("objects:", len(objs))
tv = tf = 0
lo = [1e30] * 3
hi = [-1e30] * 3
for o in objs:
    if o.type == "MESH":
        m = o.data
        tv += len(m.vertices)
        tf += len(m.polygons)
        print(" mesh", o.name, "verts", len(m.vertices), "faces", len(m.polygons), "uv layers", len(m.uv_layers), "materials", [s.material.name if s.material else None for s in o.material_slots])
        for v in m.vertices:
            w = o.matrix_world @ v.co
            for a in range(3):
                lo[a] = min(lo[a], w[a])
                hi[a] = max(hi[a], w[a])
    elif o.type == "ARMATURE":
        print(" armature", o.name, "bones", len(o.data.bones))
print("TOTAL verts", tv, "faces", tf)
print("bounds", [round(x, 2) for x in lo], [round(x, 2) for x in hi])

# materials: image textures the Cast File nodes made
imgs = set()
for m in bpy.data.materials:
    if m.node_tree:
        for n in m.node_tree.nodes:
            if n.type == "TEX_IMAGE" and n.image:
                imgs.add((n.image.name, tuple(n.image.size)))
print("image textures:", len(imgs), sorted(imgs)[:3])

# animations: every further path is imported on top of the model's armature
arm = next((o for o in bpy.data.objects if o.type == "ARMATURE"), None)
for ap in paths[1:]:
    before = set(bpy.data.actions.keys())
    bpy.ops.import_scene.cast(filepath=ap)
    new = [bpy.data.actions[k] for k in bpy.data.actions.keys() if k not in before]
    for a in new:
        fc = list(a.fcurves) if hasattr(a, "fcurves") else []
        if not fc:
            # Blender 5 stores curves in layered action slots
            for layer in getattr(a, "layers", []):
                for strip in layer.strips:
                    for cb in strip.channelbags:
                        fc.extend(cb.fcurves)
        kp = sum(len(f.keyframe_points) for f in fc)
        print(" animation", a.name, "fcurves", len(fc), "keyframes", kp, "frame range", tuple(round(x, 1) for x in a.frame_range))
        # the pose really moves: compare a bone's rotation at the first and the middle frame
        if arm is not None and fc:
            arm.animation_data.action = a
            f0, f1 = a.frame_range
            moved = 0
            snaps = {}
            for fr in (f0, (f0 + f1) / 2):
                bpy.context.scene.frame_set(int(fr))
                snaps[fr] = {b.name: tuple(b.rotation_quaternion) + tuple(b.location) for b in arm.pose.bones}
            ks = list(snaps)
            for name in snaps[ks[0]]:
                if any(abs(x - y) > 1e-4 for x, y in zip(snaps[ks[0]][name], snaps[ks[1]][name])):
                    moved += 1
            print("  bones whose pose differs between first and middle frame:", moved, "of", len(arm.pose.bones))

# the posed mesh: bounds of the evaluated (skinned) meshes at the first and the middle frame of the last animation
if arm is not None and len(paths) > 1:
    dg = bpy.context.evaluated_depsgraph_get()
    for fr in (0, int(bpy.context.scene.frame_end // 2) or 1):
        bpy.context.scene.frame_set(fr)
        dg = bpy.context.evaluated_depsgraph_get()
        lo = [1e30] * 3
        hi = [-1e30] * 3
        for o in bpy.data.objects:
            if o.type == "MESH" and o.name != "Cube":
                e = o.evaluated_get(dg)
                me = e.to_mesh()
                for v in me.vertices:
                    w = e.matrix_world @ v.co
                    for a in range(3):
                        lo[a] = min(lo[a], w[a])
                        hi[a] = max(hi[a], w[a])
                e.to_mesh_clear()
        print("posed bounds at frame", fr, [round(x, 1) for x in lo], [round(x, 1) for x in hi])
