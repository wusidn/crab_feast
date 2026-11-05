#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把 Mixamo 基础移动动画重投影到各角色控制器 rig 上，并导出为纯动画 GLB。

背景
----
assets/source/character/animations/Default 里的动画是按 Y Bot（约 1.79m）导出的，
直接套到其它角色会出问题：
  * Mixamo 骨骼名/层级一致，但骨骼长度（髋高、腿长、臂长）差别很大；
  * 动画的根位移（Hips 的 Translation）是 Y Bot 的绝对米数，矮角色会严重滑步；
  * 交给 Mixamo Rig 插件做 IK 重定向时，脚部 IK 目标会被钉在 Y Bot 的绝对位置，
    矮角色直接悬空（Amy 实测脚离地 28cm）。
所以这里做「位移重投影」：按 腿部有效长度（髋高 - 脚趾高）的比值缩放源动画的
世界空间位移，再用 FK 方式只传旋转 + 重投影后的根位移，让姿态由目标角色自己的
骨长决定。

每个（角色 × 动画）的流程：
  1. 打开该角色预先构建好的控制器 rig 缓存（.blend，由 FBX + mr.make_rig 生成）
  2. 导入动画 FBX 作为重定向源，按比值缩放源骨架的世界空间尺度
  3. bpy.ops.mr.import_anim_to_rig()（Mixamo Rig 的 Apply Animation to Control Rig）
  4. 地面校正：按脚部顶点把整段动画的垂直偏移归零
  5. 只选中骨架导出 GLB（不含网格），保证动画包只有几百 KB

用法（必须通过 Blender 无头模式运行）：

    /Applications/Blender.app/Contents/MacOS/Blender -b \
        --python scripts/export_mixamo_animations.py -- [参数]

    # 或包装脚本（支持并行）
    scripts/export_animations.sh                 # 全部
    scripts/export_animations.sh -j 4            # 4 个进程并行
    scripts/export_animations.sh --only "Amy"    # 只处理名字含 Amy 的角色
    scripts/export_animations.sh --clips walking,idle
    scripts/export_animations.sh --force         # 覆盖已存在的 GLB
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import struct
import sys
import time
import traceback

import bpy
from mathutils import Vector

ADDON_CANDIDATES = (
    "bl_ext.blender_org.mixamo_rig",
    "mixamo_rig",
)

SKIP_DIRS = {".git", "target", "build", "node_modules"}

# 身高/腿长比值的兜底参考值（Y Bot 实测，仅当 Y Bot 缓存不可用时使用）
FALLBACK_REF_LEG = 0.965


def log(msg: str) -> None:
    print(f"[anim_export] {msg}", flush=True)


# --------------------------------------------------------------------------- #
# 参数
# --------------------------------------------------------------------------- #
def default_cache_dir() -> str:
    if sys.platform == "darwin":
        base = os.path.join(os.path.expanduser("~"), "Library", "Caches")
    else:
        base = os.path.join(os.path.expanduser("~"), ".cache")
    return os.path.join(base, "crab_feast", "mixamo_rigs")


