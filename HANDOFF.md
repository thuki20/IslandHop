# IslandHop 交接文档（macOS 侧）

> 目的：换一台机器 / 换一个 WorkBuddy 会话，读完这份就能直接接手开发、测试、发版。
> 更新时间：2026-11-15 · 版本 **v0.6.67** · 分支 `main` · HEAD `4aecb80`

---

## 0. 一句话现状

IslandHop 是把 Windows 版「岛仔（xiaotang-daozai）」全量 fork 到 macOS 的灵动岛 App，
保留上游全部功能，并内嵌 OpenHop 局域网键鼠共享 + 剪贴板同步。
**当前 macOS 端可日常使用**：岛高对齐菜单栏、刘海死区不显示内容、左右分列布局、HUD 下挂弹窗、
电源状态零延迟、OTA 自动更新闭环打通；测试基线 **904 通过 / 0 失败**。

| 项 | 值 |
|---|---|
| 仓库 | https://github.com/thuki20/IslandHop （`main`） |
| 上游 | `Renly1994/xiaotang-daozai`（岛仔 v0.6.64，全量 fork） |
| 本地目录 | `/Users/Tsuki/Downloads/IslandHop` |
| 技术栈 | Rust 2021 · eframe/egui 0.31（vendored）· winit 0.30.13 · objc2 0.6 · keyring 3 |
| 主产物 | `IslandHop.app`（macOS，ad-hoc 或 `OpenHopSign` 签名） |
| 版本 | v0.6.68 |
| 云端构建 | `.github/workflows/release-macos.yml`（GitHub `macos-15` runner，**不需要 Mac**，见 §2.5） |
| 更新源 | **多源自动回退**（`src/updates.rs: FEEDS`）：① `https://203.195.201.85/home/updates/islandhop/`（当前生效，LE **IP 证书**）② `http://203.195.201.85/.well-known/acme-challenge/islandhop/`（无证书兜底）③ `https://ncsai.top/home/updates/islandhop/`（**目前不可用**，见 §6 第 14 条） |
| 测试基线 | `cargo test --release --lib` → 904 passed / 0 failed / 77 ignored |

---

## 1. 新机器上手（5 步）

```bash
git clone https://github.com/thuki20/IslandHop.git
cd IslandHop
cargo build --release --bin IslandHop        # 首次 5~10 分钟
cargo test --release --lib                   # 应看到 904 passed; 0 failed
bash packaging/macos-app.sh --app-only       # 生成 IslandHop.app（自动签名）
```

运行（**必须绝对路径**，原因见 §6 坑 1）：

```bash
pkill -f IslandHop; sleep 2
open /Applications/IslandHop.app             # 或先 cp -R IslandHop.app /Applications/
```

**依赖前置**：Xcode Command Line Tools（`xcode-select --install`）。本机没有 Homebrew，也不需要。

> **别整个文件夹拷过去**：本机仓库 12G，其中 `target/` 占 11G（编译产物，换机器必须重编）、
> `.git` 只有 15M、`dist/` 是历史 zip 55M。走 `git clone` 拿到的是干净源码，几分钟就能编译好。
> 只有一种情况需要拷文件：对方**没有 github 访问权限**——那就 `git bundle` 或打包源码目录并排除 `target/`。

---

## 2. 迭代 → 发版闭环（一条命令）

```bash
# 1) 改代码 + 本地验证（见 §5）
cargo build --release --bin IslandHop
cargo test --release --lib

# 2) 升版本：Cargo.toml 里 [workspace.package] 和 [package] **两处**都要改
#    并往 CHANGELOG.md 顶部加一节

# 3) 一条命令构建 + 打包 + 生成 manifest + 推云端
bash packaging/push-update.sh "更新说明第一行" "第二行" "第三行"

# 4) 用户机器下次启动自动检测 → 弹窗 → 下载 → 校验 → 换包 → 重启
```

