# plan_0_1_3 — 0.1.3 版本计划

> 版本目标：**v0.1.3** · 起草：2026-10-14 · 上位文档：[`prd.md`](prd.md)
> 上一版：**v0.1.2**（已发布，见 [`../CHANGELOG.md`](../CHANGELOG.md)）
> 定位：本文件是"这一版做什么"的排期，**不是需求源**。产品需求一律以 PRD 为准；
> 本文件只做 **选题 → 切片 → 分工 → 验收**。
>
> **主题：托盘可用 + 代码健康（封装 / 抽象 / 复用 / 稳健 / 耦合）。不新增产品功能。**
>
> 说明：本版多数条目是**工程债**而非产品需求，因此不需要 PRD 立项；唯一涉及产品行为的
> 「通知区点击转发」**已在 [`prd_1_1.md`](prd_1_1.md) 通知区一节标 ⬜**，符合"需求先回写 PRD"的约定。

---

## 0. 与 0.2.0 的关系（先讲清，避免两版打架）

| 版本 | 内容 | 与本版关系 |
|------|------|------------|
| 0.2.0 | 体验补齐（P0-1 文件夹堆栈 ✅、P0-2 菜单搜索/最近使用 ✅）+ 通知区点击转发（P1-1）+ 长跑取证（P2） | **P0-1 / P0-2 已交付**；**P1-1 由本版承接**；P2 长跑与 UWP 合并留 0.2.0 |
| **0.1.3（本版）** | **托盘收敛** + **代码健康** | 0.1.x 线上的"清偿版"：把最危险的债先还掉 |

**为什么把 P1-1 提到 0.1.3**：调研确认托盘问题**本身就是耦合问题**——`src/tray.rs`（镜像）与
`src/tray_host.rs`（宿主）是同一功能的**两套数据模型**，宿主路径的点击转发已写完却 `#[allow(dead_code)]`
挂着（`tray_host.rs:63,72-81`）。把它放进"耦合治理"一起做，比在 0.2.0 单独做省一半力气。

**本版明确不做**（见 §6 门控）：拆分 `src/taskbar.rs`（2850 行上帝模块）——顺延 0.2.0。

---

## 1. 起点：v0.1.2 结束时站在哪

| 维度 | 实测值（2026-10-14） |
|------|---------------------|
| 规模 | `src/` **33 个文件 / 13,786 行**；三巨头 `taskbar.rs` 2850、`start_menu.rs` 1391、`settings_ui.rs` 1270 |
| 文档 | `docs/` 8 个 PRD 子文档 + 1 个版本计划；`CHANGELOG.md` / `README.md` |
| 质量关卡 | `cargo test` **215 passed**、`cargo fmt --check` ✅、`cargo clippy --all-targets -D warnings` ✅、CI run `38518940155` success |
| 资源 | 私有字节 ≈ **7.6 MB**；release 二进制 1.87 MB |
| 本地件 | `scripts/` 15 个文件，其中 `*.py` 全部 gitignore；`shellctl.cmd/.ps1` 入库 |

**判断**：形态与质量关卡都稳，但**四份专项调研（耦合 / 抽象 / 复用 / 稳健）一致指向同一类问题——
"同一个意思写了不止一遍"，以及"外壳进程的 panic 面没有护栏"**。前者让每次小改都要动多个地方，
后者直接对应过"点一下栏就没了"的真实事故。0.1.3 应当只做这两件事。

---

## 2. 本版本目标与非目标

### 目标（3 条）

1. **托盘在替换模式下真正可用**：图标是**真图标**（不是现占位的彩色方块）、**点击能转发回宿主程序**。
2. **外壳"不该死"**：把消息线程上的 panic 面（41 处 `lock().unwrap()` + 无 `catch_unwind` 的 wndproc）压到 0；
   把关键路径上的**静默失败**补上日志。
3. **同义实现收敛为一份**：几何 / 宽字符 / 弹出菜单样板 / 矩形命中 / 配置 setter / 主题，各留一份。

### 非目标（本版明确不做）

- 任何**新产品功能**（不做可浏览桌面、不做堆叠窗口组、不做主题拾色器）。
- 不改 `Cargo.toml` / 不新增 windows crate feature（沿用现有 0.62 冻结 feature 集）。
- 不动 explorer 夺权逻辑（`shell_launch` / `shell_swap`，v0.1.2 刚动过，本版只碰它的**错误日志**）。
- 不引入 watchdog 守护进程（业主已明确不做）。
- **不拆 `src/taskbar.rs`**（见 §6 门控）。

---

## 3. 选题与切片

> 每条都给出**调研证据（file:line）**，便于开工时直接定位，不用重新找。

