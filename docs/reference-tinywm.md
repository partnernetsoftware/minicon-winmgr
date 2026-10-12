# 参考实现：tinywm（可借鉴清单）

来源：`D:\dev\moltbaby\tinywm`（Cosmopolitan APE + QuickJS 驱动的三平台 shell/窗口管理器）。它是 **替换型外壳**，我们是 **companion 外壳**，所以借鉴时要区分前提。

## 结论速览

| # | 想法 | 我们的适用性 | 价值 |
|---|------|--------------|------|
| 1 | **AppBar 停靠**（`SHAppBarMessage`） | ✅ 直接可用 | 高 |
| 2 | 托盘"宿主 vs 读取" | ⚠️ 前提不同 | 高（认知） |
| 3 | 绘制命令列表（`sys.draw(cmds[])`+`invalidate`） | 🟡 可选重构 | 中 |
| 4 | 窗口 Snap / 移动显示器热键 | ✅ 直接可用 | 高 |
| 5 | 声明式窗口规则 `wm.rules([{match,action}])` | ✅ 可用 | 中 |
| 6 | 任务栏/托盘右键菜单、Shell 菜单 | ✅ 可用 | 中 |
| 7 | JSON-RPC + `remote_eval`（可脚本化控制） | 🟡 利于测试/自动化 | 中 |
| 8 | `sys.toast` / `sys.shell_open` / `sys.exec` 等小工具 | ✅ 可用 | 低 |

## 1. AppBar 停靠（最高价值）

tinywm 的 `shell/shell_cosmo_appbar.inc` 用一个粗糙做法把工作区上移：
`SystemParametersInfoW(SPI_SETWORKAREA, …, SPIF_SENDCHANGE)`。

**更好的正统做法**是 AppBar API：`SHAppBarMessage(ABM_NEW / ABM_QUERYPOS / ABM_SETPOS / ABM_REMOVE)`，
让系统把任务栏占位计入工作区——最大化窗口不会压到我们的栏下面。
我们现在是 `WS_EX_TOPMOST` 悬浮栏，最大化窗口会**盖住它**。采纳后应改为 AppBar。

## 2. 托盘：宿主 vs 读取（关键前提）

tinywm 的托盘是 **宿主**：它自己 `Shell_NotifyIcon(NIM_ADD)` 成为通知区，应用把图标加给它
（`g_tray_icons`）。这只在 **它替换了 explorer** 时成立。
我们是 companion（Cairo 才是 shell），应用把图标加给 Cairo，故只能 **读** 现有 shell 的
`Shell_TrayWnd → ToolbarWindow32`（我们的 `src/tray.rs` 做法）。
**结论：我们的实现方向正确；只有当 M5 真正接管外壳时，才谈得上自己托管托盘。**

## 3. 绘制命令列表

tinywm 让 JS 产出绘制命令数组、C 在 `WM_PAINT` 末尾执行（overlay 模式，`sys.draw(cmds[])`）。
这是一个干净的数据驱动渲染模型；我们的 `taskbar::paint` 是直接 GDI 调用。
可选：抽一个 `Vec<DrawCmd>` 渲染层，便于主题化与测试，但非必要。

## 4. 窗口 Snap / 多屏移动热键

tinywm 默认热键（`init.js`）值得直接抄：
- `Win+←/→/↑/↓` → 左半屏 / 右半屏 / 最大化 / 还原
- `Win+Shift+←/→` → 移到相邻显示器
- `Win+C` 循环切换、`Win+E` 资源管理器、`Win+L` 锁屏

我们已有 `Alt+`` 键盘导航；下一步可加这套 Snap/多屏热键（我们已有 `monitor::enumerate`）。

## 5. 声明式窗口规则

`wm.rules([{match, action}])`：按标题/类名自动放置、尺寸、置顶。可用于公司工作站的窗口布局策略。

## 6. 菜单

任务条右键 → 窗口操作菜单（还原/最小化/最大化/Snap/关闭）；托盘右键 → Shell 菜单（应用/锁屏/重启/关机）。
我们用 `TrackPopupMenu` 可做，注意锁屏/关机是破坏性动作，需二次确认（与 M5 同级谨慎）。

## 7. JSON-RPC / remote_eval

`jsonrpc.register` + `handle`，以及 `remote_eval`。对可脚本化控制与自动化测试很有用；
可作为我们 `--launch` 之类 CLI 的长期替代（一个本地控制通道）。

## 8. 小工具

`sys.toast(title,msg)`（冒泡通知）、`sys.shell_open(path)`、`sys.exec(cmd)`、
`sys.monitor_list()`（含每屏工作区，与我们 `monitor.rs` 对齐）、`sys.dark_mode()`（主题跟随系统）。

## 参考文件

- `shell/shell_cosmo_appbar.inc` — AppBar 停靠
- `shell/shell_cosmo.c` — 托盘宿主 + 任务栏绘制（`g_tray_icons`）
- `modules/tray.js` / `modules/taskbar.js` / `modules/orb.js` — JS 侧 UI
- `README.md` §热键 / §JS API 概览 — 功能面清单
