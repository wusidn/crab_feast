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
  3. rest 重定基：把源骨架的静止姿态换成目标角色自己的静止姿态（同名骨骼），
     保留源动画的局部旋转 → 世界空间 Copy Rotation 变成「增量重定向」。
     不做这步，角色的锁骨/脊柱/脚会被硬套成 Y Bot 的绝对朝向（耸肩、肩变宽）。
     三个例外：
       a) Hips 不换 rest —— 否则根位移通道换坐标系，整段下沉约 0.2m；
       b) 手臂链（Arm/ForeArm/Hand/手指）不换 rest —— 否则 A-pose 的张开角会被
          叠成「下垂 80° + 前甩」，双臂摆到身前；
       c) 手臂链仍要参与共轭：它自己虽没换 rest，但父骨换了，它在**新父链下**的
          世界 rest 已经变了（按层级递归推算 W_new），不补偿就会凭空外展 13°。
     另外锁骨增量按 --shoulder-increment 衰减（默认 0），锁骨的旋转会带着上臂转，
     所以还要把锁骨实际生效的增量从上臂上抵消掉 —— 于是肩宽等于绑定宽度、手臂
     保持源的下垂角，「肩宽」和「手臂自然下垂」不再互相冲突。
  4. bpy.ops.mr.import_anim_to_rig()（Mixamo Rig 的 Apply Animation to Control Rig）
  5. 地面校正：按脚部顶点把整段动画的垂直偏移归零
  6. 只选中骨架导出 GLB（不含网格），保证动画包只有几百 KB

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
import math
import os
import re
import struct
import sys
import time
import traceback

import bpy
from mathutils import Matrix, Vector, Quaternion

# 锁骨增量保留系数：0=只跟目标 rest（肩宽回到绑定宽度），1=完全传递源增量。
# 手臂已经解耦（见 apply_conjugate），这里只影响肩宽，所以默认 0：肩宽等于绑定宽度。
SHOULDER_INCREMENT = 0.0


_CUR_MODEL = ""


def _load_abduct_override():
    """读取脚本自动求解出的外展角（scripts/solve_arm_abduct.py 写）。

    存在就覆盖手写表；表退化为「求解用的初值」。这样「肩宽自然垂落 → 太近就调」
    的逻辑跑在脚本里，人不用再逐个角色试。
    """
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "arm_abduct.json")
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
        return {str(k): float(v) for k, v in data.items()}
    except (OSError, ValueError):
        return {}


ABDUCT_OVERRIDE = _load_abduct_override()


def _load_leg_mode_override():
    """读取脚本求解出的腿重定基方式（scripts/solve_leg_mode.py 写）。

    rebase = 腿也按目标 rest 重定基（保留角色自己的站姿）；skip = 腿保持源动画的
    绝对朝向（站姿收窄）。每个角色选哪个由踝间距−髋间距的实测值决定。
    """
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "leg_mode.json")
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
        return {str(k): str(v.get("mode", "rebase")) for k, v in data.items()}
    except (OSError, ValueError, AttributeError):
        return {}


LEG_MODE_OVERRIDE = _load_leg_mode_override()


def leg_mode(name: str = None) -> str:
    """腿的处理方式：环境变量 CRAB_LEG_MODE > leg_mode.json > rebase。"""
    env = os.environ.get("CRAB_LEG_MODE")
    if env in ("rebase", "skip"):
        return env
    if os.environ.get("CRAB_SKIP_LEGS") == "1":
        return "skip"
    return LEG_MODE_OVERRIDE.get(name or _CUR_MODEL, "rebase")


def arm_abduct(name: str = None) -> float:
    """额外外展角（度）。

    优先级：环境变量 CRAB_ARM_ABDUCT（探针/调试用） > arm_abduct.json（自动求解结果）
    > ARM_ABDUCT_TABLE（手写初值） > 0。

    手写表只是初值，最终角度由 scripts/solve_arm_abduct.py 用「肘腕离身体轮廓的余量
    ≥ 2cm」这个口径逐角色求解后写进 json；探测脚本证实这个余量随角度单调，
    而「最近顶点距离」「顶点/BVH 面法线穿透」都是伪判据（详见 scripts/arm_gap.py）。
    """
    key = name if name is not None else _CUR_MODEL
    if key in ABDUCT_OVERRIDE:
        return ABDUCT_OVERRIDE[key]
    return ARM_ABDUCT_TABLE.get(key, 0.0)


