#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""探针：测量跳跃类剪辑的时长、髋高曲线与双脚高度，输出起跳/顶点/落地锚点。

纯 Python GLB 解析，不依赖 Blender。用法：

    python3 scripts/probe_jump_clips.py [角色目录 ...]

默认扫 assets/characters/animations/Amy。
"""

from __future__ import annotations

import json
import math
import os
import struct
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

TYPE_N = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT4": 16}
FMT = {5126: "f", 5125: "I", 5123: "H", 5122: "h", 5121: "B", 5120: "b"}


def read_glb(path: str):
    with open(path, "rb") as fh:
        data = fh.read()
    _, _, length = struct.unpack("<III", data[:12])
    off, gltf, blob = 12, None, b""
    while off < length:
        clen, ctype = struct.unpack("<II", data[off:off + 8])
        payload = data[off + 8:off + 8 + clen]
        off += 8 + clen
        if ctype == 0x4E4F534A:
            gltf = json.loads(payload.decode("utf-8"))
        elif ctype == 0x004E4942:
            blob = payload
    return gltf, blob


def accessor(doc, blob, index):
    acc = doc["accessors"][index]
    view = doc["bufferViews"][acc["bufferView"]]
    n = TYPE_N[acc["type"]]
    fmt = FMT[acc["componentType"]]
    size = struct.calcsize(fmt)
    start = view.get("byteOffset", 0) + acc.get("byteOffset", 0)
    stride = view.get("byteStride") or n * size
    out = []
    for k in range(acc["count"]):
        vals = struct.unpack_from("<" + fmt * n, blob, start + k * stride)
        out.append(vals if n > 1 else vals[0])
    return out


def quat_mat(q):
    x, y, z, w = q
    xx, yy, zz = x * x, y * y, z * z
    xy, xz, yz = x * y, x * z, y * z
    wx, wy, wz = w * x, w * y, w * z
    return [
        [1 - 2 * (yy + zz), 2 * (xy - wz), 2 * (xz + wy)],
        [2 * (xy + wz), 1 - 2 * (xx + zz), 2 * (yz - wx)],
        [2 * (xz - wy), 2 * (yz + wx), 1 - 2 * (xx + yy)],
    ]


def local_mat(tr, ro, sc):
    r = quat_mat(ro)
    return [
        [r[0][0] * sc[0], r[0][1] * sc[1], r[0][2] * sc[2], tr[0]],
        [r[1][0] * sc[0], r[1][1] * sc[1], r[1][2] * sc[2], tr[1]],
        [r[2][0] * sc[0], r[2][1] * sc[1], r[2][2] * sc[2], tr[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]


def mat_mul_point(m, p):
    return (
        m[0][0] * p[0] + m[0][1] * p[1] + m[0][2] * p[2] + m[0][3],
        m[1][0] * p[0] + m[1][1] * p[1] + m[1][2] * p[2] + m[1][3],
        m[2][0] * p[0] + m[2][1] * p[1] + m[2][2] * p[2] + m[2][3],
    )


def mat_mul(a, b):
    out = [[0.0] * 4 for _ in range(4)]
    for i in range(4):
        for j in range(4):
            out[i][j] = sum(a[i][k] * b[k][j] for k in range(4))
    return out


def channel(an, doc, blob, node, path):
    ch = next((c for c in an["channels"]
               if c["target"]["node"] == node and c["target"]["path"] == path), None)
    if ch is None:
        return None
    sampler = an["samplers"][ch["sampler"]]
    return accessor(doc, blob, sampler["input"]), accessor(doc, blob, sampler["output"])


def sample(series, t, quat):
    if series is None:
        return None
    times, values = series
    if t <= times[0]:
        return values[0]
    if t >= times[-1]:
        return values[-1]
    lo, hi = 0, len(times) - 1
    while lo < hi - 1:
        mid = (lo + hi) // 2
        if times[mid] <= t:
            lo = mid
        else:
            hi = mid
    span = times[lo + 1] - times[lo]
    a = 0.0 if span <= 0 else (t - times[lo]) / span
    v0, v1 = values[lo], values[lo + 1]
    if quat:
        if sum(x * y for x, y in zip(v0, v1)) < 0:
            v1 = tuple(-x for x in v1)
        mixed = tuple((1 - a) * x + a * y for x, y in zip(v0, v1))
        norm = math.sqrt(sum(x * x for x in mixed)) or 1.0
        return tuple(x / norm for x in mixed)
    return tuple((1 - a) * x + a * y for x, y in zip(v0, v1))


def probe(path: str, samples: int = 64) -> None:
    doc, blob = read_glb(path)
    an = doc["animations"][0]
    nodes = doc["nodes"]
    parent = {}
    for i, n in enumerate(nodes):
        for c in n.get("children", []):
            parent[c] = i

    dur = 0.0
    for s in an["samplers"]:
        acc = doc["accessors"][s["input"]]
        if "max" in acc:
            dur = max(dur, acc["max"][0])

    def find(suffix: str):
        for i, n in enumerate(nodes):
            name = n.get("name") or ""
            if name.startswith("Ctrl") or "IK" in name or "FK" in name:
                continue
            if name.endswith(suffix):
                return i
        return None

    hips = find("Hips")
    feet = {"L": find("LeftFoot"), "R": find("RightFoot")}

    def chain(node):
        out, cur = [], node
        while cur is not None:
            out.append(cur)
            cur = parent.get(cur)
        out.reverse()
        return out

    series = {}
    chains = {"hips": chain(hips)} if hips is not None else {}
    for side, foot in feet.items():
        if foot is not None:
            chains[side] = chain(foot)
    for c in chains.values():
        for node in c:
            for p in ("translation", "rotation", "scale"):
                series[(node, p)] = channel(an, doc, blob, node, p)

    hips_y, foot_y = [], {"L": [], "R": []}
    for k in range(samples):
        t = dur * k / (samples - 1)
        if hips is not None:
            tr = sample(series[(hips, "translation")], t, False)
            hips_y.append(tr[1] if tr else 0.0)
        for side, foot in feet.items():
            if foot is None:
                continue
            m = [[1.0 if i == j else 0.0 for j in range(4)] for i in range(4)]
            for node in chains[side]:
                tr = sample(series[(node, "translation")], t, False) or tuple(
                    nodes[node].get("translation", (0, 0, 0)))
                ro = sample(series[(node, "rotation")], t, True) or tuple(
                    nodes[node].get("rotation", (0, 0, 0, 1)))
                sc = sample(series[(node, "scale")], t, False) or tuple(
                    nodes[node].get("scale", (1, 1, 1)))
                m = mat_mul(m, local_mat(tr, ro, sc))
            foot_y[side].append(mat_mul_point(m, (0.0, 0.0, 0.0))[1])

    print(f"\n=== {os.path.basename(path)}  时长 {dur:.3f}s ===")
    if hips_y:
        base = hips_y[0]
        apex = max(range(len(hips_y)), key=lambda i: hips_y[i])
        print(f"髋高: 首帧 {base:.3f}  顶点 {hips_y[apex]:.3f} @ {dur * apex / (samples - 1):.3f}s"
              f"  末帧 {hips_y[-1]:.3f}  起伏 {max(hips_y) - min(hips_y):.3f}")
        # 起跳：髋高从蹲姿回升并越过首帧基线；落地：顶点后髋高回到基线附近
        above = [i for i, y in enumerate(hips_y) if y > base + 0.03]
        if above:
            takeoff = above[0]
            land = next((i for i in range(apex + 1, len(hips_y))
                         if hips_y[i] <= base + 0.03), len(hips_y) - 1)
            print(f"锚点(髋高基线±0.03): 离地 {dur * takeoff / (samples - 1):.3f}s"
                  f"  落地 {dur * land / (samples - 1):.3f}s")
        line = " ".join(f"{y - base:+.2f}" for y in hips_y)
        print(f"髋高相对首帧: {line}")
    for side in ("L", "R"):
        ys = foot_y[side]
        if ys:
            lo = min(ys)
            print(f"{side} 脚高: 最低 {lo:.3f}  首 {ys[0]:.3f}  末 {ys[-1]:.3f}"
                  f"  曲线 " + " ".join(f"{y - lo:+.2f}" for y in ys))


def main() -> int:
    dirs = sys.argv[1:] or [os.path.join(ROOT, "assets/characters/animations/Amy")]
    names = ("Jumping.glb", "RunningJump1.glb", "RunningJump1Left.glb",
             "RunningJump2.glb", "RunningJump2Right.glb", "RunningJump3.glb", "RunningJump3Left.glb",
             "JumpingUp.glb", "JumpingDown.glb")
    for d in dirs:
        for name in names:
            p = os.path.join(d, name)
            if os.path.isfile(p):
                probe(p)
    return 0


if __name__ == "__main__":
    sys.exit(main())