### P0-1 稳健性：消息线程不再"一 panic 就掉桌面" ⚠️ 最高优先

**证据**
- `src/taskbar.rs` 有 **33 处** `Mutex::lock().unwrap()`（`window_icon.rs` 6、`config.rs` 2，共 **41 处**）；
  这些点全部位于消息线程。
- wndproc 是 `unsafe extern "system" fn`（`taskbar.rs:1276` 等）——**ABI 不允许 unwind**，
  体内 panic 会 **abort 整个进程**；即便不 abort，持锁期间 panic 会**毒化 Mutex**，后续 `.lock().unwrap()`
  连锁 panic。**唯一消息线程上的首个 panic = 用户桌面消失**（这正是历史事故的形态）。
- 反例已存在：`tray_host.rs:159-165 / 324-329 / 576-579` 已经用了 `Err(poisoned) => poisoned.into_inner()`
  的恢复写法——**正确姿势项目里已经有了，只是没推广**。

**做法**
1. 抽 `win_util::lock_or_recover(&Mutex<T>) -> MutexGuard<T>`（**纯函数 + 单测**：故意毒化一个锁，
   断言仍能拿到内部值），全仓库 41 处替换。
2. 给**每个** wndproc 包 `catch_unwind`：捕获后记 `error!` 并返回 `DefWindowProcW`，不让 unwind 越过 ABI 边界。
3. 补 6 处关键**静默失败**的日志（`let _ =` 且无 log）：
   `taskbar.rs:269,289`（工作区恢复）、`taskbar.rs:818-821`（AppBar 停靠）、
   `win_util.rs:41-74`（激活回退链——失败即"点了没反应"）、`window.rs:245`（WM_CLOSE 投递）、
   `shell_swap.rs:184`（TerminateProcess）、`taskbar.rs:349`（remove_file）。

**约束**：不改变任何行为，只把"静默的死"变成"有日志的活"。

---

### P0-2 托盘收敛：从"看得到假方块"到"真图标 + 能点"

**现状（调研结论）**：两套实现互不相通，任务栏只接了废的那套。

| 路径 | 模块 | 问题 |
|------|------|------|
| 镜像（在用） | `tray.rs` + `tray_bar.rs` | ① **我们是外壳时读不到**（我们那条 `Shell_TrayWnd` 是 `tray_host` 建的假窗，无 `TrayNotifyWnd` 子窗 → `find_tray_toolbar()` 返回 None）→ **替换模式下托盘条是空的**；② `pid` 恒为 0（`tray.rs:224` 自认推不出）→ 永远同色；③ `tray_bar.rs:101` 画的是 `CreateSolidBrush` **彩色方块**（`tray_bar.rs:82` 自述 "no bitmaps yet"） |
| 宿主（替换模式该用的） | `tray_host.rs` | 数据齐（`TrayIconHandle{uid,hwnd,callback_msg,tooltip,hicon}`），**点击转发已实现**（`StripLayout::hit_test` / `MouseEvent` / `forward_target` / `forward_at` / `forward_to`）**且整段 `#[allow(dead_code)]`**；`taskbar.rs` **从不引用它** |

**做法（4 步，按序）**
1. **单一数据源**：`taskbar::refresh()` 改为"宿主优先"——`tray_host` 已安装 → 用 `tray_host::icons()`；
   否则回退 `tray::enumerate()` 镜像。删掉 `tray_bar.rs` 自带的几何，改调 `tray_host::StripLayout`
   （**消掉 `CELL/GAP` 与 `HIT_CELL/HIT_GAP` 两份常量**，否则命中与绘制会错位）。
2. **画真图标**：宿主图标用 `hicon` + `DrawIconEx`；镜像路径没有 hicon，保留占位但**降低对比度**，
   不再冒充真图标（`config.rs:127` 那条"experimental"描述同步改）。
3. **点击接线**：任务栏托盘条加命中测试 → `forward_at(MouseEvent::LeftDown/DoubleClick, layout, x, y)`。
   **`MouseEvent` 目前缺右键**，需补 `WM_RBUTTONDOWN`（普通宿主程序右键菜单是刚需）。
4. **摘掉 `#[allow(dead_code)]`**，让编译器重新把未接线的地方报出来。

**关键安全点（务必遵守，来自稳健调研）**
- `tray_host` 收到的 `hicon` **属于别的进程**（`tray_host.rs:535,596`）。现状"只存不画"是安全的；
  **一旦开始 `DrawIconEx` 就要小心**：先用 `CopyImage(IMAGE_ICON)` 取本进程副本再画，
  **绝不 `DestroyIcon` 那个外来句柄**。
