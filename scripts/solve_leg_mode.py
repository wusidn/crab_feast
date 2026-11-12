"""给每个角色选腿的重定基方式：rebase（保留角色自己的静止站姿）还是 skip
（腿保持 Y Bot 的绝对朝向，站姿收窄到源动画那样）。

为什么需要选
------------
`rebase_source_rest` 把源骨 rest 换成目标 rest，好处是保留角色结构（锁骨坡度、脊柱
曲度），代价是**角色的静止站姿也被继承**。Mixamo 里 Megan / Steve / Pete 这类资产
本身是模特式宽站姿（静止踝间距 30–38cm，髋间距才 15–20cm），重定向后走路就是大腿
外张的 V 型。而 Big Vegas 相反：他髋宽本身 37cm，跳过腿 rebase 反而更开。

判据
----
比较两种模式下 idle 的 `踝间距 − 髋间距`，除以腿长归一：谁更小用谁。
源动画（Y Bot 自己）这个值是 0.091，所以阈值取 0.12 留余量。

用法：
    blender -b --python scripts/solve_leg_mode.py -- <repo> [clip]
写 scripts/leg_mode.json。
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
CACHE_DIR = os.path.expanduser("~/Library/Caches/crab_feast/mixamo_rigs")


def spread(repo, cache, clip, deg):
    """返回 (Δ踝−髋 cm, 腿长 cm)。"""
    import prop_gap
    E, arm, body, props = prop_gap.load_target(repo, cache, clip, deg)
    import bpy
    bpy.context.scene.frame_set(1)
    bpy.context.view_layer.update()

    def bx(name):
        pb = next((b for b in arm.pose.bones if b.name.split(":")[-1] == name), None)
        return None if pb is None else (arm.matrix_world @ pb.head).x

    hip = bx("RightUpLeg") - bx("LeftUpLeg")
    ankle = bx("RightFoot") - bx("LeftFoot")
    delta = abs(abs(ankle) - abs(hip)) * 100
    return delta, E.rest_leg_length(arm) * 100


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    repo = argv[0]
    clip = argv[1] if len(argv) > 1 else "idle"
    with open(os.path.join(repo, "scripts", "arm_abduct.json"), encoding="utf-8") as f:
        abduct = json.load(f)

    out = {}
    for fn in sorted(os.listdir(CACHE_DIR)):
        if not fn.endswith(".blend"):
            continue
        name = fn[:-len(".blend")].replace("_", " ")
        deg = abduct.get(name, 0.0)
        path = os.path.join(CACHE_DIR, fn)
        res = {}
        for mode in ("rebase", "skip"):
            os.environ["CRAB_SKIP_LEGS"] = "0" if mode == "rebase" else "1"
            delta, leg = spread(repo, path, clip, deg)
            res[mode] = {"delta_cm": round(delta, 2), "ratio": round(delta / leg, 3)}
        best = min(res, key=lambda m: res[m]["delta_cm"])
        out[name] = {"mode": best, **res}
        print("%-28s rebase %5.1fcm(%.3f)  skip %5.1fcm(%.3f)  → %s" % (
            name, res["rebase"]["delta_cm"], res["rebase"]["ratio"],
            res["skip"]["delta_cm"], res["skip"]["ratio"], best), flush=True)

    with open(os.path.join(repo, "scripts", "leg_mode.json"), "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, indent=2)
    print("写入 scripts/leg_mode.json", flush=True)


if __name__ == "__main__":
    main()
