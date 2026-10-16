# prd_2_1 — 可靠性底座 · 观测 / 自恢复 / 不变量

> 索引：2.1 · 树路径：`可靠性底座` · 上位文档：[`prd.md`](prd.md)
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

## 2. 稳定性证据与口径

当前公开版为 0.1.9，完成的测试范围见[验收摘要](validation.md)。单元测试数量和短时间资源计数不能证明长期无泄漏；早期 30 秒观察和 debug 单点数字不作为当前版本内存承诺。

0.1.10 正在实现[专项计划](plan_0_1_10.md)。外部只读采样区分私有提交、工作集及可用时的私有工作集，并记录句柄、GDI/USER、线程与观测开销。数据绑定同一进程句柄与创建时间；退出立即停止，不跟随复用 PID。原始 CSV/JSON 和曲线只留在忽略目录。

资源归因以同一构建、相同规模、操作前后及静置阶段对照，重复场景并单列 2 小时/至少 8 小时观察。禁止用强制修剪工作集替代资源释放。观察尚未完成时明确标记未完成，不以短测结论覆盖长跑缺口。

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
2. **文件/跨进程 IO 不得在持 `bars()`/`tasks()` 锁时做**。镜像通知区的跨进程读曾在这里同步跑（2000 ms 超时，
   宿主卡死即把整条栏卡住数秒）——0.1.3 已把它**移出 `refresh()`**，改为**心跳（默认 30 s）采样 + 缓存**、
   超时降到 **250 ms**，`refresh()` 只读缓存（见 [`prd_1_1.md`](prd_1_1.md)「通知区 / 托盘」）。其余 IO
   （如扫描开始菜单）仍须遵守本条。
3. **UI 不同步调用未知窗口的截图/图标处理**：0.1.10 开发将预览改为单个 DWM 关系，隐藏/换目标/销毁释放；
   图标由固定 worker 查询、复制为 owned 快照，界面只读完成缓存。请求/结果/通知合并，超时不新增线程；见[资源审计](resources_0_1_10.md)。
4. **窗口类必须设 `hCursor`**（`IDC_ARROW`）：否则悬停显示"应用正在启动"的忙光标。
5. **不改 `Cargo.toml` 的 feature 冻结约定**：禁止用 `windows::core::link!` 绕过未启用 feature；
   改用语义化 feature（如 `PrintWindow` 需 `Win32_Storage_Xps`）。
6. **companion 下 `Win+*` 常被占**：`run_dialog` / `hotkeys` 均回退 `Ctrl+Alt+*`；回退也被占则记 `error` 日志、
   热键不可用（用户无感）。
7. **任务栏默认每窗口一个按钮**（对齐 Alt+Tab）；分组 `(+N)` 会藏窗口，故分组 opt-in（`MINICON_GROUP_TASKS=1`）。
8. **悬停预览默认关闭**（用户判定价值低，`MINICON_HOVER_PREVIEW=1` 开）。
9. **任务按钮位置固定**：按钮顺序按"首次出现"固定，激活窗口不重排。`EnumWindows` 是 **Z 序**（顶层在前），
   直接当按钮顺序会让按钮"跟着焦点跑"；`refresh()` 经 `stable_order()` 只追加新窗口到最右。
10. **外壳进程必须"脱附"启动**：拉起 `explorer.exe` 只能用
    `CreateProcessW(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)` 且**不继承句柄**、用完关闭两个 handle。
    用 `std::process::Command::spawn` 曾让**调用者自己收到 SIGTERM**（子进程继承管道/控制台，被一起回收）。
    原因：外壳必须比启动者活得更久。
11. **还原外壳前必须有"接管记录"，否则拒绝**：`give_back` 读取 exe 旁 `shell-backup.txt`，
    **文件缺失 = 本程序从未接管过 → 直接返回错误**，绝不改写并非由本程序配置的 `Shell`
    （曾发生的真实事故：无备份时把 Shell 值删掉，而本机 Shell 本就指向 minicon → 被清空）。
    备份**只写一次、永不覆盖**，以免重复接管把"原值"冲成自己。
12. **`Shell` 写 `None` = 删除该值**，不是写空 `REG_SZ`：`shell_registry::write_string(None)` 走
    `RegDeleteValueW`（`ERROR_FILE_NOT_FOUND` 视为成功）。写空字符串会让 Winlogon 找不到有效外壳。
13. **外壳身份不可让渡（除"归还桌面"这一个显式动作外）**：`explorer.exe` 有双重身份——**带参数**启动是文件浏览器，
    **不带参数**启动会判定"没有 shell"并创建 `Shell_TrayWnd` + 桌面，从而**把会话夺回去**。
    → **所有我们发起的"打开"动作必须走 `src/shell_launch.rs`**（quick-launch 钉、开始菜单 Places、
    Run 对话框、launcher），由 `plan()` 把危险的"裸 explorer"改写成带参数的形式；
    `shell:` moniker 也一并转成真实路径/CLSID。禁止各处自行 `ShellExecuteW` / `cmd /C start`。
    （真实事故：quick-launch 钉了 `C:\Windows\explorer.exe`，点击后 explorer 夺权。）
14. **设置窗口是进程内复用的一次性资源，关闭 = 隐藏**：`WM_CLOSE` 只 `ShowWindow(SW_HIDE)`，
    绝不 `DestroyWindow`。曾发生：窗口被销毁后 `SETTINGS` 置 0，任务栏设置按钮**整场会话再点都没反应**
    （日志 `settings show ignored: window not installed`）。
    兜底：`window()` 用 `IsWindow` 校验陈旧句柄，`toggle()` 发现窗口缺失就**懒重建**（`install` 记住 `HINSTANCE`）。
15. **外来外壳只检测、不擅自处置**：心跳（30s）调用 `shell_swap::check_foreign_shell()`，
    在"出现/消失"**状态跳变时**各记一条日志，不做自动夺回（夺回会连带关掉用户的文件窗口，属用户决策）；
    恢复手段是显式的 `--shell-takeover`。

## 6. 待加固
- 长跑观察 ≥ 数小时（成功度量之一）。
- 崩溃自愈：目前是"下次启动恢复残留"；更进一步的守护（watchdog）**用户明确暂不做**。
