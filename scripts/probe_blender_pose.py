#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""无头摆帧，读跑步跳派生剪辑左右腿髋/膝/踝世界位置与膝角。

用来定位「腾空段一条腿僵直垂直下垂」：膝角接近 180° 即伸直，髋踝水平距小即垂直挂着。

用法（Blender 无头）：

    /Applications/Blender.app/Contents/MacOS/Blender -b \
        --python scripts/probe_blender_pose.py -- \
        --glb assets/characters/animations/Amy/RunningJumpRight.glb --times 0.15 0.30 0.45
"""
import argparse
import bpy
import math
import os
import sys
from mathutils import Vector


def parse_args():
    argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
    p = argparse.ArgumentParser()
    p.add_argument("--glb", required=True)
    p.add_argument("--times", nargs="+", type=float, required=True)
    return p.parse_args(argv)


def obj(name):
    return bpy.data.objects.get(name)


def joint(side, suffix):
    return obj(f"mixamorig:{side}{suffix}")


def world_pos(node):
    return node.matrix_world.translation.copy() if node else None


def knee_angle(hip, knee, ankle):
    if hip is None or knee is None or ankle is None:
        return None
    a = (hip - knee).normalized()
    b = (ankle - knee).normalized()
    dot = max(-1.0, min(1.0, a.dot(b)))
    return math.degrees(math.acos(dot))


def main():
    args = parse_args()
    bpy.ops.wm.read_factory_settings(use_empty=True)
    bpy.context.scene.render.fps = 60
    bpy.ops.import_scene.gltf(filepath=args.glb)

    for t in args.times:
        bpy.context.scene.frame_set(round(t * 60))
        bpy.context.view_layer.update()
        print(f"\n=== t={t:.2f}s  frame={round(t*60)}")
        for side in ("Left", "Right"):
            hip = world_pos(joint(side, "UpLeg"))
            knee = world_pos(joint(side, "Leg"))
            ankle = world_pos(joint(side, "Foot"))
            if hip is None or knee is None or ankle is None:
                print(f"  {side}: 缺节点")
                continue
            ang = knee_angle(hip, knee, ankle)
            spread = (ankle - hip)
            spread.z = 0.0
            print(
                f"  {side:5} hip=({hip.x:+.3f},{hip.y:+.3f},{hip.z:+.3f}) "
                f"knee=({knee.x:+.3f},{knee.y:+.3f},{knee.z:+.3f}) "
                f"ankle=({ankle.x:+.3f},{ankle.y:+.3f},{ankle.z:+.3f}) "
                f"膝角={ang:5.1f} 髋踝水平距={spread.length:.3f}"
            )


if __name__ == "__main__":
    main()
