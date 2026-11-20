#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""[已废弃] 不要用这个脚本生成派生剪辑。

旧机制把支撑腿 FK 链在腾空段满权重回正到 rest 站立姿态，膝角被钉到 ~178°、
整条腿僵直垂直垂挂。左右镜像请改用 `make_runjump_mirror.py`（真正的骨骼左右反射）。
本文件仅留作历史参考。

---

从 `RunningJump.glb` 程序化派生左右腿版本：`RunningJumpLeft.glb` / `RunningJumpRight.glb`。

为什么要派生而不是重做
----------------------
`RunningJump.glb` 是奔跑起跳（原片右腿蹬伸、左腿前摆落地）。跑动起跳时支撑脚
可能是左也可能是右，所以按起跳支撑脚各出一条：支撑腿在空中不回膝盖（向绑定姿态
回正），摆动腿原样保留。髋曲线、时长、垂直驱动全部不变，跳跃层时间轴不用改。

做法（与 `make_hop_clips.py` 同一套机制）
----------------------------------------
对支撑腿的 FK 链（`UpLeg / Leg / Foot / ToeBase`）做「向绑定姿态回正」的带权混合：
权重 1 = 这条腿回到绑定姿态（伸直），0 = 完全用剪辑原本的姿态。

实测锚点（Amy `RunningJump.glb`，0.900s）：离地 0.03s，顶点 0.33s，落地 0.66s。
回正窗口 0.05→0.60：离地后迅速伸直，落地前淡出，借剪辑自己的落地屈膝接上。

注意
----
这份 GLB 是纯动画包（没有蒙皮），Blender 导入后得到的是一堆 **empty 节点**而不是
骨架，所以改的是对象级的 `rotation_quaternion` 曲线，不是 pose bone。导入器会把
每个节点做 Y-up → Z-up 的旋转共轭，绑定姿态也要按同一规则换算过去；共轭是线性的，
所以在 Blender 空间里混到「换算后的绑定姿态」等于在 glTF 空间里混到原始绑定姿态。

用法（必须 Blender 无头模式）：

    /Applications/Blender.app/Contents/MacOS/Blender -b \
        --python scripts/make_runjump_clips.py -- \
        --src assets/characters/animations/Amy/RunningJump.glb \
        --out-dir assets/characters/animations/Amy
