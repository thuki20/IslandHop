#!/usr/bin/env bash
# 岛仔上游同步助手。
#
# 背景：IslandHop 是 Renly1994/xiaotang-daozai（岛仔）v0.6.64 的整树派生，
# 我们的第一次提交就把上游历史压成了一个快照，所以 git merge 没有共同祖先、
# 用不了。这里改用「三份快照比对」：上游基线（vX.Y.Z）、上游最新、我方当前文件，
# 逐文件判断这次上游改动能不能直接吃进来，会撞车的单独列出来人工合并。
#
# 用法：
#   bash scripts/upstream-sync.sh                    # 只出报告（不动代码）
#   bash scripts/upstream-sync.sh report v0.6.70     # 指定上游目标版本
#   bash scripts/upstream-sync.sh apply              # 把「可直接应用」的文件全部落盘
#   bash scripts/upstream-sync.sh apply --test       # 落盘后跑 cargo test --lib
#
# 报告写到 upstream-report.md，同时打印摘要。
# 网络：只走 codeload/API（本机 git 协议常被代理拦，tarball 稳定）。
set -uo pipefail
cd "$(dirname "$0")/.."

UPSTREAM="${ISLANDHOP_UPSTREAM:-Renly1994/xiaotang-daozai}"
BASE_FILE=".upstream-base"
CACHE=".upstream-cache"
MODE="report"
TARGET=""
RUN_TEST=0
for arg in "$@"; do
  case "$arg" in
    report|apply) MODE="$arg" ;;
    --test) RUN_TEST=1 ;;
    -*) echo "未知参数：$arg" >&2; exit 2 ;;
    *) TARGET="$arg" ;;
  esac
done

BASE="$(tr -d '[:space:]' < "$BASE_FILE" 2>/dev/null)"
[ -n "${BASE:-}" ] || { echo "缺少 ${BASE_FILE}（应记录上次同步的上游版本，如 v0.6.64）" >&2; exit 2; }

api() { curl -fsSL --max-time 30 "https://api.github.com/repos/$UPSTREAM/$1"; }

# 默认追上游 main（连未发版的 bug 修复一起抓），也可传具体版本号。
TARGET="${TARGET:-main}"
LATEST_TAG="$(api tags | python3 -c 'import json,sys; t=json.load(sys.stdin); print(t[0]["name"] if t else "")' 2>/dev/null || true)"
echo "==> 上游 ${UPSTREAM}：基线 ${BASE} → 目标 ${TARGET}（最新标签 ${LATEST_TAG:-未知}）"
mkdir -p "$CACHE"
fetch_tree() { # $1=ref  $2=目录名
  local ref="$1" dir="$CACHE/$2" tgz="$CACHE/$2.tar.gz" u attempt ok=0
  if [ -d "$dir" ] && [ -n "$(ls -A "$dir" 2>/dev/null)" ]; then return 0; fi
  mkdir -p "$dir"
  local urls=("https://codeload.github.com/$UPSTREAM/tar.gz/refs/tags/$ref")
  case "$ref" in
    v[0-9]*) ;;
    *) urls=("https://codeload.github.com/$UPSTREAM/tar.gz/refs/heads/$ref" "${urls[@]}") ;;
  esac
  for u in "${urls[@]}"; do
    for attempt in 1 2 3; do
      # codeload 是按需打包的流式响应，不支持 Range 续传，只能一次性拉完；
      # 链路慢时给足 600 秒（上游约 16MB）。
      rm -f "$tgz"
      if curl -fsSL --retry 2 --retry-delay 3 --max-time 600 "$u" -o "$tgz"; then ok=1; break 2; fi
      echo "  下载重试（第 $attempt 次）：$u" >&2
      sleep 3
    done
  done
  [ "$ok" = 1 ] || { echo "下载上游 $ref 失败" >&2; exit 1; }
  tar -xzf "$tgz" -C "$dir" --strip-components=1
}
fetch_tree "$BASE" "base-$BASE"
fetch_tree "$TARGET" "target-$TARGET"
BASE_DIR="$CACHE/base-$BASE"
TARGET_DIR="$CACHE/target-$TARGET"

