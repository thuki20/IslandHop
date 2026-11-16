#!/usr/bin/env bash
# 把 GitHub Release 上的 IslandHop macOS / Windows 包镜像到门户更新目录。
#
# 为什么需要它：GitHub Actions runner 的出口 IP 每次都变，服务器安全软件
# （YJ-FIREWALL）会按 IP 封 22 端口，CI 没法直接 ssh 推包。改成 CI 只负责
# 出包并发 Release，本脚本定时把最新 Release 拉回来，客户端仍从服务器更新。
#
# 幂等：每个平台版本不高于本地就跳过该平台。由 crontab 每 5 分钟调用。
# Windows 还额外镜像安装版 Setup exe 到门户目录，供下载页直接给出固定链接。
set -euo pipefail

REPO="thuki20/IslandHop"
PORTAL_DIR="/opt/ncsai-portal/data/updates/islandhop"
LEGACY_DIR="/var/www/letsencrypt/.well-known/acme-challenge/islandhop"
BASE_URL="https://203.195.201.85/home/updates/islandhop"
LEGACY_URL="http://203.195.201.85/.well-known/acme-challenge/islandhop"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# 两个平台的清单都尝试拉取：谁存在就镜像谁（缺一个不影响另一个）。
got_any=0
for mf in manifest.json manifest-windows.json; do
  if curl -fsSL --max-time 30 "https://github.com/$REPO/releases/latest/download/$mf" \
       -o "$TMP/$mf" 2>/dev/null; then
    got_any=1
  fi
done
[ "$got_any" -eq 1 ] || exit 0

python3 - "$TMP" "$PORTAL_DIR" "$LEGACY_DIR" "$BASE_URL" "$LEGACY_URL" "$REPO" <<'PY'
import hashlib, json, os, shutil, sys, urllib.request

tmp, portal_dir, legacy_dir, base_url, legacy_url, repo = sys.argv[1:7]


def parse(text):
    try:
        return tuple(int(part) for part in text.split("."))
    except ValueError:
        return (0,)


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def mirror(manifest_name):
    remote_path = os.path.join(tmp, manifest_name)
    if not os.path.exists(remote_path):
        return
    with open(remote_path, encoding="utf-8") as fh:
        remote = json.load(fh)
    version = str(remote.get("version", "")).strip()
    if not version:
        return

    local_path = os.path.join(portal_dir, manifest_name)
    local_version = "0.0.0"
    if os.path.exists(local_path):
        try:
            with open(local_path, encoding="utf-8") as fh:
                local_version = str(json.load(fh).get("version", "0.0.0"))
        except (OSError, ValueError):
            pass
    if parse(version) <= parse(local_version):
        return

    asset = os.path.basename(str(remote.get("url", "")))
    if not asset.endswith(".zip") or not remote.get("sha256"):
        return

    print(f"[islandhop-sync] {manifest_name}: {local_version} -> {version}（{asset}）", flush=True)
    asset_path = os.path.join(tmp, asset)
    download = f"https://github.com/{repo}/releases/download/v{version}/{asset}"
    try:
        urllib.request.urlretrieve(download, asset_path)
    except Exception as error:                                # noqa: BLE001
        print(f"[islandhop-sync] 下载失败：{error}", flush=True)
        return

    actual = sha256(asset_path)
    if actual != str(remote["sha256"]).lower():
        print(f"[islandhop-sync] sha256 不符，丢弃：{actual[:16]} != {remote['sha256'][:16]}", flush=True)
        return
    expected_size = remote.get("size")
    if isinstance(expected_size, int) and expected_size > 0 and os.path.getsize(asset_path) != expected_size:
        print("[islandhop-sync] 包大小不符，丢弃", flush=True)
        return

    for directory in (portal_dir, legacy_dir):
        os.makedirs(directory, exist_ok=True)

    # manifest 的 url 改写成门户目录（与本机 push-update.sh 的产物一致）。
    main = dict(remote, url=f"{base_url.rstrip('/')}/{asset}")
    legacy = dict(remote, url=f"{legacy_url.rstrip('/')}/{asset}")
    shutil.copyfile(asset_path, os.path.join(portal_dir, asset))
    shutil.copyfile(asset_path, os.path.join(legacy_dir, asset))
    for path, payload in ((os.path.join(portal_dir, manifest_name), main),
                          (os.path.join(legacy_dir, manifest_name), legacy)):
        with open(path, "w", encoding="utf-8") as fh:
            fh.write(json.dumps(payload, ensure_ascii=False, indent=2) + "\n")

    # Windows：额外把安装版 Setup exe 镜像到门户，供下载页给固定链接。
    # 名字从 manifest notes 里取（package.ps1 写入），找不到就用版本拼默认名。
    if manifest_name == "manifest-windows.json":
        setup = f"IslandHop-Setup-v{version}-x64.exe"
        setup_path = os.path.join(tmp, setup)
        try:
            urllib.request.urlretrieve(
                f"https://github.com/{repo}/releases/download/v{version}/{setup}", setup_path)
            for directory in (portal_dir, legacy_dir):
                shutil.copyfile(setup_path, os.path.join(directory, setup))
        except Exception as error:                            # noqa: BLE001
            print(f"[islandhop-sync] Setup exe 镜像失败（zip 已就绪）：{error}", flush=True)

    print(f"[islandhop-sync] 已镜像 {manifest_name} v{version}", flush=True)


for name in ("manifest.json", "manifest-windows.json"):
    mirror(name)
PY
