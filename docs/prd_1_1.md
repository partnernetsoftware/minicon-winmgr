# prd_1_1 — 外壳功能对齐 · 逐项现状

> 索引：1.1 · 树路径：`外壳功能对齐` · 上位文档：[`prd.md`](prd.md)
> 目的：把"成熟外壳该有的每一个面"落到**现状 / 入口 / 实现文件 / 已知限制**，供做或改功能前快速定位。

状态图例：✅ 已实现 · 🟡 部分 · ⬜ 未做

## 任务栏（`src/taskbar.rs`）
- ✅ 自绘效率条，**默认常显**（`MINICON_NO_TASKBAR=1` 隐藏），支持四边位置及明暗/自动主题，应用图标 + 标题；0.1.11 工作树时钟采用 `Www yyyy-mm-dd HH:MM`。
- ✅ **任务按钮：默认每窗口一个**（对齐 Alt+Tab）。
- ✅ **合并与密度（已实现，完整规则见 [`prd_6_1.md`](prd_6_1.md)）**：**合并窗口**三态（`从不合并` 默认 / `任务栏放不下时合并` / `始终合并`）；`始终/已满` 时按 **合并依据**（`按应用`＝exe 路径，默认；或 `按进程`＝pid）归并，组标签追加 `(+N)`，锚定**组内首次出现的存活窗口**。不合并时按 **密度阶梯**自适应：`D0 完整(112–200) → D1 紧凑(80–119) → D2 仅图标(40–48)`，相邻档间 **8px 迟滞死区**；D2 只画居中图标、不画标题（多窗口方块右上角标 `(+N)`），`never` 下降到 D2 则该应用局部折叠；仍放不下才 `»` 溢出（保底 ≥1 个可见按钮）。
- ✅ **按钮顺序稳定（不随焦点重排）**：按**首次出现顺序**排列、位置固定；`refresh()` 用 `stable_order()` 只把**新窗口追加到最右**。原理、Z 序坑与单测覆盖见 [`prd_2_1.md`](prd_2_1.md) §5.9。
- ✅ **任务窗口判定**（`src/window_snapshot.rs::is_app_window`）：可见 + 有标题 + 非本进程 + 无 owner（或有 `WS_EX_APPWINDOW`）+ 非 `WS_EX_TOOLWINDOW` + **非 DWM cloaked**（`DWMWA_CLOAKED`）。最后一条挡掉 Windows 保留的隐形窗口——例如开始菜单宿主的 `Windows.UI.Core.CoreWindow`（标题"开始"，无 owner、无 `WS_EX_TOOLWINDOW`、带 `WS_EX_NOACTIVATE`），否则它会变成任务栏上一个点不动也关不掉的幽灵按钮。
- ✅ **溢出自适应**：窗口多到放不下时显示 `N 可见 + » 折叠`，时钟区永不被覆盖；点 `»` 弹出未显示窗口。合并档下点组按钮：若组内窗口已在前台则弹出成员列表，否则切回锚窗口。
- ✅ **即时刷新**：`src/window_events.rs` 用 `SetWinEventHook`（create/destroy/show/hide/foreground/minimize，去抖 120ms）
  驱动刷新——关闭窗口后按钮立刻消失，不必等 30s 心跳；菜单动作后也立即 `refresh()`。