# 额外外展角（度）。空表示 0：源动画的 8° 下垂对这些体型刚好贴着体侧，不需要额外推。
# 只有躯干/甲壳比源厚到「上臂会插进去」的角色才给正值。
ARM_ABDUCT_TABLE = {
    "Castle Guard 02": 10.0,       # 重甲：护肩外凸 + 胸甲/腰甲厚，7° 仍有压模
    "Ortiz": 6.0,                  # 肩宽 0.70，胸廓厚
    "Pumpkinhulk L Shaw": 6.0,     # 体型壮硕 + 护肩
    "Big Vegas": 12.0,             # 极胖：躯干半宽 0.41，上臂会整个埋进腹部轮廓
    # 窄肩组：肩半宽 ≤ 0.185m，袖子会整片贴在躯干上，手臂与身体没有可见空隙
    "Megan": 6.0,
    "Amy": 4.0,
    "Swat": 6.0,
    "Paladin WProp J Nordstrom": 4.0,
}


def hand_twist(name: str = None) -> float:
    """持物手绕自身长轴（局部 Y）的补偿角（度），默认 0。

    只给「剑/盾横在身前、剑尖压在腿上」的角色用。肩关节外展只把整条手臂往外推，
    剑仍跟着手走（实测 Paladin 从 4° 加到 16°，剑与腿的距离一点没变）；拧腕才是
    让剑身绕手臂轴扫开的那个自由度。
    """
    key = name if name is not None else _CUR_MODEL
    return HAND_TWIST_TABLE.get(key, 0.0)


# 正值 = 剑身向内收（剑尖垂到体侧下方）。实测穿透深度：0° 0.83cm/14 点、
# −20° 2.96cm/48 点（越转越糟，剑插进腹部）、+10° 0.19cm/2 点、+20° 0/0。
# 注意不能靠正面渲染判断：−25° 那张图看着剑「离开了大腿」，其实是捅进肚子里。
HAND_TWIST_TABLE = {
    "Paladin WProp J Nordstrom": 20.0,
}


def apply_prop_joint_pose(arm) -> int:
    """把剑/盾等道具关节拧开，减少穿过躯干。

    道具挂在 *_joint 骨上（Paladin 是 Sword_joint ← RightHand、Shield_joint ← LeftForeArm），
    这两根骨在源动画骨架里不存在，所以对源曲线做任何补偿都无效；但它们在目标骨架里
    没有约束，且导出用 export_force_sampling=True（采样最终姿态），因此在导出前直接
    设置 pose 就能带进 GLB。

    角度是实测出来的（穿透深度口径见 scripts/prop_gap.py）：剑绕 Z −30°、盾绕 Z +30°，
    walking 全零、idle 0.8cm、jump 峰值 1.2cm；再加大盾会反向穿进胸甲（+40° 到 3.1cm）。
    """
    swings = PROP_JOINT_SWING.get(_CUR_MODEL)
    if not swings:
        return 0
    axes = {"X": Vector((1.0, 0.0, 0.0)), "Y": Vector((0.0, 1.0, 0.0)),
            "Z": Vector((0.0, 0.0, 1.0))}
    applied = 0
    for pb in arm.pose.bones:
        spec = swings.get(pb.name.split(":")[-1])
        if not spec:
            continue
        axis, deg = spec
        pb.rotation_mode = "QUATERNION"
        pb.rotation_quaternion = (pb.rotation_quaternion @
                                  Quaternion(axes[axis], math.radians(deg)))
        applied += 1
    if applied:
        log("  道具关节姿态 %d 根：%s" % (applied, swings))
    return applied


# 道具关节的固定姿态（轴, 角度）。只给道具确实穿过身体的角色写。
#
# Paladin 实测过：Sword_joint 绕 Z −30° 能把剑的穿透从 1.89cm/1 点压到 0，
# Shield_joint 绕 Z +30° 能把盾从 0.62cm/4 点压到 0 —— 但剑会从「垂在体侧」
# 被拧成「横指身后」，绕 X 轴则会让盾转成正面挡在身前，两个都不自然。
# 权衡后不启用：握姿自然 + 残余 ≤1.9cm（116 个剑顶点里 1 个擦过裙甲，
# 肉眼不可见），比「零穿透但剑横飘」更可信。需要压到零时把下面两行放开重导即可。
PROP_JOINT_SWING = {
    # "Paladin WProp J Nordstrom": {"Sword_joint": ("Z", -30.0),
    #                               "Shield_joint": ("Z", 30.0)},
}


