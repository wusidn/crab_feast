#!/usr/bin/env python3
"""把家里公网 IPv6 地址同步到 DNS 的 AAAA 记录。

    python3 scripts/dev/ota_ddns.py --check        # 只比对，不改
    python3 scripts/dev/ota_ddns.py                # 比对不一致就更新

为什么需要它：家宽 IPv6 前缀由运营商下发，重拨/换机后会变。OTA 要的是「域名 + 证书」，
域名必须始终指向当前地址，否则户外扫码就是超时。

支持的 DNS：cloudflare / dnspod / alidns
输出：DDNS_OK（已更新）/ DDNS_NOCHANGE（本来就一致）/ DDNS_NO_V6（本机没拿到公网 v6）/ DDNS_FAILED
"""
import argparse
import base64
import hashlib
import hmac
import json
import os
import re
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

TTL = 60

# 各家免费版的最低 TTL：DNSPod / 阿里云解析免费版都是 600 秒，填小了会被拒
TTL_MIN = {"dnspod": 600, "alidns": 600}


def ttl_for(provider: str) -> int:
    return max(TTL, TTL_MIN.get(provider, 0))


# ---------------------------------------------------------------- 本机地址
def local_ipv6(iface: str) -> str:
    """取 iface 上可用的全球单播地址：优先 secured（稳定 IID），排除临时/已弃用。"""
    out = subprocess.run(["ifconfig", iface], capture_output=True, text=True).stdout
    cands = []
    for line in out.splitlines():
        m = re.search(r"inet6 ([0-9a-f:]+) prefixlen \d+(.*)", line, re.I)
        if not m:
            continue
        addr, flags = m.group(1), m.group(2)
        if addr.startswith("fe80") or addr.startswith("::1"):
            continue
        if "deprecated" in flags:
            continue
        cands.append((0 if "temporary" not in flags else 1, addr))
    if not cands:
        return ""
    return sorted(cands, key=lambda x: x[0])[0][1]


def resolve_aaaa(host: str) -> str:
    try:
        infos = socket.getaddrinfo(host, None, socket.AF_INET6, socket.SOCK_STREAM)
    except socket.gaierror:
        return ""
    return infos[0][4][0] if infos else ""


# ---------------------------------------------------------------- DNS 厂商
def http_json(url: str, data=None, headers=None, method=None):
    body = None
    if data is not None:
        body = urllib.parse.urlencode(data).encode()
    req = urllib.request.Request(url, data=body, headers=headers or {}, method=method)
    if body and "Content-Type" not in (headers or {}):
        req.add_header("Content-Type", "application/x-www-form-urlencoded")
    for attempt in range(3):
        try:
            with urllib.request.urlopen(req, timeout=20) as r:
                return json.loads(r.read().decode() or "{}")
        except urllib.error.HTTPError as e:
            # 阿里云这类网关会用 4xx 返回带 Code/Message 的 JSON，直接透出原始信息，别吞成 "404"
            raw = ""
            try:
                raw = e.read().decode("utf-8", "ignore")
            except Exception:  # noqa: BLE001
                pass
            try:
                payload = json.loads(raw)
                detail = payload.get("Message") or payload.get("message") or raw[:200]
                code = payload.get("Code") or payload.get("code") or ""
            except Exception:  # noqa: BLE001
                detail, code = raw[:200], ""
            raise RuntimeError(f"HTTP {e.code} {code}: {detail}") from None
        except (urllib.error.URLError, ConnectionError) as e:
            # 阿里云 API 偶发 connection reset，重试两次再报
            if attempt == 2:
                raise RuntimeError(f"网络错误: {e}") from None
            time.sleep(1 + attempt)


