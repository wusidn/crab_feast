#!/usr/bin/env python3
"""optimize_glb.py —— 把 GLB 里的巨型 PNG 纹理重新编码，体积能砍 80~90%

这几个角色模型的 glb 里，网格只有 1~2MB，剩下全是 4K PNG 贴图
（Steve 122MB 里 120MB 是 8 张 PNG）。PNG 是无损格式，对 diffuse/glossiness
这类贴图是巨大浪费，重编码成 JPEG 并适当降分辨率对观感几乎没有影响。

用法：
  python3 tools/optimize_glb.py --in assets/characters/rigs/Steve.glb --out /tmp/opt
  python3 tools/optimize_glb.py --in assets/characters/rigs --out /tmp/opt --all
  python3 tools/optimize_glb.py --in ... --out ... --max-tex 1024 --quality 85

策略（可按 --normal-mode 调）：
  normal / roughness 这类贴图 → PNG（保留精度，JPEG 会把法线压出脏边）
  diffuse / glossiness / specular / emissive → JPEG，默认质量 88
  超过 --max-tex 的边长等比缩小（默认 2048）
"""
import argparse
import io
import json
import os
import struct
import sys

from PIL import Image

ALIGN = 4
NORMAL_HINTS = ("normal", "roughness", "metal", "orm", "height", "displace")


def read_glb(path):
    with open(path, "rb") as f:
        magic, ver, _length = struct.unpack("<III", f.read(12))
        if magic != 0x46546C67:
            raise SystemExit(f"{path} 不是 GLB")
        chunks = []
        while True:
            head = f.read(8)
            if len(head) < 8:
                break
            clen, ctype = struct.unpack("<II", head)
            data = f.read(clen)
            chunks.append((ctype, data))
    js = json.loads(next(d for t, d in chunks if t == 0x4E4F534A))
    bin_ = next((d for t, d in chunks if t == 0x004E4942), b"")
    return js, bin_


def write_glb(path, js, bin_):
    js_bytes = json.dumps(js, separators=(",", ":")).encode("utf-8")
    js_pad = (ALIGN - len(js_bytes) % ALIGN) % ALIGN
    js_bytes += b" " * js_pad
    bin_pad = (ALIGN - len(bin_) % ALIGN) % ALIGN
    bin_ = bin_ + b"\x00" * bin_pad
    total = 12 + 8 + len(js_bytes) + (8 + len(bin_) if bin_ else 0)
    with open(path, "wb") as f:
        f.write(struct.pack("<III", 0x46546C67, 2, total))
        f.write(struct.pack("<II", len(js_bytes), 0x4E4F534A))
        f.write(js_bytes)
        if bin_:
            f.write(struct.pack("<II", len(bin_), 0x004E4942))
            f.write(bin_)


def is_normal(name, js, image_index):
    lname = (name or "").lower()
    if any(h in lname for h in NORMAL_HINTS):
        return True
    # 材质里被 normalTexture 引用 → 就是法线
    for mat in js.get("materials", []):
        nt = mat.get("normalTexture")
        if nt and js["textures"][nt["index"]].get("source") == image_index:
            return True
    return False


def optimize_image(data, mime, normal, max_tex, quality, fmt="png", quantize=False, normal_quantize=False):
    im = Image.open(io.BytesIO(data))
    orig_size = im.size
    w, h = im.size
    if max_tex and max(w, h) > max_tex:
        scale = max_tex / max(w, h)
        im = im.resize((max(1, int(w * scale)), max(1, int(h * scale))), Image.LANCZOS)

    if fmt == "png" or normal:
        # PNG 路线：格式始终 image/png，Bevy 默认就能加载
        if im.mode not in ("RGB", "RGBA", "L"):
            im = im.convert("RGBA" if "A" in im.mode else "RGB")
        out = io.BytesIO()
        want_quant = (normal and normal_quantize) or ((not normal) and quantize)
        if want_quant:
            # 调色板 PNG：256 色，体积能再砍一大截；法线默认不量化（会压出脏边）
            src = im.convert("RGBA")
            # RGBA 只能用 FASTOCTREE/libimagequant；RGB 用 MEDIANCUT 效果更好
            if src.getchannel("A").getextrema() == (255, 255):
                q = src.convert("RGB").quantize(colors=256, method=Image.MEDIANCUT,
                                                dither=Image.FLOYDSTEINBERG)
            else:
                q = src.quantize(colors=256, method=Image.FASTOCTREE)
            q.save(out, format="PNG", optimize=True)
        else:
            im.save(out, format="PNG", optimize=True, compress_level=9)
        return out.getvalue(), "image/png", orig_size, im.size

    # JPEG 路线（默认不用：Bevy 需显式开启 jpeg feature）
    if im.mode in ("RGBA", "LA", "P"):
        im = im.convert("RGBA")
        # 全不透明的图丢掉 alpha 通道，JPEG 不支持 alpha 但少一个通道更小
        alpha = im.getchannel("A")
        if alpha.getextrema() == (255, 255):
            im = im.convert("RGB")
        else:
            # 有真透明：贴图边缘塞黑底，避免 JPEG 把透明区糊成白边
            bg = Image.new("RGB", im.size, (0, 0, 0))
            bg.paste(im, mask=alpha)
            im = bg
    elif im.mode not in ("RGB", "L"):
        im = im.convert("RGB")

    out = io.BytesIO()
    im.save(out, format="JPEG", quality=quality, optimize=True, progressive=True)
    return out.getvalue(), "image/jpeg", orig_size, im.size


