#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""从 `RunningJump.glb` 生成左右镜像版本：沿身体正中矢状面（x 轴）反射整套 FK 骨架。

与旧的 `make_runjump_clips.py`（把支撑腿回正到 rest 站立姿态，导致腾空段膝角
被钉到 ~178° 僵直垂挂）不同，本脚本做的是**真正的骨骼左右镜像**：

* 左右骨配对交换：`mixamorig:LeftXxx ↔ mixamorig:RightXxx`；
* 中间骨（Hips/Spine/Neck/Head…）不交换，只把水平偏移镜像；
* 所有变换都在「相对绑定姿态的偏移空间」里做，再乘回对侧骨的 rest，避免左右
  rest 本身不对称把姿态叠错。

为什么在偏移空间反射
--------------------
设源骨 X 的 rest 旋转 `qXr`、帧旋转 `q`。相对 rest 的姿态偏移：

    ΔR = qXr⁻¹ ⊗ q

对 ΔR 做矢状面反射（矩阵共轭 `M · ΔR · M`，M = diag(-1, 1, 1)），再放回目标骨 Y：

    q' = qYr ⊗ reflect(ΔR)

平移同理：`δ = t − tXr`，`δ' = M·δ`（x 取反），`t' = tYr + δ'`。

纯 Python 直接改 GLB 数据，不经过 Blender 导入/导出，避免 Y-up↔Z-up 二次共轭。

用法
----
    python3 scripts/make_runjump_mirror.py \
        --src assets/characters/animations/Amy/RunningJump.glb \
        --out  assets/characters/animations/Amy/RunningJumpLeft.glb
