#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""从跳跃动画资源（glb）里算资源参数表要用的数字，并和 Rust 表对账。

纯 Python 解析 glTF/GLB，不依赖 Blender。测的是**资源本身**能测的东西：

- 剪辑时长 / 结束帧
- 髋高基线法的事件锚点：离地帧、落地帧、顶点帧
- 蓄力段可见下蹲深度（判断「有没有深蹲可看」）
- 落地恢复尾段长度（end − land）
- 起跳支撑脚、末帧着地脚（左右脚相对自身最低点的离地量）

手感参数（launch_vy / air_predict / slow_min …）来自物理标定，不在动画里，
脚本只测 `JumpClipSpec` 那几项，不猜。

用法：

    python3 scripts/jump_params.py                     # 测参考角色 + 全角色一致性 + 与 Rust 表对账
    python3 scripts/jump_params.py --ref Amy           # 指定参考角色
    python3 scripts/jump_params.py --json jump.json    # 额外导出实测结果
    python3 scripts/jump_params.py --check             # CI 模式：表与资源不一致就非零退出

脚本只读资源与源码，不改任何文件。
"""

from __future__ import annotations

import argparse
import glob
import json
import math
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe_jump_clips import (  # noqa: E402  (复用同一份 GLB 解析)
    accessor,
    channel,
    local_mat,
    mat_mul,
    mat_mul_point,
    read_glb,
    sample,
)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSET_ROOT = os.path.join(ROOT, "assets")
TABLE_PATH = os.path.join(ROOT, "crab_feast/src/anim/jump/resources.rs")

# 与 `jump.rs::EVENT_BASELINE` 同口径：髋高抬离首帧基线超过这个值判为离地。
EVENT_BASELINE = 0.03
# 两脚离地量差不到这个值就当作「双脚一起」。
FOOT_EPS = 0.05
# 默认采样频率：锚点表是按剪辑本地秒给的，采样要够密才能对到毫秒级。
DEFAULT_HZ = 240
# 表校验容差（秒）：表里的锚点最初来自 64 帧探针（步长 = 时长/63，0.03~0.05s），
# 240Hz 实测与它最多差一格。要抓的是「资源换了、表没跟着改」的漂移，不是半帧误差。
DEFAULT_TOL = 0.05


# ---------------------------------------------------------------------------
# 资源解析：髋高 + 双脚世界高度
# ---------------------------------------------------------------------------


def _find_node(nodes, suffix):
    for i, n in enumerate(nodes):
        name = n.get("name") or ""
        if name.startswith("Ctrl") or "IK" in name or "FK" in name:
            continue
        if name.endswith(suffix):
            return i
    return None


def _chain(nodes, parent, node):
    out, cur = [], node
    while cur is not None:
        out.append(cur)
        cur = parent.get(cur)
    out.reverse()
    return out


def _world_y(nodes, series, chain, t):
    m = [[1.0 if i == j else 0.0 for j in range(4)] for i in range(4)]
    for node in chain:
        tr = sample(series[(node, "translation")], t, False) or tuple(
            nodes[node].get("translation", (0.0, 0.0, 0.0))
        )
        ro = sample(series[(node, "rotation")], t, True) or tuple(
            nodes[node].get("rotation", (0.0, 0.0, 0.0, 1.0))
        )
        sc = sample(series[(node, "scale")], t, False) or tuple(
            nodes[node].get("scale", (1.0, 1.0, 1.0))
        )
        m = mat_mul(m, local_mat(tr, ro, sc))
    return mat_mul_point(m, (0.0, 0.0, 0.0))[1]


def measure(path, hz=DEFAULT_HZ):
    """测一条跳跃剪辑；返回秒 / 米口径的参数字典。"""
    doc, blob = read_glb(path)
    an = doc["animations"][0]
    nodes = doc["nodes"]

    parent = {}
    for i, n in enumerate(nodes):
        for c in n.get("children", []):
            parent[c] = i

    duration = 0.0
    for s in an["samplers"]:
        acc = doc["accessors"][s["input"]]
        if "max" in acc:
            duration = max(duration, acc["max"][0])
    if duration <= 0.0:
        raise ValueError(f"{path}: 剪辑时长为 0")

    hips = _find_node(nodes, "Hips")
    feet = {"L": _find_node(nodes, "LeftFoot"), "R": _find_node(nodes, "RightFoot")}
    if hips is None:
        raise ValueError(f"{path}: 找不到 Hips 节点")

    chains = {"hips": _chain(nodes, parent, hips)}
    for side, foot in feet.items():
        if foot is not None:
            chains[side] = _chain(nodes, parent, foot)
    series = {}
    for chain in chains.values():
        for node in chain:
            for p in ("translation", "rotation", "scale"):
                series[(node, p)] = channel(an, doc, blob, node, p)

    steps = max(2, int(math.ceil(duration * hz)) + 1)
    times = [duration * k / (steps - 1) for k in range(steps)]
    # 髋高走**局部平移**：运行时 `analyze_clip` 采的就是 hips 的 `Transform::translation.y`，
    # 表里的锚点必须和它同口径（世界坐标会把根位移也算进来，锚点会偏）。
    hip_series = series[(hips, "translation")]
    hip_y = [sample(hip_series, t, False)[1] for t in times]
    foot_y = {
        side: [_world_y(nodes, series, chains[side], t) for t in times]
        for side in ("L", "R")
        if side in chains
    }

    # 髋高基线法（与运行时 `detect_events` 同口径）
    base = hip_y[0]
    apex_i = max(range(len(hip_y)), key=lambda i: hip_y[i])
    above = [i for i, y in enumerate(hip_y) if y > base + EVENT_BASELINE]
    takeoff_i = above[0] if above else 0
    land_i = next(
        (i for i in range(apex_i + 1, len(hip_y)) if hip_y[i] <= base + EVENT_BASELINE),
        len(hip_y) - 1,
    )
    takeoff, land = times[takeoff_i], times[land_i]
    apex = times[apex_i]

    crouch = min(hip_y[: takeoff_i + 1]) - base

    def clearance(side):
        ys = foot_y[side]
        lo = min(ys)
        return [y - lo for y in ys]

    def mean_in_window(series_y, t0, t1):
        picked = [y for t, y in zip(times, series_y) if t0 <= t <= t1]
        return sum(picked) / len(picked) if picked else 0.0

    def foot_label(values):
        return "both" if abs(values["L"] - values["R"]) <= FOOT_EPS else (
            "right" if values["R"] < values["L"] else "left"
        )

    clr = {side: clearance(side) for side in foot_y}
    # 起跳脚：蹬离前 0.10s 里哪只脚还贴着地
    takeoff_feet = {
        side: mean_in_window(ys, max(0.0, takeoff - 0.10), takeoff + 0.02)
        for side, ys in clr.items()
    }
    # 末帧脚：剪辑收尾时哪只脚还在支撑（落地对齐用）
    landing_feet = {
        side: mean_in_window(ys, max(0.0, duration - 0.03), duration)
        for side, ys in clr.items()
    }

    return {
        "clip": os.path.basename(path)[:-4],
        "duration": duration,
        "entry": 0.0,
        "end": duration,
        "takeoff": takeoff,
        "land": land,
        "apex": apex,
        "take_secs": takeoff,
        "tail_secs": max(0.0, duration - land),
        "crouch_low": crouch,
        "takeoff_foot": foot_label(takeoff_feet) if len(takeoff_feet) == 2 else "both",
        "landing_foot": foot_label(landing_feet) if len(landing_feet) == 2 else "both",
    }


# ---------------------------------------------------------------------------
# Rust 资源表解析
# ---------------------------------------------------------------------------


def load_table(path=TABLE_PATH):
    """从 `resources.rs` 抽出每行的 resource 参数（只取 JumpClipSpec 相关字段）。"""
    with open(path, encoding="utf-8") as fh:
        text = fh.read()
    starts = [m.end() for m in re.finditer(r"JumpPoseDef\s*\{", text)]
    if not starts:
        raise ValueError(f"{path}: 没找到 JumpPoseDef 表行")
    # 表数组在 `];` 收尾；最后一行不能把后面单测里的 `end: 1.20` 之类当成表字段
    array_end = text.find("\n];", starts[0])
    if array_end < 0:
        array_end = len(text)
    rows = {}
    for i, start in enumerate(starts):
        end = starts[i + 1] if i + 1 < len(starts) else len(text)
        end = min(end, array_end)
        block = text[start:end]

        def num(name):
            m = re.search(rf"\b{name}:\s*([0-9.]+)", block)
            return float(m.group(1)) if m else None

        def word(name, pattern):
            m = re.search(rf"\b{name}:\s*{pattern}", block)
            return m.group(1) if m else None

        clip = word("clip", r'"([^"]+)"')
        if not clip:
            continue
        rows[clip] = {
            "pose": word("pose", r"JumpPose::(\w+)"),
            "clip": clip,
            "entry": num("entry"),
            "takeoff": num("takeoff"),
            "land": num("land"),
            "end": num("end"),
            "support": word("support", r"SupportFoot::(\w+)"),
            "landing_foot": word("landing_foot", r"SupportFoot::(\w+)"),
            "anticipation": word("anticipation", r"Anticipation::(\w+)"),
            "recover_secs": num("recover_secs"),
        }
    return rows


# ---------------------------------------------------------------------------
# 输出
# ---------------------------------------------------------------------------


def fmt(value, digits=3):
    return "—" if value is None else f"{value:.{digits}f}"


def print_measurements(rows, ref):
    print(f"\n== 实测参数（参考角色 {ref}）==")
    header = (
        f"{'剪辑':<18}{'时长':>7}{'离地':>8}{'落地':>8}{'顶点':>8}"
        f"{'尾段':>8}{'蓄力':>8}{'下蹲':>8}  {'起跳脚':<9}{'末帧脚':<9}"
    )
    print(header)
    print("-" * len(header))
    for name, m in rows.items():
        print(
            f"{name:<18}{m['duration']:>7.3f}{m['takeoff']:>8.3f}{m['land']:>8.3f}"
            f"{m['apex']:>8.3f}{m['tail_secs']:>8.3f}{m['take_secs']:>8.3f}"
            f"{m['crouch_low']:>8.3f}  {m['takeoff_foot']:<9}{m['landing_foot']:<9}"
        )


def print_json(rows, ref):
    payload = {"reference": ref, "clips": rows}
    print(json.dumps(payload, ensure_ascii=False, indent=2))


def main():
    parser = argparse.ArgumentParser(description="从跳跃动画资源计算资源参数表")
    parser.add_argument("--ref", help="参考角色目录名（默认 Amy，没有就取第一个）")
    parser.add_argument("--hz", type=float, default=DEFAULT_HZ, help="采样频率（默认 240）")
    parser.add_argument(
        "--tol",
        type=float,
        default=DEFAULT_TOL,
        help="表校验时间容差（秒，默认 0.05：表锚点是 64 帧网格标定的）",
    )
    parser.add_argument("--json", metavar="PATH", help="把实测结果写成 JSON")
    parser.add_argument("--check", action="store_true", help="表/资源不一致时退出非零（CI 用）")
    parser.add_argument(
        "--strict",
        action="store_true",
        help="跨角色锚点偏差 / 左右脚语义问题也算失败（默认只告警）",
    )
    args = parser.parse_args()

    anim_root = os.path.join(ASSET_ROOT, "characters/animations")
    chars = sorted(
        d for d in glob.glob(os.path.join(anim_root, "*")) if os.path.isdir(d)
    )
    if not chars:
        print(f"没有找到角色动画目录：{anim_root}", file=sys.stderr)
        return 2

    ref = args.ref or ("Amy" if any(os.path.basename(c) == "Amy" for c in chars) else None)
    ref_dir = (
        os.path.join(anim_root, ref) if ref else chars[0]
    )
    if not os.path.isdir(ref_dir):
        print(f"参考角色不存在：{ref_dir}", file=sys.stderr)
        return 2
    ref = os.path.basename(ref_dir)

    table = load_table()
    clips = sorted(table)
    missing_assets = []
    for clip in clips:
        for d in chars:
            if not os.path.isfile(os.path.join(d, f"{clip}.glb")):
                missing_assets.append(f"{os.path.basename(d)}/{clip}.glb")
    if missing_assets:
        print("资源参数表登记了但资源缺失：", file=sys.stderr)
        for item in missing_assets:
            print(f"  - {item}", file=sys.stderr)

    measured = {}
    for clip in clips:
        path = os.path.join(ref_dir, f"{clip}.glb")
        if os.path.isfile(path):
            measured[clip] = measure(path, args.hz)
    print_measurements(measured, ref)

    # 全角色一致性：Mixamo 逐帧重定向，锚点跨角色应当一致
    print(f"\n== 跨角色一致性（容差 {args.tol:.2f}s）==")
    inconsistent = []
    foot_issues = []
    for clip, ref_m in measured.items():
        row = table.get(clip, {})
        worst = (0.0, "")
        for d in chars:
            name = os.path.basename(d)
            path = os.path.join(d, f"{clip}.glb")
            if not os.path.isfile(path):
                continue
            other = measure(path, args.hz)
            for field in ("duration", "takeoff", "land", "apex"):
                delta = abs(other[field] - ref_m[field])
                if delta > worst[0]:
                    worst = (delta, f"{name}:{field}")
            # 派生片的左右脚是有语义的：表里登记了 Left/Right 就必须真的镜像
            if row.get("support") in ("Left", "Right"):
                foot_issues.extend(
                    (clip, "takeoff_foot", name, other["takeoff_foot"], row["support"].lower())
                    for _ in [0]
                    if other["takeoff_foot"] not in ("both", row["support"].lower())
                )
            if row.get("landing_foot") in ("Left", "Right"):
                foot_issues.extend(
                    (clip, "landing_foot", name, other["landing_foot"], row["landing_foot"].lower())
                    for _ in [0]
                    if other["landing_foot"] not in ("both", row["landing_foot"].lower())
                )
        if worst[0] > args.tol:
            inconsistent.append(f"{clip} 锚点跨角色偏差 {worst[0]:.3f}s（{worst[1]}）")
        else:
            print(f"  {clip:<18} 最大偏差 {worst[0]:.3f}s")

    # 同一种「脚错了」按 剪辑 + 字段 + 实测值 聚合，别一角色刷一行
    groups = {}
    for clip, field, name, value, want in foot_issues:
        groups.setdefault((clip, field, value, want), []).append(name)
    for (clip, field, value, want), names in sorted(groups.items()):
        label = "起跳支撑脚" if field == "takeoff_foot" else "末帧着地脚"
        sample = "、".join(names)
        inconsistent.append(
            f"{clip} 的{label}在 {len(names)} 个角色上测出 {value}（表要求 {want}）：{sample}"
        )

    # 与 Rust 表对账
    print("\n== 资源参数表对账 ==")
    errors = []
    for clip, m in measured.items():
        row = table[clip]
        max_delta = 0.0
        details = []
        for field in ("entry", "takeoff", "land", "end"):
            expected = row[field]
            if expected is None:
                errors.append(f"{clip}: 表里没有 {field}")
                continue
            delta = abs(m[field] - expected)
            max_delta = max(max_delta, delta)
            details.append(f"{field} {m[field]:.3f}/{expected:.3f}")
            if delta > args.tol:
                errors.append(
                    f"{clip}.{field}: 表 {expected:.3f}s / 实测 {m[field]:.3f}s"
                )
        landing = row["landing_foot"]
        if landing in ("Left", "Right") and m["landing_foot"] not in ("both", landing.lower()):
            errors.append(
                f"{clip}.landing_foot: 表 {landing} / 实测 {m['landing_foot']}"
            )
        if not any(e.startswith(f"{clip}.") for e in errors):
            print(f"  {clip:<18} ✓ 最大偏差 {max_delta:.3f}s  ({'，'.join(details)})")
    for err in errors:
        print(f"  ✗ {err}")

    if inconsistent:
        print("\n== 跨角色 / 资源语义不一致 ==")
        for item in inconsistent:
            print(f"  ! {item}")

    if args.json:
        def rounded(value):
            if isinstance(value, float):
                return round(value, 4)
            if isinstance(value, dict):
                return {k: rounded(v) for k, v in value.items()}
            return value

        with open(args.json, "w", encoding="utf-8") as fh:
            json.dump(
                {"reference": ref, "clips": rounded(measured)},
                fh,
                ensure_ascii=False,
                indent=2,
            )
        print(f"\n实测结果已写入 {args.json}")

    failed = bool(errors) or bool(missing_assets)
    if args.strict and inconsistent:
        failed = True
    if args.check and failed:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
