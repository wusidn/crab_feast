#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""探针：跑跳落地淡出段的**合成姿态**逐帧脚底位移（1× 实时）。

`probe_land_handover.py` 只看交回那一帧的静态错位；这个脚本把
「动作剪辑按 1.62× 播完尾巴 + 移动剪辑按相位推进 + 两者按权重线性混合」
整段算出来，按真实帧率（默认 74 fps）逐帧量**着地脚在世界里走了多远**，
用来判断"落地后快速换脚 / 残影"到底出在哪几帧。

混合模型与引擎一致：`Pose::sample(动作剪辑, clip_time)` 后
`blend_in_place(&action_pose, weight)`，即骨骼局部 TRS 按权重线性插值，
权重 `weight = fade_in · (1 − k)`，`k = t / RECOVER`，`clip_time = LAND + k·(END − LAND)`。

着地脚的世界前后位移 = 身体前进 `V·t` + 踝相对髋的 z（两条剪辑都把根运动
烘进了动画里，只有相对量可比）。

    python3 scripts/probe_land_blend.py [角色目录] [--fps N] [--clip RunningJump1]
"""

from __future__ import annotations

import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe_jump_clips import (  # noqa: E402
    ROOT, channel, mat_mul, mat_mul_point, sample,
)

# 跑跳时间轴（`SPEC_RUN`）
LAND = 0.657
END = 0.900
RECOVER = 0.24
# 跑速（`Running` manifest `gait.speed`）
SPEED = 3.02
# 移动剪辑右脚的触地 / 离地相位（manifest `Running.gait`）
RUN_CONTACT_RIGHT = 0.927
RUN_LIFT_RIGHT = 0.135
# 交接目标相位（`landing_phase` 算出来的 0.066）与推进量（RECOVER / 0.700）
TARGET = 0.066
ADVANCE = RECOVER / 0.700
# 动作剪辑（`RunningJump1`）右脚的触地 / 离地相位
ACT_CONTACT_RIGHT = 0.875
ACT_LIFT_RIGHT = 0.062

BONES = ("Hips", "LeftFoot", "RightFoot")


def mat_from_trs(translation, rotation, scale):
    """局部矩阵（与 `probe_jump_clips.local_mat` 同约定）。"""
    from probe_jump_clips import local_mat  # noqa: E402

    return local_mat(translation, rotation, scale)


def nlerp(a, b, t):
    # 四元数双重覆盖：同一旋转的 `q` 与 `−q` 必须先对齐符号，否则线性插值会
    # 穿过「折断」的中间姿态（大腿从身前后摆到体后时脚会被折到胯高）。
    dot = sum(a[i] * b[i] for i in range(4))
    if dot < 0.0:
        b = tuple(-v for v in b)
    out = [a[i] * (1.0 - t) + b[i] * t for i in range(4)]
    norm = math.sqrt(sum(v * v for v in out)) or 1.0
    return tuple(v / norm for v in out)


class Rig:
    """一条剪辑按名字索引的局部 TRS 采样 + 可混合的 FK。"""

    def __init__(self, path: str):
        from probe_jump_clips import read_glb  # noqa: E402

        self.doc, self.blob = read_glb(path)
        self.anim = self.doc["animations"][0]
        self.nodes = self.doc["nodes"]
        self.duration = 0.0
        for sampler in self.anim["samplers"]:
            acc = self.doc["accessors"][sampler["input"]]
            if "max" in acc:
                self.duration = max(self.duration, acc["max"][0])
        self.by_name = {}
        for index, node in enumerate(self.nodes):
            name = node.get("name") or ""
            if name and name not in self.by_name:
                self.by_name[name] = index
        self.parent = {}
        for index, node in enumerate(self.nodes):
            for child in node.get("children", []):
                self.parent[child] = index
        self.series = {}
        for index in range(len(self.nodes)):
            for path_name in ("translation", "rotation", "scale"):
                self.series[(index, path_name)] = channel(
                    self.anim, self.doc, self.blob, index, path_name
                )
        self.feet = {}
        for suffix in BONES:
            index = self.find(suffix)
            self.feet[suffix] = index

    def find(self, suffix: str):
        for index, node in enumerate(self.nodes):
            name = node.get("name") or ""
            if name.startswith("Ctrl") or "IK" in name or "FK" in name:
                continue
            if name.endswith(suffix):
                return index
        return None

    def local(self, index: int, time: float):
        tr = sample(self.series[(index, "translation")], time, False) or tuple(
            self.nodes[index].get("translation", (0, 0, 0))
        )
        ro = sample(self.series[(index, "rotation")], time, True) or tuple(
            self.nodes[index].get("rotation", (0, 0, 0, 1))
        )
        sc = sample(self.series[(index, "scale")], time, False) or tuple(
            self.nodes[index].get("scale", (1, 1, 1))
        )
        return tr, ro, sc

    def chain(self, index: int):
        out, cur = [], index
        while cur is not None:
            out.append(cur)
            cur = self.parent.get(cur)
        out.reverse()
        return out


def blend_points(base: Rig, other: Rig, base_time: float, other_time: float, weight: float):
    """两套剪辑按 `weight` 混合后，Hips / 双脚的世界坐标（other 权重 = weight）。"""
    out = {}
    for suffix in BONES:
        node = base.feet[suffix]
        if node is None:
            continue
        matrix = [[1.0 if i == j else 0.0 for j in range(4)] for i in range(4)]
        for item in base.chain(node):
            name = base.nodes[item].get("name") or ""
            btr, bro, bsc = base.local(item, base_time)
            oidx = other.by_name.get(name)
            if oidx is None:
                otr, oro, osc = btr, bro, bsc
            else:
                otr, oro, osc = other.local(oidx, other_time)
            tr = tuple(btr[i] * (1.0 - weight) + otr[i] * weight for i in range(3))
            ro = nlerp(bro, oro, weight)
            sc = tuple(bsc[i] * (1.0 - weight) + osc[i] * weight for i in range(3))
            matrix = mat_mul(matrix, mat_from_trs(tr, ro, sc))
        out[suffix] = mat_mul_point(matrix, (0.0, 0.0, 0.0))
    return out


def phase_at(target: float, advance: float, progress: float):
    return (target - (1.0 - min(max(progress, 0.0), 1.0)) * advance) % 1.0


def run(directory: str, fps: float, clip_name: str, recover: float, target: float, advance: float):
    loco = Rig(os.path.join(directory, "Running.glb"))
    action = Rig(os.path.join(directory, clip_name + ".glb"))
    hold_phase = RUN_CONTACT_RIGHT
    print(
        f"\n=== {os.path.basename(directory)} {clip_name} ===\n"
        f"帧率 {fps:.0f} fps，恢复段 {recover:.3f}s → {int(round(recover * fps)) + 1} 帧；"
        f"跑速 {SPEED:.2f} m/s；交接目标 {target:.3f}，推进 {advance:.3f} 周期"
    )
    frames = int(round(recover * fps)) + 1
    rows = []
    for i in range(frames):
        t = i / fps
        k = min(t / recover, 1.0)
        weight = 1.0 - k
        action_time = LAND + k * (END - LAND)
        old_phase = hold_phase
        new_phase = phase_at(target, advance, k)

        def measure(phase):
            pts = blend_points(loco, action, phase * loco.duration, action_time, weight)
            hips = pts["Hips"]
            return {
                side: (pts[side][2] - hips[2], pts[side][1] - hips[1])
                for side in ("LeftFoot", "RightFoot")
            }

        old = measure(old_phase)
        new = measure(new_phase)
        rows.append((t, k, weight, old, new))

    # 世界前后位移：V·t + 踝相对髋
    def world_z(rel, t):
        return SPEED * t + rel[0]

    for tag, pick in (("现状（相位冻结 0.927）", 3), ("改后（每帧推到 0.066）", 4)):
        print(f"\n[{tag}]")
        print("  帧    t     权重   右脚 z(相对髋) 世界z    Δ世界   |  左脚 z   世界z    Δ世界")
        prev = None
        for row in rows:
            t, k, weight = row[0], row[1], row[2]
            rel = row[pick]
            r = rel["RightFoot"]
            l = rel["LeftFoot"]
            rz, lz = world_z(r, t), world_z(l, t)
            dr = "" if prev is None else f"{rz - prev[0]:+.3f}"
            dl = "" if prev is None else f"{lz - prev[1]:+.3f}"
            print(
                f"  {k * recover * fps:4.0f}  {t:5.3f}  {weight:5.2f}   "
                f"{r[0]:+.3f}/{r[1]:+.3f}   {rz:+.3f}  {dr:>6s}  |  "
                f"{l[0]:+.3f}/{l[1]:+.3f}   {lz:+.3f}  {dl:>6s}"
            )
            prev = (rz, lz)
        steps = []
        prev = None
        for row in rows:
            t, rel = row[0], row[pick]
            rz = world_z(rel["RightFoot"], t)
            if prev is not None:
                steps.append(rz - prev)
            prev = rz
        if steps:
            print(
                f"  着地脚逐帧位移：最小 {min(steps):+.3f} m，最大 {max(steps):+.3f} m，"
                f"最大 |Δ| {max(abs(s) for s in steps):.3f} m/帧"
                f"（{fps:.0f} fps → {max(abs(s) for s in steps) * fps:.2f} m/s）"
            )
            flips = sum(1 for a, b in zip(steps, steps[1:]) if a * b < 0)
            print(f"  位移方向反转次数（正负切换）：{flips}")


def main() -> int:
    args = [a for a in sys.argv[1:]]
    fps = 74.0
    clip_name = "RunningJump1"
    recover = RECOVER
    target = TARGET
    advance = None
    directories = []
    skip = False
    for index, arg in enumerate(args):
        if skip:
            skip = False
            continue
        if arg == "--fps":
            fps = float(args[index + 1])
            skip = True
        elif arg == "--clip":
            clip_name = args[index + 1]
            skip = True
        elif arg == "--recover":
            recover = float(args[index + 1])
            skip = True
        elif arg == "--target":
            target = float(args[index + 1])
            skip = True
        elif arg == "--advance":
            advance = float(args[index + 1])
            skip = True
        else:
            directories.append(arg)
    if advance is None:
        advance = recover / 0.700
    for directory in directories or [os.path.join(ROOT, "assets/characters/animations/Amy")]:
        if not os.path.isdir(directory):
            print(f"跳过（不是目录）：{directory}")
            continue
        run(directory, fps, clip_name, recover, target, advance)
    return 0


if __name__ == "__main__":
    sys.exit(main())
