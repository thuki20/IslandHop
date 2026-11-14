#!/usr/bin/env bash
# IslandHop: 验证 macOS 键鼠捕获是否就绪（需要「输入监控」授权）。
#
# 背景：引擎用 CGEventTap(Location::HID) 捕获本机键鼠。没有输入监控权限时
# 创建 tap 会失败，引擎静默退化成 NoCapture —— 控制台仍显示「运行中 / 已连接」，
# 但鼠标永远切不过去。本脚本通过线程采样确认 mac-event-tap 线程是否存在。
#
# 用法（授权后运行；会重启 IslandHop，请先保存工作）：
#   bash packaging/verify-macos-capture.sh
#   bash packaging/verify-macos-capture.sh /path/to/IslandHop.app

set -u
APP="${1:-/Applications/IslandHop.app}"
EXE="$APP/Contents/MacOS/IslandHop"
if [ ! -x "$EXE" ]; then
  echo "找不到可执行文件：$EXE" >&2
  exit 1
fi

echo "==> 重启 $APP"
pkill -f "$EXE" 2>/dev/null || true
sleep 2
open "$APP" || { echo "启动失败" >&2; exit 1; }
sleep 7

PID=$(pgrep -f "$EXE" | head -n 1)
if [ -z "$PID" ]; then
  echo "❌ IslandHop 未在运行"
  exit 1
fi
echo "==> pid=$PID"

OUT=$(mktemp /tmp/islandhop-sample.XXXXXX.txt)
sample "$PID" 1 -file "$OUT" >/dev/null 2>&1

echo "==> 引擎线程："
grep -oE 'Thread_[0-9]+: [A-Za-z0-9_.:-]+' "$OUT" 2>/dev/null | sed 's/^/    /' | head -n 30

echo
if grep -q 'mac-event-tap' "$OUT" 2>/dev/null; then
  echo "✅ 键鼠捕获已就绪（mac-event-tap 线程存在）"
  echo "   现在把鼠标推过屏幕右边缘，应能切到 Windows。"
else
  echo "❌ 键鼠捕获未启用（缺 mac-event-tap 线程，引擎退化为 NoCapture）"
  echo "   处理：系统设置 → 隐私与安全性 → 输入监控 →「+」添加 $APP 并打开开关，"
  echo "   然后彻底退出 IslandHop 再重开，最后重跑本脚本。"
fi

echo "==> TCP 会话："
lsof -nP -iTCP:24950 2>/dev/null | grep -E 'LISTEN|ESTABLISHED' | sed 's/^/    /' || echo "    (无)"
echo "==> 采样文件：$OUT"
