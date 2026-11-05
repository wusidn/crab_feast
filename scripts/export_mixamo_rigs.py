#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""批量把 Mixamo FBX 角色导出为 GLB（经由 Mixamo Rig 插件生成控制器）。

用法（必须通过 Blender 的无头模式运行，脚本本身不是普通 Python 脚本）：

    # 导出全部（注意：不要加 --factory-startup，否则用户的扩展仓库不可见）
    /Applications/Blender.app/Contents/MacOS/Blender -b \
        --python scripts/export_mixamo_rigs.py -- [参数]

    # 或者用包装脚本
    scripts/export_rigs.sh                 # 全部导出
    scripts/export_rigs.sh --only "X Bot"  # 只导出名字含 "X Bot" 的文件
    scripts/export_rigs.sh --force         # 覆盖已存在的 GLB
    scripts/export_rigs.sh -j 3 --force    # 3 个 Blender 进程并行

单个文件的处理流程：
    1. 清空场景 -> 导入 FBX
    2. 选中 Mixamo 骨架 -> bpy.ops.mr.make_rig（创建控制器并烘焙动画）
    3. 排除控制器的自定义形状控件（cs_grp）后导出 GLB

输出命名规则（通用）：截断第一个下划线及其后内容，
    Ch02_nonPBR.fbx -> Ch02.glb，Ch49_nonPBR.fbx -> Ch49.glb，
    Big Vegas.fbx / X Bot.fbx 这类没有下划线的名字原样保留。
    需要保留完整文件名时加 --keep-name。
