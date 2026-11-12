"""对带道具的角色复测「道具穿过身体」，输出 PASS/FAIL 报告。

口径同 scripts/prop_gap.py：只统计道具顶点落进身体内部的那部分（盾贴胸甲是设计，
距离 0 不算穿模，顶点在身体里才算）。阈值 2.0cm —— 剑/盾是贴身收着的，压到 0 只能靠把剑拧成「横指身后」，
姿态就不对了。自然垂下时残余 ≤1.9cm（1–9 个顶点擦过裙甲，肉眼不可见）。

用法：
    blender -b --python scripts/check_prop_gap.py -- <repo> [clip1,clip2,...]
报告写到 assets/characters/animations/prop_gap_report.json。
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

DEPTH_LIMIT_CM = 2.0
CACHE_DIR = os.path.expanduser("~/Library/Caches/crab_feast/mixamo_rigs")
DEFAULT_CLIPS = ("idle", "walking", "jump")


def main():
    argv = sys.argv[sys.argv.index("--") + 1:]
    repo = argv[0]
    clips = ([c.strip() for c in argv[1].split(",") if c.strip()]
             if len(argv) > 1 else list(DEFAULT_CLIPS))

    import prop_gap
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
            r = prop_gap.evaluate(repo, path, clip, deg)
            if not r["props"]:
                continue          # 这个角色没有道具网格，跳过
            ok = r["max_depth_cm"] <= DEPTH_LIMIT_CM
            rows.append({"model": name, "clip": clip, "deg": deg,
                         "props": r["props"],
                         "max_depth_cm": r["max_depth_cm"],
                         "max_npen": r["max_npen"], "ok": ok})
            detail = "，".join("%s %.2fcm/%d点" % (k.split("_")[-1], v["depth_cm"], v["npen"])
                              for k, v in r["props"].items())
            print("%-28s %-10s %5.1f°  %s  %s" % (
                name, clip, deg, detail, "PASS" if ok else "FAIL"), flush=True)

    bad = [r for r in rows if not r["ok"]]
    out = os.path.join(repo, "assets/characters/animations/prop_gap_report.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump({"threshold_cm": DEPTH_LIMIT_CM, "clips": clips,
                   "rows": rows, "fail": len(bad)}, f, ensure_ascii=False, indent=2)
    print("\n共 %d 条：PASS %d / FAIL %d" % (len(rows), len(rows) - len(bad), len(bad)), flush=True)
    print("报告：%s" % out, flush=True)


if __name__ == "__main__":
    main()