def ortho3(m):
    """去掉缩放后的纯旋转矩阵。

    源骨架是按腿长比值缩放的，`bone.matrix.to_3x3()` 里带着缩放分量，直接
    `to_quaternion()` 会算出偏离的旋转 —— 手里的换算会整体偏掉，所以统一先
    按列归一化再做四元数运算。
    """
    cols = [m.col[c].to_3d().normalized() for c in range(3)]
    import mathutils
    o = mathutils.Matrix(((cols[0].x, cols[1].x, cols[2].x),
                          (cols[0].y, cols[1].y, cols[2].y),
                          (cols[0].z, cols[1].z, cols[2].z)))
    # 右手系兜底：第三列被反射时把第一列取反
    if o.determinant() < 0:
        o[0][0], o[1][0], o[2][0] = -o[0][0], -o[1][0], -o[2][0]
    return o

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
    p.add_argument("--no-rebase-rest", action="store_true",
                   help="关闭「把源骨架 rest 换成目标 rest」的增量重定向（默认开）。"
                        "关掉以后世界空间 Copy Rotation 会把角色骨骼硬套成 Y Bot 的绝对朝向，"
                        "锁骨坡度大的角色会出现耸肩/肩变宽")
    p.add_argument("--shoulder-increment", type=float, default=0.0, metavar="K",
                   help="锁骨保留多少源动画增量（0=锁骨只跟目标 rest，肩宽等于绑定宽度；"
                        "1=完全传递源增量，陡锁骨角色肩宽会窄 3~7cm）。按锁骨朝向偏差在"
                        "0 与 1 之间插值，偏差小于 15° 的角色（Y Bot/X Bot）不受影响。"
                        "手臂的下垂角已由上臂补偿独立保证，不受此值影响。默认 0")
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
def rebase_source_rest(src, tar) -> dict:
    """把源骨架的静止姿态换成目标角色自己的静止姿态（同名骨骼一一对应）。

    为什么必须换
    ------------
    `mr.import_anim_to_rig` 给每根 `Ctrl_*` 挂的是 **世界空间** 的 Copy Rotation，
    它把目标骨骼的最终世界朝向直接钉成源骨骼的世界朝向。插件自己其实想补偿这一点
    （`redefine_source_rest_pose` 想把源的 rest 改成目标的 rest），但那个函数是按
    **目标骨骼名**（`Ctrl_*`）去查源骨架的 edit bone，而源骨架叫 `mixamorig:*`，
    查不到就静默跳过 —— 补偿从未生效，于是目标角色的骨骼被硬套成 Y Bot 的绝对朝向。

    对锁骨最致命：Mixamo 各角色的锁骨静止朝向差得很远（Y Bot 基本水平，Castle Guard
    向下 33°），硬套过去等于把目标角色的锁骨从自己的静止角拉到 Y Bot 的角度，肩线
    整体抬高 20° 以上 —— 观感就是「耸肩 + 肩膀变宽」，而且越是宽肩/重甲的模型越明显。

    做法
    ----
    在导入动画后、插件重定向之前，把源骨架每根同名骨骼的 rest（head/tail/roll）改成
    目标角色对应变形骨的 rest，**保留源动画的 local rotation 关键帧**。这样源的姿态
    变成「目标静止朝向 × 源动画相对静止的增量」，世界空间 Copy Rotation 传过去就
    正好是增量重定向；角色自己的锁骨坡度、脊柱曲度、脚背角度都不会被抹掉。

    两类骨必须排除，否则副作用比收益大
    ----------------------------------
    * **Hips**：它的 local translation 就是整段动画的根位移通道，换 rest 等于给这条
      通道换坐标系 —— 基准高度和位移方向一起走样（实测 Amy 每条剪辑整段下沉 0.20m，
      再被贴地校正整体抬回去，等于角色浮在地面上）。
    * **手臂链（Arm / ForeArm / Hand 及手指）**：这里的 rest 差异只是绑定时手臂张开
      角（T-pose vs A-pose）的不同，不是角色结构差异；手臂在 Idle 里要从静止姿态往下
      摆 80°，是非线性大旋转，rest 的任何扭转差都会被这套旋转放大成「一边下垂一边前
      甩」（实测 Megan 手从髋侧移到身前 10.8cm、外展角从 9° 掉到 5°，看着像抱在身前）。
      手臂保持绝对朝向（等于旧行为），锁骨/躯干/腿脚仍走增量重定基。

    roll 只在「最小旋转」下传递
    --------------------------
    换完轴向不要直接把 roll 对齐目标骨的 Z 轴：那会把 rest 的扭转差一起算进姿态增量。
    这里只把源骨原来的 Z 轴随「旧轴向 → 新轴向」的最小旋转转过去，扭转量保持不变。
    """
    def base(name: str) -> str:
        return name.split(":")[-1]

    # 例外一：根骨（根位移通道）、例外二：手臂链（见 docstring 里的说明）
    skip = {"Hips", "LeftArm", "RightArm", "LeftForeArm", "RightForeArm"}
    if leg_mode() == "skip":
        skip |= {"LeftUpLeg", "RightUpLeg", "LeftLeg", "RightLeg",
                 "LeftFoot", "RightFoot", "LeftToeBase", "RightToeBase"}

    tm = tar.matrix_world
    tm3 = tm.to_3x3()
    target = {}
    every = set()    # 全部 mixamorig 骨（含 Hips / 手臂链），只用来算共轭 */

    for b in tar.data.bones:
        # 只取变形骨（Mixamo 前缀那套）；Ctrl_* / cs_* 控制器骨的 rest 不是网格的绑定姿态
        if not b.name.startswith("mixamorig"):
            continue
        every.add(base(b.name))
        if base(b.name) in skip or "Hand" in b.name:
            continue
        target[base(b.name)] = (
            tm @ b.head_local,
            tm @ b.tail_local,
            (tm3 @ (b.matrix_local.to_3x3() @ Vector((0.0, 0.0, 1.0)))).normalized(),
        )
    if not target:
        return {"rebased": 0, "note": "目标骨架没有 mixamorig:* 变形骨，跳过 rest 重定基"}

    inv = src.matrix_world.inverted()
    inv3 = inv.to_3x3()

    bpy.ops.object.mode_set(mode="EDIT")

    # 定向偏差只做统计，方便看出哪个角色的哪根骨骼本来会被拧得多狠
    report = []
    for eb in src.data.edit_bones:
        ent = target.get(base(eb.name))
        if ent is None:
            continue
        want_dir = (inv @ ent[1] - inv @ ent[0]).normalized()
        own_dir = (eb.tail - eb.head).normalized()
        dot = max(-1.0, min(1.0, own_dir.dot(want_dir)))
        report.append((math.degrees(math.acos(dot)), eb.name))

    for eb in src.data.edit_bones:
        eb.use_connect = False

    # 先把所有骨的原始矩阵快照下来：改一根骨的 head/tail 会连带改变子骨在
    # armature 空间的矩阵，边改边读会拿到「已经被改过一半」的坐标系
    snap = {}
    r_before = {}
    for eb in src.data.edit_bones:
        # 手臂链不改 rest，但它的 rest 世界朝向要留着做「锁骨增量补偿」的换算
        if base(eb.name) in target or base(eb.name) in ("LeftArm", "RightArm"):
            r_before[eb.name] = ortho3(eb.matrix.to_3x3())
            snap[eb.name] = (ortho3(eb.matrix.to_3x3()),
                             (eb.tail - eb.head).normalized())

    # 第一遍：只换轴向（head/tail ← 目标角色的静止位姿）
    for eb in src.data.edit_bones:
        ent = target.get(base(eb.name))
        if ent is None:
            continue
        eb.head = inv @ ent[0]
        eb.tail = inv @ ent[1]

    # 第二遍：定 roll —— 只把旧的 Z 轴随最小旋转转到新轴向，扭转量保持源骨自己的
    for eb in src.data.edit_bones:
        ent = target.get(base(eb.name))
        if ent is None:
            continue
        old_R, old_dir = snap[eb.name]
        new_dir = (eb.tail - eb.head).normalized()
        mode = os.environ.get("CRAB_ROLL", "min")
        if mode == "align":
            eb.align_roll(inv3 @ ent[2])
            continue
        if mode == "keep":
            eb.align_roll(old_R @ Vector((0.0, 0.0, 1.0)))
            continue
        axis = old_dir.cross(new_dir)
        if axis.length < 1e-6:
            new_z = old_R @ Vector((0.0, 0.0, 1.0))
        else:
            new_z = Matrix.Rotation(old_dir.angle(new_dir), 3, axis.normalized()) \
                    @ (old_R @ Vector((0.0, 0.0, 1.0)))
        eb.align_roll(new_z if new_z.length > 1e-6 else (inv3 @ ent[2]))

    # 改后的 rest 旋转（armature 空间），用于把动画的局部增量换算到新 rest 坐标系
    r_after = {eb.name: ortho3(eb.matrix.to_3x3())
               for eb in src.data.edit_bones if base(eb.name) in target}

    bpy.ops.object.mode_set(mode="OBJECT")

    # 共轭：源动画的局部增量 q 是相对「源原有 rest 坐标系」表达的；rest 换成目标的以后，
    # 同一组 q 会在目标骨的局部轴上重新解释，等于把一个世界旋转按错了坐标系搬运。
    # 换算 q_new = M q M⁻¹，M = R_new⁻¹ R_old（同一根骨新旧 rest 旋转之差）。
    # 少了这层，锁骨那种 21° 的增量在陡锁骨上会被读成「内收 + 下沉」：肩点距、肩高
    # 一起掉（实测 Megan 窄 3.4cm、Castle Guard 窄 6.5cm）。
    conj = {}
    dev = {n: a for a, n in report}
    for name, R0 in r_before.items():
        R1 = r_after.get(name)
        if R1 is None:
            continue
        Mq = (R1.inverted() @ R0).to_quaternion()
        if abs(Mq.angle) < math.radians(0.3):
            continue
        conj[name] = (Mq, dev.get(name, 0.0))

    def _half_shoulder(obj):
        # 注意单位：源 FBX 以厘米导入（armature 的 matrix_world 带 0.01 缩放），
        # head_local 直接用会差 100 倍，必须折算到世界空间再比。
        L = next((b for b in obj.data.bones if base(b.name) == "LeftArm"), None)
        R = next((b for b in obj.data.bones if base(b.name) == "RightArm"), None)
        if L is None or R is None:
            return 0.0
        M = obj.matrix_world
        return abs((M @ L.head_local).x - (M @ R.head_local).x) / 2.0

    abd = 0.0
    if os.environ.get("CRAB_ARM_ABDUCT") is not None:
        abd = float(os.environ["CRAB_ARM_ABDUCT"])
    else:
        abd = arm_abduct()
    if abd > 0.05:
        log(f"  按角色表额外外展 {abd:.1f}°（肩宽 {_half_shoulder(tar)*2:.3f}m / "
            f"源 {_half_shoulder(src)*2:.3f}m）")

    if os.environ.get("CRAB_HAND_TWIST") is not None:
        twist = float(os.environ["CRAB_HAND_TWIST"])
    else:
        twist = hand_twist()
    if twist:
        log(f"  手腕轴向补偿 {twist:+.1f}°")

    swing = float(os.environ.get("CRAB_PROP_SWING", "0") or 0)
    if swing:
        log(f"  道具关节补偿 {swing:+.1f}°")

    if conj and os.environ.get("CRAB_NO_CONJ") != "1":
        apply_conjugate(src, conj, r_before, abduct_deg=abd, hand_twist=twist,
                        prop_swing=swing)

    report.sort(reverse=True)
    worst = [(round(a, 1), n) for a, n in report[:3]]
    return {"rebased": len(report), "worst": worst}


