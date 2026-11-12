"""道具（剑/盾/枪）穿过身体的检测，按「顶点组全部指向 *_joint」识别道具网格。

这套资产的道具网格名字是靠不住的：Paladin 里叫 `Sword` 的网格是整个身体（7093 顶点），
叫 `Shield` 的是头盔（绑 Head），真正的剑是那个没名字的 116 顶点网格（绑 Sword_joint），
盾绑 Shield_joint。所以只能按绑定骨找。

判定：把身体网格建成 BVH，对每个道具顶点找最近面，用面法线判断在身体内侧还是外侧，
内侧的深度就是穿透量（cm）。只统计穿透，不统计距离 —— 盾牌贴着胸甲是设计，
「距离 0cm」不代表穿模，但「顶点在身体里面」一定是。

用法：
    blender -b --python scripts/prop_gap.py -- <repo> <cache.blend> <clip> <deg>
输出一行 `PROP {...}` JSON。
"""
import bpy
import json
import os
import sys
from types import SimpleNamespace
from mathutils.bvhtree import BVHTree

NEAR_LIMIT = 0.10          # 离身体超过 10cm 的道具顶点不参与判定


def load_target(repo, cache, clip, deg):
    """真实重定向。返回 (E, armature, body_mesh, prop_meshes)。"""
    sys.path.insert(0, os.path.join(os.path.abspath(repo), "scripts"))
    import importlib
    E = importlib.import_module("export_mixamo_animations")
    os.environ["CRAB_ARM_ABDUCT"] = str(deg)
    E._CUR_MODEL = os.path.basename(cache).rsplit(".blend", 1)[0].replace("_", " ")

    args = SimpleNamespace(ground_margin=0.15, frame_step=2, no_ground=True, ik=False,
                           cache_dir=E.default_cache_dir(), ratio_mode="leg",
                           no_rebase_rest=False)
    ref = E.reference_leg(args)
    bpy.ops.wm.open_mainfile(filepath=cache)
    E.ensure_addon()
    arm = next(o for o in bpy.context.scene.objects if o.type == "ARMATURE")
    ratio = E.rest_leg_length(arm) / ref
    anim = os.path.join(os.path.abspath(repo),
                        "assets/source/character/animations/Default/%s.fbx" % clip)
    src = E.import_anim_source(anim, arm, ratio, rebase=True)
    bpy.context.scene.mix_source_armature = src
    E.deselect_all()
    arm.select_set(True)
    bpy.context.view_layer.objects.active = arm
    if bpy.context.object and bpy.context.object.mode != "OBJECT":
        bpy.ops.object.mode_set(mode="OBJECT")
    result = bpy.ops.mr.import_anim_to_rig()
    if "FINISHED" not in result:
        raise RuntimeError("import_anim_to_rig 失败：%s" % (result,))

    # 与导出口径保持一致：导出前会给 Sword_joint / Shield_joint 设固定姿态，
    # 检测也必须设同样的值，否则测到的是没修的状态。
    E.apply_prop_joint_pose(arm)

    meshes = [o for o in bpy.context.scene.objects if o.type == "MESH" and
              (o.parent == arm or any(m.type == "ARMATURE" and m.object == arm
                                      for m in o.modifiers))]
    body = max(meshes, key=lambda o: (len(o.data.vertices), len(o.vertex_groups)))
    props = []
    for o in meshes:
        if o is body:
            continue
        gs = {g.name.split(":")[-1] for g in o.vertex_groups}
        if gs and all(g.endswith("joint") for g in gs):
            props.append(o)
    return E, arm, body, props


def measure(arm, body, props, frame):
    """返回 {道具名: {n, npen, depth_cm}}，depth_cm 是最深穿透。"""
    bpy.context.scene.frame_set(frame)
    dg = bpy.context.evaluated_depsgraph_get()
    dg.update()

    ev = body.evaluated_get(dg)
    me = ev.to_mesh()
    M = body.matrix_world
    verts = [M @ v.co for v in me.vertices]
    polys = [list(p.vertices) for p in me.polygons if len(p.vertices) >= 3]
    bvh = BVHTree.FromPolygons(verts, polys)
    ev.to_mesh_clear()

    out = {}
    for o in props:
        e2 = o.evaluated_get(dg)
        m2 = e2.to_mesh()
        M2 = o.matrix_world
        worst = 0.0
        npen = 0
        n = 0
        for v in m2.vertices:
            p = M2 @ v.co
            loc, nor, idx, d = bvh.find_nearest(p, NEAR_LIMIT)
            if loc is None:
                continue
            n += 1
            if (p - loc).dot(nor) < 0:
                npen += 1
                worst = min(worst, (p - loc).dot(nor))
        e2.to_mesh_clear()
        out[o.name] = {"n": n, "npen": npen, "depth_cm": round(-worst * 100, 2)}
    return out


def evaluate(repo, cache, clip, deg, frames=(1, 60, 120, 180)):
    E, arm, body, props = load_target(repo, cache, clip, deg)
    if not props:
        return {"model": E._CUR_MODEL, "clip": clip, "deg": deg,
                "props": {}, "max_depth_cm": 0.0, "max_npen": 0}
    per_frame = {str(f): measure(arm, body, props, f) for f in frames}
    props_min = {}
    for v in per_frame.values():
        for name, d in v.items():
            cur = props_min.setdefault(name, {"depth_cm": 0.0, "npen": 0})
            cur["depth_cm"] = max(cur["depth_cm"], d["depth_cm"])
            cur["npen"] = max(cur["npen"], d["npen"])
    return {"model": E._CUR_MODEL, "clip": clip, "deg": deg,
            "frames": per_frame,
            "props": props_min,
            "max_depth_cm": max((v["depth_cm"] for v in props_min.values()), default=0.0),
            "max_npen": max((v["npen"] for v in props_min.values()), default=0)}


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    repo, cache, clip, deg = argv[0], argv[1], argv[2], float(argv[3])
    print("PROP " + json.dumps(evaluate(repo, cache, clip, deg), ensure_ascii=False))


if __name__ == "__main__":
    main()
