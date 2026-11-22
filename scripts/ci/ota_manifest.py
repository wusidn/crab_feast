#!/usr/bin/env python3
"""生成 iOS OTA 安装所需的 manifest.plist 与安装页 index.html。

itms-services 安装的硬性要求：
  * manifest.plist 必须通过 HTTPS 提供（不接受 http，也不接受自签名证书）；
  * manifest 里的 software-package URL 同样必须是 HTTPS；
  * 设备 UDID 必须在描述文件（development / ad-hoc）里。

用法示例：
  python3 scripts/ci/ota_manifest.py \
    --ipa-url "https://github.com/wusidn/crab_feast/releases/download/mobile-latest/crab_feast-ios.ipa?v=abc1234" \
    --manifest-url "https://wusidn.github.io/crab_feast/manifest.plist?v=abc1234" \
    --bundle-id org.crab.feast --version 1.0.0 --build 42 \
    --subtitle "build 42 · abc1234" --out-dir .ota \
    --apk-url "https://github.com/wusidn/crab_feast/releases/download/mobile-latest/crab_feast-android.apk" \
    --install-page-url "https://wusidn.github.io/crab_feast/"
"""

from __future__ import annotations

import argparse
import html
import pathlib
import sys
from urllib.parse import quote

MANIFEST_TEMPLATE = """<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
\t<key>items</key>
\t<array>
\t\t<dict>
\t\t\t<key>assets</key>
\t\t\t<array>
\t\t\t\t<dict>
\t\t\t\t\t<key>kind</key>
\t\t\t\t\t<string>software-package</string>
\t\t\t\t\t<key>url</key>
\t\t\t\t\t<string>{ipa_url}</string>
\t\t\t\t</dict>
\t\t\t</array>
\t\t\t<key>metadata</key>
\t\t\t<dict>
\t\t\t\t<key>bundle-identifier</key>
\t\t\t\t<string>{bundle_id}</string>
\t\t\t\t<key>bundle-version</key>
\t\t\t\t<string>{bundle_version}</string>
\t\t\t\t<key>kind</key>
\t\t\t\t<string>software</string>
\t\t\t\t<key>title</key>
\t\t\t\t<string>{title}</string>
\t\t\t\t<key>subtitle</key>
\t\t\t\t<string>{subtitle}</string>
\t\t\t</dict>
\t\t</dict>
\t</array>
</dict>
</plist>
"""

PAGE_TEMPLATE = """<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>{title} · 安装</title>
<style>
  :root {{ color-scheme: dark; }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; min-height: 100vh; display: flex; align-items: center; justify-content: center;
    background: #0f1115; color: #e8eaed;
    font: 16px/1.6 -apple-system, "PingFang SC", "Helvetica Neue", sans-serif;
    padding: 32px 20px;
  }}
  .card {{ width: 100%; max-width: 420px; }}
  .logo {{ font-size: 44px; line-height: 1; margin-bottom: 8px; }}
  h1 {{ font-size: 26px; margin: 0 0 4px; letter-spacing: .5px; }}
  .sub {{ color: #8b93a1; font-size: 14px; margin-bottom: 28px; word-break: break-all; }}
  .btn {{
    display: block; text-align: center; text-decoration: none; font-weight: 600;
    padding: 16px 20px; border-radius: 14px; margin-bottom: 12px;
    background: #2f6fed; color: #fff;
  }}
  .btn.ghost {{ background: #1b1f27; color: #cfd5e0; border: 1px solid #2a3040; }}
  .meta {{ margin-top: 24px; padding-top: 16px; border-top: 1px solid #232833;
           color: #7f8794; font-size: 13px; }}
  .meta div {{ display: flex; justify-content: space-between; gap: 16px; padding: 3px 0; }}
  .warn {{ margin-top: 18px; color: #d9a441; font-size: 13px; }}
  a {{ color: #6f9bff; }}
</style>
</head>
<body>
<div class="card">
  <div class="logo">🦀</div>
  <h1>{title}</h1>
  <div class="sub">{subtitle}</div>
  <a class="btn" href="{itms_url}">安装到 iPhone / iPad</a>
  {apk_block}
  <div class="meta">
    <div><span>Bundle ID</span><span>{bundle_id}</span></div>
    <div><span>Version</span><span>{version}</span></div>
    <div><span>Build</span><span>{build}</span></div>
    <div><span>构建标识</span><span>{build_id}</span></div>
    <div><span>资源版本</span><span>{assets_version}</span></div>
  </div>
  <div class="warn">
    首次安装后用「设置 → 通用 → VPN 与设备管理」信任开发者证书。
    该链接只能用 Safari 打开；iOS 会直接覆盖安装同名 App。
  </div>
</div>
</body>
</html>
"""

