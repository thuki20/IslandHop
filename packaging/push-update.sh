#!/usr/bin/env bash
# IslandHop macOS 迭代一键推送：构建 → 打包 → 生成 manifest → 推送到软件工作台。
# 一条命令同时更新三处消费方：
#   1. 门户网页 /home/updates/islandhop/            （https 域名 manifest）
#   2. 已装旧版客户端 http://IP/.well-known/...     （兼容副本，不必重新发版）
#   3. 门户 index.json（网页版本卡片）              （调用云端同步器刷新）
#
# 用法：
#   bash packaging/push-update.sh "更新说明第一行" ["更多说明"]...
#
# 可用环境变量覆盖：
#   ISLANDHOP_UPDATE_SERVER   ssh 别名，默认 ncsai
#   ISLANDHOP_PORTAL_DIR      门户更新目录，默认 /opt/ncsai-portal/data/updates/islandhop
#   ISLANDHOP_LEGACY_DIR      旧客户端兼容目录
#   ISLANDHOP_BASE_URL        新版 manifest 下载地址前缀
#   ISLANDHOP_LEGACY_URL      旧版 manifest 下载地址前缀
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(python3 -c "import tomllib;print(tomllib.load(open('Cargo.toml','rb'))['package']['version'])")"
ZIP_NAME="IslandHop-v${VERSION}-macos.zip"
SERVER="${ISLANDHOP_UPDATE_SERVER:-ncsai}"
PORTAL_DIR="${ISLANDHOP_PORTAL_DIR:-/opt/ncsai-portal/data/updates/islandhop}"
LEGACY_DIR="${ISLANDHOP_LEGACY_DIR:-/var/www/letsencrypt/.well-known/acme-challenge/islandhop}"
BASE_URL="${ISLANDHOP_BASE_URL:-https://203.195.201.85/home/updates/islandhop}"
LEGACY_URL="${ISLANDHOP_LEGACY_URL:-http://203.195.201.85/.well-known/acme-challenge/islandhop}"
# 注意：manifest 里的 url 必须指向「客户端连得上」的地址。ncsai.top 的 443
# 目前被链路上的 SNI 阻断（服务器本机自测 200，公网 RST），且 nginx 用的是
# Let's Encrypt **IP 证书**（SAN 只有 203.195.201.85），域名访问会证书不匹配。
# 因此 BASE_URL 用 IP；等域名备案/证书就位、443 通了再把 ISLANDHOP_BASE_URL 换回
# https://ncsai.top/home/updates/islandhop 即可，客户端多源回退会自动接管。

[ $# -ge 1 ] || { echo "用法: $0 \"更新说明\" [\"更多说明\"]..." >&2; exit 1; }

echo "==> cargo build --release"
# 不要把输出接给 tail：编译失败时只剩最后一行 "could not compile"，定位不了
# 具体错误（CI 上必须看到完整 rustc 报错）。
cargo build --release --bin IslandHop

echo "==> 打包 .app"
bash packaging/macos-app.sh --app-only

echo "==> 生成 $ZIP_NAME"
rm -f "dist/$ZIP_NAME"; mkdir -p dist
ditto -c -k --sequesterRsrc --keepParent IslandHop.app "dist/$ZIP_NAME"
SIZE=$(stat -f%z "dist/$ZIP_NAME")
HASH=$(shasum -a 256 "dist/$ZIP_NAME" | awk '{print $1}')

echo "==> 生成 manifest.json (v$VERSION) 与旧地址兼容副本"
python3 - "$VERSION" "$ZIP_NAME" "$SIZE" "$HASH" "$BASE_URL" "$LEGACY_URL" "$@" <<'PY'
import json, sys
version, zip_name, size, sha, base, legacy = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4], sys.argv[5], sys.argv[6]
notes = "\n".join(sys.argv[7:])
common = {"version": version, "notes": notes, "sha256": sha, "size": size}
main = dict(common, url=f"{base.rstrip('/')}/{zip_name}")
old = dict(common, url=f"{legacy.rstrip('/')}/{zip_name}")
for path, data in (("dist/manifest.json", main), ("dist/manifest-legacy.json", old)):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(json.dumps(data, ensure_ascii=False, indent=2) + "\n")
print(json.dumps(main, ensure_ascii=False, indent=2))
PY

if [ "${ISLANDHOP_NO_PUBLISH:-}" = "1" ]; then
  # 只出包模式：CI 用。产物留在 dist/，由调用方（GitHub Release）分发，
  # 服务器端再由 sync-islandhop-update 定时镜像回门户目录。
  echo "==> 跳过推送（ISLANDHOP_NO_PUBLISH=1）"
  echo "✔ 产物就绪：dist/$ZIP_NAME + dist/manifest.json（v${VERSION}）"
  exit 0
fi

echo "==> 推送到 $SERVER:$PORTAL_DIR/（门户）与旧兼容目录"
ssh "$SERVER" "mkdir -p '$PORTAL_DIR' '$LEGACY_DIR'"
rsync -avz --checksum "dist/$ZIP_NAME" "dist/manifest.json" "$SERVER:$PORTAL_DIR/"
rsync -avz --checksum "dist/$ZIP_NAME" "$SERVER:$LEGACY_DIR/"
rsync -avz --checksum "dist/manifest-legacy.json" "$SERVER:$LEGACY_DIR/manifest.json"

echo "==> 刷新门户网页版本卡片（云端同步器；失败不影响客户端更新）"
ssh "$SERVER" "timeout 120 python3 /opt/ncsai-portal/deploy/sync_github.py --quiet" || \
  echo "!! 同步器本次未成功（timer 会重试），客户端 manifest 已生效"

echo "==> 验证线上可访问"
curl --noproxy '*' -sk -fsS "https://203.195.201.85/home/updates/islandhop/manifest.json" || \
  echo "!! 门户 manifest 还不可访问，请检查 $PORTAL_DIR"
curl --noproxy '*' -fsS "http://203.195.201.85/.well-known/acme-challenge/islandhop/manifest.json" >/dev/null || \
  echo "!! 旧地址兼容 manifest 不可访问，请检查 $LEGACY_DIR"
echo "✔ 推送完成：v${VERSION}（网页与新旧客户端下次检查更新即收到）"
