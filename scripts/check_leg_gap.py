"""验收双腿站姿：踝间距不能比髋间距宽太多（否则走路是外张的 V 型）。

口径：`|踝间距| − |髋间距|` 除以腿长归一。源动画（Y Bot 自己）是 0.091，
阈值取 0.12 —— 超过就是继承了角色的宽站姿，需要把腿改成 skip 重定基。

用法：
    blender -b --python scripts/check_leg_gap.py -- <repo> [clip1,clip2,...]
报告写到 assets/characters/animations/leg_gap_report.json。
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

RATIO_LIMIT = 0.12
CACHE_DIR = os.path.expanduser("~/Library/Caches/crab_feast/mixamo_rigs")
DEFAULT_CLIPS = ("idle", "walking")


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    repo = argv[0]
    clips = ([c.strip() for c in argv[1].split(",") if c.strip()]
             if len(argv) > 1 else list(DEFAULT_CLIPS))

    import prop_gap
    import bpy
    with open(os.path.join(repo, "scripts", "arm_abduct.json"), encoding="utf-8") as f:
        abduct = json.load(f)

    rows = []
    for fn in sorted(os.listdir(CACHE_DIR)):
        if not fn.endswith(".blend"):
            continue
        name = fn[:-len(".blend")].replace("_", " ")
        deg = abduct.get(name, 0.0)
        path = os.path.join(CACHE_DIR, fn)
        for clip in clips:
            E, arm, body, props = prop_gap.load_target(repo, path, clip, deg)
            leg = E.rest_leg_length(arm)

            def bx(n):
                pb = next((b for b in arm.pose.bones if b.name.split(":")[-1] == n), None)
                return None if pb is None else (arm.matrix_world @ pb.head).x

            worst = 0.0
            detail = []
            for f in (1, 60, 120, 180):
                bpy.context.scene.frame_set(f)
                bpy.context.view_layer.update()
                hip = abs(bx("RightUpLeg") - bx("LeftUpLeg"))
                ankle = abs(bx("RightFoot") - bx("LeftFoot"))
                d = (ankle - hip) / leg
                worst = max(worst, d)
                detail.append(round(d, 3))
            ok = worst <= RATIO_LIMIT
            rows.append({"model": name, "clip": clip, "deg": deg,
                         "mode": E.leg_mode(name), "leg_len_cm": round(leg * 100, 2),
                         "ratio": round(worst, 3), "frames": detail, "ok": ok})
            print("%-28s %-8s %-7s 踝−髋/腿长 %.3f  %s" % (
                name, clip, E.leg_mode(name), worst, "PASS" if ok else "FAIL"), flush=True)

    bad = [r for r in rows if not r["ok"]]
    out = os.path.join(repo, "assets/characters/animations/leg_gap_report.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump({"threshold_ratio": RATIO_LIMIT, "clips": clips,
                   "rows": rows, "fail": len(bad)}, f, ensure_ascii=False, indent=2)
    print("\n共 %d 条：PASS %d / FAIL %d" % (len(rows), len(rows) - len(bad), len(bad)), flush=True)
    print("报告：%s" % out, flush=True)


if __name__ == "__main__":
    main()