def apply_conjugate(src, conj: dict, rest_before: dict = None,
                    abduct_deg: float = 0.0, hand_twist: float = 0.0,
                    prop_swing: float = 0.0) -> None:
    """把已经烘进曲线里的局部旋转增量，等价地搬到新的 rest 坐标系里。

    骨骼的 `rotation_quaternion` 是「相对自身 rest 坐标系」的旋转。把 rest 的朝向换成
    目标角色以后，同一组四元数会作用在另一套局部轴上：数值没变，世界效果却变了。
    这里对每个关键帧做相似变换 q ← M q M⁻¹（M = 新 rest⁻¹ · 旧 rest），让最终的世界
    增量与源动画一致 —— 这才是「增量重定向」想要的语义。

    额外一层（手臂链）：上臂是相对父骨复制的，锁骨一转它就被带着转；锁骨又被衰减到 k，
    于是手臂比源更向外张（Megan Idle 从 8° 张到 17°，走路 30°）。这里把锁骨实际生效的
    世界增量 Δ_eff 之外的那部分（Δ · Δ_eff⁻¹）补回上臂，手臂就恢复到源的下垂角，而
    肩宽仍由锁骨的 rest 与衰减系数决定 —— 两个诉求解耦。
    """
    from bpy_extras import anim_utils

    SH_K = float(os.environ.get("CRAB_SHOULDER_K", str(SHOULDER_INCREMENT)))
    ad = src.animation_data
    if not ad or not ad.action:
        return
    cb = anim_utils.action_get_channelbag_for_slot(ad.action, ad.action_slot)
    if cb is None:
        return

    QID = Quaternion((1.0, 0.0, 0.0, 0.0))

    def curves(bone):
        dp = 'pose.bones["%s"].rotation_quaternion' % bone
        fcs = {fc.array_index: fc for fc in cb.fcurves if fc.data_path == dp}
        if len(fcs) != 4:
            return None
        n = len(fcs[0].keyframe_points)
        if any(len(f.keyframe_points) != n for f in fcs.values()):
            return None
        return fcs, n

    def shoulder_k(dev: float) -> float:
        # 只压「锁骨本来就拧得厉害」的角色：Y Bot / X Bot 这类近水平锁骨照常吃源动画
        return 1.0 - (1.0 - SH_K) * min(1.0, dev / 15.0)

    def rewrite(bone, fn, n_hint=None):
        c = curves(bone)
        if c is None:
            return
        fcs, n = c
        prev = None
        for i in range(n):
            q = Quaternion(tuple(fcs[cc].keyframe_points[i].co[1] for cc in range(4)))
            if q.magnitude < 1e-9:
                q = QID.copy()
            q.normalize()
            q2 = fn(q, i)
            q2.normalize()
            # 保持四元数符号连续，否则相邻帧分量翻号会让插值绕远路
            if prev is not None and q2.dot(prev) < 0:
                q2 = Quaternion(tuple(-v for v in q2))
            prev = q2.copy()
            for cc in range(4):
                kp = fcs[cc].keyframe_points[i]
                kp.co[1] = q2[cc]
                kp.handle_left[1] = q2[cc]
                kp.handle_right[1] = q2[cc]

    # 先把锁骨原始曲线上每帧的世界增量 Δ 取出来，算好要补回上臂的旋转
    comp = {}
    if rest_before:
        for side in ("Left", "Right"):
            sh = "mixamorig:%sShoulder" % side
            arm = "mixamorig:%sArm" % side
            if sh not in conj or arm not in rest_before:
                continue
            c = curves(sh)
            if c is None:
                continue
            fcs, n = c
            k = shoulder_k(conj[sh][1])
            if k >= 0.999:      # 完全不衰减（源自身），没有要补的量
                continue
            Q0 = rest_before[sh].to_quaternion()
            Qa = rest_before[arm].to_quaternion()
            lst = []
            for i in range(n):
                q = Quaternion(tuple(fcs[cc].keyframe_points[i].co[1] for cc in range(4)))
                if q.magnitude < 1e-9:
                    q = QID.copy()
                q.normalize()
                d = Q0 @ q @ Q0.conjugated()            # 源锁骨的世界增量
                de = QID.slerp(d, k)                    # 衰减后实际生效的增量
                dr = d @ de.conjugated()                # 被缩掉的那部分，补回上臂
                lst.append(Qa.conjugated() @ dr @ Qa)   # 换到上臂局部坐标系
            comp[arm] = lst

    # 额外外展：绕世界 Z（前后轴）把上臂往外推，宽体角色的躯干体积大，源那 8° 会插进躯干
    P_abd = {}
    if abduct_deg > 0.05 and rest_before:
        for side, sign in (("Left", 1.0), ("Right", -1.0)):
            arm = "mixamorig:%sArm" % side
            if arm not in rest_before:
                continue
            Qa = rest_before[arm].to_quaternion()
            A = Quaternion(Vector((0.0, 0.0, 1.0)), math.radians(sign * abduct_deg))
            P_abd[arm] = Qa.conjugated() @ A @ Qa

    # 第一遍：所有被改了 rest 的骨
    for bone, (Mq, dev) in conj.items():
        Minv = Mq.conjugated()
        bb = bone.split(":")[-1]
        k = shoulder_k(dev) if bb in ("LeftShoulder", "RightShoulder") else 1.0

        def fn(q, i, Mq=Mq, Minv=Minv, k=k):
            q2 = Mq @ q @ Minv
            if 0.0 <= k < 0.999:
                q2 = QID.slerp(q2, k)
            return q2

        rewrite(bone, fn)

    # 第二遍：手臂链没改 rest，但要把锁骨衰减掉的增量补回上臂，并按体型补外展
    for arm in ("mixamorig:LeftArm", "mixamorig:RightArm"):
        lst = comp.get(arm)
        P = P_abd.get(arm)
        if lst is None and P is None:
            continue

        def fn(q, i, lst=lst, P=P):
            q2 = (lst[i] @ q) if (lst is not None and i < len(lst)) else q
            if P is not None:
                q2 = P @ q2
            return q2

        rewrite(arm, fn)

    # 道具关节自身的旋转：剑/盾挂在 *_joint 骨上（Paladin 是 Sword_joint / Shield_joint），
    # 只转手腕带不动盾，得单独把这些 joint 绕自身轴拧一下才能推离身体。
    if abs(prop_swing) > 0.01:
        for pb in src.data.bones:
            if not pb.name.split(":")[-1].endswith("joint"):
                continue
            Sw = Quaternion(Vector((0.0, 0.0, 1.0)), math.radians(prop_swing))

            def fn(q, i, Sw=Sw):
                return q @ Sw

            rewrite(pb.name, fn)

    # 第三遍：手腕绕自身长轴（局部 Y）微拧。给「剑横过身前、剑尖压在腿上」这类角色用：
    # 肩关节只把整条手臂往外推，剑仍跟着手走；拧腕才会让剑身绕手臂轴扫开。
    if abs(hand_twist) > 0.01:
        for side in ("Left", "Right"):
            hand = "mixamorig:%sHand" % side
            Tw = Quaternion(Vector((0.0, 1.0, 0.0)), math.radians(hand_twist))

            def fn(q, i, Tw=Tw):
                return q @ Tw

            rewrite(hand, fn)