def parse_args() -> argparse.Namespace:
    argv = sys.argv
    argv = argv[argv.index("--") + 1:] if "--" in argv else []

    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)

    p = argparse.ArgumentParser(
        prog="export_mixamo_animations.py",
        description="把 Default 基础移动动画重投影到各角色控制器 rig 并导出动画 GLB",
    )
    p.add_argument("--rigs", default=os.path.join(repo, "assets/source/character/rigs"),
                   help="角色 FBX 源目录（默认 assets/source/character/rigs）")
    p.add_argument("--anims", default=os.path.join(repo, "assets/source/character/animations/Default"),
                   help="动画 FBX 源目录（默认 assets/source/character/animations/Default）")
    p.add_argument("--out", default=os.path.join(repo, "assets/characters/animations"),
                   help="动画输出根目录（默认 assets/characters/animations）")
    p.add_argument("--rig-glb", default=os.path.join(repo, "assets/characters/rigs"),
                   help="已导出的角色 rig GLB 目录，用于校验节点名一致性")
    p.add_argument("--cache-dir", default=default_cache_dir(),
                   help="控制器 rig 缓存目录（.blend）")
    p.add_argument("--only", action="append", default=[], metavar="PATTERN",
                   help="只处理匹配的角色名（支持 * 通配，可重复）")
    p.add_argument("--skip", action="append", default=[], metavar="PATTERN",
                   help="跳过匹配的角色名（支持 * 通配，可重复）")
    p.add_argument("--clips", default=None, metavar="A,B",
                   help="只处理这些动画（按文件名或导出的 Pascal 名匹配，逗号分隔）")
    p.add_argument("--force", action="store_true",
                   help="覆盖已存在的 GLB（默认跳过）")
    p.add_argument("--rebuild-rig", action="store_true",
                   help="强制重建控制器 rig 缓存（默认复用）")
    p.add_argument("--ik", action="store_true",
                   help="重定向使用 IK（默认 FK；IK 会把脚钉在源角色绝对位置，"
                        "体型差异大时会悬空，一般不要开）")
    p.add_argument("--ratio-mode", choices=("leg", "height", "none"), default="leg",
                   help="位移重投影的比值基准：leg=腿长比（默认，最不易浮空）、"
                        "height=整体身高比、none=不重投影")
    p.add_argument("--no-ground", action="store_true",
                   help="关闭贴地校正（默认把整段动画最低脚部顶点归到地面）")
    p.add_argument("--ground-margin", type=float, default=0.15, metavar="M",
                   help="参与贴地测量的脚部顶点范围（静止姿态 z < M 米，默认 0.15）")
    p.add_argument("--frame-step", type=int, default=2, metavar="N",
                   help="贴地测量采样步长（默认每 2 帧）")
    p.add_argument("--shard", default=None, metavar="I/N",
                   help="分片：只处理第 I 份（1..N）的角色，供包装脚本并行调用")
    p.add_argument("--list", action="store_true", help="只列出待处理组合，不执行")
    p.add_argument("--manifest", action="store_true",
                   help="写出 manifest 分片文件（按角色）")
    return p.parse_args(argv)


def matches(name: str, patterns) -> bool:
    low = name.lower()
    for pat in patterns:
        p = pat.lower()
        if any(ch in p for ch in "*?["):
            if fnmatch.fnmatch(low, p):
                return True
        elif p in low:
            return True
    return False


# --------------------------------------------------------------------------- #
# 命名
# --------------------------------------------------------------------------- #
def model_name(fbx_name: str) -> str:
    """角色名：截断第一个下划线及其后（Ch02_nonPBR -> Ch02），保持与 rig GLB 一致。"""
    stem = os.path.splitext(fbx_name)[0]
    return stem.split("_", 1)[0] if "_" in stem else stem


def clip_name(fbx_name: str) -> str:
    """动画名：left strafe walking -> LeftStrafeWalking，right turn 90 -> RightTurn90。"""
    stem = os.path.splitext(fbx_name)[0]
    parts = [p for p in re.split(r"[^0-9a-zA-Z]+", stem) if p]
    return "".join(p[:1].upper() + p[1:] for p in parts)


# --------------------------------------------------------------------------- #
# Blender 基础操作
# --------------------------------------------------------------------------- #
def ensure_addon() -> str:
    import addon_utils

    if hasattr(bpy.types, "MR_OT_make_rig"):
        return "already-enabled"
    for mod in ADDON_CANDIDATES:
        try:
            __import__(mod)
        except Exception:
            pass
        if hasattr(bpy.types, "MR_OT_make_rig"):
            return mod
        addon_utils.enable(mod, default_set=True, handle_error=None)
        if hasattr(bpy.types, "MR_OT_make_rig"):
            return mod
    raise RuntimeError(
        "未找到 Mixamo Rig 插件（MR_OT_make_rig 未注册）。"
        "请确认已在 Blender 中启用，并且启动时不要加 --factory-startup。"
    )


