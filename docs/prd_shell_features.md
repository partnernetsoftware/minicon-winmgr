# prd_外壳功能对齐 — 逐项现状

> 树路径：`外壳功能对齐` · 上位文档：[`prd.md`](prd.md)
> 目的：把"成熟外壳该有的每一个面"落到**现状 / 入口 / 实现文件 / 已知限制**，供做或改功能前快速定位。

状态图例：✅ 已实现 · 🟡 部分 · ⬜ 未做

## 任务栏（`src/taskbar.rs`）
- ✅ 自绘效率条，**默认常显**（`MINICON_NO_TASKBAR=1` 隐藏），36px、深色、文本优先。
- ✅ **任务按钮：默认每窗口一个**（对齐 Alt+Tab）；按 pid 分组为 **opt-in** `MINICON_GROUP_TASKS=1`。
- ✅ 左键切换窗口（最小化先还原）；右键窗口菜单（还原/最小化/最大化/关闭）。
- ✅ 键盘导航：**Alt+`** 唤出并聚焦，←/→ 选择、Enter 切换、Esc 取消（环绕逻辑有单测）。
- ✅ 快速启动：`src/quick_launch.rs`，把 **recents 前 N（≤5）** 钉到最左（无历史则空）。
- ✅ 列表按钮：`src/task_list.rs`，弹出全部分组窗口菜单，选中即切换。
- ✅ 设置入口：最左"滑杆"按钮 → `src/settings_ui.rs`。
- ✅ 右侧只读区：**仅时钟**（`yy-mm-dd HH:MM`，5s 定时器保证准点）+ 其左侧通知图标条。
- ✅ 多显示器：每屏一栏，任务列表共享，布局/hover/键盘光标按栏独立。
- ✅ AppBar 停靠（`SHAppBarMessage`）；无 explorer 中转时**自管工作区**（见可靠性文档）。
- ✅ 贴底：按**显示器物理边界** `rcMonitor` 锚定。
- 🟡 主题来自 `config`（`minicon.conf` / `MINICON_CONFIG`）。

## 通知区 / 托盘
- ✅ 镜像：`src/tray.rs`，companion 模式读现有 shell 的托盘（`Shell_TrayWnd→ToolbarWindow32`）；第三方外壳接管时读不到 → `count=0`。
- ✅ 图标条：`src/tray_bar.rs`，任务栏右区绘制镜像图标（无图标则不占位）。
- ✅ 宿主（替换模式）：`src/tray_host.rs`，`MINICON_TRAY_HOST=1` 或自身为 shell 时注册宿主，经 `WM_COPYDATA` → `NOTIFYICONDATAW`。
- ⬜ 通知区图标**点击转发**（把点击回投给宿主程序）——`tray_host` 进阶。

## 程序菜单 / 启动器
- ✅ 启动器检索：`src/launcher.rs`（`--launch` / `--launch-run` CLI；`launch` 会写 recents）。
- ✅ 启动器 UI：`src/launcher_ui.rs`，**Alt+Space** 弹窗，按频次排序（`recents`）。
- ✅ 主菜单（ORB）：`src/start_menu.rs`，品牌点唤出；**应用列表 + Places（计算机/文档/下载/图片/设置）+ 电源（注销/重启/关机，默认"否"确认）**。
- ⬜ 菜单内"最近使用"专区、拖拽整理分类。

## 运行对话框
- ✅ `src/run_dialog.rs`：**Win+R** 弹出；被占用时**回退 Ctrl+Alt+R**；Enter/OK 经 `ShellExecuteW` 启动。

## 窗口管理
- ✅ Snap 热键：`src/hotkeys.rs`，`Win+←/→/↑` → 回退 `Ctrl+Alt+←/→/↑`（纯几何 `snap_rect` + 单测）。
- ✅ 悬停窗口预览：`src/hover_preview.rs`（`PrintWindow(PW_RENDERFULLCONTENT)` 缩略图，`WM_TIMER` 节流，单窗口复用）。**默认关闭**，`MINICON_HOVER_PREVIEW=1` 开启。
- ⬜ 堆叠窗口组。

## 设置
- ✅ `src/settings_ui.rs`：**设置窗**（占位 5 行）+ **替代为外壳 / 回滚到原外壳** 两个动作（见替换文档）。
- ⬜ 真实设置项（主题/任务栏/行为/快捷键）接入。

## 多显示器
- ✅ `src/monitor.rs`：枚举工作区 + 物理边界；每屏一栏由 `taskbar` 落地。

## 个性化 / 主题
- ✅ `src/config.rs`：`key=value` 配置（`theme.bg|surface|hover|accent|text|muted`、`hotkey`、`show_taskbar`）。

## 窗口外观
- ✅ `src/dwm.rs`：圆角（默认）+ 亚克力（`MINICON_ACRYLIC=1`）；不支持的 OS 优雅降级（Win Server 2022 实测均不支持）。
- ✅ 窗口类统一 `hCursor = IDC_ARROW`（否则悬停显示"忙"光标）。

## 外壳集成
- ✅ 受控替换 / 回滚：`src/shell_registry.rs` + CLI + 设置窗动作，见 [`prd_shell_replace_rollback.md`](prd_shell_replace_rollback.md)。

---

## 环境变量 / 开关
`MINICON_NO_TASKBAR=1`、`MINICON_ACRYLIC=1`、`MINICON_HOVER_PREVIEW=1`、`MINICON_GROUP_TASKS=1`、
`MINICON_HEARTBEAT_SECS=N`、`MINICON_LOG_DIR=path`、`MINICON_ALT_SHELL=path`、`MINICON_TRAY_HOST=1|0`、
`MINICON_CONFIG=path`、`MINICON_CONFIRM_SHELL=yes`、`RUST_LOG=...`。

## CLI
`--shell-status` / `--shell-list`（只读）、`--shell-set <explorer|alt|minicon|path>` / `--shell-replace` / `--shell-rollback <value|default>`（写入需 `MINICON_CONFIRM_SHELL=yes`）、`--launch` / `--launch-run`、`--help`。