def import_anim_source(anim_path: str, target_arm, ratio: float, rebase: bool = True):
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

    if rebase:
        deselect_all()
        src.select_set(True)
        bpy.context.view_layer.objects.active = src
        if bpy.context.object.mode != "OBJECT":
            bpy.ops.object.mode_set(mode="OBJECT")
        info = rebase_source_rest(src, target_arm)
        log(f"  rest 重定基：{info.get('rebased', 0)} 根同名骨，"
            f"原先偏差最大的 {info.get('worst')}")
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


def action_fcurves(action, slot=None) -> list:
    """取 action 里的所有 fcurve。

    Blender 4.4 起 action 改成「layer → strip → channelbag(slot)」的分层存储，
    5.x 里 `action.fcurves` 已经彻底移除（用它会抛
    `AttributeError: 'Action' object has no attribute 'fcurves'`）。
    slot 用来只取绑定本骨架那条 slot 的通道，避免多 slot action 混进别人的曲线。
    """
    legacy = getattr(action, "fcurves", None)
    if legacy is not None:
        return list(legacy)

    bags = []
    for layer in action.layers:
        for strip in layer.strips:
            if slot is not None and hasattr(strip, "channelbag"):
                bag = strip.channelbag(slot)
                if bag is not None:
                    bags.append(bag)
                continue
            bags.extend(getattr(strip, "channelbags", []))
    return [fc for bag in bags for fc in bag.fcurves]


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
    slot = arm.animation_data.action_slot if arm.animation_data else None
    for fc in action_fcurves(action, slot):
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

    global SHOULDER_INCREMENT
    SHOULDER_INCREMENT = max(0.0, min(1.0, args.shoulder_increment))
    src = import_anim_source(anim_path, arm, ratio, rebase=not args.no_rebase_rest)
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
    apply_prop_joint_pose(arm)
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
        global _CUR_MODEL
        _CUR_MODEL = name
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