# 上游历史里改动过的文件清单（对比两份快照）。
cd "$BASE_DIR"
CHANGED="$(diff -rq --exclude=.git . "$TARGET_DIR" 2>/dev/null | sed -E 's/^Files \.\/(.*) and .* differ$/\1/; s/^Only in .*\/(.*): (.*)$/\1\/\2/' | sort -u)"
cd - >/dev/null

APPLY_LIST=(); MANUAL_LIST=(); SKIP_LIST=()
while IFS= read -r f; do
  [ -n "$f" ] || continue
  case "$f" in
    .github/*|.gitignore|dist/*|target/*) SKIP_LIST+=("$f"); continue ;;  # 上游 CI 与产物：我们用自己的
  esac
  [ -f "$TARGET_DIR/$f" ] || { SKIP_LIST+=("${f}（上游已删除）"); continue; }
  if [ ! -f "$f" ]; then
    # 我方没有这个文件：纯新增，直接吃
    APPLY_LIST+=("$f|新增")
  elif cmp -s "$f" "$BASE_DIR/$f"; then
    # 我方没动过 → 上游改了就能整份覆盖
    APPLY_LIST+=("$f|上游改动（我方未动）")
  elif cmp -s "$f" "$TARGET_DIR/$f"; then
    SKIP_LIST+=("${f}（已一致）")
  else
    MANUAL_LIST+=("$f|双方都改过")
  fi
done <<< "$CHANGED"

COMMITS="$(api "compare/$BASE...$TARGET" | python3 -c '
import json,sys
d=json.load(sys.stdin)
for c in d.get("commits",[]):
    print("- `%s` %s" % (c["sha"][:8], c["commit"]["message"].splitlines()[0]))
')"

{
  echo "# 上游同步报告：$BASE → $TARGET"
  echo
  echo "生成时间：$(date '+%Y-%m-%d %H:%M')　仓库：$UPSTREAM"
  echo
  echo "## 上游提交"
  echo
  [ -n "$COMMITS" ] && echo "$COMMITS" || echo "（对比范围内没有提交）"
  echo
  echo "## 汇总"
  echo
  echo "| 分类 | 数量 |"
  echo "| --- | --- |"
  echo "| 可直接应用 | ${#APPLY_LIST[@]} |"
  echo "| 需人工合并 | ${#MANUAL_LIST[@]} |"
  echo "| 跳过 | ${#SKIP_LIST[@]} |"
  echo
  echo "## 可直接应用"
  echo
  for item in "${APPLY_LIST[@]:-}"; do [ -n "$item" ] && echo "- ${item%%|*}（${item#*|}）"; done
  echo
  echo "## 需人工合并"
  echo
  for item in "${MANUAL_LIST[@]:-}"; do [ -n "$item" ] && echo "- ${item%%|*}"; done
  echo
  echo "## 冲突明细（我方 vs 上游）"
  echo
  for item in "${MANUAL_LIST[@]:-}"; do
    f="${item%%|*}"
    [ -n "$f" ] || continue
    echo "### $f"
    echo '```diff'
    diff -u "$f" "$TARGET_DIR/$f" | sed -n '3,60p'
    echo '```'
  done
} > upstream-report.md

echo "==> 可直接应用 ${#APPLY_LIST[@]} 个文件，需人工合并 ${#MANUAL_LIST[@]} 个，跳过 ${#SKIP_LIST[@]} 个"
echo "==> 报告：upstream-report.md"

if [ "$MODE" = "apply" ]; then
  for item in "${APPLY_LIST[@]:-}"; do
    f="${item%%|*}"
    [ -n "$f" ] || continue
    mkdir -p "$(dirname "$f")"
    cp "$TARGET_DIR/$f" "$f"
    echo "  ✔ $f"
  done
  echo "==> 已应用 ${#APPLY_LIST[@]} 个文件；人工合并项见 upstream-report.md 的 diff 段落"
  if [ "$RUN_TEST" = "1" ]; then
    echo "==> cargo test --lib"
    cargo test --lib 2>&1 | tail -5
  fi
  echo "==> 下一步：合完人工项 → 更新 CHANGELOG → bump 两处 version → 提交"
fi