- 跨进程 `SendMessageTimeoutW` 超时是 **2000ms**（`tray.rs:32`），而 `refresh()` 在**消息线程**上跑
  `TB_BUTTONCOUNT + 每按钮 2 次`——托盘宿主卡死时一次刷新可阻塞数秒。降到 ~250ms 或加缓存。

**验收方式（2026-10-14 校正）**：agent 沙箱**不能新起 GUI 进程**，但**能驱动已在运行的界面**——
`SetCursorPos` + `mouse_event` / `keybd_event` 发真实输入 → `scripts/capture.py` 截图 → `Read` 读图，
并用 ctypes 读回 `GetForegroundWindow` / `IsIconic` 做程序化佐证。因此托盘点击**本代理可自行闭环验收**
（方法见项目 `MEMORY.md`「目视验证回路」；该闭环已于 2026-10-14 在任务按钮 toggle 上跑通）。
业主只在发版前做一次体感确认，**不作为阻塞项**。**本版没有"无法自证"的条目。**

---

### P1-1 复用收敛（零行为变更，纯搬家）

| 重复项 | 证据 | 收敛方向 |
|--------|------|----------|
| **托盘格子几何写两遍** | `tray_bar.rs:25,39` ↔ `tray_host.rs:227,240,261` | 保留 `StripLayout`（它多了 i64 溢出保护与 `hit_test`），删裸函数 |
| **弹出菜单样板 5 处手抄** | `taskbar.rs:2149,2231,2359` / `task_list.rs:51` / `start_menu.rs:933`（均 `CreatePopupMenu → AppendMenuW → TrackPopupMenu → DestroyMenu → id 分发`，连 `TPM_*` 标志也三份） | 抽 `win_util::popup_menu(owner, items) -> Option<usize>` |
| **`wide()` 4 份** | 已有一份在 `win_util.rs:18`；`tray.rs:299` 另写同名一份；`shell_swap.rs:229` / `single_instance.rs:44` 内联 `encode_utf16().chain(once(0))`；另有 ~12 处内联 `encode_utf16` | 全改调 `win_util::wide`，删重复 |
| **矩形命中谓词 6 份** | `quick_launch.rs:197`、`settings_ui.rs:320,333`、`taskbar.rs:1294,1512`、`tray_host.rs:191` | 收成 `win_util::rect_contains(r, x, y)` |
| **居中原点 3 份** | `launcher_ui.rs:206` ↔ `run_dialog.rs:239`（**逐字相同**）、`settings_ui.rs:236` | 收成一个 `centered_origin(w, h, quarter: bool)` |
| **锚点弹层定位 2 份** | `hover_preview.rs:83` ↔ `bar_tip.rs:68`（**连注释都同构**） | 合成 `place_near(anchor, work, w, h, gap)` |
| **配置 setter 样板** | `config.rs:401,406,411,421,431` 五个 setter 手写 `update(|c| ...,"key","value")`，bool→`"true"/"false"` 重复 3 次；且 `apply()`（`config.rs:192-262`）对同一批 key **再写一遍解析** | `(key, field)` **表驱动**，解析与写入共用一张表 |
| **主题访问器 3 份** | `bar_tip.rs:132` / `settings_ui.rs:351` / `taskbar.rs:204` 各自 `OnceLock<Theme>` 包装同一个 config 值 | 收敛为 config 单一访问器 |
| **同义常量散落** | `ERROR_CLASS_ALREADY_EXISTS=1410` 三份（`bar_tip.rs:40`/`hover_preview.rs:54`/`tray_host.rs:52`）；同名多义 `GAP` = 4（`taskbar.rs:64`/`quick_launch.rs:21`/`start_menu.rs:47`）与 6（`bar_tip.rs:47`/`hover_preview.rs:46`） | 集中到 `metrics` / 新 `layout` 模块 |

**约束**：**每一条都必须"先有单测/characterization，再搬家"**；纯函数搬家允许零行为变更，禁止顺手改逻辑。

---

### P1-2 耦合：解掉两个循环依赖

| 环 | 证据 | 解法 |
|----|------|------|
| `taskbar` ↔ `settings_ui` | `taskbar.rs:1396`（`settings_ui::toggle()`） ↔ `settings_ui.rs:599`（`taskbar::refresh()`） | 用回调/事件解环：settings 只**发** `refresh_requested`，由装配层（`window.rs`）注册的处理函数去调 `taskbar::refresh` |
| `launcher` ↔ `recents` | `launcher.rs:94,95,168` 用 `recents::` ↔ `recents.rs:11`（`use crate::launcher::Shortcut`） | 把 `Shortcut` 提到独立的 `src/model.rs`（或 `launcher.rs` 之外的叶子模块），recents 不再反向依赖 launcher |