`push-update.sh` 做的事：`cargo build` → `macos-app.sh` → `ditto` 打 zip → 算 sha256 →
写 `manifest.json`（+ 旧地址兼容副本）→ `rsync` 到云服务器两处目录 → 刷新门户网页版本卡片 → curl 验证。

**新机器可能遇到的限制（重要）**

| 限制 | 说明 | 规避 |
|---|---|---|
| `ssh ncsai` 别名 | 脚本用 `ssh ncsai` 推 203.195.201.85，别名配在**本机** `~/.ssh/config` | 新机补 ssh 配置，或把 zip+manifest 拷给有权限的机器推；也可 `ISLANDHOP_UPDATE_SERVER=user@host` 覆盖 |
| 签名证书 `OpenHopSign` | 本机钥匙串自建代码签名证书，TCC 按「签名身份 + bundle id」记授权 | 新机没有该证书会退回 ad-hoc → **每次重打包都要重新授权辅助功能**。要稳定就新建同名证书：钥匙串访问 → 证书助理 → 创建证书 → 类型「代码签名」 |
| 客户端更新源 | 已改为多源候选（`src/updates.rs: FEEDS`），按序自动回退，域名排最后 | 无需改；调试单个源用 `ISLANDHOP_FEED_URL=<url>` 覆盖 |

---

## 2.5 云端构建：Windows 也能发版（不需要 Mac）

`.github/workflows/release-macos.yml` 在 GitHub 的 Apple Silicon runner 上编译、签名、打包并**发布 GitHub Release**，
服务器上的定时任务再把 Release 镜像到更新目录，**改完代码 push 就完事**，本机不需要装 Rust、也不需要出包。

```bash
# Windows / 任意机器上：
# 1) 改代码
# 2) Cargo.toml 两处 version 一起 bump（这是发版开关）
# 3) CHANGELOG.md 顶部加一节
# 4) git push
# → Actions 约 15 分钟出包并建 Release → 服务器 5 分钟内镜像 → 客户端下次启动弹更新
```

手动触发（可自定义更新说明）：仓库 Actions → **Release macOS (OTA)** → Run workflow → 填 `notes`。

**为什么不让 CI 直接 ssh 推包？** GitHub runner 的出口 IP 每次都变，而服务器 `YJ-FIREWALL-INPUT` 链里有大量
`REJECT ... tcp dpt:22` 黑名单（安全软件自动生成的防爆破规则），CI 的 ip 一进去就是 `Connection refused`，
且没法用白名单维持。所以方向反过来：**CI 只往 GitHub 推，服务器定时往回拉**。

| 项 | 值 | 备注 |
|---|---|---|
| runner | `macos-15`（arm64） | **不要用 `macos-14`**：实测 1 秒内 cancelled、0 step；`macos-13` 长期 queued。可用仓库变量 `MACOS_RUNNER` 覆盖 |
| 触发 | push 到 main（改 `src/` `crates/` `vendor/` `packaging/` `Cargo.toml`）+ 手动 | 只改文档不会触发 |
| 实测耗时 | 首次约 16 分钟（904 测试占一半），有缓存后更快 | 公开仓库免费，不烧额度 |
| 产物去向 | GitHub Release `vX.Y.Z`：`IslandHop-vX.Y.Z-macos.zip` + `manifest.json` + `manifest-legacy.json` | 同时留一份 Actions artifact（30 天） |
| 服务器镜像 | `/opt/ncsai-portal/deploy/sync-islandhop-update.sh`，crontab `*/5`，日志 `/opt/ncsai-portal/data/islandhop-sync.log` | 只拉版本更高的；校验 sha256 + size 后才落盘 |
| 需要的 Secrets | `MACOS_CERT_P12`（OpenHopSign 的 p12 base64）、`MACOS_CERT_PASSWORD` | 缺了就走 ad-hoc 签名（用户需重新授权辅助功能） |

**两个必须知道的坑：**