APK_BLOCK = '<a class="btn ghost" href="{apk_url}">下载 Android APK</a>'


def xml_escape(value: str) -> str:
    return html.escape(str(value), quote=True)


def main() -> int:
    parser = argparse.ArgumentParser(description="生成 OTA manifest.plist 与安装页")
    parser.add_argument("--ipa-url", required=True, help="ipa 的 HTTPS 直链")
    parser.add_argument("--manifest-url", required=True, help="manifest.plist 的 HTTPS 直链（写进入页面的 itms-services 链接）")
    parser.add_argument("--bundle-id", required=True)
    parser.add_argument("--version", required=True, help="CFBundleShortVersionString，如 1.0.0")
    parser.add_argument("--build", required=True, help="构建号，如 42")
    parser.add_argument(
        "--bundle-version",
        default=None,
        help="写进清单的 bundle-version，必须等于 ipa 里的 CFBundleVersion，默认取 --build",
    )
    parser.add_argument("--build-id", default="", help="人类可读的出包标识，如 20261121-223501")
    parser.add_argument("--assets-version", default="", help="热更资源版本号")
    parser.add_argument("--title", default="Crab Feast")
    parser.add_argument("--subtitle", default="")
    parser.add_argument("--out-dir", default=".ota")
    parser.add_argument("--apk-url", default="")
    parser.add_argument("--install-page-url", default="", help="安装页自身的 URL，仅用于打印")
    args = parser.parse_args()

    for name, value in (("--ipa-url", args.ipa_url), ("--manifest-url", args.manifest_url)):
        if not value.startswith("https://"):
            print(f"error: {name} 必须是 https 链接（iOS OTA 不接受 http）", file=sys.stderr)
            return 2

    out_dir = pathlib.Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    subtitle = args.subtitle or f"build {args.build}"
    bundle_version = args.bundle_version or args.build
    manifest = MANIFEST_TEMPLATE.format(
        ipa_url=xml_escape(args.ipa_url),
        bundle_id=xml_escape(args.bundle_id),
        version=xml_escape(args.version),
        title=xml_escape(args.title),
        bundle_version=xml_escape(bundle_version),
    subtitle=xml_escape(subtitle),
    )
    (out_dir / "manifest.plist").write_text(manifest, encoding="utf-8")

    itms_url = "itms-services://?action=download-manifest&url=" + quote(args.manifest_url, safe="")
    apk_block = APK_BLOCK.format(apk_url=xml_escape(args.apk_url)) if args.apk_url else ""
    page = PAGE_TEMPLATE.format(
        title=xml_escape(args.title),
        bundle_version=xml_escape(bundle_version),
    subtitle=xml_escape(subtitle),
        itms_url=html.escape(itms_url, quote=True),
        apk_block=apk_block,
        bundle_id=xml_escape(args.bundle_id),
        version=xml_escape(args.version),
        build=xml_escape(args.build),
        build_id=xml_escape(args.build_id or "-"),
        assets_version=xml_escape(args.assets_version or "-"),
    )
    (out_dir / "index.html").write_text(page, encoding="utf-8")

    print(f"manifest: {out_dir / 'manifest.plist'}")
    print(f"index:    {out_dir / 'index.html'}")
    if args.install_page_url:
        print(f"安装页:   {args.install_page_url}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
