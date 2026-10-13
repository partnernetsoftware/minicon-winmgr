# prd_可靠性底座 — 观测 / 自恢复 / 不变量

> 树路径：`可靠性底座` · 上位文档：[`prd.md`](prd.md)
> 这是本项目的**护城河**，也是**改代码前必读**的一节。

## 1. 可观测
- `src/logging.rs`：结构化日志，**滚动文件 + stderr 双写**；`panic` hook 落盘。
- `src/single_instance.rs`：命名互斥量 `CreateMutexW`，二次启动被拒。
- `src/metrics.rs`：周期采样**内存 / 句柄 / GDI / user 对象**，写入 `metrics line=...` 日志行。
- `src/session.rs`：WTS 会话变更 + `WM_QUERYENDSESSION/ENDSESSION`，记录回收路径。
- `src/window_events.rs`：`SetWinEventHook`（object create/destroy/show/hide + system foreground/minimize，
  两个钩子因事件范围 0x8xxx/0x00xx 相距远）驱动任务栏刷新；**去抖 120ms**（复用同一 `WM_TIMER`，不新增线程）。
  目的：窗口一被销毁，任务按钮立刻消失，而不是等 30s 心跳。
  - **坑**：`SetWinEventHook` 的 `eventmin ≤ eventmax`，否则直接失败（曾把 0x8000 当 min、0x0017 当 max → 失败）。

## 2. 稳定性证据（实测）
- `cargo test` = **97 passed / 0 failed**；`cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 干净。
- 分钟级采样：`handles` / `gdi_objects` **持平**（无泄漏）。
- 优雅退出路径：`message loop exited → work area restored on teardown → stopped`。

## 3. 工作区自管（无 explorer 也能当唯一 shell）
背景：`SHAppBarMessage(ABM_NEW)` 需 **explorer 充当 AppBar broker**；explorer 不在时注册失败，
最大化窗口会钻到栏下面。为此我们**自己收窄工作区**：

- `taskbar::reserve_workarea`：AppBar 失败时 `SPI_SETWORKAREA` 把底边 `BAR_H` 留出；收窄时把
  `orig`/`mine` 两个矩形写入 **exe 旁 `minicon-workarea.state`**。
- 正常退出：`taskbar::shutdown()`（在 `run_companion` 里 `window::run()` 之后调用）**主动 `DestroyWindow` 各栏**
  → 触发 `WM_DESTROY → restore_workarea` 并删状态文件。
  - **为什么必须主动销毁**：优雅退出走的是 **message-only 窗口** 的 `WM_CLOSE`，栏窗口收不到 `WM_DESTROY`，
    只依赖栏的 `WM_DESTROY` 会导致退出后工作区不还原（实测确认）。
- 强杀/崩溃兜底：`taskbar::recover_workarea()`（启动时）读状态文件，**仅当当前工作区仍等于我们上次设的 `mine`**
  才还原为 `orig`（避免覆盖其它外壳的正常改动），随后清文件。

## 4. 焦点 / 前台正确性
- **前台锁坑**：任务栏/设置等是 **top-most tool window**，直接 `SetForegroundWindow` 常被系统前台锁拒绝
  → 「点了切不过去」。统一用 `win_util::force_foreground`，**分层兜底**：
  1. `SetWindowPos(HWND_TOP)` 提 z 序 + `SetForegroundWindow`；
  2. `AttachThreadInput` 挂前台线程再置前、随后脱开；
  3. `SW_MINIMIZE` → `SW_RESTORE`（前台锁下仍可靠）。
  返回是否真的成为前台。
- **激活前 `IsWindow` 守卫**：句柄可能已关闭并被复用，否则会把前台切到错误窗口。
- **显隐状态用真实查询**：`settings_ui::toggle` 用 `IsWindowVisible` + `GetForegroundWindow`
  （可见但非前台→置前；可见且前台→隐藏），**不能缓存内存标志**。

## 5. 不变量与约定（硬规则）
1. **taskbar 状态只允许消息线程访问**：`tasks()` / `bars()` / `tray_icons()` / `pins()` 共用唯一消息线程。
   当前加锁顺序**不统一**（`paint` tasks→bars→pins；`refresh` bars→tasks），仅因单线程才安全。
   → **禁止新增 worker 线程去抢这些锁**。定时器也走消息线程（如 `CLOCK_TIMER_ID` / `HOVER_TIMER_ID`）。
2. **文件/跨进程 IO 不得在持 `bars()`/`tasks()` 锁时做**（`refresh` 会同步读托盘 + 扫开始菜单；过长会卡 UI）。
3. **悬停预览禁止在 `WM_MOUSEMOVE` 同步 `PrintWindow`**：用 `WM_TIMER` 节流（350ms），单窗口 show/hide 复用，
   `HBITMAP` 在 target 变化与 `WM_DESTROY` 时释放。
4. **窗口类必须设 `hCursor`**（`IDC_ARROW`）：否则悬停显示"应用正在启动"的忙光标。
5. **不改 `Cargo.toml` 的 feature 冻结约定**：禁止用 `windows::core::link!` 绕过未启用 feature；
   改用语义化 feature（如 `PrintWindow` 需 `Win32_Storage_Xps`）。
6. **companion 下 `Win+*` 常被占**：`run_dialog` / `hotkeys` 均回退 `Ctrl+Alt+*`；回退也被占则记 `error` 日志、
   热键不可用（用户无感）。
7. **任务栏默认每窗口一个按钮**（对齐 Alt+Tab）；分组 `(+N)` 会藏窗口，故分组 opt-in（`MINICON_GROUP_TASKS=1`）。
8. **悬停预览默认关闭**（用户判定价值低，`MINICON_HOVER_PREVIEW=1` 开）。

## 6. 待加固
- 长跑观察 ≥ 数小时（成功度量之一）。
- 崩溃自愈：目前是"下次启动恢复残留"；更进一步的守护（watchdog）**用户明确暂不做**。