def deselect_all() -> None:
    for obj in bpy.context.view_layer.objects:
        obj.select_set(False)


def clear_scene() -> None:
    """删物体并清理孤立 datablock。不能用 read_factory_settings，会卸载插件。"""
    obj = bpy.context.object
    if obj is not None and obj.mode != "OBJECT":
        bpy.ops.object.mode_set(mode="OBJECT")
    for o in list(bpy.data.objects):
        bpy.data.objects.remove(o, do_unlink=True)
    for coll in (bpy.data.meshes, bpy.data.armatures, bpy.data.actions,
                 bpy.data.materials, bpy.data.images, bpy.data.collections,
                 bpy.data.node_groups, bpy.data.cameras, bpy.data.lights):
        for block in list(coll):
            if block.users == 0:
                coll.remove(block)


def find_armature() -> "bpy.types.Object":
    arms = [o for o in bpy.context.scene.objects if o.type == "ARMATURE"]
    if not arms:
        raise RuntimeError("文件里没有骨架对象")
    def score(o):
        return sum(1 for b in o.data.bones if "Hips" in b.name)
    arms.sort(key=score, reverse=True)
    if score(arms[0]) == 0:
        raise RuntimeError(f"骨架 {arms[0].name} 不含 Mixamo 的 Hips")
    return arms[0]


def bone_head_z(arm, names) -> float | None:
    """按骨骼基名取头部高度，兼容 mixamorig: / mixamorig1: 这类带编号的前缀。"""
    for want in names:
        for b in arm.data.bones:
            base = b.name.split(":")[-1]
            if base == want:
                return (arm.matrix_world @ b.head_local).z
    return None


def rest_leg_length(arm) -> float:
    """腿部有效长度 = 髋高 - 脚趾高。位移重投影与贴地都以它为基准。"""
    hips = bone_head_z(arm, ["Hips"])
    toes = [z for z in (bone_head_z(arm, ["LeftToeBase"]),
                        bone_head_z(arm, ["RightToeBase"])) if z is not None]
    if hips is None or not toes:
        raise RuntimeError("找不到 Hips / ToeBase 骨骼，无法计算腿长比")
    return hips - min(toes)


# --------------------------------------------------------------------------- #
# 控制器 rig 缓存
# --------------------------------------------------------------------------- #
def cache_path(cache_dir: str, name: str) -> str:
    safe = re.sub(r"[^0-9A-Za-z._ -]", "_", name).strip().replace(" ", "_")
    return os.path.join(cache_dir, f"{safe}.blend")


def build_rig_cache(fbx_path: str, dest: str, use_ik: bool) -> None:
    """导入 FBX -> mr.make_rig -> 去掉贴图/材质 -> 存成 .blend，供每个动画复用。"""
    t0 = time.time()
    clear_scene()
    bpy.ops.import_scene.fbx(filepath=fbx_path, use_anim=True)
    arm = find_armature()
    log(f"  导入 {os.path.basename(fbx_path)} {time.time() - t0:.1f}s，骨架 {arm.name}")
    # 去掉原 FBX 自带的动画，缓存里应当只有静止姿态
    if arm.animation_data:
        arm.animation_data_clear()
    for act in list(bpy.data.actions):
        bpy.data.actions.remove(act)

    deselect_all()
    arm.select_set(True)
    bpy.context.view_layer.objects.active = arm
    if bpy.context.object.mode != "OBJECT":
        bpy.ops.object.mode_set(mode="OBJECT")

    if "mr_control_rig" not in arm.data.keys():
        result = bpy.ops.mr.make_rig(
            bake_anim=False,
            ik_arms=use_ik,
            ik_legs=use_ik,
        )
        if "FINISHED" not in result:
            raise RuntimeError(f"mr.make_rig 失败：{result}")
    log(f"  控制器 rig 就绪：{len(arm.data.bones)} 骨")

    # 网格保留（贴地测量要用），但材质/贴图全部剥离，让缓存从上百 MB 降到几十 MB
    for mat in list(bpy.data.materials):
        bpy.data.materials.remove(mat)
    for img in list(bpy.data.images):
        bpy.data.images.remove(img)
    bpy.data.orphans_purge(do_local_ids=True, do_linked_ids=True, do_recursive=True)

    os.makedirs(os.path.dirname(dest), exist_ok=True)
    try:
        bpy.ops.wm.save_as_mainfile(filepath=dest, compress=True, copy=False)
    except TypeError:
        bpy.ops.wm.save_as_mainfile(filepath=dest, compress=True)
    log(f"  缓存写入 {dest}（{os.path.getsize(dest) / 1e6:.1f}MB，"
        f"{time.time() - t0:.1f}s）")


