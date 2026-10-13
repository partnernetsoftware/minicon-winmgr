# prd_外壳替换与回滚

> 树路径：`外壳集成 / 替换` · 上位文档：[`prd.md`](prd.md)
> 原始独立文档 `replacement-readiness.md` 已并入本文。

替换 = 把 `HKCU\...\Winlogon\Shell` 指向本程序（`<repo>\target\debug\minicon-winmgr.exe`）。
**默认 companion 模式不改注册表**；替换是可选、受控、可回滚的步骤。用户授权：
「判断真能替代时可直接替换，由用户测试」。

## 1. 试替换记录（2026-10-12 ~ 10-13）
- 已执行过 `--shell-set minicon`：`Shell` 一度指向本程序。
- **无 explorer 也能运行**：杀掉 explorer 后本程序以 shell 模式运行
  （`running as the shell; console detached` + `tray_host` sink 创建成功）。
- **AppBar 未能注册**（`ABM_NEW` 失败）——AppBar 需 explorer 当 broker；无 explorer 时**由我们自管工作区**替代，
  最大化窗口仍停在栏上方（见 [`prd_reliability.md`](prd_reliability.md) §3）。
- 之后按需回滚/保留，`Shell` 现值以 `--shell-status` 为准。

## 2. 硬门槛（全绿方可长期保留替换）
| # | 项 | 状态 | 说明 |
|---|----|------|------|
| 1 | 任务栏可用 | ✅ | 自绘、每窗口一按钮、点击/键盘切换、每屏一栏、主题可配 |
| 2 | 最大化窗口不压栏 | ✅ | 有 explorer → AppBar 停靠；无 explorer → 自管工作区收窄并 `recover` |
| 3 | 替换后不丢通知区 | 🟡 | `tray_host` sink 已创建；待真替换会话中确认能收到各程序托盘图标 |
| 4 | 有开始菜单 / 启动器 | ✅ | `start_menu`（品牌点）+ `launcher_ui`（Alt+Space）+ `run_dialog` |
| 5 | 会话生命周期 | ✅ | WTS 会话变更 + `WM_QUERYENDSESSION/ENDSESSION` |
| 6 | 一键回滚 | ✅ | 见下 |
| 7 | 稳定性长跑 | ⏳ | 需 ≥ 数小时常驻、无 panic / 泄漏 / 卡死 |

## 3. 切换流程
1. `--shell-status` 记录当前值。
2. `--shell-set minicon`（需 `MINICON_CONFIRM_SHELL=yes`）。
3. 注销 / 重登，确认任务栏 / 托盘 / 开始菜单齐全。
4. 异常 → 回滚命令。

## 4. 回滚（任何异常立即执行）
```
MINICON_CONFIRM_SHELL=yes "<repo>\target\debug\minicon-winmgr.exe" --shell-rollback default
```
- `default` = 删除该值 → 回到系统默认 `explorer.exe`。
- 或 `--shell-rollback "<切换前记录的值>"` 恢复到替换前的第三方外壳。
- 登录后没桌面时：`Ctrl+Shift+Esc` → 运行新任务 → 执行上面命令（或直接 `explorer.exe`）。

## 5. GUI 内入口（设置窗）
`settings_ui` 底部两个动作（`src/settings_ui.rs`）：
- **替代为 Windows 外壳**：先把当前 `Shell` 值**备份到 exe 旁的 `shell-backup.txt`**，再 `replace`。
- **回滚到原外壳**：读 `shell-backup.txt` → `restore(Some)`；无备份则 `restore(None)`（回 explorer）。
  带 **MessageBox 二次确认（默认"否"）**。

## 6. 开发期热替换
`scripts/hotswap.py`：Windows 不能覆盖运行中的 exe，但能改名——脚本先**改名移走在跑的 exe**，
再 `cargo build`，随后请旧实例退出（必要时强杀），可选启动新 exe。把"掉壳"窗口压到亚秒级。

## 7. 已知缺口 / 非目标
- 桌面图标 / 动态桌面、文件夹堆栈、菜单"最近使用"、通知区点击转发（见 [`prd_backlog_research.md`](prd_backlog_research.md)）。
- 系统级（HKLM）替换、打包发版、代码签名：不做。

## 8. 风险
- 替换是**共享系统状态**改动，仅当前用户级；**始终保留可回滚值**。
- 工作区自管在崩溃时靠 `minicon-workarea.state` 恢复；若该文件丢失且进程被强杀，可能残留底部空带
  （手动 `--shell-rollback` 或重启会话可解）。
