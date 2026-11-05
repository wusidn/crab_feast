#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""校验导出到 assets/characters/animations/<角色>/ 的动画 GLB，并在每个角色目录下写 manifest.json。

检查项：
  * 每个 GLB 正好一个动画、不含网格/蒙皮（纯动画包）
  * 动画驱动的节点名必须都能在对应角色的 rig GLB 里找到（Bevy 按节点名路径匹配）
  * 同一角色的动画集合是否齐全（默认对照动画源目录）
  * 汇总时长、通道数、体积、根位移幅度

清单默认散落在各角色目录（<角色>/manifest.json）；需要总览时加 --root-index
在输出根目录额外生成一份汇总。

普通 Python 脚本，不需要 Blender：

    python3 scripts/validate_animations.py
    python3 scripts/validate_animations.py --root-index
"""

from __future__ import annotations

import argparse
import json
import math
import os
import struct
import sys


def read_glb(path: str) -> tuple[dict, bytes]:
    with open(path, "rb") as fh:
        data = fh.read()
    _, _, length = struct.unpack("<III", data[:12])
    off, gltf, bin_chunk = 12, None, b""
    while off < length:
        clen, ctype = struct.unpack("<II", data[off:off + 8])
        payload = data[off + 8:off + 8 + clen]
        off += 8 + clen
        if ctype == 0x4E4F534A:
            gltf = json.loads(payload.decode("utf-8"))
        elif ctype == 0x004E4942:
            bin_chunk = payload
    if gltf is None:
        raise RuntimeError(f"{path} 没有 JSON chunk")
    return gltf, bin_chunk


TYPE_N = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT4": 16}
FMT = {5126: "f", 5125: "I", 5123: "H", 5122: "h", 5121: "B", 5120: "b"}


def accessor(doc: dict, blob: bytes, index: int) -> list:
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


def node_names(doc: dict) -> set:
    return {n.get("name") for n in doc.get("nodes", []) if n.get("name")}


def describe(doc: dict, blob: bytes) -> dict:
    anims = doc.get("animations", [])
    an = anims[0]
    nodes = doc["nodes"]
    animated = {nodes[c["target"]["node"]].get("name") for c in an["channels"]}
    dur = 0.0
    for s in an["samplers"]:
        acc = doc["accessors"][s["input"]]
        if "max" in acc:
            dur = max(dur, acc["max"][0])
    hips = next((i for i, n in enumerate(nodes)
                 if (n.get("name") or "").endswith("Hips")
                 and not (n.get("name") or "").startswith("Ctrl")), None)
    stride = None
    if hips is not None:
        ch = next((c for c in an["channels"]
                   if c["target"]["node"] == hips and c["target"]["path"] == "translation"), None)
        if ch is not None:
            vals = accessor(doc, blob, an["samplers"][ch["sampler"]]["output"])
            xs = [v[0] for v in vals]
            zs = [v[2] for v in vals]
            ys = [v[1] for v in vals]
            stride = {
                "horizontal": round(math.hypot(max(xs) - min(xs), max(zs) - min(zs)), 3),
                "vertical": round(max(ys) - min(ys), 3),
            }
    return {
        "animation": an.get("name"),
        "animation_count": len(anims),
        "channels": len(an["channels"]),
        "duration": round(dur, 3),
        "nodes": len(nodes),
        "animated_nodes": len(animated),
        "has_mesh": bool(doc.get("meshes")),
        "stride": stride,
    }, animated


def clip_name(stem: str) -> str:
    import re
    parts = [p for p in re.split(r"[^0-9a-zA-Z]+", stem) if p]
    return "".join(p[:1].upper() + p[1:] for p in parts)


def main() -> int:
    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)
    p = argparse.ArgumentParser(description="校验导出的动画 GLB 并重建 manifest.json")
    p.add_argument("--out", default=os.path.join(repo, "assets/characters/animations"))
    p.add_argument("--rigs", default=os.path.join(repo, "assets/characters/rigs"))
    p.add_argument("--anims", default=os.path.join(repo, "assets/source/character/animations/Default"),
                   help="用于检查动画集合是否齐全的源目录（传空字符串则跳过）")
    p.add_argument("--quiet", action="store_true")
    p.add_argument("--root-index", action="store_true",
                   help="额外在输出根目录写一份汇总 manifest.json（默认只写各角色目录下的）")
    args = p.parse_args()

    out_dir = os.path.abspath(args.out)
    rigs_dir = os.path.abspath(args.rigs)

    expected_clips = []
    if args.anims and os.path.isdir(args.anims):
        expected_clips = sorted(clip_name(os.path.splitext(f)[0])
                                for f in os.listdir(args.anims) if f.lower().endswith(".fbx"))

    models = {}
    for name in sorted(os.listdir(out_dir)):
        model_dir = os.path.join(out_dir, name)
        if not os.path.isdir(model_dir):
            continue
        local_issues = []
        rig_glb = os.path.join(rigs_dir, name + ".glb")
        rig_nodes = node_names(read_glb(rig_glb)[0]) if os.path.exists(rig_glb) else set()
        if not rig_nodes:
            local_issues.append(f"找不到/读不出 rig GLB（{rig_glb}）")

        clips = {}
        for f in sorted(os.listdir(model_dir)):
            if not f.endswith(".glb"):
                continue
            path = os.path.join(model_dir, f)
            try:
                doc, blob = read_glb(path)
                info, animated = describe(doc, blob)
            except Exception as exc:
                local_issues.append(f"{f}: 解析失败 {exc}")
                continue
            if info["animation_count"] != 1:
                local_issues.append(f"{f}: 含 {info['animation_count']} 个动画")
            if info["has_mesh"]:
                local_issues.append(f"{f}: 含网格（应为纯动画包）")
            missing = sorted(animated - rig_nodes) if rig_nodes else []
            if missing:
                local_issues.append(f"{f}: {len(missing)} 个节点不在 rig GLB 里，如 {missing[:3]}")
            info["file"] = f
            info["size_kb"] = round(os.path.getsize(path) / 1024)
            info["node_match"] = not missing
            clips[os.path.splitext(f)[0]] = info

        missing_clips = [c for c in expected_clips if c not in clips]
        if missing_clips:
            local_issues.append(f"缺少动画 {missing_clips}")

        size_kb = sum(c["size_kb"] for c in clips.values())
        model_doc = {
            "model": name,
            "rig": f"characters/rigs/{name}.glb",
            "summary": {
                "animations": len(clips),
                "size_mb": round(size_kb / 1024, 2),
                "channels": sorted({c["channels"] for c in clips.values()}),
                "issues": len(local_issues),
            },
            "missing_clips": missing_clips,
            "clips": dict(sorted(clips.items())),
            "issues": local_issues,
        }
        with open(os.path.join(model_dir, "manifest.json"), "w", encoding="utf-8") as fh:
            json.dump(model_doc, fh, ensure_ascii=False, indent=2)

        models[name] = {"clips": model_doc["clips"],
                        "clip_count": len(clips),
                        "missing_clips": missing_clips,
                        "size_kb": size_kb,
                        "issues": local_issues}

    issues = [(f"{name}: {i}" if not i.startswith(name + ":") else i)
              for name, m in models.items() for i in m["issues"]]
    total = sum(m["clip_count"] for m in models.values())
    size_mb = sum(m["size_kb"] for m in models.values()) / 1024
    manifest = {
        "summary": {
            "models": len(models),
            "animations": total,
            "size_mb": round(size_mb, 2),
            "issues": len(issues),
        },
        "source_animations": expected_clips,
        "models": models,
        "issues": issues,
    }
    manifest_path = os.path.join(out_dir, "manifest.json")
    if args.root_index:
        with open(manifest_path, "w", encoding="utf-8") as fh:
            json.dump(manifest, fh, ensure_ascii=False, indent=2)

    if not args.quiet:
        print(f"{'model':28} {'clips':>5} {'总大小':>8} {'通道':>5} {'时长s':>10}  状态")
        for name, m in models.items():
            chans = sorted({c["channels"] for c in m["clips"].values()})
            durs = [c["duration"] for c in m["clips"].values()]
            size = m["size_kb"] / 1024
            ok = not m["missing_clips"]
            print(f"{name:28} {m['clip_count']:>5} {size:>7.2f}M {str(chans):>5} "
                  f"{min(durs):.2f}-{max(durs):.2f}  {'OK' if ok else '缺 ' + ','.join(m['missing_clips'])}")
        print(f"\n合计 {len(models)} 个角色 / {total} 个动画 / {size_mb:.2f}MB")
        if issues:
            print(f"\n发现 {len(issues)} 个问题：")
            for i in issues:
                print(f"  - {i}")
        else:
            print("校验通过：所有动画包结构正确、节点名与 rig GLB 对齐。")
        print(f"清单已写入各角色目录：<角色>/manifest.json"
              + (f"；汇总：{manifest_path}" if args.root_index else ""))

    return 0 if not issues else 1


if __name__ == "__main__":
    sys.exit(main())
