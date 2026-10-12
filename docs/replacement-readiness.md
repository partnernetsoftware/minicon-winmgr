# 替换 Cairo Desktop 的就绪门槛（Replacement Readiness）

用户授权：**当我们判断"真能替代"时，可直接替换**（写 `HKCU\...\Winlogon\Shell`），由用户测试。
本文定义"真能替代"的客观门槛，避免凭感觉切换导致桌面不可用。

## 硬门槛（必须全绿才可切换）

| # | 项 | 状态 | 说明 |
|---|----|------|------|
| 1 | 任务栏可用 | ✅ | 自绘、按应用分组、点击/键盘切换、每屏一栏、主题可配 |
| 2 | **最大化窗口不压栏** | ⚠️ | AppBar 停靠已实现，但 **companion 下 `ABM_NEW` 返回 0**（Cairo 占着底部边缘）→ 已优雅退回悬浮并告警；**需替换模式复验** |
| 3 | **替换后不丢通知区** | ⏳ | `tray_host` 注册 `Shell_TrayWnd` 宿主、解析 `WM_COPYDATA`/`NOTIFYICONDATAW`；**当且仅当我们是当前 `Shell` 时自动启用**（`MINICON_TRAY_HOST=0` 可强制关；单测 5 个）——**待替换模式实测能收到图标** |
| 4 | 有开始菜单/启动器 | ✅ | `start_menu`（点品牌点唤出）+ `launcher_ui`（Alt+Space）——本机已建窗口 |
| 5 | 会话生命周期 | ✅ | WTS 会话变更 + `WM_QUERYENDSESSION/ENDSESSION` |
| 6 | 一键回滚 | ✅ | `--shell-status` 记录当前值；`--shell-rollback` 可还原 |
| 7 | 稳定性观察 | ⏳ | 至少一次长时间（≥ 数小时）常驻，无 panic/无句柄泄漏/无卡死 |

## 切换流程（门槛全绿后）

1. `minicon-winmgr --shell-status` 记录当前值（本机为 `C:\Program Files\Cairo Shell\CairoDesktop.exe`）。
2. 以替换模式启动验证：`MINICON_TRAY_HOST=1` 下确认通知区图标能被我们接收（门槛 3）。
3. 设置 Shell：`MINICON_CONFIRM_SHELL=yes minicon-winmgr --shell-set minicon`。
4. 注销/重新登录，观察登录后桌面：任务栏/托盘/开始菜单是否齐全。
5. 任何异常立即回滚：
   `MINICON_CONFIRM_SHELL=yes minicon-winmgr --shell-rollback "C:\Program Files\Cairo Shell\CairoDesktop.exe"`

## 非目标 / 已知缺口（切换后可继续补）

- 桌面图标 / 动态桌面、文件夹堆栈、窗口悬停预览、任务栏右键菜单、窗口 Snap（可用 `Win+方向` 后续加）。
- 通知区图标点击转发（把图标点击事件回投给宿主 app）——`tray_host` 的进阶。

## 风险与保守策略

- 替换是**共享系统状态**改动，仅当前用户级；**始终保留可回滚值**。
- 若门槛 3（托盘）无法在本机验证（本机 shell 是 Cairo，无 `Shell_TrayWnd` 供读取），
  则**先在替换模式下验证** `tray_host` 能接收图标，再决定是否长期保留替换。
- 不满足门槛就继续 companion，只把栏作为"叠加增强"。参见 `docs/prd.md` §4 思维树。
