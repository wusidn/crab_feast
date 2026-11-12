"""手臂 / 道具 ↔ 身体 的间隙检测，在真实重定向后的姿态上量。

两项指标（cm）：

1. `joint_cm` —— 肘、腕关节中心相对「身体轮廓在该高度、该纵深处的最大 |x|」的余量。
   负值 = 关节落在身体轮廓里，就是肉眼看到的「手臂穿进身体」。
   只取关节、不取手臂顶点：掌心、护肩、袖口本来就会压进轮廓，纳进来全是误报
   （实测把 Castle Guard 的护臂顶点算进去会给出 −4cm 的假穿模）。
   不测道具：Paladin 里名为 `Sword` 的网格其实是整个身体（7093 顶点，权重散在 Head /
   Spine2 / UpLeg），和腿必然重叠，任何「道具↔腿」判据都会给出 0cm 假穿模。

为什么不用「最近顶点距离」当目标函数：它随角度忽大忽小（X Bot 在 6° / 8° / 10° 分别是
0.30 / 0.37 / 0.18cm），因为最近点对会跳部位，无法二分。横向余量随角度单调。

用法：
    blender -b --python scripts/arm_gap.py -- <repo> <cache.blend> <clip> <deg> [frames]
输出一行 `GAP {...}` JSON。
"""
import bpy
import json
import os
import sys
from types import SimpleNamespace

BODY_KEYS = ("Hips", "Spine", "Spine1", "Spine2", "Neck", "Chest")
LEG_KEYS = ("LeftUpLeg", "RightUpLeg", "LeftLeg", "RightLeg", "LeftFoot", "RightFoot")


def classify(obj):
    """按顶点组最大权重把顶点分成 body（躯干+腿）/ other。"""
    groups = {g.index: g.name for g in obj.vertex_groups}
    out = {}
    for v in obj.data.vertices:
        if not v.groups:
            out[v.index] = "other"
            continue
        name = groups.get(max(v.groups, key=lambda g: g.weight).group, "").split(":")[-1]
        out[v.index] = "body" if (name in BODY_KEYS or name in LEG_KEYS) else "other"
    return out


def load_target(repo, cache, clip, deg):
    """打开 rig 缓存，把指定 clip 真实重定向进去。返回 (E, armature, meshes, cls)。"""
    sys.path.insert(0, os.path.join(os.path.abspath(repo), "scripts"))
    import importlib
    E = importlib.import_module("export_mixamo_animations")
    os.environ["CRAB_ARM_ABDUCT"] = str(deg)
    E._CUR_MODEL = os.path.basename(cache).rsplit(".blend", 1)[0].replace("_", " ")

    args = SimpleNamespace(ground_margin=0.15, frame_step=2, no_ground=True, ik=False,
                           cache_dir=E.default_cache_dir(), ratio_mode="leg",
                           no_rebase_rest=False)
    ref = E.reference_leg(args)              # 必须先算：它会打开 Y Bot 缓存
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

    meshes = [o for o in bpy.context.scene.objects if o.type == "MESH" and
              (o.parent == arm or any(m.type == "ARMATURE" and m.object == arm
                                      for m in o.modifiers))]
    cls = {o.name: classify(o) for o in meshes}
    return E, arm, meshes, cls


def measure(arm, meshes, cls, frame):
    """测一帧。姿态必须来自烘焙好的 Action —— COPY_TRANSFORMS 会覆盖手工摆的姿势。"""
    bpy.context.scene.frame_set(frame)
    dg = bpy.context.evaluated_depsgraph_get()
    dg.update()

    buckets = {}       # y 桶(3cm) -> [(z, |x|)]，只放躯干和腿
    for o in meshes:
        ev = o.evaluated_get(dg)
        me = ev.to_mesh()
        M = o.matrix_world
        c = cls[o.name]
        for v in me.vertices:
            p = M @ v.co
            if c.get(v.index) == "body":
                buckets.setdefault(round(p.y / 0.03), []).append((p.z, abs(p.x)))
        ev.to_mesh_clear()

    pbs = {p.name.split(":")[-1]: p for p in arm.pose.bones}

    def clearance(p, zwin=0.08):
        cand = []
        b = round(p.y / 0.03)
        for bi in (b - 1, b, b + 1):
            cand += [(z, ax) for (z, ax) in buckets.get(bi, []) if abs(z - p.z) < zwin]
        if not cand:
            return None
        return abs(p.x) - max(ax for _, ax in cand)

    worst = 1e9
    where = None
    for side in ("Left", "Right"):
        for tag, bone in ((side + "Elbow", side + "ForeArm"), (side + "Wrist", side + "Hand")):
            if bone not in pbs:
                continue
            c_ = clearance(arm.matrix_world @ pbs[bone].head)
            if c_ is not None and c_ < worst:
                worst, where = c_, tag

    return {"joint_cm": round(worst * 100, 2) if worst < 1e8 else None,
            "where": where}


def evaluate(repo, cache, clip, deg, frames=(1, 60, 120, 180)):
    E, arm, meshes, cls = load_target(repo, cache, clip, deg)
    per_frame = {str(f): measure(arm, meshes, cls, f) for f in frames}
    joints = [v["joint_cm"] for v in per_frame.values() if v["joint_cm"] is not None]
    return {"model": E._CUR_MODEL, "clip": clip, "deg": deg,
            "frames": per_frame,
            "min_joint": min(joints) if joints else None}


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    repo, cache, clip, deg = argv[0], argv[1], argv[2], float(argv[3])
    print("GAP " + json.dumps(evaluate(repo, cache, clip, deg), ensure_ascii=False))


if __name__ == "__main__":
    main()