1. **secrets 是 run 启动时快照的**。改完 secret 必须**重新触发**一次 workflow，正在跑的 job 拿不到新值
   （首次实测就栽在这：job 10:17 启动，secret 10:18 写入，env 里仍是空）。
2. **自签名证书导出成 p12 会丢 TrustSettings**，CI 里导入后 `find-identity -v` 认为它不受信 → `macos-app.sh`
   静默退回 ad-hoc 签名。workflow 里已用 `security add-trusted-cert -d -r trustRoot` 修掉，并在看不到身份时
   直接报错而不是悄悄降级。

> 本机仍然可以直接推（`bash packaging/push-update.sh "说明"`，走 `ssh ncsai` + rsync，因为本机 IP 不在黑名单里）。
> 两条路径产出格式一致，客户端无感；CI 里用 `ISLANDHOP_NO_PUBLISH=1` 让它只出包不推。

---

## 3. 代码地图（改哪里）

| 文件 | 职责 | 行数量级 |
|---|---|---|
| `src/main.rs` | Shell：每帧循环、`mac_cursor` 取值、命中区 `hits` 组装、穿透调用、系统轮询（电量/音量/时钟）、命令分发 | ~1.1k |
| `src/app.rs` | **岛体核心**：布局（`notch_slots` / `bar_h()`）、绘制、`hit_rect`、`FrameOut`（region/hit/notice_hit/hud_hit）、展开动画 | ~3.3k |
| `src/extras.rs` | 设置页 UI（`settings_ui`）、`alert()` / HUD、`draw_notice`、各类设置控件 | ~2.0k |
| `src/mac_window.rs` | macOS 平台层：`elevate`（窗口层级 25）、`cursor_local`、`sync_passthrough`、`menu_bar_height`、`activate_app` | ~300 |
| `src/macos_sys.rs` | IOKit 电源事件、文件选择、系统集成 | ~225 |
| `src/updates.rs` | OTA：`check_feed` / `parse_feed` / 下载校验 / `launch_installer`（ditto + `apply.sh`） | ~820 |
| `src/ui_tests.rs` | 全部 UI 快照/几何断言测试（**8383 行，改 UI 前必看**） | ~8.4k |
| `crates/openhop-core/` | 内嵌的 OpenHop 引擎（键鼠共享）：`engine.rs`、`platform/mod.rs`（`set_ui_exclude`） | ~5k |
| `vendor/egui-winit/` | **打过补丁**：幽灵鼠标事件过滤 + 指针位置兜底（别删，见 §6 坑 12） | — |
| `packaging/` | `macos-app.sh`（出包+签名）、`push-update.sh`（发版）、`make-icon.py` | — |

### 关键几何常量（app.rs 顶部）

```
WIN_W = 640      透明画布宽（岛体实际宽 PW 由动画决定）
WIN_H = 270      透明画布高（窗口永远这么大，靠穿透让空白处点击落到底层）
PILL_GAP = 6     胶囊皮肤与屏幕顶边间隙
```

### macOS 关键事实（别再踩）

- **egui 逻辑点 == AppKit 点，1:1**，换算屏幕坐标时**不要乘 `pixels_per_point`**。
- 窗口层级 `NSStatusWindowLevel = 25`（高于菜单栏 24），否则窗口会被挤到菜单栏下方。
- 本机屏幕 1512×982，**刘海死区 x∈[663,848]（宽 185）高 32**，菜单栏高 **38pt**。
- 刘海几何走权威接口：`NSScreen.safeAreaInsets` / `auxiliaryTopLeftArea`，已封装为
  `openhop_core::extras::system::notch()`，**不要自己猜**。

### 点击穿透机制（最容易改坏的地方）

