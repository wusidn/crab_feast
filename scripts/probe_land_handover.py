#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""探针：跑跳落地交回移动时，两套姿态的着地脚对不对得上。

纯 Python GLB 解析（复用 `probe_jump_clips.py` 的读取与采样），不依赖 Blender。

量的是**踝相对髋的前后位移**（减去髋的 z，因为两条剪辑都把根运动烘进了
动画里，绝对坐标不能直接比）。输出三组数：

- `Running` 相位 → 双脚相对髋的前后 / 高度；
- `RunningJump*` 落地段（land → end）同一组数；
- 交回姿态对比：动作末帧 vs 移动剪辑在给定相位，着地脚差多少米。

用途：调 `follow_landing_phase`（`crab_feast/src/anim/jump.rs`）的相位时，
先看交回错位有多大，再看淡出期间着地脚在世界里被拖动多少。

    python3 scripts/probe_land_handover.py [角色目录 ...]

默认扫 assets/characters/animations/Amy。
落地锚点、恢复段时长、跑速来自 `JumpSpec`（跑跳 land 0.657 / end 0.900 /
recover 0.15s）与 manifest 的 `speed`；换角色要跟着改常数。
"""

from __future__ import annotations

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe_jump_clips import (  # noqa: E402
    ROOT, accessor, channel, local_mat, mat_mul, mat_mul_point, quat_mat, read_glb, sample,
)

# 跑跳时间轴（`SPEC_RUN`）
LAND = 0.657
END = 0.900
RECOVER = 0.24
# 移动剪辑的触地 / 离地事件（manifest 里的 `gait`，跨相位 0/1）
RUN_CONTACT_RIGHT = 0.927
RUN_LIFT_RIGHT = 0.135
RUN_CONTACT_LEFT = 0.406
RUN_LIFT_LEFT = 0.625


class Clip:
    """一条 glb 剪辑的世界坐标采样（节点级 FK，不做蒙皮）。"""

    def __init__(self, path: str):
        self.doc, self.blob = read_glb(path)
        self.anim = self.doc["animations"][0]
        self.nodes = self.doc["nodes"]
        self.parent = {}
        for index, node in enumerate(self.nodes):
            for child in node.get("children", []):
                self.parent[child] = index
        self.duration = 0.0
        for sampler in self.anim["samplers"]:
            acc = self.doc["accessors"][sampler["input"]]
            if "max" in acc:
                self.duration = max(self.duration, acc["max"][0])
        self.series = {}

    def node(self, suffix: str):
        for index, node in enumerate(self.nodes):
            name = node.get("name") or ""
            if name.startswith("Ctrl") or "IK" in name or "FK" in name:
                continue
            if name.endswith(suffix):
                return index
        return None

    def chain(self, node: int):
        out, cur = [], node
        while cur is not None:
            out.append(cur)
            cur = self.parent.get(cur)
        out.reverse()
        return out

    def ensure(self, suffix: str):
        node = self.node(suffix)
        if node is None:
            return None
        for item in self.chain(node):
            for path in ("translation", "rotation", "scale"):
                key = (item, path)
                if key not in self.series:
                    self.series[key] = channel(self.anim, self.doc, self.blob, item, path)
        return node

    def world(self, node: int, time: float):
        matrix = [[1.0 if i == j else 0.0 for j in range(4)] for i in range(4)]
        for item in self.chain(node):
            tr = sample(self.series[(item, "translation")], time, False) or tuple(
                self.nodes[item].get("translation", (0, 0, 0)))
            ro = sample(self.series[(item, "rotation")], time, True) or tuple(
                self.nodes[item].get("rotation", (0, 0, 0, 1)))
            sc = sample(self.series[(item, "scale")], time, False) or tuple(
                self.nodes[item].get("scale", (1, 1, 1)))
            matrix = mat_mul(matrix, local_mat(tr, ro, sc))
        return mat_mul_point(matrix, (0.0, 0.0, 0.0))

    def points(self, time: float):
        out = {}
        for suffix in ("Hips", "LeftFoot", "RightFoot"):
            node = self.ensure(suffix)
            if node is not None:
                out[suffix] = self.world(node, time)
        return out

    def hip_relative(self, time: float):
        """双脚相对髋的前后（z，+ 为体前）与高度（y）。"""
        pts = self.points(time)
        hips = pts["Hips"]
        return {
            side: (pts[side][2] - hips[2], pts[side][1] - hips[1])
            for side in ("LeftFoot", "RightFoot")
        }


def print_table(tag: str, rel: dict):
    cells = " | ".join(
        f"{side[0]}{side[1]} z{z:+.3f} y{y:+.3f}" for side in ("LeftFoot", "RightFoot") for z, y in [rel[side]]
    )
    print(f"  {tag}  {cells}")


def report(directory: str):
    run = Clip(os.path.join(directory, "Running.glb"))
    jump = Clip(os.path.join(directory, "RunningJumpRight.glb"))
    print(f"\n=== {os.path.basename(directory)}  Running {run.duration:.3f}s  "
          f"RunningJumpRight {jump.duration:.3f}s ===")

    print("\n[移动剪辑]")
    for phase in (RUN_CONTACT_RIGHT, 0.95, 0.99, 0.03, 0.066, 0.10, RUN_LIFT_RIGHT, RUN_CONTACT_LEFT):
        print_table(f"相位 {phase:5.3f}", run.hip_relative((phase % 1.0) * run.duration))

    print("\n[动作剪辑 落地段]")
    for time in (LAND, 0.72, 0.75, 0.7875, 0.85, END):
        print_table(f"t {time:6.4f}", jump.hip_relative(time))

    print("\n[交回姿态：动作末帧 vs 移动相位，差多少米]")
    end = jump.hip_relative(END)
    for phase in (RUN_CONTACT_RIGHT, 0.066):
        rel = run.hip_relative((phase % 1.0) * run.duration)
        for side in ("LeftFoot", "RightFoot"):
            dz = abs(rel[side][0] - end[side][0])
            dy = abs(rel[side][1] - end[side][1])
            print(f"  相位 {phase:5.3f} {side:10s} 差 z {dz:.3f} m / y {dy:.3f} m")


def main() -> int:
    directories = sys.argv[1:] or [os.path.join(ROOT, "assets/characters/animations/Amy")]
    for directory in directories:
        if not os.path.isdir(directory):
            print(f"跳过（不是目录）：{directory}")
            continue
        report(directory)
    return 0


if __name__ == "__main__":
    sys.exit(main())