"""

from __future__ import annotations

import argparse
import json
import math
import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe_jump_clips import read_glb, accessor, quat_mat  # noqa: E402

TYPE_N = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4}
COMP_FLOAT = 5126


# --------------------------------------------------------------------------- #
# 四元数 / 矩阵工具
# --------------------------------------------------------------------------- #

def qmul(a, b):
    ax, ay, az, aw = a
    bx, by, bz, bw = b
    return (
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    )


def qinv(a):
    x, y, z, w = a
    norm = x * x + y * y + z * z + w * w or 1.0
    return (-x / norm, -y / norm, -z / norm, w / norm)


def qnorm(a):
    x, y, z, w = a
    norm = math.sqrt(x * x + y * y + z * z + w * w) or 1.0
    return (x / norm, y / norm, z / norm, w / norm)


def mat_to_quat(m):
    tr = m[0][0] + m[1][1] + m[2][2]
    if tr > 0:
        s = math.sqrt(tr + 1.0) * 2.0
        w = s / 4.0
        x = (m[2][1] - m[1][2]) / s
        y = (m[0][2] - m[2][0]) / s
        z = (m[1][0] - m[0][1]) / s
    elif m[0][0] > m[1][1] and m[0][0] > m[2][2]:
        s = math.sqrt(1.0 + m[0][0] - m[1][1] - m[2][2]) * 2.0
        w = (m[2][1] - m[1][2]) / s
        x = s / 4.0
        y = (m[0][1] + m[1][0]) / s
        z = (m[0][2] + m[2][0]) / s
    elif m[1][1] > m[2][2]:
        s = math.sqrt(1.0 + m[1][1] - m[0][0] - m[2][2]) * 2.0
        w = (m[0][2] - m[2][0]) / s
        x = (m[0][1] + m[1][0]) / s
        y = s / 4.0
        z = (m[1][2] + m[2][1]) / s
    else:
        s = math.sqrt(1.0 + m[2][2] - m[0][0] - m[1][1]) * 2.0
        w = (m[1][0] - m[0][1]) / s
        x = (m[0][2] + m[2][0]) / s
        y = (m[1][2] + m[2][1]) / s
        z = s / 4.0
    return qnorm((x, y, z, w))


def reflect_matrix(m):
    """M · R · M，M = diag(-1, 1, 1)：沿 YZ 平面（x 轴）反射。"""
    mirror = (-1.0, 1.0, 1.0)
    return [[mirror[i] * m[i][j] * mirror[j] for j in range(3)] for i in range(3)]


def rest_of(node):
    tr = node.get("translation", [0.0, 0.0, 0.0])
    ro = node.get("rotation", [0.0, 0.0, 0.0, 1.0])
    sc = node.get("scale", [1.0, 1.0, 1.0])
    return list(tr), list(ro), list(sc)


def reflect_rotation(q, src_rest, dst_rest):
    delta = qmul(qinv(src_rest), q)
    reflected = mat_to_quat(reflect_matrix(quat_mat(delta)))
    return qnorm(qmul(dst_rest, reflected))


def reflect_translation(t, src_rest, dst_rest):
    dx = t[0] - src_rest[0]
    dy = t[1] - src_rest[1]
    dz = t[2] - src_rest[2]
    return (dst_rest[0] - dx, dst_rest[1] + dy, dst_rest[2] + dz)


# --------------------------------------------------------------------------- #
# GLB 重建
# --------------------------------------------------------------------------- #

def pair_nodes(nodes):
    """返回 partner: 源节点索引 → 镜像目标节点索引；中间骨映射到自身。"""
    by_name = {i: n.get("name", "") for i, n in enumerate(nodes)}
    left = {}
    partner = {}
    for i, name in by_name.items():
        if not name.startswith("mixamorig:"):
            continue
        if ":Left" in name:
            left[name.split(":Left", 1)[1]] = i
        elif ":Right" in name:
            pass
    for i, name in by_name.items():
        if not name.startswith("mixamorig:"):
            partner[i] = i
            continue
        if ":Right" in name:
            suffix = name.split(":Right", 1)[1]
            j = left.get(suffix)
            if j is not None:
                partner[i] = j
                partner[j] = i
            else:
                partner[i] = i
        elif ":Left" in name:
            # 右配对在上面循环里补上；缺对侧则保持自身
            partner.setdefault(i, i)
        else:
            partner[i] = i
    return partner


def build(src_path: str, out_path: str) -> None:
    doc, blob = read_glb(src_path)
    nodes = doc["nodes"]
    anim = doc["animations"][0]
    # 输出 GLB 内部动画名与文件名 stem 对齐（源片可能沿用旧名）
    anim["name"] = os.path.splitext(os.path.basename(out_path))[0]
    samplers = anim["samplers"]
    channels = anim["channels"]
    partner = pair_nodes(nodes)

    src_rest = [rest_of(n) for n in nodes]

    # 从零组装新 bin：时间轴 input 序列化一次复用，各 output 单独成块；
    # 不保留源 bin 里不再被引用的旧数据，避免包体积翻倍。
    out_bin = bytearray()
    new_views = []
    new_accessors = []

    def append_array(values, typ, src_acc=None):
        n = TYPE_N[typ]
        def pack(v):
            return struct.pack("<f", v) if n == 1 else struct.pack("<" + "f" * n, *v)
        payload = b"".join(pack(v) for v in values)
        # bufferView 起始 4 字节对齐
        pad = (-len(out_bin)) % 4
        out_bin.extend(b"\x00" * pad)
        view_offset = len(out_bin)
        out_bin.extend(payload)
        view_idx = len(new_views)
        new_views.append({
            "buffer": 0,
            "byteLength": len(payload),
            "byteOffset": view_offset,
        })
        acc = {
            "bufferView": view_idx,
            "componentType": COMP_FLOAT,
            "count": len(values),
            "type": typ,
        }
        if src_acc is not None:
            for key in ("min", "max"):
                if key in src_acc:
                    acc[key] = src_acc[key]
        acc_idx = len(new_accessors)
        new_accessors.append(acc)
        return acc_idx

    # 所有 sampler 共享同一条时间轴，先序列化一次。
    first_sampler = samplers[0]
    input_values = accessor(doc, blob, first_sampler["input"])
    input_acc = append_array(
        input_values, "SCALAR", doc["accessors"][first_sampler["input"]])

    new_samplers = []
    new_channels = []
    mirrored = 0
    copied = 0

    for ch in channels:
        src_node = ch["target"]["node"]
        path = ch["target"]["path"]
        dst_node = partner[src_node]
        sampler = samplers[ch["sampler"]]
        values = accessor(doc, blob, sampler["output"])

        src_tr, src_ro, _ = src_rest[src_node]
        dst_tr, dst_ro, _ = src_rest[dst_node]

        if path == "rotation":
            reflected = [reflect_rotation(q, src_ro, dst_ro) for q in values]
            out_acc = append_array(reflected, "VEC4")
            mirrored += 1
        elif path == "translation":
            reflected = [reflect_translation(t, src_tr, dst_tr) for t in values]
            out_acc = append_array(reflected, "VEC3")
            mirrored += 1
        else:  # scale：均匀缩放镜像不变，原样复制
            out_acc = append_array(values, "VEC3")
            copied += 1

        new_sampler = {"input": input_acc, "output": out_acc}
        new_samplers.append(new_sampler)
        new_channels.append({
            "target": {"node": dst_node, "path": path},
            "sampler": len(new_samplers) - 1,
        })

    anim["samplers"] = new_samplers
    anim["channels"] = new_channels
    doc["accessors"] = new_accessors
    doc["bufferViews"] = new_views
    doc["buffers"] = [{"byteLength": len(out_bin)}]

    write_glb(out_path, doc, bytes(out_bin))
    print(f"[mirror] 通道 {len(channels)}（反射 {mirrored}，复制 scale {copied}）"
          f" → {out_path}")


def write_glb(path: str, doc: dict, blob: bytes) -> None:
    js = json.dumps(doc, separators=(",", ":")).encode("utf-8")
    js += b"\x20" * ((-len(js)) % 4)
    blob += b"\x00" * ((-len(blob)) % 4)

    def chunk(ctype: int, payload: bytes) -> bytes:
        return struct.pack("<II", len(payload), ctype) + payload

    glb = struct.pack("<III", 0x46546C67, 2, 28 + len(js) + len(blob))
    glb += chunk(0x4E4F534A, js)
    glb += chunk(0x004E4942, blob)
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path, "wb") as fh:
        fh.write(glb)


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="生成跑步跳剪辑的左右镜像版本")
    p.add_argument("--src", required=True)
    p.add_argument("--out", required=True)
    return p.parse_args()


def main() -> int:
    args = parse_args()
    if not os.path.isfile(args.src):
        print(f"源文件不存在：{args.src}")
        return 1
    build(args.src, args.out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
