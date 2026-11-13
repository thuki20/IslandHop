# IslandHop

基于 [xiaotang-daozai（岛仔）](https://github.com/Renly1994/xiaotang-daozai) **v0.6.64 全量派生**的桌面岛：
以岛仔源码为主干、功能与整套 UI 不删不重写，加入 OpenHop 的**局域网键鼠共享 + 跨机剪贴板同步**。

- 产品名 **IslandHop**；上游 102 个业务模块、vendored egui/eframe、图标与桌面伙伴素材原样保留
- 双端策略 **B**：Windows 侧保真，Mac 平台层逐模块补齐，不砍功能、不拿占位冒充
- 许可 MIT：上游版权行与 `THIRD_PARTY_NOTICES.txt` 完整保留；上游原文见 `UPSTREAM_README.md`
- 与旧仓库 `thuki20/island-hop`（自绘简化版 v0.1.0）**无继承关系**，旧项目作废、不删不归档

---

## TL;DR（2026-11-13）

| 阶段 | 状态 | 证据 |
|---|---|---|
| 上游 v0.6.64 全量落地 | ✅ | tarball 521 文件 / 9.3 MB，关键文件字节数与 GitHub API 一致 |
| 新仓库 `thuki20/IslandHop` | ✅ | 已创建，公开，`main` |
| 上游在 macOS 编译 | ✅ | 修掉上游 2 处漏 `cfg` 守卫后 `cargo check` RC=0 |
| 挂入 `openhop-core` 并双 crate 共存 | ✅ | 补 `web/phone/` 内嵌资源后 RC=0，egui 零冲突 |
| **OpenHop 引擎接入** | ✅ | 旧端口组曾实机冒烟（TCP 24850 LISTEN / UDP 24851 绑定）；本轮隔离到 24950/24951 后 `cargo check` + **54 项单测通过**；引擎监听未重跑实测（见下） |
| **`.app` 启动冒烟（Mac）** | ✅ | 10 s 存活、stderr 空、窗口 `layer=25 / y=0`、旧 OpenHop 配置哈希未变、24850 仍归 OpenHop 且 24950 空闲 |
| **跨屏控制台窗口** | ✅ | 独立 viewport，620×688，可配对/改角色/排屏幕 |
| **Mac 主岛窗口贴顶** | ✅ | `y=38 / layer=3` → **`y=0 / layer=25`**（CGWindowList 实测） |
| **与旧版 OpenHop 隔离** | ✅ | 独立端口 24950/24951、独立配置目录、macOS 数据目录修复；不再抢端口、不回写旧配置 |
| **macOS 应用包** | ✅ | 本机产出 `IslandHop.app` + 8.7 MB dmg（ad-hoc 签名 / arm64 / 图标 / Info.plist 校验通过） |
| Mac 平台层全量重写 | ⏳ | **261 处 `cfg(windows)`、0 处 macOS 分支**，工单见下 |
| Windows 端编译 / 出包 | ⏳ | 本机无 MSVC/mingw，交 GitHub Actions；本轮 CI 运行中 |
| 双机端到端实测 | ⏳ | **跨屏切换与剪贴板同步从未实测，不报通过** |

> ⚠️ **最关键的一句**：`cargo check` 通过 ≠ 岛仔功能在 Mac 上可用。上游非 Windows 分支基本是**空实现**
> （`smtc::query → None`、`clipboard::watch → 空 Watcher`、`display_layout::connected → Vec::new()`）。
> Windows 侧是原样全量；Mac 侧目前拿到的是完整 UI 外壳 + 可跑的跨屏引擎，平台数据源要逐模块重写。

---

## 架构

```
IslandHop/
├─ src/                     岛仔 102 个模块（一字未改）
│  ├─ cross_screen.rs       ← 新增：OpenHop 引擎后台线程 + 独立控制台窗口
│  └─ mac_window.rs         ← 新增：macOS NSWindow 层级 / 刘海几何
├─ vendor/                  岛仔 5 个魔改库（原样，[patch.crates-io] 不变）
├─ assets/ docs/ installer/ scripts/ tests/   上游原样
├─ crates/openhop-core/     ← 新增：OpenHop 纯 Rust 引擎（54 文件，零 egui 依赖）
├─ web/phone/               ← 新增：引擎 include_str! 依赖的手机页资源
└─ .github/workflows/
   ├─ ci.yml                上游原样（Windows 测试 + NSIS 安装包）
   └─ islandhop-build.yml   ← 新增：Windows x64 zip 构建
```

| 挂载点 | 做法 |
|---|---|
| 引擎 | `Shell::new` 里 `CrossScreen::new()` 读配置并 `Engine::start`，`update()` 每帧只做一次 `status()` |
| 配置 | 沿用 `Config::default_path()` = `~/Library/Application Support/openhop/config.toml`，**自动继承旧 OpenHop 的配对 / 口令 / 屏幕排列**（本机已确认该文件存在） |
| 界面 | 上游把页数硬编码成 `[bool; 9]` / `[usize; 9]` 并散落在大量 `match page` 与 612KB 录制测试里；硬插第 10 页会破坏设置持久化与测试基线，故用**独立 viewport 控制台**，不动上游 UI 一行 |
| 剪贴板 | OpenHop 引擎内部已有回声抑制；岛仔剪贴板历史会把对端写入也记一条（历史污染，不是同步循环） |
| 依赖并存 | `windows` 0.58（岛仔）与 0.62.2（core）、`tungstenite` 0.28/0.30 可共存；边界只传裸句柄。**egui/eframe 零冲突**，这是方案成立的关键 |

---

## 对上游源码做了什么（精确清单）

| 文件 | 改动 |
|---|---|
| `src/` 102 个业务模块、`vendor/`、`assets/`、`docs/` | **一字未改** |
| `src/main.rs` | ① 修上游 `network` / `volume` 两处漏 `#[cfg(windows)]` 守卫（原样在 macOS 编译报 `no field system_state`）；② `Shell` 加 `cross` / `elevated` 字段；③ `update` 参数 `_frame → frame`，加 elevate、快捷键、控制台调用 |
| `src/lib.rs` | 加 2 行 `pub mod cross_screen;` / `pub mod mac_window;` |
| `Cargo.toml` | 加 `[workspace]` + `openhop-core` 路径依赖 + macOS `objc2`；二进制名 `daozai → IslandHop`；**lib crate 名保持 `daozai`**（102 个模块全用 `daozai::` 引用，改名会全量失效） |
| `Cargo.lock` | cargo 自动加 `objc2 0.6.5` 等 |
| `LICENSE` | 完整保留上游 MIT 与版权行，追加 IslandHop 派生说明 |
| `README.md` / `UPSTREAM_README.md` | 本文件重写；上游 README 原样另存 |
| `.github/workflows/islandhop-build.yml` | 新增；上游 `ci.yml` 未改 |

已用 tarball 原始文件逐行 diff 核对，上面就是全部改动。

---

## 使用方法（当前可跑）

```bash
cargo build --release
./target/release/IslandHop --cross-screen     # 启动即打开跨屏控制台
./target/release/IslandHop                    # 只启动岛；Cmd/Ctrl + Shift + K 随时呼出控制台
```

控制台里可以：切换主控/受控、改名称与共享口令、看本机配对码、发现并配对同网段设备、IP 直连、调整屏幕左右排列、启停引擎、查看引擎错误与最近事件。

- 两端角色必须**相反**、共享口令一致、TCP **24950** / UDP **24951** 放行，**两端都要装 IslandHop**（端口与旧版 OpenHop 不通）
- macOS 需在「系统设置 → 隐私与安全性 → 辅助功能 / 输入监控」勾选 IslandHop 并**彻底退出重开**（TCC 按应用路径授权）
- 端口与配置已与旧版 OpenHop 隔离（24950/24951 vs 24850/24851），不会抢端口；但两者都会注入键鼠事件，**实际跨屏时请只启用其中一个**
- 配置落在 `~/Library/Application Support/IslandHop/config.toml`，岛仔自身数据落 `~/Library/Application Support/IslandHop/`；旧版 `openhop/config.toml`（含共享口令与屏幕排列）**不会被读取或回写**

---

## 打包

```bash
# macOS（本机，arm64）
python3 packaging/make-icon.py          # 生成 AppIcon.{png,ico,icns}，纯标准库 + sips/iconutil
cargo build --release --bin IslandHop
bash packaging/macos-app.sh             # 产出 IslandHop.app + dist/IslandHop-macOS-arm64.dmg
```

`.app` 为 ad-hoc 签名、带 Dock 图标（`LSUIElement` 故意不设，否则菜单栏与 Dock 图标一起消失）；
岛窗口本身由 `mac_window.rs` 抬到 `NSStatusWindowLevel = 25`。Windows 端 `app.rc` 已换成 IslandHop
图标与产品名，`build.rs` 调 `rc.exe` 编进 exe；zip 由 `.github/workflows/islandhop-build.yml` 产出。

> 未做开发者签名 / 公证，也未创建 GitHub Release、未上传任何资产。

---

## Mac 平台层缺口清单（策略 B 工单）

统计口径：`src/**/*.rs` 里 `cfg(windows)` **261** 处、`cfg(not(windows))` **59** 处、`target_os = "macos"` **0** 处。

| 分组 | 模块数 | Windows 分支 | 代表文件 | Mac 现状 / 需做 |
|---|---|---|---|---|
| 媒体 / 音乐 | 4 | 66 | `smtc.rs` 86 KB、`lyrics.rs` 64 KB | 空实现；需 MediaRemote / AppleScript 重写，网易云等客户端需逐家适配 |
| 通知 / IM 未读 | 3 | 23 | `messenger.rs`、`input.rs`、`system.rs` | 微信/QQ 未读靠 Windows 任务栏闪动，**Mac 无等价公开接口，无解** |
| 托盘 / 窗口 / 毛玻璃 | 3+ | 4+ | `display_layout.rs`、`avoidance.rs`、`native`、`tray`、`backdrop` | `native/tray/backdrop/composition` 整个模块 `#[cfg(windows)]` 不存在；需 NSStatusItem / SMAppService / 自绘模糊 |
| AI 额度 | 11 | 34 | `grok_quota.rs`、`gemini_quota.rs`、`api_quota.rs`… | 依赖 Chrome 调试协议与 Windows 路径，需逐个改 |
| 邮件 / 日程 | 3 | 7 | `mail.rs` 77 KB、`calendar.rs` 102 KB | 网络逻辑可搬；凭据 `keyring(windows-native)` 要换 Keychain |
| 剪贴板 / 置物架 | 1 | 7 | `clipboard.rs` | 监听是空 Watcher；需 NSPasteboard 轮询 |
| 更新 / 诊断 / 资源 | 4 | 12 | `updates.rs`、`diagnostics.rs` | Windows 安装器语义，需换成 dmg / Sparkle |
| 外壳 | 1 | 82 | `main.rs` | 已补窗口层级与定位；其余系统采样仍走 Windows |
| 图标 / 主题 | 2 | 12 | `icons.rs`、`theme.rs` | 部分资源路径 Windows 专属 |

**已实修的 Mac 项**：窗口层级（`NSStatusWindowLevel=25` + `collectionBehavior`）、主岛贴顶 `y=0`、OpenHop 引擎接入（含独立端口 / 独立配置目录）、`config_dir()` 补 macOS / Linux 标准路径（原来退化成相对路径 `./Daozai`）、刘海尺寸读取接口（`openhop_core::extras::system::notch()`，本机 185×32 pt）。

**尚未适配的视觉项**：岛体形状仍按上游 Windows 顶部居中胶囊自绘，**没有按刘海做「两侧分列 + 从刘海弹出」**。这是 `app.rs` / `shape.rs` 的绘制逻辑，属于 Mac 平台层工单，不是窗口坐标问题。

---

## 未实现 / 未验证（不拿占位冒充）

- Windows 端编译与安装包：CI 已配，本轮运行中
- macOS `.app` / dmg：本机包已产出，见上「打包」；**未做开发者签名 / 公证**
- 双机跨屏切换、剪贴板同步端到端：**至今未实测**
- 改端口后的**引擎监听实测**：未重跑。本轮 `.app` 冒烟把 `enabled` 设为 false，避免与用户正在使用的 OpenHop 抢键鼠、也避免 ad-hoc 签名触发 TCC 授权弹窗；因此 24950/24951 是否真正 LISTEN 尚未验证
- 窗口内部标题仍是上游「岛仔」文案，未做品牌文案替换（保持全量派生）
- 刘海外形、桌面伙伴在 Mac 的资源加载、托盘、开机自启、自动更新、DWM 毛玻璃
- 微信/QQ 未读提醒：Mac 无解，将明确标注「仅 Windows」
- 本轮已出本机 macOS 包，但**未创建 GitHub Release、未上传任何资产**

---

## 合规

上游 `LICENSE` 是标准 MIT，覆盖 "the Software and associated documentation files"，
源码、图标、桌面伙伴素材均在许可范围内。本项目保留完整版权行与许可正文，
随附上游 `THIRD_PARTY_NOTICES.txt`（3.0 MB），并以 IslandHop 命名避免与上游品牌混淆。
OpenHop 引擎同为 MIT。`THIRD_PARTY_NOTICES.md`（旧项目里那句「Logo/伙伴形象不随 MIT 授予」）
是此前**过度保守且不准确的判断**，本仓库不沿用。
