"""按「肩宽自然垂落」求每个角色的肩关节外展角，写进 arm_abduct.json 供导出管线读取。

规则（对应用户口径：默认按肩宽自然垂落，离身体太近或穿模才调肩关节角度）：

1. 初值 = ARM_ABDUCT_TABLE 里的按角色表值（上一个已认可的版本），没有就是 0°。
2. 用 scripts/arm_gap.py 的同一套口径在真实重定向姿态上检测 idle 的 4 个相位帧。
3. 达标线：肘/腕最小余量 ≥ 2.0cm。
4. 不达标就 +2° 重测，上限 16°（再大就成了 A-pose，比穿模还难看）。

只用「肘腕关节」不用「手臂顶点」：掌心、护肩、袖口天然会压在轮廓里，算进去会把
Castle Guard 的护臂报成 −4cm 假穿模。道具只测腿不测躯干：盾贴胸是设计。

用法：
    blender -b --python scripts/solve_arm_abduct.py -- <repo> [clip] [--dry-run]
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

JOINT_MIN_CM = 2.0      # 肘/腕至少离身体轮廓 2cm，才有肉眼可见的空隙
STEP = 2.0
MAX_DEG = 16.0

CACHE_DIR = os.path.expanduser("~/Library/Caches/crab_feast/mixamo_rigs")


def solve(repo, clip="idle"):
    import arm_gap
    from export_mixamo_animations import ARM_ABDUCT_TABLE

    caches = sorted(f for f in os.listdir(CACHE_DIR) if f.endswith(".blend"))
    rows = []
    for fn in caches:
        name = fn[:-len(".blend")].replace("_", " ")
        path = os.path.join(CACHE_DIR, fn)
        start = ARM_ABDUCT_TABLE.get(name, 0.0)
        deg = start
        picked = None
        trail = []
        while deg <= MAX_DEG:
            r = arm_gap.evaluate(repo, path, clip, deg)
            ok = (r["min_joint"] is None) or (r["min_joint"] >= JOINT_MIN_CM)
            trail.append({"deg": deg, "joint": r["min_joint"]})
            print("  试 %s %s° -> joint %s %s" % (
                name, deg, r["min_joint"], "OK" if ok else "不达标"), flush=True)
            if ok:
                picked = deg
                break
            deg += STEP
        if picked is None:
            picked = MAX_DEG
            print("  !! %s 到 %.0f° 仍不达标，取上限" % (name, MAX_DEG), flush=True)
        rows.append({"model": name, "deg": picked, "init": start, "trail": trail})
    return rows


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    repo = argv[0]
    clip = "idle"
    dry = "--dry-run" in argv
    rows = solve(repo, clip)
    out = {r["model"]: r["deg"] for r in rows}
    if not dry:
        p = os.path.join(repo, "scripts/arm_abduct.json")
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            json.dump(out, f, ensure_ascii=False, indent=2, sort_keys=True)
        print("写入 %s" % p, flush=True)
    print("\n角色\t初值\t最终\t变化")
    for r in rows:
        print("%-28s %5.1f %5.1f %+5.1f" % (r["model"], r["init"], r["deg"],
                                            r["deg"] - r["init"]), flush=True)


if __name__ == "__main__":
    main()