# --------------------------------------------------------------------------- #
# 重定向
# --------------------------------------------------------------------------- #
def import_anim_source(anim_path: str, target_arm, ratio: float):
    """导入动画 FBX 作为重定向源，并按 ratio 缩放它的世界空间尺度。

    缩放源骨架对象 = 把整段动画（根位移、骨盆起伏、肢体摆动半径）等比投到目标体型；
    插件的 import_anim_to_rig 本身会按 src.scale[0] 处理 location 曲线，
    所以这里直接改 scale 即可（源是 cm，导入后本来就是 0.01）。
    """
    bpy.ops.import_scene.fbx(filepath=anim_path, use_anim=True)
    cands = [o for o in bpy.context.scene.objects
             if o.type == "ARMATURE" and o is not target_arm]
    if not cands:
        raise RuntimeError("动画 FBX 没有带骨架，无法作为重定向源")
    src = max(cands, key=lambda o: sum(1 for b in o.data.bones if "Hips" in b.name))

    # 源模型网格不参与任何计算，直接删掉省内存
    def belongs_to_src(obj) -> bool:
        parent = obj.parent
        while parent is not None:
            if parent == src:
                return True
            parent = parent.parent
        return False

    for o in list(bpy.context.scene.objects):
        if o.type == "MESH" and belongs_to_src(o):
            bpy.data.objects.remove(o, do_unlink=True)

    if ratio != 1.0:
        src.scale = [src.scale[0] * ratio] * 3
    return src


def keep_only_action(arm, name: str):
    """只保留目标骨架上的当前动作，其余删除（保证一个 GLB 只有一个 Animation）。"""
    ad = arm.animation_data
    if ad is None or ad.action is None:
        raise RuntimeError("重定向后目标骨架没有动作")
    action = ad.action
    for a in list(bpy.data.actions):
        if a is not action:
            bpy.data.actions.remove(a)
    action.name = name
    action.use_fake_user = True
    return action


def hips_local_axis(arm) -> Vector:
    """世界 Z 轴在 Ctrl_Hips 局部（matrix_basis）空间中的方向，用于贴地校正。

    注意：pose_bone.location 的分量就是按骨骼自身静止朝向（bone.matrix_local）表述的，
    不要再乘父骨骼的逆矩阵，否则会把"向上"错映射成"向前"。
    """
    pb = arm.pose.bones.get("Ctrl_Hips")
    if pb is None:
        raise RuntimeError("控制器 rig 缺少 Ctrl_Hips")
    rest = pb.bone.matrix_local.to_3x3()
    return rest.inverted() @ Vector((0.0, 0.0, 1.0))