"""

from __future__ import annotations

import argparse
import fnmatch
import os
import sys
import time
import traceback

import bpy

# Mixamo Rig 在 Blender 4.2+ 里是扩展（extension），模块名带 bl_ext 前缀；
# 老版本作为普通 addon 安装，模块名为 addon_utils 里的名字。
ADDON_CANDIDATES = (
    "bl_ext.blender_org.mixamo_rig",
    "mixamo_rig",
)

MAKE_RIG_OP = "mr.make_rig"
CS_ROOT = "cs_grp"  # 控制器自定义形状的父级空物体


def log(msg: str) -> None:
    print(f"[export_rigs] {msg}", flush=True)


def output_stem(fbx_name: str, strip_suffix: bool = True) -> str:
    """由 FBX 文件名推导 GLB 基名。

    默认按通用规则截断：只保留下划线之前的部分
    （Ch02_nonPBR.fbx -> Ch02，Ch49_nonPBR.fbx -> Ch49）；
    没有下划线的名字（Big Vegas、X Bot 等）原样保留。
    """
    stem = os.path.splitext(fbx_name)[0]
    if strip_suffix and "_" in stem:
        stem = stem.split("_", 1)[0]
    return stem


def matches(name: str, patterns) -> bool:
    """文件名匹配：支持通配符；不含通配符时按子串匹配（大小写不敏感）。"""
    stem, _ = os.path.splitext(name)
    low = name.lower()
    for pat in patterns:
        p = pat.lower()
        if any(ch in p for ch in "*?["):
            if fnmatch.fnmatch(low, p) or fnmatch.fnmatch(stem.lower(), p):
                return True
        elif p in low or p in stem.lower():
            return True
    return False


# --------------------------------------------------------------------------- #
# 参数
# --------------------------------------------------------------------------- #
def parse_args() -> argparse.Namespace:
    argv = sys.argv
    argv = argv[argv.index("--") + 1:] if "--" in argv else []

    here = os.path.dirname(os.path.abspath(__file__))
    repo = os.path.dirname(here)

    p = argparse.ArgumentParser(
        prog="export_mixamo_rigs.py",
        description="批量把 Mixamo FBX 角色导出为带控制器的 GLB",
    )
    p.add_argument("--src", default=os.path.join(repo, "assets/source/character/rigs"),
                   help="FBX 源目录（默认 assets/source/character/rigs）")
    p.add_argument("--out", default=os.path.join(repo, "assets/characters/rigs"),
                   help="GLB 输出目录（默认 assets/characters/rigs）")
    p.add_argument("--only", action="append", default=[],
                   help="只处理匹配的文件名（支持 * 通配，可重复）")
    p.add_argument("--skip", action="append", default=[],
                   help="跳过匹配的文件名（支持 * 通配，可重复）")
    p.add_argument("--force", action="store_true",
                   help="覆盖已存在的 GLB；默认跳过已存在文件")
    p.add_argument("--no-anim", action="store_true",
                   help="创建控制器时不烘焙动画（默认烘焙）")
    p.add_argument("--fk-arms", action="store_true", help="手臂用 FK（默认 IK）")
    p.add_argument("--fk-legs", action="store_true", help="腿用 FK（默认 IK）")
    p.add_argument("--no-exclude-shapes", action="store_true",
                   help="不排除控制器自定义形状控件（默认排除 cs_grp 子树）")
    p.add_argument("--keep-name", action="store_true",
                   help="保留 FBX 原始文件名（默认截断第一个下划线及其后内容，"
                        "如 Ch02_nonPBR -> Ch02）")
    p.add_argument("--compress", action="store_true",
                   help="压缩预设：贴图转 JPEG（quality=80）+ 贴图最长边限制 2048；"
                        "可用后面的细项覆盖")
    p.add_argument("--tex-quality", type=int, default=None, metavar="N",
                   help="贴图 JPEG 质量 1-100（默认 75；0 表示不做贴图压缩）")
    p.add_argument("--tex-format", choices=("auto", "jpeg"), default=None,
                   help="贴图格式：auto=保留原格式，jpeg=统一转 JPEG（默认 auto）")
    p.add_argument("--tex-max-size", type=int, default=None, metavar="PX",
                   help="贴图最长边上限，超过则等比缩小（0/不填=不缩放）")
    p.add_argument("--tex-alpha", choices=("keep", "jpeg"), default=None,
                   help="带透明通道的贴图如何处理：keep=保留 PNG（默认），"
                        "jpeg=也转 JPEG（会丢失透明通道）")
    p.add_argument("--tex-normal", choices=("keep", "jpeg"), default=None,
                   help="法线贴图如何处理：keep=保留 PNG（默认，画质优先），"
                        "jpeg=转 JPEG（体积更小，法线细节会有压缩痕迹）")
    p.add_argument("--decimate", type=float, default=None, metavar="R",
                   help="网格减面比例 0.05-1.0（1.0=不减面，0.5=保留约一半三角面）")
    p.add_argument("--draco", type=int, default=None, metavar="LEVEL",
                   help="Draco 网格压缩等级 0-10；注意 Bevy 0.20.1 glTF 不支持 "
                        "KHR_draco_mesh_compression，导出后引擎读不出")
    p.add_argument("--list", action="store_true", help="只列出待处理文件，不执行")
    p.add_argument("--shard", default=None, metavar="I/N",
                   help="分片：只处理第 I 份（1..N），供包装脚本并行调用")
    return p.parse_args(argv)


# --------------------------------------------------------------------------- #
# 环境准备
# --------------------------------------------------------------------------- #
def _op_ready() -> bool:
    """bpy.ops 的属性访问是惰性的，用注册的操作类判断插件是否真的可用。"""
    return hasattr(bpy.types, "MR_OT_make_rig")


def ensure_addon() -> str:
    """确保 Mixamo Rig 已注册，返回可用的模块名。"""
    import addon_utils

    if _op_ready():
        return "already-enabled"

    for mod in ADDON_CANDIDATES:
        if mod not in sys.modules:
            try:
                __import__(mod)  # 已启用的扩展，import 即完成注册
            except Exception:
                pass
        if _op_ready():
            log(f"插件已就绪：{mod}")
            return mod

        addon_utils.enable(mod, default_set=True, handle_error=None)
        if _op_ready():
            log(f"已启用插件：{mod}")
            return mod

    raise RuntimeError(
        "未找到 Mixamo Rig 插件（MR_OT_make_rig 未注册）。请在 Blender 中安装并启用：\n"
        "  Preferences > Extensions > Get Extensions > 搜索 Mixamo Rig\n"
        "  https://github.com/tdw46/mixamo_blender4-main\n"
        "另外不要使用 --factory-startup 启动，否则用户的扩展仓库不可见。"
    )


def reset_scene() -> None:
    """清空场景但保留插件状态。

    注意：不能用 bpy.ops.wm.read_factory_settings(use_empty=True)，
    它会重置偏好设置并把 Mixamo Rig 扩展卸载，导致 bpy.ops.mr 消失。
    """
    obj = bpy.context.object
    if obj is not None and obj.mode != "OBJECT":
        bpy.ops.object.mode_set(mode="OBJECT")

    for o in list(bpy.data.objects):
        bpy.data.objects.remove(o, do_unlink=True)

    for coll in (
        bpy.data.meshes,
        bpy.data.armatures,
        bpy.data.actions,
        bpy.data.materials,
        bpy.data.images,
        bpy.data.collections,
        bpy.data.node_groups,
        bpy.data.cameras,
        bpy.data.lights,
    ):
        for datablock in list(coll):
            if datablock.users == 0:
                coll.remove(datablock)

    bpy.context.scene.frame_set(1)


def deselect_all() -> None:
    view_layer = bpy.context.view_layer
    for obj in view_layer.objects:
        obj.select_set(False)


# --------------------------------------------------------------------------- #
# 压缩：贴图 / 网格
#
# 只做 Bevy 0.20.1 能真正加载的压缩：
#   - 贴图 -> JPEG（glTF 核心格式，bevy_gltf 原生支持）
#   - 贴图降分辨率、网格减面（不依赖任何 glTF 扩展）
# Draco / WebP / KHR_mesh_quantization 在 bevy_gltf 0.20.1 的支持表里都是 ❌，
# 用它们导出的文件引擎读不出来，所以 --draco 只做显式告警而不偷偷启用。
# --------------------------------------------------------------------------- #
def resolve_compress_args(args) -> None:
    """处理 --compress 预设与默认值、参数范围校验。"""
    if args.compress:
        if args.tex_quality is None:
            args.tex_quality = 80
        if args.tex_max_size is None:
            args.tex_max_size = 2048
        if args.tex_format is None:
            args.tex_format = "jpeg"

    if args.tex_quality is None:
        args.tex_quality = 0
    if args.tex_max_size is None:
        args.tex_max_size = 0
    if args.tex_format is None:
        args.tex_format = "auto"
    if args.tex_alpha is None:
        args.tex_alpha = "keep"
    if args.tex_normal is None:
        args.tex_normal = "keep"
    if args.decimate is None:
        args.decimate = 1.0

    if not 0 <= args.tex_quality <= 100:
        raise ValueError("--tex-quality 需要在 0-100 之间")
    if args.tex_max_size < 0:
        raise ValueError("--tex-max-size 不能为负")
    if not 0.0 < args.decimate <= 1.0:
        raise ValueError("--decimate 需要在 (0, 1] 之间")


def collect_model_images() -> list:
    """收集当前场景材质里实际用到的贴图。"""
    images = []
    for mat in bpy.data.materials:
        if not mat.use_nodes or mat.node_tree is None:
            continue
        for node in mat.node_tree.nodes:
            if node.type == "TEX_IMAGE" and node.image is not None:
                if node.image not in images:
                    images.append(node.image)
    return images


def source_format(img) -> str:
    fmt = (img.file_format or "").upper()
    if fmt in {"JPEG", "JPG"}:
        return "JPEG"
    if img.filepath.lower().endswith((".jpg", ".jpeg")):
        return "JPEG"
    return "PNG"


def image_uses_alpha(img) -> bool:
    """判断贴图是否被用作带透明的遮罩（睫毛、发片、衣角等）。"""
    for mat in bpy.data.materials:
        if not mat.use_nodes or mat.node_tree is None:
            continue
        for node in mat.node_tree.nodes:
            if node.type == "TEX_IMAGE" and node.image is img:
                for out in node.outputs:
                    if out.name == "Alpha" and out.is_linked:
                        return True

    # 没有 Alpha 连线但像素本身带透明（例如整体 alpha 混合的材质）
    try:
        import numpy as np

        if img.channels < 4:
            return False
        if len(img.pixels) == 0:
            img.reload()
        buf = np.empty(len(img.pixels), dtype=np.float32)
        img.pixels.foreach_get(buf)
        if buf.size:
            return bool(float(buf[3::4].min()) < 0.95)
    except Exception:
        pass
    return False


def image_is_normal_map(img) -> bool:
    """贴图是否接到 Normal Map 节点的颜色输入（法线图不适合低质量 JPEG）。"""
    for mat in bpy.data.materials:
        if not mat.use_nodes or mat.node_tree is None:
            continue
        for node in mat.node_tree.nodes:
            if node.type != "TEX_IMAGE" or node.image is not img:
                continue
            for out in node.outputs:
                for link in out.links:
                    if link.to_node.type == "NORMAL_MAP":
                        return True
    return False


def pack_kb(img) -> float:
    if img.packed_file is not None:
        return img.packed_file.size / 1024
    try:
        return os.path.getsize(bpy.path.abspath(img.filepath)) / 1024
    except OSError:
        return 0.0


def optimize_textures(args) -> dict:
    """按参数重写贴图文件（转格式 / 降分辨率），并让材质指向新文件。"""
    stats = {"total": 0, "resized": 0, "converted": 0,
             "kb_before": 0.0, "kb_after": 0.0, "alpha_kept": 0}
    if args.tex_quality <= 0 and args.tex_max_size <= 0:
        return stats

    import tempfile

    tmpdir = tempfile.mkdtemp(prefix="rigtex_")
    used_names = {}

    for img in collect_model_images():
        width, height = img.size
        if width <= 0 or height <= 0:
            continue

        stats["total"] += 1
        stats["kb_before"] += pack_kb(img)

        src_fmt = source_format(img)

        keep_alpha = args.tex_alpha == "keep" and image_uses_alpha(img)
        is_normal = image_is_normal_map(img)

        # 默认「保留」= 只缩分辨率、不动格式，避免把本来就小的 JPEG 转成 PNG 反而变大
        if (keep_alpha or (is_normal and args.tex_normal == "keep")):
            target = src_fmt
            if keep_alpha:
                stats["alpha_kept"] += 1
        elif args.tex_format == "jpeg" or is_normal:
            target = "JPEG"
        else:
            target = src_fmt

        resized = False
        new_w, new_h = width, height
        if args.tex_max_size > 0 and max(width, height) > args.tex_max_size:
            ratio = args.tex_max_size / max(width, height)
            new_w, new_h = max(1, round(width * ratio)), max(1, round(height * ratio))
            img.scale(new_w, new_h)
            resized = True
            stats["resized"] += 1

        if not resized and target == src_fmt:
            stats["kb_after"] += pack_kb(img)
            continue

        # 输出文件名贴近原名，避免 GLB 里出现 tex_0 / tex_1 这类名字
        base = bpy.path.clean_name(os.path.splitext(img.name)[0]) or "texture"
        used_names[base] = used_names.get(base, 0) + 1
        if used_names[base] > 1:
            base = f"{base}_{used_names[base]}"
        ext = ".jpg" if target == "JPEG" else ".png"
        path = os.path.join(tmpdir, base + ext)

        quality = args.tex_quality if target == "JPEG" else 90
        if is_normal and target == "JPEG":
            quality = max(quality, min(95, args.tex_quality + 10))

        img.file_format = target
        img.save(filepath=path, quality=quality, save_copy=True)

        # 关键：先解除打包，否则 reload 会把原始 4K 打包数据读回来，缩放等于白做
        img.source = "FILE"
        img.filepath = path
        img.filepath_raw = path
        try:
            img.unpack(method="REMOVE")
        except Exception:
            pass
        img.reload()

        if target != src_fmt:
            stats["converted"] += 1
        stats["kb_after"] += pack_kb(img)
        log(f"贴图 {img.name}: {width}x{height} {src_fmt} -> "
            f"{img.size[0]}x{img.size[1]} {target}")

    return stats


def tri_count(mesh) -> int:
    return sum(len(p.vertices) - 2 for p in mesh.polygons)


def decimate_meshes(ratio: float) -> list:
    """按比例减面（保留顶点组/权重，形态键网格跳过）。"""
    if ratio >= 0.999:
        return []

    results = []
    for obj in [o for o in bpy.context.scene.objects if o.type == "MESH"]:
        if obj.data.shape_keys is not None:
            log(f"跳过减面（含形态键）：{obj.name}")
            continue

        before = tri_count(obj.data)
        deselect_all()
        obj.select_set(True)
        bpy.context.view_layer.objects.active = obj
        if bpy.context.object.mode != "OBJECT":
            bpy.ops.object.mode_set(mode="OBJECT")

        mod = obj.modifiers.new("DecimateExport", "DECIMATE")
        mod.decimate_type = "COLLAPSE"
        mod.ratio = ratio
        mod.use_collapse_triangulate = True
        bpy.ops.object.modifier_apply(modifier=mod.name)

        after = tri_count(obj.data)
        results.append((obj.name, before, after))
    return results


# --------------------------------------------------------------------------- #
# 单个文件
# --------------------------------------------------------------------------- #
def find_armature():
    arms = [o for o in bpy.context.scene.objects if o.type == "ARMATURE"]
    if not arms:
        raise RuntimeError("FBX 里没有骨架对象")

    # 优先选带 Hips 的 Mixamo 骨架
    def score(o) -> int:
        return sum(1 for b in o.data.bones if "Hips" in b.name)

    arms.sort(key=score, reverse=True)
    best = arms[0]
    if score(best) == 0:
        raise RuntimeError(f"骨架 {best.name} 不含 Mixamo 的 Hips，可能不是 Mixamo 模型")
    if len(arms) > 1:
        log(f"发现 {len(arms)} 个骨架，使用 {best.name}")
    return best


def import_fbx(path: str):
    bpy.ops.import_scene.fbx(filepath=path, use_anim=True)
    return find_armature()


def make_control_rig(arm, args) -> None:
    """创建控制器；返回原始 FBX 的动作名，便于导出时保留动画命名。"""
    orig_actions = [a.name for a in bpy.data.actions]

    deselect_all()
    arm.select_set(True)
    bpy.context.view_layer.objects.active = arm

    if bpy.context.object.mode != "OBJECT":
        bpy.ops.object.mode_set(mode="OBJECT")

    if "mr_control_rig" in arm.data.keys():
        log(f"{arm.name} 已带控制器，跳过创建")
        return None

    result = bpy.ops.mr.make_rig(
        bake_anim=not args.no_anim,
        ik_arms=not args.fk_arms,
        ik_legs=not args.fk_legs,
    )
    if "FINISHED" not in result:
        raise RuntimeError(f"创建控制器失败：{result}")
    return orig_actions


def restore_action_names(orig_actions, arm) -> None:
    """把烘焙出来的新动作改回 FBX 里的原始动作名，保持 glTF 动画命名可读。"""
    if not orig_actions:
        return
    ad = arm.animation_data
    new_actions = [a for a in bpy.data.actions if a.name not in orig_actions]
    if not new_actions:
        return
    action = (ad.action if ad and ad.action in new_actions else new_actions[0])
    base = orig_actions[0]
    action.name = base
    log(f"动画重命名为 {base}")


def export_glb(out_path: str, exclude_shapes: bool, args) -> None:
    deselect_all()

    skip = set()
    if exclude_shapes:
        cs = bpy.data.objects.get(CS_ROOT)
        if cs is not None:
            skip.add(cs.name)
            for o in bpy.context.scene.objects:
                parent = o.parent
                while parent is not None:
                    if parent is cs:
                        skip.add(o.name)
                        break
                    parent = parent.parent

    exported = []
    for o in bpy.context.scene.objects:
        if o.name in skip:
            o.hide_set(True)
            continue
        o.select_set(True)
        exported.append(o.name)

    if bpy.context.view_layer.objects.active not in bpy.context.selected_objects:
        armature = next(o for o in bpy.context.scene.objects if o.type == "ARMATURE")
        bpy.context.view_layer.objects.active = armature

    export_kwargs = dict(
        filepath=out_path,
        export_format="GLB",
        use_selection=True,
        export_apply=False,
        export_animations=True,
        export_animation_mode="ACTIONS",
        export_skins=True,
        export_def_bones=False,      # 保留控制器骨骼，不只导出变形骨
        export_yup=True,
        export_morph=True,
        export_materials="EXPORT",
        export_image_format="AUTO",  # 按贴图自身格式走（已在上游压好）
        export_jpeg_quality=max(1, min(100, args.tex_quality or 75)),
    )

    if args.draco:
        log("警告：Draco（KHR_draco_mesh_compression）在 bevy_gltf 0.20.1 的支持表里标记为不支持，"
            "导出后 Bevy 无法加载，仅在你确认引擎侧另有处理时使用")
        export_kwargs.update(
            export_draco_mesh_compression_enable=True,
            export_draco_mesh_compression_level=max(0, min(10, args.draco)),
        )

    bpy.ops.export_scene.gltf(**export_kwargs)
    log(f"已写出 {out_path}（对象 {len(exported)} 个，排除 {len(skip)} 个形状控件）")


def export_one(src_path: str, out_path: str, args) -> dict:
    info = {"src": src_path, "out": out_path, "status": "ok", "seconds": 0.0}
    t0 = time.time()
    reset_scene()
    try:
        arm = import_fbx(src_path)

        decimated = decimate_meshes(args.decimate)
        if decimated:
            before = sum(d[1] for d in decimated)
            after = sum(d[2] for d in decimated)
            info["faces"] = f"{before}->{after}"
            log(f"网格减面 ratio={args.decimate}: {before} -> {after} 三角面")

        orig_actions = make_control_rig(arm, args)
        restore_action_names(orig_actions, arm)

        tex = optimize_textures(args)
        if tex["total"]:
            log(f"贴图处理：{tex['total']} 张，转格式 {tex['converted']}，缩小 {tex['resized']}，"
                f"保留透明 {tex['alpha_kept']}，"
                f"{tex['kb_before']/1024:.1f}MB -> {tex['kb_after']/1024:.1f}MB")

        os.makedirs(os.path.dirname(out_path), exist_ok=True)
        export_glb(out_path, exclude_shapes=not args.no_exclude_shapes, args=args)
        size = os.path.getsize(out_path) / 1e6
        info["size_mb"] = round(size, 1)
        info["action_count"] = len(bpy.data.actions)
    except Exception as exc:  # 单个文件失败不影响整批
        info["status"] = "failed"
        info["error"] = f"{type(exc).__name__}: {exc}"
        log(f"失败 {os.path.basename(src_path)} -> {info['error']}")
        traceback.print_exc()
    finally:
        info["seconds"] = round(time.time() - t0, 1)
    return info


# --------------------------------------------------------------------------- #
# 主流程
# --------------------------------------------------------------------------- #
def main() -> int:
    args = parse_args()

    src_dir = os.path.abspath(args.src)
    out_dir = os.path.abspath(args.out)

    if not os.path.isdir(src_dir):
        log(f"源目录不存在：{src_dir}")
        return 2

    files = sorted(
        f for f in os.listdir(src_dir)
        if f.lower().endswith((".fbx", ".fbx7")) and not f.startswith(".")
    )
    if args.only:
        files = [f for f in files if matches(f, args.only)]
    if args.skip:
        files = [f for f in files if not matches(f, args.skip)]

    if args.shard:
        try:
            idx_str, total_str = args.shard.split("/")
            idx, total = int(idx_str), int(total_str)
            if not (1 <= idx <= total):
                raise ValueError
        except ValueError:
            log(f"--shard 参数非法：{args.shard}（应为 I/N，如 2/3）")
            return 2
        files = [f for n, f in enumerate(files) if n % total == idx - 1]
        log(f"分片 {idx}/{total}：本次处理 {len(files)} 个文件")

    if not files:
        log("没有需要处理的 FBX 文件")
        return 0 if args.shard else 1

    if args.list:
        for f in files:
            print(f)
        return 0

    try:
        ensure_addon()
        resolve_compress_args(args)
    except Exception as exc:
        log(str(exc))
        return 2

    log(f"源目录 {src_dir}")
    log(f"输出目录 {out_dir}")
    log(f"待处理 {len(files)} 个文件")
    if args.tex_quality > 0 or args.tex_max_size > 0 or args.decimate < 1.0:
        log(f"压缩参数：tex-format={args.tex_format} tex-quality={args.tex_quality} "
            f"tex-max-size={args.tex_max_size or '不限'} tex-alpha={args.tex_alpha} "
            f"tex-normal={args.tex_normal} decimate={args.decimate} draco={args.draco or 0}")

    results = []
    for i, name in enumerate(files, 1):
        src_path = os.path.join(src_dir, name)
        out_path = os.path.join(out_dir, output_stem(name, not args.keep_name) + ".glb")

        if os.path.exists(out_path) and not args.force:
            log(f"[{i}/{len(files)}] 跳过已存在 {os.path.basename(out_path)}（--force 可覆盖）")
            results.append({"src": src_path, "out": out_path,
                            "status": "skipped", "seconds": 0.0})
            continue

        log(f"[{i}/{len(files)}] 处理 {name}")
        results.append(export_one(src_path, out_path, args))

    # 汇总
    ok = [r for r in results if r["status"] == "ok"]
    skipped = [r for r in results if r["status"] == "skipped"]
    failed = [r for r in results if r["status"] == "failed"]

    log("=" * 60)
    log(f"完成：成功 {len(ok)}，跳过 {len(skipped)}，失败 {len(failed)}")
    for r in ok:
        extra = f"  faces={r['faces']}" if "faces" in r else ""
        log(f"  ✓ {os.path.basename(r['out'])}  {r.get('size_mb', '?')} MB  "
            f"{r['seconds']}s  actions={r.get('action_count', 0)}{extra}")
    for r in failed:
        log(f"  ✗ {os.path.basename(r['src'])}  {r.get('error')}")

    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
