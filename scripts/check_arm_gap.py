"""对全部角色 × 全部 clip 复测手臂/身体余量，输出 PASS/FAIL 报告。

判定：肘、腕在 idle 的每个采样帧上离身体轮廓 ≥ 2cm 视为达标（arm_gap.py 同口径）。
报告写到 assets/characters/animations/arm_gap_report.json，控制台打一张表。

用法：
    blender -b --python scripts/check_arm_gap.py -- <repo> [clip1,clip2,...]
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

JOINT_MIN_CM = 2.0
CACHE_DIR = os.path.expanduser("~/Library/Caches/crab_feast/mixamo_rigs")
ANIM_DIR = "assets/source/character/animations/Default"

# 只测「手臂自然垂落」的 clip。横向移动、转身、跳跃里腿会大幅抬起横摆，腿顶点会落进
# 关节的高度/纵深窗口，把身体轮廓的最大 |x| 撑到 0.3m 以上，余量一律变成负几十厘米
# （Big Vegas 的 left strafe walking 报 −79cm），那是口径污染不是穿模。
DEFAULT_CLIPS = ("idle", "walking")


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    repo = argv[0]
    if len(argv) > 1:
        clips = [c.strip() for c in argv[1].split(",") if c.strip()]
    else:
        clips = list(DEFAULT_CLIPS)

    import arm_gap
    with open(os.path.join(repo, "scripts/arm_abduct.json"), encoding="utf-8") as f:
        abduct = json.load(f)

    rows = []
    for fn in sorted(os.listdir(CACHE_DIR)):
        if not fn.endswith(".blend"):
            continue
        name = fn[:-len(".blend")].replace("_", " ")
        deg = abduct.get(name, 0.0)
        path = os.path.join(CACHE_DIR, fn)
        for clip in clips:
            r = arm_gap.evaluate(repo, path, clip, deg)
            ok = (r["min_joint"] is None) or (r["min_joint"] >= JOINT_MIN_CM)
            rows.append({"model": name, "clip": clip, "deg": deg,
                         "min_joint": r["min_joint"], "ok": ok,
                         "frames": r["frames"]})
            print("%-28s %-26s %5.1f°  %6s  %s" % (
                name, clip, deg,
                "—" if r["min_joint"] is None else "%.2f" % r["min_joint"],
                "PASS" if ok else "FAIL"), flush=True)

    bad = [r for r in rows if not r["ok"]]
    out = os.path.join(repo, "assets/characters/animations/arm_gap_report.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump({"threshold_cm": JOINT_MIN_CM,
                   "clips": clips,
                   "rows": rows,
                   "fail": len(bad)}, f, ensure_ascii=False, indent=2)
    print("\n共 %d 条：PASS %d / FAIL %d" % (len(rows), len(rows) - len(bad), len(bad),), flush=True)
    if bad:
        print("未达标：", flush=True)
        for r in bad:
            print("  %s / %s  余量 %s cm" % (r["model"], r["clip"], r["min_joint"]), flush=True)
    print("报告：%s" % out, flush=True)


if __name__ == "__main__":
    main()
