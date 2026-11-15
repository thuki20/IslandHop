#!/usr/bin/env bash
# 把 GitHub Release 上的 IslandHop macOS 包镜像到门户更新目录。
#
# 为什么需要它：GitHub Actions runner 的出口 IP 每次都变，服务器安全软件
# （YJ-FIREWALL）会按 IP 封 22 端口，CI 没法直接 ssh 推包。改成 CI 只负责
# 出包并发 Release，本脚本定时把最新 Release 拉回来，客户端仍从服务器更新。
#
# 幂等：版本不高于本地就什么都不做。由 crontab 每 5 分钟调用。
set -euo pipefail

REPO="thuki20/IslandHop"
PORTAL_DIR="/opt/ncsai-portal/data/updates/islandhop"
LEGACY_DIR="/var/www/letsencrypt/.well-known/acme-challenge/islandhop"
BASE_URL="https://203.195.201.85/home/updates/islandhop"
LEGACY_URL="http://203.195.201.85/.well-known/acme-challenge/islandhop"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 没有 Release 时静默退出（首次部署、或 release 被删）
curl -fsSL --max-time 30 "https://github.com/$REPO/releases/latest/download/manifest.json" \
  -o "$TMP/manifest.json" 2>/dev/null || exit 0

python3 - "$TMP" "$PORTAL_DIR" "$LEGACY_DIR" "$BASE_URL" "$LEGACY_URL" "$REPO" <<'PY'
import json, os, shutil, sys, urllib.request

tmp, portal_dir, legacy_dir, base_url, legacy_url, repo = sys.argv[1:7]
remote_path = os.path.join(tmp, "manifest.json")

with open(remote_path, encoding="utf-8") as fh:
    remote = json.load(fh)
version = str(remote.get("version", "")).strip()
if not version:
    sys.exit(0)


def parse(text):
    try:
        return tuple(int(part) for part in text.split("."))
    except ValueError:
        return (0,)


local_path = os.path.join(portal_dir, "manifest.json")
local_version = "0.0.0"
if os.path.exists(local_path):
    try:
        with open(local_path, encoding="utf-8") as fh:
            local_version = str(json.load(fh).get("version", "0.0.0"))
    except (OSError, ValueError):
        pass

if parse(version) <= parse(local_version):
    sys.exit(0)

zip_name = os.path.basename(str(remote.get("url", "")))
if not zip_name.endswith(".zip") or not remote.get("sha256"):
    sys.exit(0)

print(f"[islandhop-sync] {local_version} -> {version}（{zip_name}）", flush=True)
zip_path = os.path.join(tmp, zip_name)
download = f"https://github.com/{repo}/releases/download/v{version}/{zip_name}"
try:
    urllib.request.urlretrieve(download, zip_path)
except Exception as error:                                    # noqa: BLE001
    print(f"[islandhop-sync] 下载失败：{error}", flush=True)
    sys.exit(1)

import hashlib
digest = hashlib.sha256()
with open(zip_path, "rb") as fh:
    for chunk in iter(lambda: fh.read(1 << 20), b""):
        digest.update(chunk)
actual = digest.hexdigest()
if actual != str(remote["sha256"]).lower():
    print(f"[islandhop-sync] sha256 不符，丢弃：{actual[:16]} != {remote['sha256'][:16]}", flush=True)
    sys.exit(1)

expected_size = remote.get("size")
if isinstance(expected_size, int) and expected_size > 0 and os.path.getsize(zip_path) != expected_size:
    print("[islandhop-sync] 包大小不符，丢弃", flush=True)
    sys.exit(1)

for directory in (portal_dir, legacy_dir):
    os.makedirs(directory, exist_ok=True)

# 主 manifest 的 url 指向门户目录（与本机 push-update.sh 的产物一致）
main = dict(remote, url=f"{base_url.rstrip('/')}/{zip_name}")
legacy = dict(remote, url=f"{legacy_url.rstrip('/')}/{zip_name}")

shutil.copyfile(zip_path, os.path.join(portal_dir, zip_name))
shutil.copyfile(zip_path, os.path.join(legacy_dir, zip_name))
for path, payload in ((os.path.join(portal_dir, "manifest.json"), main),
                      (os.path.join(legacy_dir, "manifest.json"), legacy)):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(json.dumps(payload, ensure_ascii=False, indent=2) + "\n")

print(f"[islandhop-sync] 已镜像 v{version}", flush=True)
PY