def process(path, out_dir, max_tex, quality, normal_mode, fmt, quantize, normal_quantize):
    js, bin_ = read_glb(path)
    views = js.get("bufferViews", [])
    images = js.get("images", [])
    if not images:
        return None

    # 哪些 bufferView 是贴图数据
    img_view = {}
    for i, img in enumerate(images):
        if "bufferView" in img:
            img_view[img["bufferView"]] = i

    new_data = {}   # view_index -> bytes
    stats = []
    for vi, img in enumerate(images):
        bv = images[vi].get("bufferView")
        if bv is None:
            continue  # 外链图片，不处理
        v = views[bv]
        raw = bin_[v.get("byteOffset", 0): v.get("byteOffset", 0) + v["byteLength"]]
        normal = is_normal(images[vi].get("name"), js, vi)
        data, mime, before, after = optimize_image(
            raw, images[vi].get("mimeType"), normal, max_tex, quality, fmt, quantize, normal_quantize)
        # 小贴图转 JPEG 有时反而更大，没压下去就保留原始字节和原始 mime
        if len(data) >= len(raw):
            data, mime, after = raw, images[vi].get("mimeType", "image/png"), before
        new_data[bv] = data
        images[vi]["mimeType"] = mime
        stats.append((images[vi].get("name") or f"img{vi}", len(raw), len(data), before, after, mime))

    # 重建 BIN：按原顺序重排所有 bufferView，accessor 的 byteOffset 是相对的，不受影响
    buf = bytearray()
    for vi, v in enumerate(views):
        if vi in new_data:
            payload = new_data[vi]
        else:
            payload = bin_[v.get("byteOffset", 0): v.get("byteOffset", 0) + v["byteLength"]]
        pad = (ALIGN - len(buf) % ALIGN) % ALIGN
        buf += b"\x00" * pad
        v["byteOffset"] = len(buf)
        v["byteLength"] = len(payload)
        buf += payload

    js["buffers"] = [{"byteLength": len(buf)}]
    # 丢掉不再需要的 base64 引用与外链 uri
    for img in images:
        img.pop("uri", None)

    os.makedirs(out_dir, exist_ok=True)
    dst = os.path.join(out_dir, os.path.basename(path))
    write_glb(dst, js, bytes(buf))

    before = os.path.getsize(path)
    after = os.path.getsize(dst)
    print(f"{os.path.basename(path)}: {before/2**20:.1f}MB → {after/2**20:.1f}MB "
          f"(-{100*(1-after/before):.0f}%)")
    for name, b_bytes, a_bytes, b_size, a_size, mime in stats:
        print(f"    {name:<28} {b_bytes/2**20:7.1f}MB → {a_bytes/2**20:6.1f}MB  "
              f"{b_size[0]}x{b_size[1]} → {a_size[0]}x{a_size[1]}  {mime}")
    return before, after


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in", dest="src", required=True, help="glb 文件或目录")
    ap.add_argument("--out", dest="out", required=True, help="输出目录")
    ap.add_argument("--all", action="store_true", help="src 是目录时递归处理")
    ap.add_argument("--max-tex", type=int, default=2048, help="贴图最长边上限，0 表示不缩放")
    ap.add_argument("--quality", type=int, default=88, help="JPEG 质量")
    ap.add_argument("--format", choices=["png", "jpeg"], default="png",
                    help="贴图输出格式：png（默认，Bevy 原生支持）或 jpeg（需自行开启 feature）")
    ap.add_argument("--quantize", action="store_true",
                    help="普通贴图转 256 色调色板 PNG，再省一半体积（会有轻微色带）")
    ap.add_argument("--normal-quantize", action="store_true",
                    help="连法线贴图也量化（不推荐）")
    ap.add_argument("--normal-mode", choices=["png", "jpeg"], default="png",
                    help="兼容旧参数，已由 --format 取代")
    a = ap.parse_args()

    targets = []
    if os.path.isdir(a.src):
        for root, _dirs, files in os.walk(a.src):
            targets += [os.path.join(root, f) for f in files if f.endswith(".glb")]
    else:
        targets = [a.src]

    total_b = total_a = 0
    for p in sorted(targets):
        r = process(p, a.out, a.max_tex, a.quality, a.normal_mode, a.format, a.quantize, a.normal_quantize)
        if r:
            total_b += r[0]
            total_a += r[1]
    if total_b:
        print(f"\n合计: {total_b/2**20:.1f}MB → {total_a/2**20:.1f}MB "
              f"(-{100*(1-total_a/total_b):.0f}%)")


if __name__ == "__main__":
    main()