def character_meshes(arm) -> list:
    """只取绑定在这副骨架上的角色网格；控制器自定义形状（cs_* 空物体带动的小网格）要排除，
    否则它们的包围盒会污染贴地判断。"""
    result = []
    for o in bpy.context.scene.objects:
        if o.type != "MESH" or o.hide_get():
            continue
        if o.parent == arm:
            result.append(o)
            continue
        for mod in o.modifiers:
            if mod.type == "ARMATURE" and getattr(mod, "object", None) == arm:
                result.append(o)
                break
    return result


class GroundProbe:
    """只采样脚部顶点，用来判断整段动画最低点离地多少。"""

    def __init__(self, meshes, margin: float):
        self.meshes = meshes
        self.indices = {}
        for m in meshes:
            mw = m.matrix_world
            idx = [v.index for v in m.data.vertices if (mw @ v.co).z < margin]
            if idx:
                self.indices[m.name] = idx

    def min_z(self) -> float | None:
        dg = bpy.context.evaluated_depsgraph_get()
        dg.update()
        lowest = None
        for m in self.meshes:
            idx = self.indices.get(m.name)
            if not idx:
                continue
            ev = m.evaluated_get(dg)
            verts = ev.data.vertices
            mw = ev.matrix_world
            for i in idx:
                z = (mw @ verts[i].co).z
                if lowest is None or z < lowest:
                    lowest = z
        return lowest

    def min_z_over(self, start: int, end: int, step: int) -> tuple[float | None, int]:
        lowest = None
        frame_low = start
        for f in range(start, end + 1, max(1, step)):
            bpy.context.scene.frame_set(f)
            z = self.min_z()
            if z is not None and (lowest is None or z < lowest):
                lowest, frame_low = z, f
        return lowest, frame_low


def ground_adjust(arm, action, probe: GroundProbe, start: int, end: int,
                  step: int) -> dict:
    """把整段动画的最低脚部顶点归到 z=0（只改 Ctrl_Hips 的垂直位移通道）。"""
    stats = {"offset": 0.0, "min_before": None, "min_after": None}
    if not probe.indices:
        return stats

    lowest, frame_low = probe.min_z_over(start, end, step)
    stats["min_before"] = round(lowest, 4) if lowest is not None else None
    if lowest is None or abs(lowest) < 0.002:
        stats["min_after"] = stats["min_before"]
        return stats

    dz = -lowest
    axis = hips_local_axis(arm)
    curves = {}
    path = 'pose.bones["Ctrl_Hips"].location'
    for fc in action.fcurves:
        if fc.data_path == path:
            curves[fc.array_index] = fc
    if len(curves) < 3:
        log(f"  警告：找不到 Ctrl_Hips 位移曲线，跳过贴地校正（最低点 {lowest:.3f}）")
        return stats

    for i in range(3):
        delta = dz * axis[i]
        for kp in curves[i].keyframe_points:
            kp.co[1] += delta
        curves[i].update()

    stats["offset"] = round(dz, 4)
    after, _ = probe.min_z_over(start, end, step)
    stats["min_after"] = round(after, 4) if after is not None else None
    log(f"  贴地校正：最低点 {lowest:+.3f} -> {after:+.3f}（偏移 {dz:+.3f}m，第 {frame_low} 帧最低）")
    return stats


# --------------------------------------------------------------------------- #
# 导出与校验
# --------------------------------------------------------------------------- #
def export_anim_glb(arm, out_path: str) -> None:
    deselect_all()
    arm.select_set(True)
    bpy.context.view_layer.objects.active = arm
    if bpy.context.object.mode != "OBJECT":
        bpy.ops.object.mode_set(mode="OBJECT")

    bpy.ops.export_scene.gltf(
        filepath=out_path,
        export_format="GLB",
        use_selection=True,
        export_apply=False,
        export_animations=True,
        export_animation_mode="ACTIONS",
        export_force_sampling=True,     # 采样含约束的最终姿态，控制器/变形骨都要有
        export_nla_strips=False,
        export_optimize_animation_size=False,
        # Mixamo 的烘焙从第 1 帧（t=1/30s）起算，首关键帧时间戳是 0.033 而不是 0。
        # 不平移的话 Bevy 的 clip_duration 仍按最后一个关键帧算（1.067s），循环时
        # 前 1/30s 会被 clamp 到首帧，每圈有一帧停顿（walk 单帧最大步进 11.9°）。
        export_anim_slide_to_zero=True,
        export_skins=False,             # 纯动画，不带蒙皮
        export_def_bones=False,         # 保留 Ctrl_* 控制器骨
        export_yup=True,
        export_morph=False,
        export_materials="NONE",
        export_extras=False,
    )


