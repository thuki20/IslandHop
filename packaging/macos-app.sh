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

# 歌词数据源：mediaremote-adapter（BSD-3，ungive/mediaremote-adapter v0.7.7）。
# 借系统 /usr/bin/perl 的可信身份（com.apple.perl5）读 macOS 15.4+ 被封锁的 MediaRemote，
# 运行时由 src/macos_sys.rs 以「Resources/mediaremote-adapter」相对自身定位并调用。
MRA="packaging/mra"
if [ -d "$MRA/MediaRemoteAdapter.framework" ]; then
  mkdir -p "$APP/Contents/Resources/mediaremote-adapter"
  cp -R "$MRA/MediaRemoteAdapter.framework" "$APP/Contents/Resources/mediaremote-adapter/"
  cp "$MRA/mediaremote-adapter.pl" "$MRA/LICENSE" "$APP/Contents/Resources/mediaremote-adapter/"
  # 先签嵌套框架，再签外层 app（--deep 会覆盖嵌套签名，保持顺序）
  codesign --force --timestamp=none --sign - "$APP/Contents/Resources/mediaremote-adapter/MediaRemoteAdapter.framework" 2>/dev/null || true
fi

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
# 固定代码签名证书：TCC 按「签名身份 + bundle id」记授权，重打包后仍有效。
# 用 ad-hoc 签名时每次 codesign 都会换 cdhash，用户每次打开都被要求重新授权辅助功能 /
# 输入监控。钥匙串里已有用户自建的 OpenHopSign 证书时优先用它；没有才退回 ad-hoc。
SIGN_IDENTITY="OpenHopSign"
if security find-identity -v -p codesigning 2>/dev/null | /usr/bin/grep -q "\"$SIGN_IDENTITY\""; then
  codesign --force --deep --timestamp=none --sign "$SIGN_IDENTITY" "$APP"
  echo "signed with $SIGN_IDENTITY (stable TCC identity; authorization survives rebuilds)"
else
  codesign --force --deep --sign - "$APP" 2>/dev/null || true
  echo "warning: ad-hoc signature; TCC authorization is lost on every rebuild" >&2
fi
echo "built $APP (version $VERSION, icon AppIcon.icns)"

if [ "$APP_ONLY" -eq 0 ]; then
  mkdir -p dist
  hdiutil create -volname IslandHop -srcfolder "$APP" -ov -format UDZO \
    dist/IslandHop-macOS-arm64.dmg >/dev/null
  echo "built dist/IslandHop-macOS-arm64.dmg"
fi
