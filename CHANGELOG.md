# Changelog

所有值得记录的变更记在这里。格式参照 [Keep a Changelog](https://keepachangelog.com/)，
版本号遵循 [Semantic Versioning](https://semver.org/)。

## 0.1.1 — 2026-10-13

首个打 tag 的版本。包含此前未发布的全部工作（任务栏二期、立即接管/归还、PRD 文档体系）。

### 新增

- **立即接管 / 立即归还当前会话外壳**（`src/shell_swap.rs`）
  - 设置窗动作改为「接管桌面（立即生效）」/「还原系统桌面（立即生效）」，**无需注销重登**。
  - 接管：备份原 `Shell` → 写注册表 → 尽力关闭外壳自动重启 → **立即停止本会话外壳**
    （`Shell_TrayWnd` / `Shell_SecondaryTrayWnd` / `Progman` → `WM_CLOSE` → 超时强杀）。
  - 归还：按备份恢复注册表 → 重新开启自动重启 → **脱附**重启 `explorer.exe`。
  - 新增 CLI：`--shell-takeover` / `--shell-give-back`（均需 `MINICON_CONFIRM_SHELL=yes`）。
- **任务栏二期**
  - 密度阶梯补上第三档 **D2 仅图标**（40–48px）：只画居中图标，多窗口方块右上角标 `(+N)`。
  - **icon-only 悬停标题气泡**（`src/bar_tip.rs`，常驻 `WS_POPUP`，无新线程）。
  - 键盘导航中 **`1..9` 直跳**第 n 个可见按钮。
  - **分组右键批量菜单**：打开组内全部 / 全部还原 / 全部最小化 / 全部关闭 / 加入快速启动。
  - 窄栏时**局部折叠快速启动 pins**，优先保住任务按钮。
  - `»` 弹层每 24 行 **`MF_MENUBARBREAK` 分列**，避免超屏不可滚。
  - 通知图标条可关（设置项「显示通知图标条（实验性）」，`taskbar.tray`）。

### 修复

- **任务栏出现点不动、关不掉的【开始】按钮**：`is_app_window` 未排除 DWM cloaked 窗口
  （开始菜单宿主的 `Windows.UI.Core.CoreWindow`）。已加 `DWMWA_CLOAKED` 过滤。
- **任务栏按钮顺序随激活状态乱跳**：`EnumWindows` 是 Z 序，直接当按钮顺序会跟着焦点跑。
  改为 `stable_order()`：按首次出现固定位置，只把新窗口追加到最右。
- **右键【时间】双击应打开系统调整时间入口**：`ShellExecuteW("timedate.cpl")` 返回 31（无关联），
  改用 `control.exe timedate.cpl`。
- **还原外壳会误删 `Shell` 值**：无备份时旧逻辑直接清空该值（而本机 `Shell` 本就指向 minicon）。
  现改为**无接管记录即拒绝归还**，且备份**只写一次、永不覆盖**。
- **拉起 explorer 导致调用者被 SIGTERM**：改用
  `CreateProcessW(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)`，不继承句柄。
- **`Shell` 写 `None` 会写成空 `REG_SZ`**：改为 `RegDeleteValueW`（删除该值）。

### 文档

- PRD 体系整理为 `docs/prd.md` + 子文档 `prd_{idx1}_{idx2}.md`（命名 = 树路径），新增方法学章节。
- `prd_3_1.md` 新增「立即接管」章节；`prd_2_1.md` 补资源实测基线与 3 条新不变量。
- `README.md` 同步 M5/M11、命令表与模块表。

### 实测基线

- `cargo test` **161 passed / 0 failed**；`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 干净。
- 常驻资源：工作集 37.6 MB / 私有 **14.5 MB** / 句柄 380 / 线程 14（30 秒采样持平，无泄漏）；
  约为同机 explorer（173.95 MB / 58.75 MB）的 **1/4**。
- release 二进制 **1.89 MB**（debug 4.55 MB）。

### 已知限制

- 立即接管后 Windows 可能自动重新拉起 explorer（关闭 `HKLM` 的 `AutoRestartShell` **需管理员权限**，
  未提权时该步会失败并记 warn）。
- 稳定性长跑（≥ 数小时）仍在观察中，仅有分钟级无泄漏取证。
- 未做：可浏览桌面、文件夹堆栈、通知区点击转发、堆叠窗口组（见 `docs/prd_5_1.md`）。