```
每帧：
1. mac_cursor = NSEvent.mouseLocation → 窗口本地坐标（cursor_local）
2. app.frame() 返回 FrameOut：
   hit       = 岛体矩形（含展开面板）
   notice_hit= 铃铛徽标/未读面板
   hud_hit   = macOS 下挂 HUD 卡片矩形（**仅卡片可见时存在**）
3. 光标落在任一命中矩形 expand(10pt) 内 → ignoresMouseEvents=NO，否则 YES（穿透）
4. 从穿透切回接收时补发一条 mouseMoved（否则 egui 无 press 起点，点击判定失败）
```

Windows 走另一条路：`src/native.rs` 用 `SetWindowRgn` 把画布裁成命中区。

### HUD 弹窗（充放电 / 音量 / 蓝牙 / 网络）

macOS 菜单栏模式下**下挂到岛体下方 8pt**，画成独立圆角卡片（`app.rs` 的 `fading_hud` 分支）；
菜单栏那条带被物理刘海挡住，画在里面等于看不见。Windows / 侧边停靠仍走旧的内嵌布局。

---

## 4. 已实现能力（v0.6.64 → v0.6.68）

| 能力 | 实现要点 | 提交 |
|---|---|---|
| 岛高对齐菜单栏 | `bar_h()` 用 `menu_bar_height()`，全量替换固定 `BAR_H` | `9730c16` |
| 刘海左右分列 | `notch_slots(gap)`：左=电源+吉祥宠，右=AI 额度+天气；刘海内不画内容；时钟隐藏 | `9730c16` |
| 点击设置打不开（历史）| ① 引擎边缘 grab → UI 排除区；② 外部进程幽灵光标事件 → vendored egui-winit 过滤 | `9730c16` |
| 电源零延迟 | IOKit `IOPSNotificationCreateRunLoopSource` 事件驱动 + 1.5s pmset 兜底 | `9730c16` |
| OTA 自动更新 | manifest + sha256 + ditto + `apply.sh` 换包重启；首测即全自动 | `9730c16` / `0cdb0dd` |
| 刘海机型高度算错修复 | `frame 顶边 − visibleFrame 顶边`（原用高度差被 Dock 污染） | `062dc84` |
| HUD 下挂岛体下方 | `fading_hud` 分支独立卡片 + 投影 + `ISLANDHOP_HUD_DEMO` 演示钩子 | `61f7105` |
| 岛下方点击被吞修复 | 排除 Windows 专用 `file_drop_zone`；`hud_hit` 仅可见时登记；穿透改读 NSWindow 真值 | `4aecb80` |
| 测试基线全绿 | 月历间距 / macOS 钥匙串 / 额度环溢出 / 主题容差 | `4aecb80` |
| 更新通道多源回退 | `FEEDS` 按序尝试 https+IP → http+IP → 域名，首个可用即用；manifest 的 url 与源自洽 | `76a166d` |

---

## 5. 验收与自测手法（真机，别只信单元测试）

UI 快照测试测不到窗口层级、穿透、刘海遮挡，关键改动**必须真机截图**。

```bash
# 1) 截图（坐标单位为屏幕逻辑点，retina 自动 2x）
screencapture -x -R 400,0,700,180 /tmp/shot.png    # 刘海+岛+下挂弹窗区域
screencapture -x -R 0,0,1512,80  /tmp/bar.png      # 整条菜单栏

# 2) HUD 免插拔电源验证（3 秒后弹一条「电源已连接」，2.5 秒后消失）
ISLANDHOP_HUD_DEMO=1 ./target/release/IslandHop
# 截图窗口在启动后 3.0~5.5 秒之间

# 3) 穿透状态日志（点击类问题首选）
ISLANDHOP_PT_LOG=1 ./target/release/IslandHop 2>&1 | grep '\[PT\]'
#   [PT] cursor=Some([320.0 70.0]) pass_through=false   ← 光标在命中区（岛/弹窗）内
#   [PT] cursor=Some([320.0 70.0]) pass_through=true    ← 光标在空白区（应穿透）
```

**程序化搬光标**（`CGWarp` 本身不产生事件，必须再补一条 `mouseMoved`，否则 App 不会重绘判定）：

