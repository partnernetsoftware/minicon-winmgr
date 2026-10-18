# prd_3_1 — 外壳替换与回滚

> 2026-10-17 核对：下文保留早期试替换记录，不能当作当前工作树的完整验收。当前启动必须普通权限，不主动请求 UAC；HKLM 自动重启设置失败仍是已知边界。本轮发现备份失败未阻止接管、恢复记录身份隔离及运行中通知宿主转换缺口，见[梳理 C01/C06/C07](review_0_1_12.md)。[0.1.12 草案](plan_0_1_12.md)只提出待讨论修复，不代表已完成。

> 索引：3.1 · 树路径：`外壳集成 / 替换` · 上位文档：[`prd.md`](prd.md)
> 原始独立文档 `replacement-readiness.md` 已并入本文。

替换 = 把 `HKCU\...\Winlogon\Shell` 指向本程序（`<repo>\target\debug\minicon-winmgr.exe`）。
**默认 companion 模式不改注册表**；替换是可选、受控、可回滚的步骤。用户授权：
「判断真能替代时可直接替换，由用户测试」。

## 1. 试替换记录（2026-10-12 ~ 10-13）
- 已执行过 `--shell-set minicon`：`Shell` 一度指向本程序。
- **无 explorer 也能运行**：杀掉 explorer 后本程序以 shell 模式运行
  （`running as the shell; console detached` + `tray_host` sink 创建成功）。
- **AppBar 未能注册**（`ABM_NEW` 失败）——AppBar 需 explorer 当 broker；无 explorer 时**由我们自管工作区**替代，
  最大化窗口仍停在栏上方（见 [`prd_2_1.md`](prd_2_1.md) §3）。
- 之后按需回滚/保留，`Shell` 现值以 `--shell-status` 为准。

## 1b. 立即接管（本会话，无需注销/重登）
注册表只在**登录时**被读取，因此"把 `Shell` 指向本程序"本身不会立刻生效。
`src/shell_swap.rs` 补齐了**当前会话的即时交接**：

- **接管**（`take_over`）：① 首次把原 `Shell` 值备份到 exe 旁 `shell-backup.txt`（**只写一次，永不覆盖**）；
  ② 写注册表指向本程序（持久意图）；③ 尽力关闭外壳自动重启（HKLM `AutoRestartShell`，**需提权**）；
  ④ **立即停止本会话正在运行的外壳**：`EnumWindows` 找 `Shell_TrayWnd` / `Shell_SecondaryTrayWnd` / `Progman`
  → `WM_CLOSE` → 超时 `TerminateProcess`。
- **归还**（`give_back`）：读备份 → `restore` 注册表 → 重新开启自动重启 → **脱附启动** `explorer.exe`。
- 只作用于**当前会话桌面**（按顶层窗口枚举，绝不触及其他登录用户）。
- **脱附启动**：`CreateProcessW(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)`，不继承句柄——
  外壳必须比启动者活得久，否则会被一起回收（早期用 `Command::spawn` 曾让调用者收到 SIGTERM）。

实测（2026-10-13，debug 构建）：备份→写注册表→`stopped 0`（本机无原生外壳窗口，非错误）；
`give_back` 按备份**精确恢复**注册表值、脱附拉起 explorer（已存在实例时单实例交接、不新增常驻进程）、
退出码 0、无 SIGTERM。`AutoRestartShell` 因 HKLM 需提权未改动（`Win32 error 5`），属预期。

## 2. 硬门槛（全绿方可长期保留替换）
| # | 项 | 状态 | 说明 |
|---|----|------|------|
| 1 | 任务栏可用 | ✅ | 自绘、每窗口一按钮、点击/键盘切换、每屏一栏、主题可配 |
| 2 | 最大化窗口不压栏 | ✅ | 有 explorer → AppBar 停靠；无 explorer → 自管工作区收窄并 `recover` |
| 3 | 替换后不丢通知区 | 🟡 | `tray_host` sink 已创建；待真替换会话中确认能收到各程序托盘图标 |
| 4 | 有开始菜单 / 启动器 | ✅ | `start_menu`（品牌点）+ `launcher_ui`（Alt+Space）+ `run_dialog` |
| 5 | 会话生命周期 | ✅ | WTS 会话变更 + `WM_QUERYENDSESSION/ENDSESSION` |
| 6 | 一键回滚 | ✅ | 见下 |
| 7 | 稳定性长跑 | ⏳ | 需 ≥ 数小时常驻、无 panic / 泄漏 / 卡死（**已部分取证**：30 秒连续采样工作集/句柄/线程完全持平，见 [`prd_2_1.md`](prd_2_1.md) §2；数小时长跑仍待观察） |

## 3. 切换流程（立即生效）
1. `--shell-status` 记录当前值。
2. `--shell-takeover`（需 `MINICON_CONFIRM_SHELL=yes`）：写注册表**并立即停止本会话外壳**，无需注销。
3. 确认任务栏 / 托盘 / 开始菜单齐全。
4. 异常 → 归还命令。

## 4. 归还 / 回滚（任何异常立即执行）
```
MINICON_CONFIRM_SHELL=yes "<repo>\target\debug\minicon-winmgr.exe" --shell-give-back
```
- `--shell-give-back`：读 `shell-backup.txt` → 恢复注册表 → 脱附重启 `explorer.exe`，**当前会话立即恢复**。
  **无备份记录时拒绝执行**（返回错误），绝不改写并非由本程序配置的 `Shell`。
- 底层等价命令（不带即时交接）：`--shell-rollback default`（删除该值 → 系统默认 `explorer.exe`）；
  或 `--shell-rollback "<切换前记录的值>"` 恢复到替换前的第三方外壳。
- 登录后没桌面时：`Ctrl+Shift+Esc` → 运行新任务 → 执行 `explorer.exe`（或上面命令）。

## 5. GUI 内入口（设置窗）
`settings_ui` 底部两个动作（`src/settings_ui.rs`），均**立即生效**：
- **接管桌面（立即生效）**：先备份当前 `Shell` 到 exe 旁 `shell-backup.txt`（只写一次），
  再 `replace` 注册表，随后**停止本会话外壳**（见 §1b）。
- **还原系统桌面（立即生效）**：读 `shell-backup.txt` → `restore` → 重新开启自动重启 → 脱附重启 `explorer.exe`；
  **无备份则拒绝**。带 **MessageBox 二次确认**；0.1.10 改为确定/取消，默认取消，Esc 或关闭确认框不执行切换。

## 6. 开发期热替换
`scripts/hotswap.py`：Windows 不能覆盖运行中的 exe，但能改名——脚本先**改名移走在跑的 exe**，
再 `cargo build`，随后请旧实例退出（必要时强杀），可选启动新 exe。把"掉壳"窗口压到亚秒级。

## 7. 已知缺口 / 非目标
- 桌面图标 / 动态桌面、文件夹堆栈、菜单"最近使用"、通知区点击转发（见 [`prd_5_1.md`](prd_5_1.md)）。
- 系统级（HKLM）替换、打包发版、代码签名：不做。

## 8. 风险
- 替换是**共享系统状态**改动，仅当前用户级；**始终保留可回滚值**。
- 工作区自管在崩溃时靠 `minicon-workarea.state` 恢复；若该文件丢失且进程被强杀，可能残留底部空带
  （手动 `--shell-rollback` 或重启会话可解）。
