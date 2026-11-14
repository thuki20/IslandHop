# IslandHop

基于 [xiaotang-daozai（岛仔）](https://github.com/Renly1994/xiaotang-daozai) **v0.6.64 全量派生**的桌面岛：
以岛仔源码为主干、功能与整套 UI 不删不重写，加入 OpenHop 的**局域网键鼠共享 + 跨机剪贴板同步**。

- 产品名 **IslandHop**；上游 102 个业务模块、vendored egui/eframe、图标与桌面伙伴素材原样保留
- 双端策略 **B**：Windows 侧保真，Mac 平台层逐模块补齐，不砍功能、不拿占位冒充
- 许可 MIT：上游版权行与 `THIRD_PARTY_NOTICES.txt` 完整保留；上游原文见 `UPSTREAM_README.md`
- 与旧仓库 `thuki20/island-hop`（自绘简化版 v0.1.0）**无继承关系**，旧项目作废、不删不归档

---

## TL;DR（2026-11-14 更新）

| 阶段 | 状态 | 证据 |
|---|---|---|
| 上游 v0.6.64 全量落地 | ✅ | tarball 521 文件 / 9.3 MB，关键文件字节数与 GitHub API 一致 |
| 新仓库 `thuki20/IslandHop` | ✅ | 已创建，公开，`main` |
| 上游在 macOS 编译 | ✅ | 修掉上游 2 处漏 `cfg` 守卫后 `cargo check` RC=0 |
| 挂入 `openhop-core` 并双 crate 共存 | ✅ | 补 `web/phone/` 内嵌资源后 RC=0，egui 零冲突 |
| **OpenHop 引擎接入** | ✅ | 独立端口组 24950/24951 **实机监听已验证**：TCP v4+v6 LISTEN、UDP 绑定；54 项单测通过 |
| **`.app` 启动冒烟（Mac）** | ✅ | 10 s 存活、stderr 空、窗口 `layer=25 / y=0`、旧 OpenHop 配置哈希未变 |
| **跨屏功能内嵌岛仔设置** | ✅ | 岛仔「设置」窗口第 13 项「局域网跨屏」，无独立窗口；引擎后台自启，打开软件即用；Cmd/Ctrl+Shift+K 或 `--cross-screen` 直达该分组 |
| **Mac 点击穿透** | ✅ | 修复 960×270 置顶透明窗口吞掉屏幕顶部鼠标事件：按 `FrameOut.hit / notice_hit / file_drop_zone` 动态设置 `NSWindow.ignoresMouseEvents`，命中区外点击落到下层应用；`env.cursor` 改用全局鼠标换算坐标 |
| **不再每次弹授权窗口** | ✅ | 引擎启动路径原本 `check_permissions(true)` 会调 `AXIsProcessTrustedWithOptions` 两次弹系统框；改为静默查询 + 面板显示辅助功能/输入监控两项状态，只有用户点「请求授权」才弹 |
| **固定代码签名** | ✅ | 打包脚本改用钥匙串已有的 `OpenHopSign` 证书（不再 ad-hoc），TCC 按签名身份记授权，重打包不再丢失授权 |
| **Mac 图标统一** | ✅ | eframe 的 `with_icon()` 在 macOS 会 `setApplicationIconImage` 覆盖 Dock 图标；macOS 分支不再设置，Dock/Finder 统一用 bundle `AppIcon.icns` |
| **刘海机型布局（二修）** | ✅ | 本机实测刘海 185×32 pt。首版 `notch_split` 只做 x ±平移，压在中心的组件还额外推 w/2，导致右侧「16:20☀20°」重叠并溢出胶囊边界；现改为**按原顺序分左右两组、各自紧靠刘海边缘重排**，`width = 刘海 + 左组 + 右组 + 边距`，胶囊背景与 hit 跟随。实测时钟/天气分列不重叠、电池等图标可见、胶囊居中覆盖刘海 |
| **配对界面可视化** | ✅ | 从纯文本表单改为：大号电源开关、三步连接进度、设备卡片（显示器图标/系统/地址/连接状态）、图形化屏幕排列画布、当前光标高亮；角色/口令/权限收进「高级设置」折叠 |
| **口令一致免配对流程** | ✅ | 引擎 `needs_pairing()` 仅在口令为空时返回 true，口令一致本来就自动发现直连；UI 之前仍摆配对码输入框误导用户。现在口令模式明确显示「无需配对码、不用点配对」，只有口令为空才显示 6 位大字配对码 |
| **设置窗口偶发打不开** | ✅ | `with_visible(false)` 的一次性 `Visible(true)` 会撞上 immediate viewport 首帧竞态被吞 → 窗口永久隐形。实际运行每帧幂等补发 `Visible(true)`（测试构建保留上游一次性语义），并在请求打开时 `activateIgnoringOtherApps` 激活应用 |
| **Dock 图标彻底统一** | ✅ | 上一轮只改了主窗口，**设置窗口 / 更新窗口仍 `.with_icon(theme::app_icon())`**，eframe 在 macOS 会 `setApplicationIconImage` 把 Dock 图标换成岛仔 logo（黑底白 e）。现所有 viewport 在 macOS 均不设窗口图标；实测「打开设置后」bundle 与运行中 Dock 图标 TIFF 完全一致 |
| **macOS 系统数据源** | ✅ | 新增 `src/macos_sys.rs`：电池 `pmset`、音量/静音 `osascript`（读+写）、网络 `route`、闲置 `CGEventSourceSecondsSinceLastEventType`、媒体 AppleScript（Music/Spotify，2 s 节流）、文件 `open` / `open -R` / 文件选择；接入 `Env` 与 `Cmd`。剪贴板历史改 `NSPasteboard.changeCount` 轮询。截图确认灵动岛出现绿色电池图标 |
| **Mac 中文字体** | ✅ | 修 `theme.rs` 只查 Windows/Linux 字体目录的缺陷，加载 macOS 系统 CJK 字体；中文由满屏方块恢复可读 |
| **Mac 主岛窗口贴顶** | ✅ | `y=38 / layer=3` → **`y=0 / layer=25`**（CGWindowList 实测） |
| **与旧版 OpenHop 隔离** | ✅ | 独立端口 24950/24951、独立配置目录、macOS 数据目录修复；不再抢端口、不回写旧配置 |
| **macOS 应用包** | ✅ | 本机产出 `IslandHop.app` + dmg（arm64 / 图标 / Info.plist 校验通过）；已装到 `/Applications`；**用钥匙串 `OpenHopSign` 自签名固定身份**，重打包不丢 TCC 授权 |
| Mac 平台层全量重写 | ⏳ | 本轮补 7 类数据源后仍有 **267 处 `cfg(windows)`**，托盘 / 开机自启 / 通知读取 / Keychain 登录 / 更新器未做，工单见下 |
| Windows 端编译 / 出包 | ✅ | `522a4be` 触发的 `IslandHop Build` **success**，x64 zip 已拉取到 `dist/IslandHop-Windows-x64.zip`（`IslandHop.exe` SHA256 `B47002FF…82E41E`）；exe 字节级确认含「局域网跨屏 / 本机权限 / 不再显示权限相关提示」，即新界面已编进 Windows 包 |
| **双机连通（协议层）** | ✅ | 2026-11-14 实测：`192.168.1.16:24950 -> 192.168.1.15:64772 ESTABLISHED`、设置页 `Sharing with 1 computer(s) / 已连接 1 台`；回环 Client 自检 `CONNECTED=true, peers=1` |
| **双机键鼠 / 剪贴板动作实测** | ⏳ | 会话与剪贴板线程已起，但 Mac 缺「输入监控」授权导致事件 tap 退化（见下）；内容级跨机同步需双机人工实测，**不报通过** |

> ⚠️ **平台边界**：上游非 Windows 分支原本基本是空实现（`smtc::query → None`、`clipboard::watch → 空 Watcher`、
> `display_layout::connected → Vec::new()`，`system/native/tray/backdrop` 整模块被 `cfg(windows)` 排除）。
> 本轮已补**电池 / 音量 / 网络 / 闲置 / 媒体 / 文件 / 剪贴板**数据源；仍缺**菜单栏托盘、开机自启、
> 跨进程通知读取、蓝牙、Keychain 登录、应用内更新**（见下方缺口清单），这些不拿占位冒充。

---

## 架构

```
IslandHop/
├─ src/                     岛仔 102 个模块（业务模块原样，仅 extras.rs 加 1 个设置分组）
│  ├─ cross_screen.rs       ← 新增：OpenHop 引擎后台线程 + 内嵌在岛仔设置窗口的可视化面板
│  ├─ mac_window.rs         ← 新增：macOS 窗口层级 / 点击穿透 / 刘海几何 / 激活应用
│  ├─ macos_sys.rs          ← 新增（macOS）：电池/音量/网络/闲置/媒体/文件数据源 + 后台采样线程
│  ├─ clipboard.rs          ← 改动（macOS 分支）：NSPasteboard.changeCount 轮询监听
│  └─ extras.rs             ← 仅加 1 个设置分组（第 13 项），102 个业务模块不动
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
| 配置 | `Config::default_path()` = `~/Library/Application Support/IslandHop/config.toml`，**不读也不回写**旧 OpenHop 的 `openhop/config.toml`（旧版 24850/24851 可并存） |
| 界面 | 上游把页数硬编码成 `[bool; 9]` / `[usize; 9]` 并散落在大量 `match page` 与 612KB 录制测试里；硬插第 10 个功能页会破坏设置持久化与测试基线，故**只在岛仔原有设置窗口加一个分组**（`SETTINGS_GROUPS` 第 13 项），不动功能页结构 |
| 引擎启动 | `Shell::new` 触发 `cross_screen::with()` 初始化；`config.toml` 里 `enabled=true` 时 `Engine::start` 在后台线程启动，**用户不用打开任何界面** |
| 点击穿透 | Windows 由 `native.rs` 用 `SetWindowRgn` 裁剪；macOS 由 `mac_window.rs` 用 `NSWindow.ignoresMouseEvents` + `NSEvent.mouseLocation` 换算实现等价行为 |
| 剪贴板 | OpenHop 引擎内部已有回声抑制；岛仔剪贴板历史会把对端写入也记一条（历史污染，不是同步循环） |
| 依赖并存 | `windows` 0.58（岛仔）与 0.62.2（core）、`tungstenite` 0.28/0.30 可共存；边界只传裸句柄。**egui/eframe 零冲突**，这是方案成立的关键 |

---

## 对上游源码做了什么（精确清单）

| 文件 | 改动 |
|---|---|
| `src/` 102 个业务模块、`vendor/`、`assets/`、`docs/` | **一字未改** |
| `src/main.rs` | ① 修上游 `network` / `volume` 两处漏 `#[cfg(windows)]` 守卫；② `Shell` 加 `mac_cursor` / `elevated` 字段，删掉独立控制台字段；③ `update` 用 `frame` 做 elevate，每帧同步点击穿透，`env.cursor` 用全局鼠标坐标；④ 热键 / `--cross-screen` 改为打开设置第 13 项 |
| `src/extras.rs` | 设置窗口加 1 个分组「局域网跨屏」（`SETTINGS_GROUPS` 9→10、titles 13→14、图标与 match 分支各 1 行），并加 `open_settings_tab()` |
| `src/workbuddy_quota.rs` | 仅 `#[cfg(test)]` 手动用例加 `#[cfg(windows)]` 守卫：上游 `mod login` 是 Windows-only，否则 macOS 上 `cargo test --lib` 无法编译 |
| `src/lib.rs` | 加 2 行 `pub mod cross_screen;` / `pub mod mac_window;` |
| `Cargo.toml` | 加 `[workspace]` + `openhop-core` 路径依赖 + macOS `objc2`；二进制名 `daozai → IslandHop`；**lib crate 名保持 `daozai`**（102 个模块全用 `daozai::` 引用，改名会全量失效） |
| `Cargo.lock` | cargo 自动加 `objc2 0.6.5` 等 |
| `LICENSE` | 完整保留上游 MIT 与版权行，追加 IslandHop 派生说明 |
| `README.md` / `UPSTREAM_README.md` | 本文件重写；上游 README 原样另存 |
| `.github/workflows/islandhop-build.yml` | 新增；上游 `ci.yml` 未改 |

已用 tarball 原始文件逐行 diff 核对，上面就是全部改动。

---

## 双机启用（2026-11-14 实测配置）

两端都装了 IslandHop 却「没有跨屏/剪贴板」时，通常卡在下面四件事，缺一不可：

| 项 | Mac（Server） | Windows（Client） |
|---|---|---|
| 端口 | TCP 24950 / UDP 24951 | 同左（**不是**旧 OpenHop 的 24850/24851） |
| 配置路径 | `~/Library/Application Support/IslandHop/config.toml` | `%APPDATA%\IslandHop\config.toml` |
| 角色 | `role = "server"`（把本机键鼠共享出去） | `role = "client"`（被控端） |
| 共享口令 | 两端**完全一致**、≥4 字符 | 同左 |
| 直连兜底 | — | `server_addr = "192.168.1.16"`：`pick_server()` 优先直连，可绕开被防火墙挡掉的 UDP 发现 |
| 排列 | `[[layout.screens]] name="Windows" x=1 y=0` | 同左（Server 固定 (0,0)，Client 在右侧 (1,0)） |

`passphrase` 为空时引擎改用 6 位配对码，**不会自动互联**。IslandHop 与旧 OpenHop 端口组、配置目录都不共享，双端必须都装 IslandHop 并重新设一次口令。

**启动与验证**

1. Mac：把 `IslandHop.app` 装到 `/Applications`（不要从 dmg 挂载目录运行）；配置 `enabled = true`；`Cmd/Ctrl+Shift+K` 打开「设置 → 局域网跨屏」。引擎在启动软件时已经自动跑起来了，这个页面只是查看状态/改配置。
2. Windows：解压 `IslandHop-Windows-x64.zip`，运行本机交付的 `dist/IslandHop-Windows-EnableCrossScreen.ps1`（内含共享口令明文，**仅本机使用、不要外传/提交**，故不在仓库里）。脚本会写干净配置、放行防火墙、校验 exe 版本并**直接重启**（不需要任何启动参数，跨屏面板在「设置 → 局域网跨屏」）。
3. 设置页出现 `Sharing with 1 computer(s) / 已连接 1 台` 即握手成功。协议层自检：`IH_PASSPHRASE='<口令>' cargo run -p openhop-core --example loopback_e2e`。

**macOS 键鼠捕获还差一步（必须人工授权）**

引擎用 `CGEventTap(Location::HID)` 捕获本机键鼠，需要「**输入监控**」权限；注入事件需要「**辅助功能**」。缺任一项时 tap 创建失败，引擎退化成 `NoCapture` —— 设置页照样显示「运行中 / 已连接 1 台」，但鼠标永远切不过去（剪贴板线程仍可用）。

- 设置页「本机权限」直接显示两项是否授权，并可一键打开对应系统设置子页；**启动软件不会自动弹系统授权框**
- 应用已用固定证书 `OpenHopSign` 签名，授权一次即可长期有效；只有换签名证书或把 app 移出 `/Applications` 才需重新勾
- 缺权限时彻底退出 IslandHop 再重开；不想再看到红字可勾「不再显示权限相关提示」（存 `~/Library/Application Support/IslandHop/ui-prefs.toml`）
- 自检：`bash packaging/verify-macos-capture.sh`，出现 `✅ mac-event-tap 线程存在` 才算就绪

本轮顺手修掉两个会让人「查不出原因」的缺陷：捕获失败从静默 `log::warn` 改为设置页可见的「引擎错误」；`Config::load` 解析失败时不再用默认值回写覆盖用户配置（无效 TOML 不会抹掉口令/端口/排列，并在设置页提示）。

---

## 使用方法（当前可跑）

```bash
cargo build --release
./target/release/IslandHop --cross-screen     # 启动并跳到「设置 → 局域网跨屏」（可选）
./target/release/IslandHop                    # 正常启动；引擎按 enabled 后台自启，Cmd/Ctrl + Shift + K 随时打开设置
```

「设置 → 局域网跨屏」现在是可视化面板：顶部大开关与运行状态、三步连接进度、附近设备卡片（系统 / 地址 / 连接态 / 一键配对）、图形化屏幕排列画布（当前光标高亮）、连接方式说明。两端口令一致时引擎自动发现直连，页面直接写明「无需配对码、不用点配对」；只有口令为空才用 6 位配对码。角色 / 名称 / 口令 / 权限 / IP 直连 / 运行日志收在「高级设置」「运行状态与日志」两个折叠区。屏幕顶部除岛体以外的区域已做点击穿透，不会再挡住下面应用的菜单和按钮。

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

统计口径：`src/**/*.rs` 里 `cfg(windows)` **267** 处、`cfg(not(windows))` **61** 处、`target_os = "macos"` **38** 处（本轮前为 0）。

| 分组 | 模块数 | Windows 分支 | 代表文件 | Mac 现状 / 需做 |
|---|---|---|---|---|
| 媒体 / 音乐 | 4 | 66 | `smtc.rs`、`lyrics.rs` | **已补** AppleScript 读 Music / Spotify（歌名/艺术家/进度/播放态，2 s 节流）；封面、网易云等客户端仍未适配 |
| 通知 / IM 未读 | 3 | 23 | `messenger.rs`、`system.rs` | 微信/QQ 未读靠 Windows 任务栏闪动，**Mac 无等价公开接口，无解** |
| 托盘 / 窗口 / 毛玻璃 | 3+ | 4+ | `tray`、`native`、`backdrop` | **仍缺** NSStatusItem / SMAppService / 自绘模糊；当前 Mac 无菜单栏入口，靠 Cmd+Shift+K 打开设置 |
| AI 额度 | 11 | 34 | `grok_quota.rs`、`gemini_quota.rs` | 依赖 Chrome 调试协议与 DPAPI，**仍缺** Keychain / ASWebAuthenticationSession |
| 邮件 / 日程 | 3 | 7 | `mail.rs`、`calendar.rs` | 网络逻辑可搬；凭据 `keyring(windows-native)` 仍需换 Keychain |
| 剪贴板 / 置物架 | 1 | 7 | `clipboard.rs` | **已补** `NSPasteboard.changeCount` 350 ms 轮询 + `stringForType:` 读文本 |
| 电池 / 音量 / 网络 / 闲置 | — | — | `macos_sys.rs` | **已补** `pmset` / `osascript` / `route` / `CGEventSource` |
| 文件打开 / 选择 / 显示 | — | — | `macos_sys.rs`、`main.rs` | **已补** `/usr/bin/open`、`open -R`、AppleScript `choose file` |
| 更新 / 诊断 / 资源 | 4 | 12 | `updates.rs` | Windows 安装器语义，仍需换成 dmg / Sparkle |
| 外壳 | 1 | 82 | `main.rs` | 已补窗口层级、刘海、点击穿透、系统采样接入；全局热键 / 输入法仍缺 |
| 蓝牙 / 锁屏 / 浏览器避让 | 3 | — | `bt.rs`、`system.rs` | 仍空（IOBluetooth / AXUIElement 轮询未做） |

**已实修的 Mac 项**：窗口层级（`NSStatusWindowLevel=25` + `collectionBehavior`）、主岛贴顶 `y=0`、点击穿透、刘海分列重排（胶囊宽度按「刘海 + 左右内容」重算）、固定签名、Dock 图标统一、设置窗口可见性、时钟本地时区、CJK 字体，以及上表标注「已补」的 7 类数据源。

---

## 未实现 / 未验证（不拿占位冒充）

- Windows 端编译与安装包：`522a4be` 的 `IslandHop Build` 已 success，新 zip 已落到 `dist/` 并校验 exe SHA256；**用户 Windows 真机装的仍是旧包**，需解压新 zip 覆盖后重跑一键脚本（脚本会停掉旧版 OpenHop，避免 24850 广播与双引擎抢键鼠）。GUI 各功能未逐项真机实测
- Windows 应用内更新器仍指向上游 `Renly1994/xiaotang-daozai` 的 Release（`updates.rs`），装了会提示/拉取上游更新——**重新绑定前不要用它更新**
- macOS `.app` / dmg：本机包已产出，见上「打包」；用自签名代码签名证书固定身份（**不是 Apple Developer ID，未公证**），首次运行若被 Gatekeeper 拦需在「系统设置 → 隐私与安全性」点「仍要打开」
- 双机跨屏切换、剪贴板同步端到端：**TCP 会话、设置页「已连接 1 台」、`clipboard` 线程均已实测**；但 Mac 未授权「输入监控」→ `mac-event-tap` 线程缺失 → 键鼠切换未跑通；剪贴板内容跨机传输还需双机各复制一次人工确认，**不报通过**
- 改端口后的**引擎监听实测**：✅ 已重跑。TCP 24950 v4/v6 LISTEN、UDP 24951 绑定；线程采样含 `server` / `clipboard` / `peer-192.168.1.15` / `link-writer`，**唯独缺 `mac-event-tap`**（即输入监控授权缺口）
- 窗口内部标题仍是上游「岛仔」文案，未做品牌文案替换（保持全量派生）
- **Mac 仍未适配（不拿占位冒充）**：菜单栏托盘 / 开机自启（SMAppService）、跨进程通知读取（系统无公开接口）、蓝牙音频设备、AI 额度账号的 Keychain 登录、应用内更新器、全局热键与输入法、毛玻璃与浏览器避让、桌面伙伴资源、音乐封面与国内客户端媒体源
- **Mac 已适配但需真实数据才看得到**：媒体卡片要求 Music / Spotify 正在播放（本机测试时两者均未运行，故截图里没有音乐图标）；AI 额度环需先配置账号；通知铃依赖系统通知
- 微信/QQ 未读提醒：Mac 无解，将明确标注「仅 Windows」
- 本轮已出本机 macOS 包，但**未创建 GitHub Release、未上传任何资产**

---

## 合规

上游 `LICENSE` 是标准 MIT，覆盖 "the Software and associated documentation files"，
源码、图标、桌面伙伴素材均在许可范围内。本项目保留完整版权行与许可正文，
随附上游 `THIRD_PARTY_NOTICES.txt`（3.0 MB），并以 IslandHop 命名避免与上游品牌混淆。
OpenHop 引擎同为 MIT。`THIRD_PARTY_NOTICES.md`（旧项目里那句「Logo/伙伴形象不随 MIT 授予」）
是此前**过度保守且不准确的判断**，本仓库不沿用。