顺带：`SHELL_HWND` 同名静态两份（`window.rs:38` / `window_events.rs:27`）——明确归属或合并。

---

### P1-3 抽象的边界：禁止绕过 `win_util`

**证据**：`win_util` 只封装了 `wide` / `force_foreground` / `window_exe_path` 三样，而：
- **`force_foreground` 被绕过 9 次**：`launcher_ui.rs:235`、`run_dialog.rs:267`、`start_menu.rs:813,953`、
  `taskbar.rs:1534,2174,2277,2369`、`task_list.rs:83` 全都裸调 `SetForegroundWindow`（多数只调一次，容易被系统拒绝）。
- `tray.rs` 整段跨进程 FFI（`OpenProcess`/`VirtualAllocEx`/`ReadProcessMemory`/`SendMessageTimeoutW`）**完全没有封装层**。

**做法**：① 9 处统一改调 `win_util::force_foreground`；② 把 `tray.rs` 的跨进程读、以及在多个模块重复的
`CreateSolidBrush + DeleteObject`（`bar_tip.rs:107`、`settings_ui.rs:363,370,377`、`taskbar.rs:555,603`、`tray_bar.rs:101`）
收成 RAII 封装（`Brush`）与 `win_util::remote_read` / `send_timeout`。

---

### P2 收尾（文档 / 可见性，低风险）

- **`pub` → `pub(crate)` 批量降级**：`src/lib.rs` 是**空文件**、`main.rs:6-37` 全是私有 `mod`，
  即这是一个**二进制 crate，没有任何外部消费者**；但全仓库有 **150+ 个 `pub` 项**——
  `pub` 什么也不表达，反而掩盖了真实耦合。降级后再由编译器告诉你哪些是真的跨模块 API。
- **`INTERFACE FROZEN` 标记清理**：**16 个文件**带该注释，但其中多数已是完整实现
  （`start_menu.rs` 1391 行、`settings_ui.rs` 1270 行）。标记失真会误导维护者以为它们是签名占位。
  **只保留真正是 stub 的**（如 `tray_bar.rs`）。同时删掉"edit only this file"这类与事实矛盾的措辞。
- **注释与 v0.1.2 现状对齐**：`config.rs:137-141` / `settings_ui.rs:155-159` 的叙述需要点明
  「退役的是**开始菜单行**，而 `task_list` 的窗口列表**按钮**是另一件事、已于 v0.1.2 一并退役」。

---

## 4. 分期与并行分工

沿用 [`prd_5_1.md`](prd_5_1.md) §11 工作方式：**大改动拆并行 subagent，每个只改唯一文件**；接线由主代理单点做。

| 波次 | 切片 | 唯一负责文件 | 说明 |
|------|------|--------------|------|
| W1 | 锁恢复工具 + 单测 | `src/win_util.rs` | 纯函数，先做，后续全依赖它 |
| W1 | 托盘宿主：补右键 + 摘 `dead_code` | `src/tray_host.rs` | 可独立并行 |
| W2 | 41 处 `lock().unwrap()` 替换 + wndproc `catch_unwind` | `src/taskbar.rs`、`src/window_icon.rs`、`src/config.rs` | 依赖 W1；**风险中**，逐文件做 |
| W2 | 几何/命中/居中/`wide` 收敛 | `src/tray_bar.rs`、`src/quick_launch.rs`、`src/settings_ui.rs`、`src/hover_preview.rs`、`src/bar_tip.rs` | 纯搬家 + characterization 单测 |
| W3 | 托盘接线（单一数据源 + 真图标 + 点击） | `src/taskbar.rs`（主代理单点） | 依赖 W1/W2；**代理自行驱动 + 截图验收** |
| W3 | 菜单样板收敛 | `src/task_list.rs`、`src/start_menu.rs` | 依赖 W1 的 `popup_menu` |
| W3 | 解 `taskbar ↔ settings_ui` 环 | `src/window.rs`（装配层）+ 两侧各一处 | 主代理做 |
| W4 | 解 `launcher ↔ recents` 环、`pub(crate)` 降级、FROZEN 清理 | `src/model.rs`（新）、全仓 | 机械替换 + 编译验证 |
| W4 | config setter 表驱动 | `src/config.rs` | 有单测兜底，可并行 |
| W5 | 关卡 + 热切换 + **代理驱动验收**（业主体感确认可选） | 无（运维） | 见 §5 |

