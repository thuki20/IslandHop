#!/usr/bin/env bash
# 生成 macOS 应用包（.app，带 IslandHop 图标），可选再打 dmg。
#   bash packaging/macos-app.sh              # .app + dist/IslandHop-macOS-arm64.dmg
#   bash packaging/macos-app.sh --app-only   # 只生成 .app
#
# 前置：cargo build --release --bin IslandHop
# 图标由 packaging/make-icon.py 生成（纯标准库 + sips/iconutil，无第三方依赖）。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP="IslandHop.app"
BIN="target/release/IslandHop"
ICON="packaging/assets/AppIcon.icns"
APP_ONLY=0
[ "${1:-}" = "--app-only" ] && APP_ONLY=1

[ -x "$BIN" ] || { echo "missing release binary: $BIN (run cargo build --release --bin IslandHop first)" >&2; exit 1; }
[ -f "$ICON" ] || { echo "missing icon: $ICON (run python3 packaging/make-icon.py first)" >&2; exit 1; }

# Cargo.toml 里 [workspace.package] 与 [package] 都有 version，必须取 [package] 的。
VERSION="$(python3 -c "import tomllib;print(tomllib.load(open('Cargo.toml','rb'))['package']['version'])")"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/IslandHop"
chmod +x "$APP/Contents/MacOS/IslandHop"
cp "$ICON" "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>IslandHop</string>
  <key>CFBundleDisplayName</key><string>IslandHop</string>
  <key>CFBundleIdentifier</key><string>dev.islandhop.app</string>
  <key>CFBundleExecutable</key><string>IslandHop</string>
  <key>CFBundleIconFile</key><string>AppIcon.icns</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# 不设置 LSUIElement=true：那会把 Dock 图标和菜单栏一起隐藏，用户会以为应用没启动。
# 岛本身由 app 内部把 NSWindow 抬到 level 25（菜单栏之上）并保持无边框。
# ad-hoc 签名：无开发者证书时也能在本机运行；授权（辅助功能/输入监控）仍需用户在系统设置里打开。
codesign --force --deep --sign - "$APP" 2>/dev/null || true
echo "built $APP (version $VERSION, icon AppIcon.icns)"

if [ "$APP_ONLY" -eq 0 ]; then
  mkdir -p dist
  hdiutil create -volname IslandHop -srcfolder "$APP" -ov -format UDZO \
    dist/IslandHop-macOS-arm64.dmg >/dev/null
  echo "built dist/IslandHop-macOS-arm64.dmg"
fi
