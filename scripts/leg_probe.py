"""测重定向后双腿的横向张开量，用来定位「走路叉得太开」。

输出三类距离（单位 cm），都按腿长归一，便于跨体型比较：
  hip  = 左右 UpLeg 的 |x| 间距
  knee = 左右 Leg 的 |x| 间距
  ankle= 左右 Foot 的 |x| 间距
并给出目标骨架静止姿态的同样距离（rest），差值就是「动画把腿带开多少」。

用法：
    blender -b --python scripts/leg_probe.py -- <repo> <cache.blend> <clip> <deg>
"""
import bpy
import json
import os
import sys
from types import SimpleNamespace
from mathutils import Vector

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

PAIRS = (("LeftUpLeg", "RightUpLeg", "hip"),
         ("LeftLeg", "RightLeg", "knee"),
         ("LeftFoot", "RightFoot", "ankle"),
         ("LeftToeBase", "RightToeBase", "toe"))


def bone_x(arm, name):
    pb = next((b for b in arm.pose.bones if b.name.split(":")[-1] == name), None)
    if pb is None:
        return None
    return (arm.matrix_world @ pb.head).x


def rest_x(arm, name):
    b = next((b for b in arm.data.bones if b.name.split(":")[-1] == name), None)
    return None if b is None else (arm.matrix_world @ b.head_local).x


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    repo, cache, clip, deg = argv[0], argv[1], argv[2], float(argv[3])
    import prop_gap
    E, arm, body, props = prop_gap.load_target(repo, cache, clip, deg)

    leg = E.rest_leg_length(arm)
    out = {"model": E._CUR_MODEL, "clip": clip, "deg": deg,
           "leg_len_cm": round(leg * 100, 2), "frames": {}}
    for f in (1, 60, 120, 180):
        bpy.context.scene.frame_set(f)
        bpy.context.view_layer.update()
        row = {}
        for l, r, key in PAIRS:
            lx, rx = bone_x(arm, l), bone_x(arm, r)
            if lx is None or rx is None:
                continue
            row[key] = round((rx - lx) * 100, 2)
            row[key + "_ratio"] = round((rx - lx) / leg, 3)
        out["frames"][str(f)] = row
    rest = {}
    for l, r, key in PAIRS:
        lx, rx = rest_x(arm, l), rest_x(arm, r)
        if lx is None or rx is None:
            continue
        rest[key] = round((rx - lx) * 100, 2)
        rest[key + "_ratio"] = round((rx - lx) / leg, 3)
    out["rest"] = rest
    print("LEG " + json.dumps(out, ensure_ascii=False))


if __name__ == "__main__":
    main()