"""

from __future__ import annotations

import argparse
import json
import math
import os
import struct
import sys

import bpy  # type: ignore

FPS = 60

# 支撑腿的回正窗口：(秒, 权重)，中间线性插值。0.900s 剪辑，空中段 0.03→0.66。
WINDOWS = {
    "Left": [(0.05, 0.0), (0.15, 1.0), (0.45, 1.0), (0.60, 0.0)],
    "Right": [(0.05, 0.0), (0.15, 1.0), (0.45, 1.0), (0.60, 0.0)],
}

# 支撑腿的 FK 链（对象名带 `mixamorig:` 前缀）
CHAIN = ("UpLeg", "Leg", "Foot", "ToeBase")


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
    p = argparse.ArgumentParser(description="从 RunningJump.glb 派生左右腿奔跑跳剪辑")
    p.add_argument("--src", required=True, help="源 RunningJump.glb")
    p.add_argument("--out-dir", required=True, help="输出目录（写 RunningJumpLeft/Right.glb）")
    p.add_argument("--base", default="RunningJump", help="输出剪辑名前缀，默认 RunningJump")
    return p.parse_args(argv)


def read_glb_json(path: str) -> dict:
    """只取 GLB 的 JSON 块，用来读节点的绑定姿态"""
    with open(path, "rb") as handle:
        data = handle.read()
    magic, _, length = struct.unpack_from("<III", data, 0)
    if magic != 0x46546C67:
        raise RuntimeError(f"{path} 不是 GLB")
    offset = 12
    while offset < length:
        chunk_len, chunk_type = struct.unpack_from("<II", data, offset)
        chunk = data[offset + 8: offset + 8 + chunk_len]
        if chunk_type == 0x4E4F534A:
            return json.loads(chunk.decode("utf-8"))
        offset += 8 + chunk_len
    raise RuntimeError(f"{path} 里没有 JSON 块")


def weight_at(time: float, window: list[tuple[float, float]]) -> float:
    if time <= window[0][0]:
        return window[0][1]
    if time >= window[-1][0]:
        return window[-1][1]
    for index in range(len(window) - 1):
        t0, w0 = window[index]
        t1, w1 = window[index + 1]
        if t0 <= time <= t1:
            span = t1 - t0
            return w0 if span <= 0.0 else w0 + (w1 - w0) * (time - t0) / span
    return window[-1][1]


def iter_fcurves(action) -> list:
    """取动作里全部 F-Curve：单层动作走 `action.fcurves`，分槽动作走 channelbag"""
    curves = list(getattr(action, "fcurves", []))
    if curves:
        return curves
    for layer in getattr(action, "layers", []):
        for strip in getattr(layer, "strips", []):
            for bag in getattr(strip, "channelbags", []):
                curves.extend(bag.fcurves)
    return curves


def curves_of(obj) -> list:
    """取**某一个对象**自己的 F-Curve。

    一个动作装着一整套 slot（每个节点一个），数据路径全都叫 `rotation_quaternion`，
    所以不能按 data_path 过滤整张动作——必须按该对象的 action slot 找到它自己的
    channelbag，否则改的是别人的曲线。
    """
    anim = getattr(obj, "animation_data", None)
    if anim is None or anim.action is None:
        return []
    action = anim.action
    slot = getattr(anim, "action_slot", None)
    if slot is not None:
        for layer in getattr(action, "layers", []):
            for strip in getattr(layer, "strips", []):
                for bag in getattr(strip, "channelbags", []):
                    if bag.slot_handle == slot.handle:
                        return list(bag.fcurves)
        return []
    return list(getattr(action, "fcurves", []))


def to_blender(quat) -> list[float]:
    """glTF 的 (x, y, z, w) → Blender 的 (w, x, y, z)。

    导入器对每个节点做了 Y-up → Z-up 的共轭：x 不变，y' = -z，z' = y。
    """
    x, y, z, w = quat
    return [w, x, -z, y]


def blend_to_rest(obj, rest_blender: list[float], window) -> int:
    curves = {fc.array_index: fc for fc in curves_of(obj) if fc.data_path == "rotation_quaternion"}
    if len(curves) != 4:
        print(f"  ! {obj.name}: 期望 4 条四元数曲线，实际 {len(curves)} 条")
        return 0

    counts = {len(fc.keyframe_points) for fc in curves.values()}
    if len(counts) != 1:
        print(f"  ! {obj.name}: 四元数分量关键帧数不一致 {counts}")
        return 0
    count = counts.pop()

    target = list(rest_blender)
    touched = 0
    for index in range(count):
        key_time = curves[0].keyframe_points[index].co.x / FPS
        weight = weight_at(key_time, window)
        if weight <= 0.0:
            continue
        q = [curves[i].keyframe_points[index].co.y for i in range(4)]
        # q 与 -q 是同一个旋转：先取同半球，免得插到另一条长路径上
        if sum(a * b for a, b in zip(q, target)) < 0.0:
            target = [-v for v in target]
        mixed = [q[i] * (1.0 - weight) + target[i] * weight for i in range(4)]
        norm = math.sqrt(sum(v * v for v in mixed))
        if norm <= 1e-8:
            continue
        for i in range(4):
            curves[i].keyframe_points[index].co.y = mixed[i] / norm
        touched += 1

    for fc in curves.values():
        fc.update()
    return touched


def build(src: str, out: str, side: str, rest: dict[str, list[float]], prefix: str) -> None:
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    # 帧率必须在导入之前定：glTF 的秒 → 帧换算按场景帧率走，导出按同一比值还原
    scene.render.fps = FPS
    scene.render.fps_base = 1.0

    bpy.ops.import_scene.gltf(filepath=src)

    objects = [obj for obj in scene.objects if obj.type == "EMPTY"]
    if not objects:
        raise RuntimeError(f"{src} 导入后没有节点")
    action = next((obj.animation_data.action for obj in objects
                   if obj.animation_data and obj.animation_data.action), None)
    if action is None:
        raise RuntimeError(f"{src} 里没有动画")
    action.name = os.path.splitext(os.path.basename(out))[0]

    print(f"[{side}] 节点 {len(objects)} 个，曲线 {len(iter_fcurves(action))} 条，"
          f"帧范围 {tuple(round(v) for v in action.frame_range)}")

    window = WINDOWS[side]
    for suffix in CHAIN:
        name = f"{prefix}{side}{suffix}"
        obj = bpy.data.objects.get(name)
        if obj is None:
            raise RuntimeError(f"找不到节点 {name}")
        touched = blend_to_rest(obj, to_blender(rest[name]), window)
        print(f"  {name}: {touched} 个关键帧回正")

    bpy.ops.export_scene.gltf(
        filepath=out,
        export_format="GLB",
        use_selection=False,
        export_animations=True,
        export_animation_mode="ACTIONS",
        export_skins=False,
        export_morph=False,
        export_apply=False,
        export_yup=True,
        export_extras=False,
        export_cameras=False,
        export_lights=False,
    )
    print(f"[{side}] → {out}")


def main() -> int:
    args = parse_args()
    if not os.path.isfile(args.src):
        print(f"源文件不存在：{args.src}")
        return 1
    os.makedirs(args.out_dir, exist_ok=True)

    doc = read_glb_json(args.src)
    rest = {
        node.get("name"): list(node.get("rotation", [0.0, 0.0, 0.0, 1.0]))
        for node in doc["nodes"]
        if node.get("name")
    }

    # 不同 rig 的前缀不一样（`mixamorig:` / `mixamorig1:` …），按左脚髋反推
    prefix = next(
        (name[: -len("LeftUpLeg")] for name in rest if name.endswith("LeftUpLeg")),
        "mixamorig:",
    )

    for side in ("Left", "Right"):
        out = os.path.join(args.out_dir, f"{args.base}{side}.glb")
        build(args.src, out, side, rest, prefix)
    return 0


if __name__ == "__main__":
    sys.exit(main())
