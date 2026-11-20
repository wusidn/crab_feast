#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""从 `Jump.glb` 派生出两条单脚跳剪辑：`JumpLeft.glb`（左脚跳）、`JumpRight.glb`（右脚跳）。

为什么要派生而不是重做
------------------
`Jump.glb` 是原地双脚跳，髋曲线（垂直位移的来源）已经在 `crab_feast/src/anim/jump.rs`
里标定过（0.40 蹲 / 0.70 离地 / 1.03 顶点 / 1.40 落地）。单脚跳只要**保持髋曲线和时长不变**、
把两条腿的角色分开，跳跃层的时间轴、垂直驱动、落地相位就全部不用改。

做法
----
对支撑腿的 FK 链（`UpLeg / Leg / Foot / ToeBase`）做「向绑定姿态回正」的带权混合：
权重 1 = 这条腿回到绑定姿态（伸直），0 = 完全用剪辑原本的姿态。

实测数据说明了权重窗口为什么这么取（Amy，单位米）：
  * 0.70 离地后，剪辑把**左腿**收到 0.65（1.03s 顶点），右腿只收到 0.48；
  * 1.10s 起两条腿交换：右腿收起、左腿伸下去，1.30s 左踝 0.112（≈着地）、1.40s 右踝 0.107。
所以「左脚跳」只要把左腿在 0.78~1.20 放直（不跟着收膝），右腿原样保留，落地正好左脚先着。
「右脚跳」要把右腿一直放直到 1.28（否则先着地的是收着膝的左腿），再淡出，让剪辑自己的落地
屈膝接上——权重 1 的直腿在 1.32 之后会插进地面，淡出正好借剪辑的屈膝把脚托住。

注意
----
这份 GLB 是纯动画包（没有蒙皮），Blender 导入后得到的是一堆 **empty 节点**而不是骨架，
所以改的是对象级的 `rotation_quaternion` 曲线，不是 pose bone。
导入器会把每个节点做 Y-up → Z-up 的旋转共轭，绑定姿态也要按同一规则换算过去；
共轭是线性的，所以在 Blender 空间里混到「换算后的绑定姿态」等于在 glTF 空间里混到原始绑定姿态。

用法（必须 Blender 无头模式）：

    /Applications/Blender.app/Contents/MacOS/Blender -b \
        --python scripts/make_hop_clips.py -- \
        --src assets/characters/animations/Amy/Jump.glb \
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

# 支撑腿的回正窗口：(秒, 权重)，中间线性插值
WINDOWS = {
    "Left": [(0.78, 0.0), (0.95, 1.0), (1.05, 1.0), (1.20, 0.0)],
    "Right": [(0.78, 0.0), (0.95, 1.0), (1.28, 1.0), (1.40, 0.0)],
}

# 支撑腿的 FK 链（对象名带 `mixamorig:` 前缀）
CHAIN = ("UpLeg", "Leg", "Foot", "ToeBase")


def parse_args() -> argparse.Namespace:
    argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
    p = argparse.ArgumentParser(description="从 Jump.glb 派生单脚跳剪辑")
    p.add_argument("--src", required=True, help="源 Jump.glb")
    p.add_argument("--out-dir", required=True, help="输出目录（写 JumpLeft.glb / JumpRight.glb）")
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

    一个动作装着 168 个 slot（每个节点一个），数据路径全都叫 `rotation_quaternion`，
    所以不能按 data_path 过滤整张动作——必须按该对象的 action slot 找到它自己的 channelbag，
    否则改的是别人的曲线（改出来的是别的肢体的姿态）。
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


def build(src: str, out: str, side: str, rest: dict[str, list[float]]) -> None:
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
        name = f"mixamorig:{side}{suffix}"
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

    for side in ("Left", "Right"):
        build(args.src, os.path.join(args.out_dir, f"Jump{side}.glb"), side, rest)
    return 0


if __name__ == "__main__":
    sys.exit(main())