```swift
// /tmp/move.swift → swiftc -O move.swift -o move；./move 756 70
import CoreGraphics
let pt = CGPoint(x: Double(CommandLine.arguments[1])!, y: Double(CommandLine.arguments[2])!)
CGWarpMouseCursorPosition(pt)
CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: pt, mouseButton: .left)?
    .post(tap: .cghidEventTap)
```

**像素级判定**（判断岛/刘海是否对齐）：Swift 打开 PNG → `NSBitmapImageRep` → 逐行扫亮度 < 70 的暗色段，
换算成逻辑点与 `663~848`（刘海）、`550~950`（岛）比对。

**测试加压复现 flake**（并行才暴露的问题）：

```bash
for i in 1 2 3 4 5 6 7 8; do (yes>/dev/null &); done
cargo test --release --lib settings_theme; pkill yes
```

---

## 6. 踩过的坑（照这个单子走，能省一天）

1. **`open -a IslandHop` 会启动错包** —— LaunchServices 会指到 `Downloads/island-hop` 里的旧副本，
   还弹一个莫名的辅助功能授权窗。永远用 `open /Applications/IslandHop.app`，并先 `pkill -f IslandHop`。
2. **`menu_bar_height` 不能用高度差** —— `frame.height − visibleFrame.height` 把底部 Dock 也算进去（本机 103pt），
   必须用顶边差：`(frame.y + frame.h) − (visible.y + visible.h)` = 38pt。
3. **macOS 上不要乘 `pixels_per_point`** —— egui 逻辑点与 AppKit 屏幕点是 1:1，乘了 ppp 会导致
   "齿轮点不到 / 弹窗偏一倍"。
4. **`CGWarpMouseCursorPosition` 不产生鼠标事件** —— 自动化测悬停/穿透时必须补 `CGEvent.mouseMoved`。
5. **`CFRunLoopAddSource` 必须传真正的 `kCFRunLoopDefaultMode` CFString** —— 传裸 C 字符串会 SIGTRAP 崩溃（踩过两次）。
6. **后台直起的二进制会随会话结束被杀** —— 截图/验证要在同一条命令里完成；常驻请用 `open /Applications/...`。
7. **GitHub 直连偶发 502 / HTTP2 framing error** —— `env -u HTTPS_PROXY -u HTTP_PROXY git push`，
   或 `git -c http.version=HTTP/1.1 push`。
8. **`sed -i ''` 在本机偶发报错** —— 直接改用编辑工具改文件。
9. **版本号两处都要改** —— `Cargo.toml` 的 `[workspace.package]` 和 `[package]`，打包脚本取 `[package]`。
10. **测试"单跑过、全量挂"多半是负载/时序问题** —— 先加 `yes` 加压复现，再判断是断言过紧还是真 bug；
    不要用"单跑通过"当验收。
11. **TCC 授权跟签名身份绑定** —— 固定用 `OpenHopSign` 签名，重打包不丢辅助功能授权；ad-hoc 每次都丢。
12. **`vendor/egui-winit` 的幽灵事件过滤别删** —— 某些远程控制软件（AweSun / UU 远程）会在 mouseDown 瞬间
    注入假的 `CursorMoved + CursorLeft`，删了补丁岛内按钮又会"点不动"。
13. **命中区只登记"看得见的东西"** —— 任何"预留位置"都会被用户感知为"这里点不动"；
    本次 bug 就是 Windows 专用的 96pt 拖放区泄漏到 macOS 命中表。