class Cloudflare:
    def __init__(self, token: str, zone: str):
        self.token = token
        self.zone_hint = zone

    def _call(self, path: str, data=None, method=None):
        return http_json(
            "https://api.cloudflare.com/client/v4" + path,
            data,
            {"Authorization": f"Bearer {self.token}"},
            method,
        )

    def zone_id(self) -> str:
        if self.zone_hint:
            r = self._call(f"/zones?name={urllib.parse.quote(self.zone_hint)}")
            if r.get("result"):
                return r["result"][0]["id"]
        r = self._call("/zones?per_page=100")
        zones = [z["name"] for z in r.get("result", [])]
        return zones[0] if zones else ""

    def upsert(self, host: str, value: str) -> str:
        zid = self.zone_id()
        if not zid:
            raise RuntimeError("Cloudflare: 找不到可用 zone（检查 CF_ZONE / token 权限）")
        zone_name = ""
        for z in self._call("/zones?per_page=100").get("result", []):
            if z["id"] == zid:
                zone_name = z["name"]
        sub = host[: -len(zone_name) - 1] if zone_name and host.endswith("." + zone_name) else ""
        name = host
        recs = self._call(f"/zones/{zid}/dns_records?type=AAAA&name={urllib.parse.quote(name)}")
        for rec in recs.get("result", []):
            if rec["content"] == value:
                return "unchanged"
            self._call(
                f"/zones/{zid}/dns_records/{rec['id']}",
                {"type": "AAAA", "name": name, "content": value, "ttl": TTL, "proxied": False},
                "PUT",
            )
            return "updated"
        self._call(
            f"/zones/{zid}/dns_records",
            {"type": "AAAA", "name": name, "content": value, "ttl": TTL, "proxied": False},
            "POST",
        )
        return "created"


class DNSPod:
    def __init__(self, token: str, zone: str):
        self.token = token
        self.zone = zone
        self.ttl = ttl_for("dnspod")

    def _call(self, path: str, **kw):
        data = {"login_token": self.token, "format": "json", "lang": "cn"}
        data.update(kw)
        return http_json("https://dnsapi.cn/" + path, data)

    def upsert(self, host: str, value: str) -> str:
        if not self.zone:
            raise RuntimeError("DNSPod: 需要 OTA_DNS_ZONE（主域名，如 example.com）")
        sub = host[: -len(self.zone) - 1] if host.endswith("." + self.zone) else host
        r = self._call("Record.List", domain=self.zone, sub_domain=sub, record_type="AAAA")
        if r.get("status", {}).get("code") != "1":
            raise RuntimeError(f"DNSPod: {r.get('status', {}).get('message')}")
        recs = r.get("records") or []
        for rec in recs:
            if rec["value"] == value:
                return "unchanged"
            self._call(
                "Record.Modify",
                domain=self.zone, record_id=rec["id"], sub_domain=sub,
                record_type="AAAA", record_line_id=rec.get("line_id", "0"),
                value=value, ttl=self.ttl,
            )
            return "updated"
        self._call(
            "Record.Create", domain=self.zone, sub_domain=sub, record_type="AAAA",
            record_line="默认", value=value, ttl=self.ttl,
        )
        return "created"


class AliDNS:
    def __init__(self, key_id: str, secret: str, zone: str):
        self.key_id, self.secret, self.zone = key_id, secret, zone
        self.ttl = ttl_for("alidns")

    def _call(self, action: str, **kw):
        params = {
            "Action": action, "Format": "JSON", "Version": "2015-01-09",
            "AccessKeyId": self.key_id, "SignatureMethod": "HMAC-SHA1",
            "SignatureVersion": "1.0",
            "SignatureNonce": uuid.uuid4().hex,
            "Timestamp": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        }
        params.update({k: str(v) for k, v in kw.items()})
        canonical = "&".join(
            f"{urllib.parse.quote(k, safe='')}={urllib.parse.quote(params[k], safe='')}"
            for k in sorted(params)
        )
        sts = "GET&%2F&" + urllib.parse.quote(canonical, safe="")
        sig = base64.b64encode(
            hmac.new((self.secret + "&").encode(), sts.encode(), hashlib.sha1).digest()
        ).decode()
        url = "https://alidns.aliyuncs.com/?" + canonical + "&Signature=" + urllib.parse.quote(sig, safe="")
        return http_json(url)

    def upsert(self, host: str, value: str) -> str:
        if not self.zone:
            raise RuntimeError("AliDNS: 需要 OTA_DNS_ZONE（主域名，如 example.com）")
        sub = host[: -len(self.zone) - 1] if host.endswith("." + self.zone) else host
        r = self._call("DescribeDomainRecords", DomainName=self.zone,
                       RRKeyWord=sub, Type="AAAA", PageSize=100)
        recs = [x for x in r.get("DomainRecords", {}).get("Record", []) if x["RR"] == sub]
        for rec in recs:
            if rec["Value"] == value:
                return "unchanged"
            self._call("UpdateDomainRecord", RecordId=rec["RecordId"], RR=sub,
                       Type="AAAA", Value=value, TTL=self.ttl)
            return "updated"
        self._call("AddDomainRecord", DomainName=self.zone, RR=sub, Type="AAAA",
                   Value=value, TTL=self.ttl)
        return "created"