- ✅ 左键切换窗口（**激活↔最小化**：已是前台则最小化，否则还原并置前台）；右键窗口菜单（还原/最小化/最大化/关闭 + **添加到快速启动 / 从快速启动移除**，取不到 exe 或已满则置灰）。**分组按钮（组内 >1 窗口）**右键弹批量菜单：打开组内全部 / 全部还原 / 全部最小化 / 全部关闭 / 添加到快速启动。
- ✅ 键盘导航：**Alt+`** 唤出并聚焦，←/→ 选择、Enter 切换、Esc 取消（环绕逻辑有单测）；进入导航后 **`1..9` 直跳**第 n 个可见按钮（`digit_index`）。
- ✅ icon-only 悬停标题气泡：`src/bar_tip.rs`（常驻 `WS_POPUP`，无新线程），仅 icon-only 按钮出气泡；Full/Compact 档维持原 opt-in 缩略图预览。
- ✅ 快速启动：`src/quick_launch.rs`，**用户手动钉住**的应用（右键窗口菜单加入），持久化在 exe 旁 `quick-launch.txt`（`label<TAB>path`，可 `MINICON_QUICKLAUNCH_FILE` 覆盖），上限 **8**（常量 `MAX_PINS`）。图标取自 exe；取不到则显示首字。左键启动、右键"打开/移除"。
- ⛔ 列表按钮（≡）**已于 2026-10-14 退役**（业主指示，任务区左侧不再有该按钮）。`src/task_list.rs` 模块保留——`»` 溢出按钮与分组按钮仍用它弹窗，弹出分组窗口菜单、选中即切换。
- ✅ 设置入口：最左"滑杆"按钮 → `src/settings_ui.rs`。
- ✅ 右侧时间区：**时钟**（`Www yyyy-mm-dd HH:MM`，5s 定时器保证准点）+ 其左侧**通知区域条**（可经 `taskbar.tray` 关闭；
  两种运行形态下的图标与可点性见下节「通知区 / 托盘」）。**单击时钟**打开通知面板（含“时间设置”），不再保留独立“通知”按钮；**双击时钟**打开系统日期/时间设置（`ShellExecuteW` → `control.exe timedate.cpl`，与系统托盘时钟双击一致）；交互细节与降级注记见 [`prd_4_1.md`](prd_4_1.md) §4。
- ✅ 0.1.14 已公开发布：时钟旁的显示桌面按钮，点击最小化当前虚拟桌面应用窗口；范围与验收见 [0.1.14](archive/plan_0_1_14.md)。
- ✅ 多显示器：每屏一栏，任务列表共享，布局/hover/键盘光标按栏独立。
- ✅ AppBar 停靠（`SHAppBarMessage`）；无 explorer 中转时**自管工作区**（见可靠性文档）。
- ✅ **全屏让位（0.1.20 交付）**：全屏前台应用（mstsc 全屏、全屏视频等）所在显示器的任务栏退出置顶、降至底层，退出全屏或焦点离开后自动恢复；其他显示器不受影响。AppBar 模式同时响应 `ABN_FULLSCREENAPP`；事件缺失由时钟节拍兜底（约 5 秒）。判定与验收见 [0.1.20 计划](archive/plan_0_1_20.md)。
- ✅ 贴底：按**显示器物理边界** `rcMonitor` 锚定。
- 🟡 主题来自 `config`（`minicon.conf` / `MINICON_CONFIG`）。

## 通知区 / 托盘

0.1.5 按 [`prd_1_1_2.md`](prd_1_1_2.md) 实现应用通知图标与隐藏面板；兼容性、实机证据及限制见 [`plan_0_1_5.md`](archive/plan_0_1_5.md)。

| 行为 | 通知宿主模式 | 伴随 Explorer 模式 |
|---|---|---|
| 宿主 | 当前程序是已配置外壳，或显式 `MINICON_TRAY_HOST=1`；已有其他宿主时仍让其保留所有权 | 保留 Explorer 的 `Shell_TrayWnd` |
| 图标 | `Shell_NotifyIconW` 注册、部分更新、GUID 身份、隐藏状态与删除；拥有本进程图标副本 | 不绘制不可操作的应用占位，面板说明由 Windows 管理 |
| 布局 | 主条最多4项，按空间缩减；隐藏/溢出项进入分页面板 | 可打开系统设置入口 |
| 交互 | 版本0/3/4回调；鼠标左键、抬起、双击、右键与键盘激活；hover只显示本地提示 | 不伪造应用回调 |
| 生命周期 | `TaskbarCreated` 重注册、失效窗口/PID清理；按住期间延后重建，发送前复核回调身份 | 不抢占已有通知宿主 |

`tray` 是共享模型，`tray_host` 管协议与图标所有权，`tray_bar` 管绘制/几何/回调，`tray_popup` 管原生分页面板。音量/网络是设置入口，不是实时状态图标；电源与输入法指示器缺口单列。

## 程序菜单 / 启动器
- ✅ 启动器检索：`src/launcher.rs`（`--launch` / `--launch-run` CLI；`launch` 会写 recents）。
- ✅ 启动器 UI：`src/launcher_ui.rs`，**Alt+Space** 弹窗，按频次排序（`recents`）。
- ✅ 主菜单（ORB）：`src/start_menu.rs`，**应用列表 + Places（计算机/文档/下载/图片/设置）+ 电源（注销/重启/关机，默认"否"确认）**。
  **入口已从 UI 退役**（2026-10-14，任务栏上不再有那个点），功能仍可用配置键
  `taskbar.menu = true` / env `MINICON_START_MENU=1` 唤回。
- ✅ 菜单内“最近使用”与配置文件夹堆栈已实现，随隐藏菜单入口按需使用；⬜ 拖拽整理分类尚未实现。

## 运行对话框
- ✅ `src/run_dialog.rs`：**Win+R** 弹出；被占用时**回退 Ctrl+Alt+R**。0.1.4 交付中文原生输入、草稿保留、失败留窗、键盘/IME、历史及本地路径建议。
- ✅ 启动目标与参数分离，工作目录明确为用户目录；后台启动/历史/候选互不阻塞消息线程。运行历史独立存于 `%LOCALAPPDATA%\minicon-winmgr\run-history.json`，最多30条，可清空。
- 需求、交互及验收唯一来源为 [`prd_1_1_1.md`](prd_1_1_1.md)，实际证据和环境限制见 [`plan_0_1_4.md`](archive/plan_0_1_4.md)。物理混合 DPI 多屏/拔插仍待实机补验。

## 窗口管理
- ✅ Snap 热键：`src/hotkeys.rs`，`Win+←/→/↑` → 回退 `Ctrl+Alt+←/→/↑`（纯几何 `snap_rect` + 单测）。
- ✅ 悬停窗口预览：`src/hover_preview.rs`（0.1.10 已交付单个 DWM 缩略图关系；取消同步 PrintWindow，隐藏/换目标/销毁时释放，实测范围见[正式验收](archive/validation_0_1_10.md)）。**默认关闭**，`MINICON_HOVER_PREVIEW=1` 开启。
- ⬜ 堆叠窗口组。

## 设置
- ✅ `src/settings_panel.rs` 提供复用原生设置窗口与按钮，统一主题绘制，保留原生键盘与可访问语义；`src/settings_ui.rs` 负责入口及桌面管理确认：
  - 任务栏：显示任务栏（复选，重启后生效）
  - 任务栏：显示通知区域（仅作外壳时可点）（复选，即时生效）
  - 窗口合并：从不合并 / 任务栏放不下时合并 / 始终合并（三态单选）
  - 合并依据：按应用（exe）/ 按进程（pid）（单选，合并≠从不时可用）
  - 行为：悬停显示缩略图预览（复选，即时生效）
  - 白天/黑夜/自动主题、上下左右位置与 English/中文即时切换；配置详情保留来源信息，普通设置不显示环境变量名称。
  - Check / Update / Restart 手动更新；0.1.10 增加按需资源诊断，默认不采样。
  - 折叠的桌面管理动作：**接管桌面（立即生效）/ 还原系统桌面（立即生效）**（见 [`prd_3_1.md`](prd_3_1.md)）；0.1.10 确认为确定/取消，默认取消，支持 Esc 返回原按钮。
- ✅ 生效与持久化：控件更新内存配置，即时刷新适用项，由单个 worker 合并写回设置文件（0.1.10 默认 `~/winmgr.cnf`，旧文件迁移见 [配置规格 §6](prd_6_1.md#6-配置与持久化配置键唯一真值表)）；保留未知键/注释，原子替换前检查外部修改并备份。主题、语言及位置即时生效，任务栏显示状态明确提示重启后生效。优先级 `环境变量 > 设置文件 > minicon.conf > 默认`；不改写用户手写的 `minicon.conf`。
- ✅ 主题强调色：设置面板“选择强调色…”与桌面设置共用拾色对话框（0.1.26），按模式做对比度校验（见 [`prd_6_1.md`](prd_6_1.md) §5）。⬜ 每屏窗口过滤（见 [`prd_6_1.md`](prd_6_1.md) §10）。

## 多显示器
- ✅ `src/monitor.rs`：枚举工作区 + 物理边界；每屏一栏由 `taskbar` 落地。

## 个性化 / 主题
- ✅ `src/config.rs`：`key=value` 配置；**键清单与默认值以 [`prd_6_1.md`](prd_6_1.md) §6「配置键唯一真值表」为准**。

## 窗口外观
- ✅ `src/dwm.rs`：圆角（默认）+ 亚克力（`MINICON_ACRYLIC=1`）；不支持的 OS 优雅降级（Win Server 2022 实测均不支持）。
- ✅ 窗口类统一 `hCursor = IDC_ARROW`（否则悬停显示"忙"光标）。

## 外壳集成
- ✅ 受控替换 / 回滚：`src/shell_registry.rs` + CLI + 设置窗动作，见 [`prd_3_1.md`](prd_3_1.md)。
- ✅ **立即接管 / 立即归还**：`src/shell_swap.rs`。注册表只在登录时被读取，故"立即生效"必须**额外停止当前会话正在运行的外壳**
  （`EnumWindows` 找 `Shell_TrayWnd`/`Shell_SecondaryTrayWnd`/`Progman` → `WM_CLOSE` → 超时 `TerminateProcess`）；
  归还时按 exe 旁 `shell-backup.txt` 恢复注册表并**脱附**重启 `explorer.exe`。
  仅作用于当前会话桌面，**无备份记录时拒绝归还**。细节与不变量见 [`prd_3_1.md`](prd_3_1.md) §1b、[`prd_2_1.md`](prd_2_1.md) §5.10–12。
- ✅ **外壳身份保护**：`src/shell_launch.rs` —— 唯一的"打开"出口。`explorer.exe` **不带参数** = 当外壳（夺权），
  **带参数** = 文件浏览器；故 quick-launch 钉 / 开始菜单 Places / Run 对话框 / launcher 全部经 `plan()` 改写后再启动。
  `shell_swap::check_foreign_shell()` 在心跳里**只检测**（跳变时记一条日志）不自动夺回。见 [`prd_2_1.md`](prd_2_1.md) §5.13–15。

---

## 环境变量 / 开关（唯一真值表）
> 本节是全仓**环境变量清单的唯一真值源**；[README 索引](../readme.md) 只引用，不另列。

`MINICON_NO_TASKBAR=1`、`MINICON_TASKBAR=1|0`、`MINICON_TRAY=1|0`、`MINICON_ACRYLIC=1`、`MINICON_HOVER_PREVIEW=1`、
`MINICON_GROUP_TASKS=1`（≡ `taskbar.group=always`，优先于设置面板）、`MINICON_GROUP_BY=exe|pid`、
`MINICON_QUICKLAUNCH_FILE=path`、`MINICON_RECENTS_FILE=path`、`MINICON_HEARTBEAT_SECS=N`、`MINICON_LOG_DIR=path`、
`MINICON_ALT_SHELL=path`、`MINICON_TRAY_HOST=1|0`、`MINICON_CONFIG=path`、`MINICON_SETTINGS=path`（设置文件位置）、
`MINICON_CONFIRM_SHELL=yes`、`RUST_LOG=...`。

> 配置优先级：`环境变量 > 设置文件（0.1.10 起默认 ~/winmgr.cnf）> minicon.conf（手写）> 内置默认`。

## CLI
`--shell-status` / `--shell-list`（只读）、`--shell-set <explorer|alt|minicon|path>` / `--shell-replace` / `--shell-rollback <value|default>`（写入需 `MINICON_CONFIRM_SHELL=yes`）、`--launch` / `--launch-run`、`--help`。

**立即交接**（同样需 `MINICON_CONFIRM_SHELL=yes`）：

| 命令 | 效果 |
|------|------|
| `--shell-takeover` | 写注册表指向本程序 **并立即停止本会话外壳**（无需注销） |
| `--shell-give-back` | 按 `shell-backup.txt` 恢复注册表 **并立即重启 explorer**（无备份则拒绝） |

## 独立管理工具（规划）

Tskmgr 已自 .16 起公开（普通/管理员双入口、任务树与协作笔记），Sysmgr 未实施。Winmgr 提供工具入口和窗口对应进程定位，独立工具不创建任务栏、不接管 Shell；缺失或不可用时保留系统任务管理器入口。Tskmgr 专注进程及网络，Sysmgr 专注整机资源与硬件；范围与权限见[工具 PRD](prd_7_1.md)，顺序见[路线](roadmap_0_1.md)。