14. **`ncsai.top` 的 443 目前公网不通，别拿它当更新源** —— 症状：TCP 能连，但带
    `SNI=ncsai.top` 的 ClientHello 一发出去就被 RST（Mac 和外部节点都复现）；而服务器本机
    `curl --resolve ncsai.top:443:203.195.201.85` 返回 200，说明 nginx 完全正常，阻断在链路。
    同时 nginx 用的是 Let's Encrypt **IP 证书**（`/etc/nginx/ssl/ncsai.crt`，SAN 只有
    `IP Address:203.195.201.85`，无域名），域名访问必然证书不匹配。
    → 所以 `FEEDS` 把域名排在最后，日常走 IP；域名要能用需先解决 443 阻断（备案/腾讯云接入），
    再补域名证书 + nginx `server_name`。
15. **IP 证书只有 6 天有效期** —— LE 短期 IP 证书，`acme.sh` cron 每 6 小时跑一次
    （`Le_ReloadCmd = systemctl reload nginx`）。若某天 OTA 全面失效，先查
    `openssl x509 -in /etc/nginx/ssl/ncsai.crt -noout -dates` 和 `~/.acme.sh/*/` 续期日志。

---

## 7. 待办 / 下一步优化（按性价比排）

| 优先 | 事项 | 线索 |
|---|---|---|
| P0 | 双机实测 OpenHop 键鼠共享 + 剪贴板同步 | 需 Windows 主机在线（上次因主机休眠未完成）；引擎在 `crates/openhop-core`，排除区 `set_ui_exclude` 已接 |
| P0 | 设置页仍有 Windows 专属文案 | macOS 上能看到「登录 **Windows** 时自动打开」（`src/extras.rs:1412`）、「兼容渲染（**Direct3D 12**）」（`src/extras.rs:1415`）、以及邮箱页的「Windows 凭据管理器」提示。应 `cfg(target_os)` 隐藏或按平台改文案 |
| P1 | HUD 卡片点击行为 | 目前只"不穿透"，点击无动作。可加"点一下立即收起"（注意 `hud_notice` 是 `self.notice` 的镜像，清除要连 `notice` 一起处理，否则下一帧又克隆回来） |
| P1 | Windows 端一致性复核 | `native.rs` 的 `SetWindowRgn` 命中区未同步 `hud_hit`；Windows 上 HUD 仍是内嵌布局，需确认没有同类"吞点击" |
| P2 | 歌词/媒体面板在刘海机型复核 | `island_lyrics_active()` 与 `notch_slots` 的交互，展开态是否压到刘海 |
| P2 | 77 个 `#[ignore]` 测试梳理 | `cargo test --release --lib -- --ignored` 看是不是人为屏蔽 |
| P3 | 自动更新体验 | 当前只在启动时检查一次；可加"每 6 小时检查"与"忽略此版本" |

---

## 8. 命令速查

```bash
# 构建 / 测试
cargo build --release --bin IslandHop
cargo test --release --lib                        # 全量（并行，约 25s）
cargo test --release --lib settings_theme         # 单测

# 出包（只 .app / 连 dmg）
bash packaging/macos-app.sh --app-only
bash packaging/macos-app.sh

# 发版（先 bump Cargo.toml 两处 + CHANGELOG）
bash packaging/push-update.sh "说明1" "说明2"

# 换装本机
pkill -f IslandHop; sleep 2
rm -rf /Applications/IslandHop.app && cp -R IslandHop.app /Applications/
open /Applications/IslandHop.app

# 校验线上包（走 IP，域名当前不通）
curl --noproxy '*' -fsS https://203.195.201.85/home/updates/islandhop/manifest.json
shasum -a 256 dist/IslandHop-v0.6.68-macos.zip
```

---

## 9. 第一天建议做的三件事

1. `cargo test --release --lib` 跑一遍确认 904/0 —— 基线不绿先别改代码；
2. 真机启动一次，截图刘海区（`screencapture -x -R 400,0,700,180`）确认：刘海黑区内无内容、
   左电源+宠物、右额度+天气；
3. 从 §7 的 **P0 第二条（设置页 Windows 文案）** 入手 —— 纯 `cfg`/文案改动，风险最低，
   顺手能跑通「改代码 → 测试 → 出包 → OTA」整条链路。
