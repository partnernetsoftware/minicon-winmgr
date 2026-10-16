# Changelog

## Unreleased

- 设置采用两个内容分组，桌面管理默认折叠；“配置与关于”集中展示值、来源、权限与保存路径，支持滚动和文本选择复制。简化普通按钮表面并保留原生键盘和 UI Automation 行为。
- 启动时检测自身权限：普通权限直接运行，检测到提升权限时以同用户、同会话的普通权限重新启动；校验子进程令牌并等待启动就绪。不主动请求 UAC，不通过提权重试；失败给出明确错误。
- 补充 35 项设置桌面验收及提升入口转交现有普通实例的实测；管理员首次启动、无过滤令牌环境和长时间资源走势仍有覆盖缺项，详见版本计划。

- 修复设置窗口行高被测量结果反复回写，窄尺寸布局后留下巨大空白、内容无法恢复的问题；首选尺寸与实际布局尺寸分离，增加真实窗口反复缩放回归。
- 重排设置界面：清晰的标题与分组字号、横向单选项、统一边距与说明文字、独立桌面管理区；保存正常时隐藏无用按钮，失败时保留完整提示与重试。

## 0.1.6 — 2026-10-15

主题：**设置日常可用与反馈闭环**。范围与验收见 [版本计划](docs/plan_0_1_6.md)。

- 原生设置控件支持 Tab/反向 Tab、单选方向键、Space/Enter/Esc；可滚动内容、固定反馈页脚、按 DPI 布局和焦点恢复。
- 展示配置来源、环境覆盖只读，以及当前任务栏状态与下次启动偏好；`--settings` 可呼出已有实例，无任务栏时仍可打开。
- 独立串行保存线程合并最新修改，保留注释/未知键、备份旧字节并临时文件替换；失败明确提示、支持重试，退出保留返回设置的选择。
- 修复预览从禁用启动后热开启；关闭取消计时器、隐藏并释放位图。
- 修复 Run 参数中的 `%USERPROFILE%` 等环境变量未展开；保留参数空格/引号和未知变量。已有文档关联的编辑器失效时明确提示指定编辑器或修复默认打开方式。
- 验证与边界：本地质量检查、真实桌面流程及 22 项 Run 回归见计划和 JSON；物理混合 DPI/热插拔、屏幕阅读器语音、高对比度及小时级长跑未覆盖。

- 合并时间与通知入口：移除独立“通知”按钮；单击时间打开通知面板，面板新增“时间设置”，与双击时间复用同一异步启动入口。应用图标条回收原按钮占用宽度，隐藏图标箭头与 Win+B 保留。


