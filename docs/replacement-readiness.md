# 替换 Cairo Desktop 的就绪门槛（Replacement Readiness）

用户授权：**当我们判断"真能替代"时，可直接替换**（写 `HKCU\...\Winlogon\Shell`），由用户测试。
本文定义"真能替代"的客观门槛，避免凭感觉切换导致桌面不可用。

## 试替换记录（2026-10-12）

- **已执行**：`--shell-set minicon` → `HKCU\...\Winlogon\Shell` 现为
  `D:\dev\partnernetsoftware\minicon-winmgr\target\debug\minicon-winmgr.exe`（原值 `C:\Program Files\Cairo Shell\CairoDesktop.exe`）。
- **登录前预演**（在 `Shell` 已指向我们的前提下直接运行，等价于登录时的代码路径）：
  - ✅ `running as the shell; console detached`（不再弹控制台）
  - ✅ 任务栏窗口创建
  - ✅ 开始菜单窗口创建
  - ✅ **托盘宿主 sink 窗口创建成功**（替换模式下自动启用）
  - ⚠️ AppBar 仍失败——因 **Cairo 仍在运行**占着底边；注销后 Cairo 不再运行，预计可成，但**未能在登录前证明**
- 生效方式：**注销/重新登录**后由 Windows 以我们为外壳启动。

## 硬门槛（必须全绿才可长期保留）

| # | 项 | 状态 | 说明 |
|---|----|------|------|
| 1 | 任务栏可用 | ✅ | 自绘、按应用分组、点击/键盘切换、每屏一栏、主题可配 |
| 2 | **最大化窗口不压栏** | ⚠️ | AppBar 已实现；companion/Cairo 在跑时 `ABM_NEW` 失败→退化为悬浮；**待注销后复验** |
| 3 | **替换后不丢通知区** | ⚠️ | `tray_host` sink 已成功创建；**待注销后确认能收到各 app 的托盘图标** |
| 4 | 有开始菜单/启动器 | ✅ | `start_menu`（点品牌点）+ `launcher_ui`（Alt+Space） |
| 5 | 会话生命周期 | ✅ | WTS 会话变更 + `WM_QUERYENDSESSION/ENDSESSION` |
| 6 | 一键回滚 | ✅ | 见下方回滚命令 |
| 7 | 稳定性观察 | ⏳ | 至少一次长时间（≥ 数小时）常驻，无 panic/泄漏/卡死 |

## 回滚（任何异常立即执行）

```
MINICON_CONFIRM_SHELL=yes "D:\dev\partnernetsoftware\minicon-winmgr\target\debug\minicon-winmgr.exe" --shell-rollback default
```
（`default` = 删除该值，回到系统默认 `explorer.exe`；或显式指定 `"C:\Program Files\Cairo Shell\CairoDesktop.exe"`。）
若登录后无桌面：Ctrl+Shift+Esc 打开任务管理器 → 运行新任务 → 执行上面的命令（或直接运行 `explorer.exe`）。

## 切换流程（门槛全绿后长期保留）

1. `--shell-status` 记录当前值。
2. `--shell-set minicon`。
3. 注销/重登，确认任务栏/托盘/开始菜单齐全。
4. 异常 → 回滚命令。

## 非目标 / 已知缺口（切换后可继续补）

- 桌面图标 / 动态桌面、文件夹堆栈、窗口悬停预览、任务栏右键菜单、窗口 Snap（`Win+方向`）。
- 通知区图标点击转发（把图标点击回投给宿主 app）——`tray_host` 的进阶。

## 风险与保守策略

- 替换是**共享系统状态**改动，仅当前用户级；**始终保留可回滚值**（见上）。
- 若门槛 2/3 注销后未通过，立即回滚并继续 companion。
- 参见 `docs/prd.md` §4 思维树。