def read_glb_json(path: str) -> dict:
    with open(path, "rb") as fh:
        data = fh.read()
    _, _, length = struct.unpack("<III", data[:12])
    off, chunks = 12, []
    while off < length:
        clen, ctype = struct.unpack("<II", data[off:off + 8])
        chunks.append((ctype, data[off + 8:off + 8 + clen]))
        off += 8 + clen
    for ctype, payload in chunks:
        if ctype == 0x4E4F534A:  # JSON
            return json.loads(payload.decode("utf-8"))
    raise RuntimeError(f"{path} 没有 JSON chunk")


def rig_node_names(rig_glb: str) -> set:
    try:
        doc = read_glb_json(rig_glb)
    except Exception:
        return set()
    return {n.get("name") for n in doc.get("nodes", []) if n.get("name")}


def validate(out_path: str, expected_nodes: set, rig_name: str) -> dict:
    """确认导出的动画节点名都能在已导出的角色 rig GLB 里找到（Bevy 按名字路径匹配）。"""
    doc = read_glb_json(out_path)
    nodes = {n.get("name") for n in doc.get("nodes", []) if n.get("name")}
    anims = doc.get("animations", [])
    channels = len(anims[0]["channels"]) if anims else 0
    animated = {doc["nodes"][c["target"]["node"]].get("name")
                for c in anims[0]["channels"]} if anims else set()
    missing = sorted(animated - expected_nodes) if expected_nodes else []
    return {
        "animation": anims[0].get("name") if anims else None,
        "animation_count": len(anims),
        "channels": channels,
        "nodes": len(nodes),
        "rig": rig_name,
        "missing_nodes": missing[:8],
        "missing_count": len(missing),
    }


# --------------------------------------------------------------------------- #
# 单个（角色 × 动画）
# --------------------------------------------------------------------------- #
def retarget_one(cache: str, arm_name: str, anim_path: str, clip: str,
                 args, ratio: float, out_path: str, expected_nodes: set) -> dict:
    info = {"clip": clip, "status": "ok"}
    t0 = time.time()
    bpy.ops.wm.open_mainfile(filepath=cache)
    ensure_addon()

    arm = next((o for o in bpy.context.scene.objects if o.type == "ARMATURE"), None)
    if arm is None:
        raise RuntimeError("缓存里没有骨架")
    meshes = character_meshes(arm)
    probe = GroundProbe(meshes, args.ground_margin)

    src = import_anim_source(anim_path, arm, ratio)
    bpy.context.scene.mix_source_armature = src

    deselect_all()
    arm.select_set(True)
    bpy.context.view_layer.objects.active = arm
    if bpy.context.object.mode != "OBJECT":
        bpy.ops.object.mode_set(mode="OBJECT")

    result = bpy.ops.mr.import_anim_to_rig()
    if "FINISHED" not in result:
        raise RuntimeError(f"mr.import_anim_to_rig 失败：{result}")

    action = keep_only_action(arm, clip)
    start, end = int(action.frame_range[0]), int(action.frame_range[1])
    bpy.context.scene.frame_start = start
    bpy.context.scene.frame_end = end
    info["frames"] = [start, end]
    info["ratio"] = round(ratio, 4)

    if not args.no_ground:
        info["ground"] = ground_adjust(arm, action, probe, start, end, args.frame_step)

    # 回到第 1 帧再导出，避免 rest/当前帧影响采样起点
    bpy.context.scene.frame_set(start)
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    export_anim_glb(arm, out_path)

    info.update(validate(out_path, expected_nodes, os.path.basename(out_path)))
    info["size_kb"] = round(os.path.getsize(out_path) / 1024)
    if info.get("missing_count"):
        info["status"] = "warn"
        log(f"  警告：{clip} 有 {info['missing_count']} 个节点名不在 rig GLB 里，"
            f"例如 {info['missing_nodes']}")
    if info["animation_count"] != 1:
        info["status"] = "warn"
        log(f"  警告：{clip} 导出了 {info['animation_count']} 个动画")
    info["seconds"] = round(time.time() - t0, 1)
    return info


