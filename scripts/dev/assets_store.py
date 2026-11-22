#!/usr/bin/env python3
"""assets_store.py —— 资源的版本仓库：备份、快照、按版本对算增量

设计要点：
  * 每个版本 = 资源目录内容摘要（内容变版本就变），打包时自动快照，等价于"给
    当前版本做一次备份"。
  * 文件按 sha256 存进 blobs/（内容寻址），同一文件跨版本只存一份，备份成本极低。
  * 客户端带着自己的版本号来问，就能算出「从它到最新」的最小增量包；同一条
    (from, to) 只用算一次，之后直接命中缓存。

目录布局（默认 ~/.crab_feast-ota/assets）：
  indexes/<ver>.json         该版本的全部文件清单（path / sha256 / size）
  blobs/<sha 前2>/<sha>      文件内容，跨版本去重
  full/<ver>.zip             整包（首次安装用）
  deltas/<from>_<to>.zip     增量包（只含新增/改动，解压覆盖即可）
  deltas/<from>_<to>.json    增量元信息，含要删除的路径列表
  versions.json              版本时间线

用法：
  python3 scripts/dev/assets_store.py snapshot --src assets --ver-out /tmp/v
  python3 scripts/dev/assets_store.py delta --from <ver> --to <ver>
  python3 scripts/dev/assets_store.py prune --keep 3
  python3 scripts/dev/assets_store.py list
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
import zipfile
from pathlib import Path

STORE = Path(os.environ.get("CRAB_ASSET_STORE", Path.home() / ".crab_feast-ota" / "assets"))

# 这些目录/文件不进运行时资源包（与 Xcode 构建阶段保持一致）
# source = Blender 源工程，android-res = 安卓专用资源，都不进 iOS 热更包。
# 需要恢复安卓资源时设 CRAB_PACK_KEEP_ANDROID_RES=1 再打包。
EXCLUDE_DIRS = {"source"}
if os.environ.get("CRAB_PACK_KEEP_ANDROID_RES") != "1":
    EXCLUDE_DIRS.add("android-res")
EXCLUDE_FILES = {
    ".DS_Store",
    # 代码里只引用 PingFang-SC-Regular.ttf，Bold/Light 各 10MB 不进包
    "fonts/PingFang-SC-Bold.ttf",
    "fonts/PingFang-SC-Light.ttf",
}


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def scan(src: Path) -> dict[str, Path]:
    """资源目录 → {相对路径: 绝对路径}，路径统一用 / 分隔"""
    out: dict[str, Path] = {}
    for root, dirs, files in os.walk(src):
        dirs[:] = [d for d in dirs if d not in EXCLUDE_DIRS]
        for name in files:
            full = Path(root) / name
            rel = full.relative_to(src).as_posix()
            # 排除项写的是相对路径（也可以只写文件名）
            if rel in EXCLUDE_FILES or name in EXCLUDE_FILES:
                continue
            out[rel] = full
    return out


def compute_version(entries: dict[str, str]) -> str:
    """版本号只由内容决定：同样的资源永远得到同一个版本，便于客户端比对"""
    h = hashlib.sha256()
    for rel in sorted(entries):
        h.update(f"{rel}\0{entries[rel]}\n".encode())
    return h.hexdigest()[:12]


def store_path(sha: str) -> Path:
    return STORE / "blobs" / sha[:2] / sha


def load_versions() -> dict:
    f = STORE / "versions.json"
    if f.exists():
        return json.loads(f.read_text())
    return {"versions": []}


def save_versions(data: dict) -> None:
    STORE.mkdir(parents=True, exist_ok=True)
    (STORE / "versions.json").write_text(json.dumps(data, indent=2))


def make_index(src: Path, version: str) -> dict:
    files = scan(src)
    entries = {rel: sha256_file(p) for rel, p in files.items()}
    version = compute_version(entries) if version == "auto" else version
    index = {
        "version": version,
        "created": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "bytes": sum(p.stat().st_size for p in files.values()),
        "files": [
            {"path": rel, "sha256": entries[rel], "size": files[rel].stat().st_size}
            for rel in sorted(entries)
        ],
    }
    return index


def snapshot(src: Path, copy_blobs: bool = True) -> dict:
    index = make_index(src, "auto")
    version = index["version"]
    idx_dir = STORE / "indexes"
    idx_dir.mkdir(parents=True, exist_ok=True)
    idx_file = idx_dir / f"{version}.json"

    # 已存在同版本快照就直接复用（内容相同，版本相同）
    if idx_file.exists():
        print(f"SNAPSHOT_EXISTS {version}", file=sys.stderr)

    if copy_blobs:
        for rel, meta in ((f["path"], f) for f in index["files"]):
            dst = store_path(meta["sha256"])
            if dst.exists():
                continue
            dst.parent.mkdir(parents=True, exist_ok=True)
            # 原子写入：先写临时名，避免两个构建同时写坏同一个 blob
            tmp = dst.with_suffix(".tmp")
            tmp.write_bytes((src / rel).read_bytes())
            tmp.replace(dst)

    idx_file.write_text(json.dumps(index, indent=2))

    data = load_versions()
    versions = data["versions"]
    if not any(v["version"] == version for v in versions):
        versions.append({
            "version": version,
            "created": index["created"],
            "bytes": index["bytes"],
            "files": len(index["files"]),
        })
        save_versions(data)

    print(f"SNAPSHOT_OK {version} files={len(index['files'])} bytes={index['bytes']}")
    return index


def load_index(version: str) -> dict:
    f = STORE / "indexes" / f"{version}.json"
    if not f.exists():
        raise SystemExit(f"没有版本 {version} 的快照索引，先 snapshot")
    return json.loads(f.read_text())


def latest_version() -> str:
    versions = load_versions()["versions"]
    if not versions:
        raise SystemExit("仓库里还没有任何版本")
    return versions[-1]["version"]


def build_full(version: str) -> Path:
    """整包：条目不带 assets/ 前缀，客户端解压到自己的资源目录即可"""
    idx = load_index(version)
    out = STORE / "full" / f"{version}.zip"
    out.parent.mkdir(parents=True, exist_ok=True)
    if out.exists():
        return out
    tmp = out.with_suffix(".tmp")
    with zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        for f in idx["files"]:
            blob = store_path(f["sha256"])
            if not blob.exists():
                raise SystemExit(f"blob 缺失 {f['sha256']} ({f['path']})")
            z.write(blob, f["path"])
    tmp.replace(out)
    return out


def build_delta(old: str, new: str, force: bool = False) -> dict:
    """只打「新增 + 内容变了」的文件；删除清单单独记在 json 里"""
    meta_file = STORE / "deltas" / f"{old}_{new}.json"
    pack_file = STORE / "deltas" / f"{old}_{new}.zip"
    if meta_file.exists() and pack_file.exists() and not force:
        meta = json.loads(meta_file.read_text())
        meta["cached"] = True
        return meta

    oi = {f["path"]: f for f in load_index(old)["files"]}
    ni = {f["path"]: f for f in load_index(new)["files"]}

    changed = [f for p, f in ni.items() if p not in oi or oi[p]["sha256"] != f["sha256"]]
    deleted = sorted(p for p in oi if p not in ni)

    pack_file.parent.mkdir(parents=True, exist_ok=True)
    tmp = pack_file.with_suffix(".tmp")
    with zipfile.ZipFile(tmp, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
        for f in changed:
            blob = store_path(f["sha256"])
            if not blob.exists():
                raise SystemExit(f"blob 缺失 {f['sha256']} ({f['path']})")
            z.write(blob, f["path"])
    tmp.replace(pack_file)

    meta = {
        "from": old,
        "to": new,
        "url": f"/assets/deltas/{old}_{new}.zip",
        "bytes": pack_file.stat().st_size,
        "sha256": sha256_file(pack_file),
        "files": changed,
        "deleted": deleted,
        "created": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "cached": False,
    }
    meta_file.write_text(json.dumps(meta, indent=2))
    return meta


def prune(keep: int) -> None:
    versions = load_versions()["versions"]
    if len(versions) <= keep:
        print(f"只有 {len(versions)} 个版本，不用清")
        return
    drop = versions[:-keep]
    for v in drop:
        (STORE / "indexes" / f"{v['version']}.json").unlink(missing_ok=True)
        (STORE / "full" / f"{v['version']}.zip").unlink(missing_ok=True)
    # 增量包只要有一端被删就一起清
    alive = {v["version"] for v in versions[-keep:]}
    for f in (STORE / "deltas").glob("*"):
        parts = f.stem.split("_")
        if len(parts) == 2 and (parts[0] not in alive or parts[1] not in alive):
            f.unlink()
    save_versions({"versions": versions[-keep:]})
    print(f"PRUNE_OK 删除 {len(drop)} 个版本")


def cmd_list() -> None:
    data = load_versions()
    print(f"仓库: {STORE}")
    for v in data["versions"]:
        print(f"  {v['version']}  {v['created']}  {v['files']} 文件  {v['bytes'] / 2**20:.1f}MB")
    print(f"最新: {latest_version() if data['versions'] else '（空）'}")


def main() -> int:
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)

    sp = sub.add_parser("snapshot", help="给当前资源做快照（=备份）")
    sp.add_argument("--src", default="assets")
    sp.add_argument("--ver-out", help="把版本号写到文件，供脚本捕获")

    dp = sub.add_parser("delta", help="生成/命中 增量包")
    dp.add_argument("--from", dest="old", required=True)
    dp.add_argument("--to", dest="new", required=True)
    dp.add_argument("--force", action="store_true")

    fp = sub.add_parser("full", help="生成整包")
    fp.add_argument("--version", default="latest")

    pp = sub.add_parser("prune")
    pp.add_argument("--keep", type=int, default=3)

    sub.add_parser("list")

    a = ap.parse_args()

    if a.cmd == "snapshot":
        idx = snapshot(Path(a.src))
        if a.ver_out:
            Path(a.ver_out).write_text(idx["version"])
        return 0

    if a.cmd == "delta":
        meta = build_delta(a.old, a.new, a.force)
        changed = sum(f["size"] for f in meta["files"])
        print(f"DELTA_{'CACHED' if meta.get('cached') else 'BUILT'} {a.old}->{a.new} "
              f"files={len(meta['files'])} deleted={len(meta['deleted'])} "
              f"bytes={meta['bytes']} 原始改动={changed}")
        return 0

    if a.cmd == "full":
        v = latest_version() if a.version == "latest" else a.version
        p = build_full(v)
        print(f"FULL_OK {v} {p}")
        return 0

    if a.cmd == "prune":
        prune(a.keep)
        return 0

    if a.cmd == "list":
        cmd_list()
        return 0

    return 1


if __name__ == "__main__":
    sys.exit(main())