所有值得记录的变更记在这里。格式参照 [Keep a Changelog](https://keepachangelog.com/)，
版本号遵循 [Semantic Versioning](https://semver.org/)。

## 0.1.5 — 2026-10-15

主题：**通知区域图标达到日常可用**。需求见 [`docs/prd_1_1_2.md`](docs/prd_1_1_2.md)，兼容性与证据见 [`docs/plan_0_1_5.md`](docs/plan_0_1_5.md)。

- 修正真实 `Shell_NotifyIconW` 编组协议和成功回执；支持部分更新、GUID、版本0/3/4、隐藏、删除、TaskbarCreated恢复及失效进程清理；拥有图标副本。
- 主条最多4项，隐藏/溢出图标进入原生分页面板。Win+B、方向键、Home/End、Tab、Enter/Space、Shift+F10、Esc可用；按工作区缩减列数并使用DPI字体。
- 修复双击/抬起重复或缺失、按住时更新导致目标变化、关闭面板保留已退出应用图标资源的问题；恶意/畸形协议警告限频。
- 音量/网络提供明确命名的设置入口及Control Panel回退；伴随Explorer保留其通知宿主，取消不可操作的应用占位。
- 验证：253项测试、fmt、严格Clippy、release构建；网易UU远程/WorkBuddy实机交互、70图标分页、400次增删更新、100轮面板开关，以及22项Run与4项IME/失焦回归通过。句柄357→357、GDI240→240；临时图标删除后GDI回到基线。
- 边界：音量/网络不是实时状态图标；尚无电源/输入法状态指示器。物理混合DPI、跨屏/拔插与数小时长跑未验证；伴随模式在实际Explorer私有桌面验证，未切换用户输入桌面。

## 0.1.4 — 2026-10-14

主题：**Win+R 运行对话框达到日常可用**。产品设计见
[`docs/prd_1_1_1.md`](docs/prd_1_1_1.md)，交付与验证边界见
[`docs/plan_0_1_4.md`](docs/plan_0_1_4.md)。

- 中文原生界面，保留草稿、重复呼出聚焦全选；失败留窗并显示可修改的错误，成功后回忆最近命令。
- 正确分离启动目标与原始参数，支持带空格路径、目标环境变量、网址及用户目录下的相对路径；裸 Explorer 仍受外壳身份保护。
- 独立的30条成功运行历史，可清空；前缀过滤及本地固定盘一级路径建议，候选先采纳再执行。历史位于 `%LOCALAPPDATA%\minicon-winmgr\run-history.json`。
- 原生键盘/IME输入，DPI字体与工作区布局，小工作区保留错误行、长错误可滚动；显示/隐藏复用窗口资源。
- 启动、历史读写与路径查询放入独立有界后台线程，查询去抖/超时和请求代号避免慢IO或旧结果干扰输入。
- 修复空文本原生测量访问异常，并增加实际GDI空文本/中文换行回归检查。
- 验证：248项测试通过，fmt/Clippy/release构建通过；22项真实桌面流程和4项IME/失焦检查通过。release热呼出100次：中位22ms/P95 38ms/max66ms，句柄/GDI无持续增长。
- 环境限制：物理混合DPI多屏/拔插、高对比度、真实慢盘/挂起启动尚未完成实机验证；当前测试机没有HTTPS默认关联，已验证对应错误反馈，未验证浏览器导航。完整覆盖情况逐项保留在版本计划。

## 0.1.3 — 2026-10-14

主题：**通知区（托盘）收敛 + 代码健康**——不新增产品功能，把 v0.1.2 之前"看得见但不可用"的
通知图标条做成真的，并把四类代码债（稳健 / 复用 / 抽象 / 耦合）清完。计划全文见
[`docs/plan_0_1_3.md`](docs/plan_0_1_3.md)。

### 变更

- **通知区重构为「一个模型 + 各管一摊」**（`src/tray.rs` / `src/tray_bar.rs` / `src/tray_host.rs`）：
  - `tray` 是唯一数据源：宿主表（我们作外壳时应用注册进来的真图标）优先，镜像缓存兜底
    （以外跟随别人的 shell 时读它的 `ToolbarWindow32`，250ms 超时、只在心跳采样，**不在重绘路径上**）。
  - `tray_bar` 只管呈现：格子几何（`Layout`）、绘制（真图标 `CopyImage` 进本进程再画，
    占位则渐隐填充）、命中（`hit_test`）、转发（`forward`）。真图标可点；镜像占位**不可点**
    （`is_forwardable`），杜绝把点击发给别人的 shell。
  - `tray_host` 只剩协议：`WM_COPYDATA`/`SHELLTRAYDATA` 解析 + 注册表维护，几何/命中/转发全部迁出。
  - 四类事件转发（`WM_LBUTTONDOWN` / `WM_LBUTTONDBLCLK` / `WM_RBUTTONDOWN` / `WM_RBUTTONUP`），
    down/up 保持独立不合并；hover **从不转发**，只弹我们的 tooltip。绘制/命中/转发共用一份快照。
- **设置行文案**：「显示通知图标条（实验性）」→「显示通知区域（仅作外壳时可点）」，并加单测锁死措辞。
- **可见性收窄**：`tray` / `tray_bar` / `tray_host` / `win_util` 的 `pub` 降为 `pub(crate)`
  （本 crate 外本就不可达，纯澄清；全仓收窄留给 0.2.0）。
- **文档清理**：13 个模块头已与事实矛盾的 `INTERFACE FROZEN` 标记全部移除（无一是 stub）；
  两处把"开始菜单退役"归因于窗口列表按钮的注释改为符合 v0.1.2 事实（两者系一并退役）；
  PRD 子文档（`prd_1_1` / `prd_4_1` / `prd_6_1` / `prd_2_1` / `prd.md` / `README.md`）的托盘叙述按
  0.1.3 决策回写。

### 修复（代码健康）

- **稳健**：59 处裸 `Mutex::lock().unwrap()` 清零（poison 时读穿 `lock_or_recover`，不再 panic——
  wndproc 的 `extern "system"` ABI 禁止 unwind，首个 panic 即 abort 整个外壳）；19 个 FFI 回调
  全部套 `guard_abi`（wndproc 退化到 `DefWindowProcW`，子类化 proc 复刻 `CallWindowProcW` 链，
  枚举回调跳过该项继续）；静默失败补日志（前台激活三条策略全被拒、工作区写回、状态文件删除等）。
- **复用**：`win_util` 新增 `rect_contains` / `popup_menu`+`MenuRow` / `centered_origin` /
  `centered_upper_quarter` / `anchored_card` / `Brush`(RAII) 等共享件；弹出菜单样板 5 处、矩形命中
  6 处、居中 3 处、画刷 7 处、`wide()` 4 份收敛为 1；`theme()` 3 份收敛为 `config::theme()`；
  同名多义的 `GAP` 按语义改名（`BTN_GAP`/`PIN_GAP`/`SECTION_GAP`/`TIP_GAP`/`PREVIEW_GAP`/`LIST_GAP`），
  全仓只剩 `tray_bar::GAP` 一个。
- **耦合**：新增两个叶子模块解两处循环依赖——`hub`（shell HWND + 重建钩子，解
  `taskbar ↔ settings_ui`）、`model`（`Shortcut`，解 `launcher ↔ recents`）。
- **config 键单一来源**：设置面板写的每个键都是 `config::keys` 常量，写端与读端不再各自手拼字符串；
  一致性有单测锁死（setter 写的键必须就是 parser 读的键）。
- **日志卫生**：启动时替换模式下不再打印误导性的 "notification area mirrored count=0"；
  图标注册/注销提升到 INFO（此前 DEBUG 级，无法从日志判断"条是空的因为没 bug 还是没人注册"）；
  `clear_workarea_state` 不再在正常拆卸路径上打 WARN（文件本就该没了）。

### 验收

- `cargo test` 224 → **231**；`cargo fmt --check` / `clippy --all-targets -D warnings` 全绿。
- computer-use 实机闭环（替换模式 + harness 应用注册真实 HICON）：真图标绘制、
  四类事件转发逐个命中、tooltip 悬浮出现、hover/点击均不改变前台（`WS_EX_NOACTIVATE` 生效）。
- 已知边界：本会话无 explorer，`Shell_NotifyIconW` 不向外来 `Shell_TrayWnd` 路由（8 种 cbSize 全拒、
  sink 零消息），"真实应用自动注册"在此环境无法验证；协议注册（字节等价 payload）链路已全通。

## 0.1.2 — 2026-10-14

### 变更

- **任务栏左侧的两个冗余入口一并退役**（`src/taskbar.rs`、`src/settings_ui.rs`、`src/config.rs`）。
  「设置」右边原来依次是**开始菜单入口点**与**窗口列表按钮**，业主认为两者都没有价值（指示，2026-10-14）：
  - **开始菜单入口**：删除绘制与点击分支，并**取消常驻的 12px 占位**——新增 `grip_w()`，只在功能开启时
    返回槽位宽度，关闭时为 0。设置面板里的「显示开始菜单（默认关闭，立即生效）」复选行一并撤掉
    （连同 `SettingAction::ToggleStartMenu` 与 `config::set_start_menu`），避免留下一个"能把冗余入口召回"的死开关。
    功能本体（Places / 文件夹堆栈 / 搜索 / 最近使用）**不删**，配置键 `taskbar.menu = true` 或
    env `MINICON_START_MENU=1` 仍可唤回；开启后入口槽位与那个点会回来。真机验证：开启后点入口
    → 日志 `start menu shown`（`places=5 recent=2 power=3`）。
  - **窗口列表按钮**（≡ hamburger，原 `list_rect`）：删除布局、绘制与点击分支。`task_list` 模块**保留**——
    `»` 溢出按钮与分组按钮仍用它弹窗。设置按钮右侧不再有任何按钮，直接接快速启动/任务区，
    并保留 4px 标准间距（`after_settings_for`，否则两个同色按钮会糊成一块）。

### 修复

- **外壳身份不再被 explorer 夺走**（新增 `src/shell_launch.rs`）。`explorer.exe` 不带参数启动会判定"没有外壳"
  并自建任务栏，从而把会话抢回去（真实事故：quick-launch 钉了 `C:\Windows\explorer.exe`，一点就夺权）。
  现在所有"打开"动作统一走 `shell_launch`：`plan()` 把裸 explorer 改写成带"此电脑"CLSID 的形式，
  `shell:` moniker 转成真实路径；接入 quick-launch、开始菜单 Places、Run 对话框、launcher。
- **设置按钮不再会永久失效**（`src/settings_ui.rs`）。原来关闭设置窗走 `DestroyWindow`，
  句柄归零后任务栏设置按钮整场会话都点不开（日志 `settings show ignored: window not installed`）。
  现在 `WM_CLOSE` 只隐藏；`window()` 用 `IsWindow` 校验陈旧句柄，`toggle()` 缺窗则懒重建。
- **「接管桌面」不再自杀**（`src/shell_swap.rs`）。`shell_running()` 会把**本进程自己**（我们是外壳，
  自己拥有 `Shell_TrayWnd`/`Progman`）也算成"会话里已有外壳"，于是 `stop_shell()` 停掉自己——
  表现为点一下接管，任务栏就消失。现在：`take_over()` 检测到"已经是当前外壳"直接拒绝，
  `stop_shell()` 只处理**其他**进程持有的外壳窗口。
- **任务栏按钮左键改为「激活 / 最小化」切换**（`src/taskbar.rs::toggle_or_activate`）。原来点任务按钮
  只做「调到前台」，永远无法最小化。现在与系统外壳一致：目标窗口已在前台 → `SW_MINIMIZE`；
  否则（含已最小化）→ `SW_RESTORE` + 置前台。列表/溢出的显式选择（`task_list::popup`）仍只激活不切换。
- **任务栏本体补上 `WS_EX_NOACTIVATE`**（同上）。根因：鼠标按下会把任务栏自己变成前台窗口，
  于是点击处理里的 `GetForegroundWindow()` 永远返回「我们自己」，切换逻辑读不到真实状态（`is_fg` 恒为 false）；
  副作用是点任务栏会打断用户当前应用的焦点，与系统外壳行为不符。`src/bar_tip.rs` 早就用了这个样式，本体漏了。
  键盘导航不受影响——那是 `Alt+`` 召唤后显式 `SetForegroundWindow`/`SetFocus` 激活的（实测召唤后数字跳转仍生效），
  右键菜单、hover 高亮、快速启动也照常。

### 新增

- **外来外壳检测**：心跳里调用 `shell_swap::check_foreign_shell()`，仅在"出现/消失"跳变时记日志，
  不做自动夺回（夺回会连坐关掉用户的文件窗口）。恢复手段是显式的 `--shell-takeover`。
- **文件夹堆栈**（`src/folder_stack.rs` + `config.folders`）：把常用文件夹钉成开始菜单行，
  双击展开其子项（`MAX_STACK_ITEMS = 15` 条，原生弹出菜单不滚动所以不取全量；首行「打开此文件夹」去看全部）。
  每个条目都经 `shell_launch` 打开，不会再让 explorer 夺权。
- **开始菜单搜索框 + 「最近使用」区**（`src/start_menu.rs`，见 P0-2）。
- **「显示开始菜单」开关**（配置键 `taskbar.menu`，**默认 `false`**；env `MINICON_START_MENU`）。
  本外壳由任务栏驱动，弹出菜单改为可选：关闭时任务栏不绘制菜单点、该区域不响应。
- **设置项依赖置灰**（`src/settings_ui.rs`）：关掉「显示任务栏」后，只在任务栏可见时才有意义的项
  （合并策略/依据、通知条、开始菜单、悬停预览）置灰并标注「（需先显示任务栏）」，点击不生效。

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

### 构建 / CI

- `clippy 1.99`（CI 的 `stable`）新增 `clippy::chunks_exact_to_as_chunks` lint，
  `src/shell_registry.rs` 与 `src/tray_host.rs` 两处 `chunks_exact(2)` 改为 `as_chunks::<2>()`。
  本机 clippy 为 1.97、**不会报此 lint**，因此之前只在 CI 暴露——**发版前务必 `gh run list` 确认 CI**，不能只信本地。

### 文档

- PRD 体系整理为 `docs/prd.md` + 子文档 `prd_{idx1}_{idx2}.md`（命名 = 树路径），新增方法学章节。
- `prd_3_1.md` 新增「立即接管」章节；`prd_2_1.md` 补资源实测基线与 3 条新不变量。
- 新增 `docs/plan_0_2_0.md`（0.2.0 版本计划），并在 `prd.md` §0.4 登记为「版本计划」。
- `README.md` 同步 M5/M11、命令表与模块表，并链接 CHANGELOG 与下一版计划。

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