**subagent 通用禁令**（照抄 PRD）：禁改 `Cargo.toml`、禁 `cargo add`、禁 `cargo fmt`、禁起 GUI；
可跑 `cargo check`；**纯逻辑必须带单测**。

---

## 5. 验收基线（硬性，不满足不发版）

| # | 条目 | 判定 |
|---|------|------|
| V1 | `cargo test` **≥ 215 passed**（v0.1.2 基线，**只增不减**；本版是收敛版，测试数应当明显上升） | `cargo test` |
| V2 | `cargo fmt --check` 通过、`cargo clippy --all-targets -- -D warnings` 干净 | CI |
| V3 | **不新增 worker 线程**；`bars()/tasks()/tray_icons()` 仅消息线程访问（`prd_2_1.md` §55） | 代码审查 |
| V4 | **重构切片必须行为等价**：每个切片单独 commit、可独立回滚；搬家前先补 characterization 单测 | `git log` + 审查 |
| V5 | 消息线程上 **0 处** 无护栏的 `lock().unwrap()`；wndproc 全部有 `catch_unwind` | Grep 取证 |
| V6 | **托盘验收：代理自行驱动**——截屏见真图标；驱动托盘图标点击，并用 ctypes 读回目标窗口状态，两者互证 | 驱动 + 截图 + 状态读回 |
| V7 | 热切换上线后日志 0 ERROR / 0 panic；私有字节 ≤ 20 MB、GDI 句柄持平 | 日志 + `metrics` |
| V8 | release 构建通过；`gh release create` 走 `.github/workflows/release.yml`（打 tag 即发布） | 实测 |

---

## 6. 风险与缓解

| 风险 | 缓解 |
|------|------|
| **重构无 UI 自动化测试兜底**（本版最大风险） | ① 每切片独立 commit + 可回滚；② 搬家前补 characterization 单测；③ 每波次后热切换目视一次 |
| **托盘 `DrawIconEx` 画外来 hicon 可能崩** | 先 `CopyImage` 取本进程副本再画；**绝不 `DestroyIcon` 外来句柄**（`tray_host.rs:535,596`） |
| 跨进程超时 2000ms 卡消息线程 | 降到 ~250ms + 结果缓存；必要时把镜像读取移出消息线程（**但不得新增持锁 worker**） |
| `catch_unwind` 掩盖真实 bug | 捕获后**必须** `error!` + 落日志（不许静默吞）；`metrics` 里加计数 |
| **上帝模块未拆，改动仍集中在 `taskbar.rs`** | 本版**接受**（见门控）；做完 P1 的收敛后，`taskbar.rs` 应显著变薄，为 0.2.0 拆分铺路 |
| 托盘图标由别的进程异步投递，时序不可控 | 驱动验收时若拍到空条，先等一轮 `refresh()` 再截；仍空则查日志里 `tray_host` 是否收到 `SHELLTRAYDATA` |

### 门控：为什么不拆 `src/taskbar.rs`（2850 行）

拆分是耦合治理里**收益最大也最危险**的一步。判断：**0.1.3 不做**。

理由：① 缺少 characterization 测试兜底（界面自绘逻辑没有自动化断言）；② 2850 行拆分回归面最大，
而本版目标是"把危险面压下去"，不宜同时引入新的不确定性；③ 先做 P0/P1 的**去重与解环**之后，
`taskbar.rs` 自然会变薄，拆分会更容易、更安全。
**门控**：若 P0/P1 全部落地且 `cargo test` 数量显著增长（有兜底了），可作为 0.1.3 的加餐；
否则**顺延 0.2.0**。

---

## 7. 待你拍板（开工前点头）

1. **本版主题确认**：0.1.3 = 「托盘收敛 + 代码健康」，**不新增产品功能**。是否同意？
2. ~~托盘验收的现实约束~~ — **已闭合**（2026-10-14）：代理可自行驱动运行中的界面并截图验收，
   见 §3 的「验收方式」校正说明。**此项不再是问题，无需拍板。**
3. **`pub` → `pub(crate)` 批量降级**：低风险但**触碰全仓 30 个文件**，会让 diff 很大、掩盖真实改动。
   做在 0.1.3、还是并进 0.2.0 的拆分一起做？

---

## 8. 时间线（建议）

1. 开工前：本计划的**决定**部分回写对应 PRD 子文档（`prd_1_1.md` 通知区一节已标 ⬜，无需新增）。
2. W1 → W2 → W3（接线）→ W4 → W5 全量关卡（V1–V8）→ 真机验收 → 发 **v0.1.3**（打 tag 即自动发 Release）。
3. 每波次结束跑一次完整关卡，**不允许攒到最后**（本版风险在"改得多"，靠小步降低暴露面）。