class DuckDNS:
    """免费方案：xxx.duckdns.org，只更新 IPv6，TTL 不可调但通常几分钟内生效。"""

    def __init__(self, token: str, zone: str):
        self.token = token

    def upsert(self, host: str, value: str) -> str:
        sub = host.split(".")[0]
        url = (
            "https://www.duckdns.org/update?domains=" + urllib.parse.quote(sub)
            + "&token=" + urllib.parse.quote(self.token)
            + "&ipv6=" + urllib.parse.quote(value) + "&verbose=true"
        )
        with urllib.request.urlopen(url, timeout=20) as r:
            body = r.read().decode().strip()
        if body.splitlines()[0].strip().upper() != "OK":
            raise RuntimeError(f"DuckDNS: {body.splitlines()[-1] if body else '空响应'}")
        return "updated"


def build(provider: str, zone: str):
    if not provider:
        raise RuntimeError("缺 OTA_DNS_PROVIDER（cloudflare/dnspod/alidns/duckdns）")
    if provider == "cloudflare":
        tok = os.environ.get("CF_API_TOKEN")
        if not tok:
            raise RuntimeError("缺 CF_API_TOKEN")
        return Cloudflare(tok, zone)
    if provider == "dnspod":
        tok = os.environ.get("DP_LOGIN_TOKEN")
        if not tok:
            raise RuntimeError("缺 DP_LOGIN_TOKEN（格式 id,key）")
        return DNSPod(tok, zone)
    if provider == "alidns":
        kid, sec = os.environ.get("ALI_ACCESS_KEY_ID"), os.environ.get("ALI_ACCESS_KEY_SECRET")
        if not (kid and sec):
            raise RuntimeError("缺 ALI_ACCESS_KEY_ID / ALI_ACCESS_KEY_SECRET")
        return AliDNS(kid, sec, zone)
    if provider == "duckdns":
        tok = os.environ.get("DUCKDNS_TOKEN")
        if not tok:
            raise RuntimeError("缺 DUCKDNS_TOKEN")
        return DuckDNS(tok, zone)
    raise RuntimeError(f"不支持的 OTA_DNS_PROVIDER={provider}（cloudflare/dnspod/alidns/duckdns）")


def load_env_file() -> None:
    """读 ~/.crab_feast-ota/ota.env，让 cron / 强制命令也能拿到配置（已存在的环境变量优先）。"""
    path = os.environ.get("OTA_ENV_FILE") or os.path.join(
        os.path.expanduser("~/.crab_feast-ota"), "ota.env"
    )
    if not os.path.isfile(path):
        return
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, val = line.split("=", 1)
            os.environ.setdefault(key.strip(), val.strip())


def main() -> int:
    load_env_file()
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default=os.environ.get("OTA_HOST", ""))
    ap.add_argument("--iface", default=os.environ.get("OTA_IFACE", "en1"))
    ap.add_argument("--provider", default=os.environ.get("OTA_DNS_PROVIDER", ""))
    ap.add_argument("--zone", default=os.environ.get("OTA_DNS_ZONE", ""))
    ap.add_argument("--check", action="store_true", help="只比对，不写 DNS")
    ap.add_argument("--print-local", action="store_true", help="只打印本机 v6 地址")
    args = ap.parse_args()

    addr = local_ipv6(args.iface)
    if args.print_local:
        if addr:
            print(addr)
            return 0
        print(f"DDNS_NO_V6  在 {args.iface} 上没有可用的公网 IPv6", file=sys.stderr)
        return 2
    if not addr:
        print(f"DDNS_NO_V6  在 {args.iface} 上没有可用的公网 IPv6")
        return 2

    cur = resolve_aaaa(args.host) if args.host else ""
    print(f"本机: {addr}")
    print(f"DNS : {cur or '(解析不到)'}")

    if not args.host:
        print("DDNS_FAILED 缺 OTA_HOST")
        return 1
    if cur and cur == addr:
        print("DDNS_NOCHANGE")
        return 0
    if args.check:
        print("DDNS_DRIFT  DNS 与本机不一致（--check 模式不写入）")
        return 3

    try:
        client = build(args.provider, args.zone)
    except RuntimeError as e:
        print(f"DDNS_FAILED {e}")
        return 1
    try:
        print(f"DDNS_OK {client.upsert(args.host, addr)}")
        return 0
    except Exception as e:  # noqa: BLE001 - 网络/鉴权错误统一报给上层
        print(f"DDNS_FAILED {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