# --------------------------------------------------------------------------- #
# 主流程
# --------------------------------------------------------------------------- #
def collect_files(directory: str, exts=(".fbx", ".fbx7")) -> list:
    if not os.path.isdir(directory):
        return []
    return sorted(f for f in os.listdir(directory)
                  if f.lower().endswith(exts) and not f.startswith("."))


def reference_leg(args) -> float:
    """Y Bot 的腿部有效长度作为位移重投影的基准。"""
    ref_cache = cache_path(args.cache_dir, "Y Bot")
    if os.path.exists(ref_cache):
        try:
            bpy.ops.wm.open_mainfile(filepath=ref_cache)
            ensure_addon()
            arm = next((o for o in bpy.context.scene.objects if o.type == "ARMATURE"), None)
            if arm is not None:
                return rest_leg_length(arm)
        except Exception as exc:
            log(f"读取 Y Bot 基准失败（{exc}），使用兜底值 {FALLBACK_REF_LEG}")
    return FALLBACK_REF_LEG


def main() -> int:
    args = parse_args()

    args.rigs = os.path.abspath(args.rigs)
    args.anims = os.path.abspath(args.anims)
    args.out = os.path.abspath(args.out)
    args.cache_dir = os.path.abspath(args.cache_dir)

    rigs = [f for f in collect_files(args.rigs) if matches(model_name(f), args.only)] \
        if args.only else collect_files(args.rigs)
    if args.skip:
        rigs = [f for f in rigs if not matches(model_name(f), args.skip)]

    anims = collect_files(args.anims)
    if args.clips:
        wanted = {c.strip().lower() for c in args.clips.split(",") if c.strip()}
        anims = [f for f in anims
                 if os.path.splitext(f)[0].lower() in wanted
                 or clip_name(f).lower() in wanted]

    if not rigs:
        log("没有匹配到角色 FBX")
        return 1
    if not anims:
        log("没有匹配到动画 FBX")
        return 1

    if args.shard:
        try:
            idx, total = (int(x) for x in args.shard.split("/"))
            if not 1 <= idx <= total:
                raise ValueError
        except ValueError:
            log(f"--shard 参数非法：{args.shard}（应为 I/N）")
            return 2
        rigs = [f for n, f in enumerate(rigs) if n % total == idx - 1]

    if args.list:
        for r in rigs:
            print(os.path.join(args.rigs, r))
            for a in anims:
                print(f"  {clip_name(a)} <- {a}")
        return 0

    try:
        ensure_addon()
    except Exception as exc:
        log(str(exc))
        return 2

    log(f"角色目录 {args.rigs}（{len(rigs)} 个）")
    log(f"动画目录 {args.anims}（{len(anims)} 个）")
    log(f"输出根目录 {args.out}")
    log(f"缓存目录 {args.cache_dir}")
    log(f"位移重投影 ratio-mode={args.ratio_mode}，重定向={'IK' if args.ik else 'FK'}，"
        f"贴地={'关' if args.no_ground else '开'}")

    all_results = []
    ok = fail = skip = 0

    # 先构建所有角色的控制器 rig 缓存（一次 FBX 导入，后面每个动画只重开缓存）
    for i, rig_file in enumerate(rigs, 1):
        name = model_name(rig_file)
        cache = cache_path(args.cache_dir, name)
        rig_glb = os.path.join(os.path.abspath(args.rig_glb), name + ".glb")
        log(f"[{i}/{len(rigs)}] 角色 {name}")
        try:
            if args.rebuild_rig or not os.path.exists(cache):
                build_rig_cache(os.path.join(args.rigs, rig_file), cache, args.ik)
            else:
                log(f"  复用缓存 {cache}")
        except Exception as exc:
            log(f"  ✗ 构建控制器 rig 失败：{type(exc).__name__}: {exc}")
            traceback.print_exc()
            fail += 1
            all_results.append({"model": name, "clip": None, "status": "failed",
                                "error": f"{type(exc).__name__}: {exc}"})
            continue

        # 每个角色的 setup：位移重投影系数 + 目标 GLB 节点集合
        # 注意 reference_leg() 会打开 Y Bot 缓存，必须在打开本角色缓存之前算好
        try:
            ref = reference_leg(args) if args.ratio_mode == "leg" else 1.7855
            bpy.ops.wm.open_mainfile(filepath=cache)
            ensure_addon()
            arm = next(o for o in bpy.context.scene.objects if o.type == "ARMATURE")
            arm_name = arm.name
            if args.ratio_mode == "none":
                target_len, ratio = 0.0, 1.0
            else:
                target_len = rest_leg_length(arm)
                ratio = target_len / ref if ref else 1.0
                if args.ratio_mode == "height":
                    model_h = max((arm.matrix_world @ b.head_local).z for b in arm.data.bones)
                    target_len, ratio = model_h, model_h / ref
            del arm
            expected = rig_node_names(rig_glb)
            if not expected:
                log(f"  提示：{rig_glb} 不可读，跳过节点名校验")
        except Exception as exc:
            log(f"  ✗ 读取 rig 参数失败：{type(exc).__name__}: {exc}")
            traceback.print_exc()
            fail += 1
            continue
        log(f"  基准={target_len:.3f}，位移重投影系数={ratio:.3f}")

        for n, anim_file in enumerate(anims, 1):
            clip = clip_name(anim_file)
            out_path = os.path.join(args.out, name, clip + ".glb")
            if os.path.exists(out_path) and not args.force:
                log(f"  [{n}/{len(anims)}] 跳过已存在 {name}/{clip}.glb")
                skip += 1
                all_results.append({"model": name, "clip": clip, "status": "skipped"})
                continue
            log(f"  [{n}/{len(anims)}] {clip}")
            try:
                info = retarget_one(cache, arm_name, os.path.join(args.anims, anim_file),
                                    clip, args, ratio, out_path, expected)
                info["model"] = name
                info["out"] = out_path
                all_results.append(info)
                if info["status"] == "ok":
                    ok += 1
                    log(f"    ✓ {name}/{clip}.glb  {info['size_kb']}KB  "
                        f"{info['channels']} 通道  {info['seconds']}s")
                else:
                    fail += 1
            except Exception as exc:
                fail += 1
                log(f"    ✗ {clip} 失败：{type(exc).__name__}: {exc}")
                traceback.print_exc()
                all_results.append({"model": name, "clip": clip,
                                    "status": "failed", "error": f"{type(exc).__name__}: {exc}"})

    if args.manifest:
        os.makedirs(args.out, exist_ok=True)
        part = f"manifest.part-{args.shard.split('/')[0] if args.shard else 0}.json"
        with open(os.path.join(args.out, part), "w", encoding="utf-8") as fh:
            json.dump(all_results, fh, ensure_ascii=False, indent=2)
        log(f"manifest 写入 {part}")

    log("=" * 60)
    log(f"完成：成功 {ok}，跳过 {skip}，失败 {fail}")
    return 0 if fail == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
