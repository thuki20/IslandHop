#!/usr/bin/env bash
# IslandHop macOS 迭代一键推送：构建 → 打包 → 生成 manifest → rsync 到云端。
# 客户端启动后自动检查 http://<server>/islandhop/manifest.json 并应用内覆盖安装。
#
# 用法：
#   bash packaging/push-update.sh "更新说明第一行" ["更多说明"]...
#
# 可用环境变量覆盖服务器与目录：
#   ISLANDHOP_UPDATE_SERVER   默认 ncsai（~/.ssh/config 里的别名）
#   ISLANDHOP_UPDATE_DIR      默认 /var/www/letsencrypt/.well-known/acme-challenge/islandhop
#                             （nginx 对该前缀直接出静态文件、无 301，ubuntu 可写，无需 sudo）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="$(python3 -c "import tomllib;print(tomllib.load(open('Cargo.toml','rb'))['package']['version'])")"
ZIP_NAME="IslandHop-v${VERSION}-macos.zip"
SERVER="${ISLANDHOP_UPDATE_SERVER:-ncsai}"
REMOTE_DIR="${ISLANDHOP_UPDATE_DIR:-/var/www/letsencrypt/.well-known/acme-challenge/islandhop}"

[ $# -ge 1 ] || { echo "用法: $0 \"更新说明\" [\"更多说明\"]..." >&2; exit 1; }

echo "==> cargo build --release"
cargo build --release --bin IslandHop 2>&1 | tail -1

echo "==> 打包 .app"
bash packaging/macos-app.sh --app-only

echo "==> 生成 $ZIP_NAME"
rm -f "dist/$ZIP_NAME"; mkdir -p dist
ditto -c -k --sequesterRsrc --keepParent IslandHop.app "dist/$ZIP_NAME"
SIZE=$(stat -f%z "dist/$ZIP_NAME")
HASH=$(shasum -a 256 "dist/$ZIP_NAME" | awk '{print $1}')

echo "==> 生成 manifest.json (v$VERSION)"
python3 - "$VERSION" "$ZIP_NAME" "$SIZE" "$HASH" "$@" <<'PY'
import json, sys
version, zip_name, size, sha = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
notes = "\n".join(sys.argv[5:])
manifest = {
    "version": version,
    "notes": notes,
    "url": f"http://203.195.201.85/.well-known/acme-challenge/islandhop/{zip_name}",
    "sha256": sha,
    "size": size,
}
open("dist/manifest.json", "w").write(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
print(json.dumps(manifest, ensure_ascii=False, indent=2))
PY

echo "==> rsync 到 $SERVER:$REMOTE_DIR/"
ssh "$SERVER" "mkdir -p '$REMOTE_DIR'"
rsync -avz --checksum "dist/$ZIP_NAME" "dist/manifest.json" "$SERVER:$REMOTE_DIR/"

echo "==> 验证线上可访问"
curl --noproxy '*' -fsS "http://203.195.201.85/.well-known/acme-challenge/islandhop/manifest.json" || \
  echo "!! manifest 还不可访问：请检查 $REMOTE_DIR"
echo "✔ 推送完成：v${VERSION}（客户端下次启动/检查更新即收到）"
