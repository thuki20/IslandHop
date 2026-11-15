# 上游同步（岛仔 xiaotang-daozai → IslandHop）

IslandHop 是 [`Renly1994/xiaotang-daozai`](https://github.com/Renly1994/xiaotang-daozai) v0.6.64 的**整树派生**，
之后我们加了 macOS 窗口层、OpenHop 局域网键鼠/剪贴板、双机数据同步等能力。

## 为什么不用 `git merge upstream/main`

第一次提交就把上游历史压成了一个快照（仓库里没有上游的 commit 对象），
两边**没有共同祖先**，`git merge` 会直接拒绝。所以采用「三份快照比对」：

| 快照 | 来源 |
| --- | --- |
| 基线 | `.upstream-base` 记录的版本（当前 `v0.6.64`） |
| 上游最新 | 上游 `main`（或指定 tag）的 tarball |
| 我方 | 当前工作区 |

逐文件判断：

- 我方文件 == 基线 ⇒ 我们没动过，上游的改动可以**整份吃进来**；
- 我方文件 ≠ 基线 且 ≠ 上游 ⇒ **双方都改过**，列入人工合并清单，报告里附 diff。

## 用法

```bash
bash scripts/upstream-sync.sh                  # 只出报告（upstream-report.md），不动代码
bash scripts/upstream-sync.sh report v0.6.70   # 指定上游版本
bash scripts/upstream-sync.sh apply            # 落盘「可直接应用」的文件
bash scripts/upstream-sync.sh apply --test     # 落盘后自动跑 cargo test --lib
```

报告结构：上游提交列表 → 三类计数（可直接应用 / 需人工合并 / 跳过）→ 每个冲突文件的 diff。

`apply` 之后必须人工过一遍报告的冲突段，再按下面的发版流程提交。

## 文件归属（人工合并时的判断依据）

| 范围 | 归属 | 处理原则 |
| --- | --- | --- |
| `src/macos_*.rs`、`src/mac_window.rs`、`src/macos_sys.rs` | 我方独有 | 上游没有，直接跳过；若上游新增 macOS 分支，先评估再合并 |
| `src/bar_layout.rs`、`src/app.rs`、`src/extras.rs` 等 UI | 双方都动 | 以上游行为为准，叠加我方平台适配（刘海分列、防误触、缩放） |
| `src/sync_state.rs`、`src/workbuddy_quota.rs` + `workbuddy_login.rs` | 我方主要改动 | 上游的额度/同步逻辑照吃，我方跨机克隆与 macOS 登录保留 |
| `crates/openhop-core/**` | 我方集成 | 上游无此 crate；只跟随上游版本的接口变化（如剪贴板 API） |
| `.github/workflows/**` | 我方自有 | 上游 CI 永不同步（我们的 runner/签名/发布链路不同） |
| `packaging/**`、`scripts/upstream-sync.sh` | 我方自有 | 同上 |
| `vendor/**` | 跟随上游 | 上游若换 egui/eframe 补丁，按上游整份覆盖后再补回我们的 patch |

## 发版流程（同步后）

1. `bash scripts/upstream-sync.sh apply --test`，人工合完冲突；
2. 更新 `.upstream-base` 为这次吃到的上游版本（如 `v0.6.70`）；
3. `CHANGELOG.md` 顶部加一节，说明「同步上游 vX.Y.Z：修了哪些」；
4. `Cargo.toml` 两处 `version` 一起 bump；
5. 提交 + push：macOS OTA 与 Windows 安装包两条流水线各自出包并挂到同一个 Release。

## 自动化

- `.github/workflows/upstream-watch.yml`：每周一早上比对上游，有新提交就开 issue 提醒（同一时段只留一个 open issue）。
- 也可以在本地随时跑 `bash scripts/upstream-sync.sh` 看当前差多少。

`.upstream-cache/` 是下载缓存（已在 `.gitignore`），删掉不影响。
