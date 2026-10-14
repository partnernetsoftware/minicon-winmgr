# plan_0_1_3 — 0.1.3 版本计划（v2 · 产品设计已修订）

> 版本目标：**v0.1.3** · 起草 2026-10-14 · 产品设计修订 2026-10-14 · 上位文档：[`prd.md`](prd.md)
> 上一版：**v0.1.2**（已发布，见 [`../CHANGELOG.md`](../CHANGELOG.md)）
> 定位：本文件是"这一版做什么"的排期，**不是需求源**。产品需求一律以 PRD 为准；
> 本文件只做 **选题 → 切片 → 分工 → 验收**。
>
> **主题：通知区可用（替换模式）+ 代码健康（封装 / 抽象 / 复用 / 稳健 / 耦合）。不新增产品功能。**
>
> —— v2 修订说明见 §0.2「本版相对草案改了什么」。

---

## 0. 版本定位

### 0.1 与 0.2.0 的关系

| 版本 | 内容 | 与本版关系 |
|------|------|------------|
| 0.2.0 | 体验补齐（P0-1 文件夹堆栈 ✅、P0-2 菜单搜索/最近使用 ✅）+ 长跑取证（P2）+ `taskbar.rs` 拆分 | **P0-1 / P0-2 已交付**；**通知区点击转发（原 P1-1）由本版承接**；拆分与长跑留 0.2.0 |
| **0.1.3（本版）** | **通知区收敛** + **代码健康** | 0.1.x 线上的"清偿版"：把最危险的债先还掉 |

**为什么把转发提到 0.1.3**：调研确认通知区问题**本身就是耦合问题**——`src/tray.rs`（镜像源）与
`src/tray_host.rs`（宿主）是同一功能的**两套数据模型 + 三份几何常量**，宿主路径的点击转发已写完却
整段 `#[allow(dead_code)]` 挂着。把它放进"耦合治理"一起做，比在 0.2.0 单独做省一半力气。

### 0.2 本版相对草案（v1）改了什么

| # | v1 的问题 | v2 的修订 |
|---|-----------|-----------|
| 1 | 托盘只有"做法 4 步"，**没有产品形态定义**——两种运行形态下的行为、可点性、降级语义全靠实现者猜 | 新增 **§2.1 运行形态矩阵** 与 **§2.2 交互规格（事件矩阵）**，把"什么形态下条长什么样、能不能点、点了会发生什么"写成规格 |
| 2 | "镜像路径保留占位但降低对比度"——**没回答"能不能点"** | 明确：**只有替换模式的条可点**；伴随模式占位符**不响应任何输入**、不转发、不显示 tooltip |
| 3 | 跨进程超时 2000ms 只作为"风险"列出，**计划仍让它在 `refresh()` 里同步跑** | 升格为**硬约束**：镜像读取超时降到 **250ms**，且**移出 `refresh()`**，改由心跳采样 + 缓存（§2.3） |
| 4 | 模块职责没定，`tray` / `tray_bar` / `tray_host` 三份几何/常量/命中/转发**继续重叠** | 定义 **§2.4 目标架构**：数据源 / 呈现交互 / 宿主协议 三分，**每个关注点只有一个所有者** |
| 5 | 命中用 `forward_at(layout, x, y)` 现读全局表，**与"画的是什么"可能不是同一份快照** | 改为**对"刚画过的那份列表"做命中**（§2.2 规则 4）：绘制、命中、转发共用同一个快照，消除错位 |
| 6 | P2 的 `pub → pub(crate)` 全仓 150+ 项，v1 自己标注"会让 diff 很大"却仍列在计划里 | **收窄范围**：本版只收紧**本版动过的托盘链条**；全量降级并入 0.2.0 的 `taskbar.rs` 拆分（§5 决策 D3） |
| 7 | 配置 setter"表驱动"重构 | **降级**为"key 常量单一来源 + 一致性单测"（§5 决策 D4）：改动面小、能防住"加了 setter 忘了解析"，真正的表驱动留给 0.2.0 |
| 8 | 验收缺**负向**判定（只验"能点"，不验"不该点的不点"） | §6 增 V9（占位符不可交互）、V10（设置/解析 key 一致性） |
| 9 | 波次里"41 处锁 + wndproc 护栏"已完成，但计划未回写状态 | §4 波次表标注 W1 **已完成**并附取证命令 |

---

## 1. 起点：v0.1.2 结束时站在哪

| 维度 | 实测值（2026-10-14） |
|------|---------------------|
| 规模 | `src/` **33 个文件 / 14,282 行**；三巨头 `taskbar.rs` 2873、`start_menu.rs` 1435、`settings_ui.rs` 1286 |
| 文档 | `docs/` 8 个 PRD 子文档 + 版本计划；`CHANGELOG.md` / `README.md` |
| 质量关卡 | `cargo test` **224 passed**、`cargo fmt --check` ✅、`cargo clippy --all-targets -D warnings` ✅ |
| 资源 | 私有字节 ≈ **7.6 MB**；release 二进制 1.87 MB |
| 本地件 | `scripts/*.py` 全部 gitignore；`shellctl.cmd/.ps1` 入库 |

**W1 已完成取证**（开工前先确认，别重复做）：

| 项 | 命令 | 期望 |
|----|------|------|
| 锁恢复 | `grep -rn "\.lock()" src/` | 只剩 `win_util.rs` 的定义与它自己的毒化单测 |
| ABI 护栏 | `grep -rc "guard_abi" src/` | 每个 `unsafe extern "system" fn` 都有薄包装层 |
| 静默失败 | `git log --oneline --grep=silently` | 命中 `31e0c07` |

**判断**：形态与质量关卡都稳，但**四份专项调研（耦合 / 抽象 / 复用 / 稳健）一致指向同一类问题——
"同一个意思写了不止一遍"，以及"外壳进程的 panic 面没有护栏"**。前者让每次小改都要动多个地方，
后者直接对应过"点一下栏就没了"的真实事故。0.1.3 只做这两件事。

---

## 2. 本版目标与规格

### 2.0 目标（3 条）

1. **通知区在替换模式下真正可用**：图标是**真图标**（不是现占位的彩色方块）、**点击能转发回宿主程序**。
2. **外壳"不该死"**：消息线程上的 panic 面（无护栏的 `lock().unwrap()` + 无 `catch_unwind` 的 FFI 回调）压到 0；
   关键路径上的**静默失败**补上日志。*（W1 已完成）*
3. **同义实现收敛为一份**：几何 / 宽字符 / 弹出菜单样板 / 矩形命中 / 居中与锚点 / 主题 / 画刷，各留一份。

### 2.1 运行形态矩阵（v2 新增 · 产品定义）

我们有两种形态，**通知区的行为由形态唯一决定**，实现者不得自行发挥：

| | **替换模式**（我们是 shell） | **伴随模式**（explorer 仍是 shell） |
|---|---|---|
| 判定 | `shell_registry::is_self_shell()` 为真，或 `MINICON_TRAY_HOST=1` | 其余（`MINICON_TRAY_HOST=0` 强制归此） |
| 数据来源 | **宿主拥有**：别的进程 `Shell_NotifyIcon` → `WM_COPYDATA` → 我们（**推送，零跨进程读**） | **镜像**：读邻居的 `ToolbarWindow32`（拉取，跨进程） |
| 图标 | **真图标**（`hicon` → `CopyImage` 本进程副本 → `DrawIconEx`） | **降级占位方块**（拿不到 hicon），**对比度刻意低于真图标** |
| 可点击 | ✅ 转发回图标所属进程 | ❌ **不可交互**：不转发、不显示 tooltip、不改变光标 |
| 未注册任何图标时 | 条宽 0（不占位） | 同 |
| 用户怎么关 | 设置里「显示通知区」 | 同（但伴随模式下关掉它基本无感知损失） |

**产品判断（v2 明确）**：伴随模式下 explorer 自己会画真托盘，我们再画一排**不可点的假方块**
产品价值很低，仅作为"条还在、布局没变"的稳定占位保留。**因此它必须是"明显不可交互"的样子**，
而不能看起来像能点。这直接否掉了 v1 里"保留占位但只降低对比度、不作说明"的模糊处理。

**非目标（明确不做）**：不做"伴随模式下也画真图标 + 也能转发"。那需要读邻居进程的
`ImageList` 与 `TRAYDATA` 指针，把跨进程面扩大一倍，换来的只是"和 explorer 的托盘重复"。

### 2.2 交互规格（v2 新增 · 事件矩阵）

**规则 1 — 可点性由形态决定**：只有"图标带可用 owner 三元组 `(hWnd, uID, uCallbackMessage)`"时才转发。
宿主路径注册时即拿到三元组；镜像路径**拿不到**（`TBBUTTON.dwData` 不是进程 id），故不可点。

**规则 2 — 事件映射**：

| 用户动作 | 转发给宿主的消息（`lParam`） | `wParam` |
|---|---|---|
| 左键按下 | `WM_LBUTTONDOWN` (0x0201) | `uID` |
| 左键双击 | `WM_LBUTTONDBLCLK` (0x0203) | `uID` |
| 右键按下 | `WM_RBUTTONDOWN` (0x0204) | `uID` |
| 右键抬起 | `WM_RBUTTONUP` (0x0205) | `uID` |

发送方式：`PostMessageW(owner_hwnd, uCallbackMessage, uID, 消息值)`——与真托盘的 notify-icon 回调同形。
**右键按下与抬起必须分别发**：有的程序在 down 弹菜单，有的在 up 才收尾，合并会打断后者。

**规则 3 — hover 不转发**：鼠标移动**不**向宿主发 `WM_MOUSEMOVE`（会给别的进程刷消息，
且坐标语义（我们条上的坐标 ≠ 对方图标的坐标）会造成误判）。改为显示**我们自己的** tooltip 气泡
（复用 `bar_tip`），文本取图标的 `szTip`。

**规则 4 — 画的就是点的那份**：绘制、命中、转发**共用同一次刷新产生的图标快照 + 同一份布局**。
禁止命中去重新读一次全局表（v1 的 `forward_at` 有这个问题：画完之后表变了就会点错图标）。

**规则 5 — 失败不打扰**：owner 窗口已销毁、`PostMessageW` 失败、命中落空 → 记 `debug!` 日志，静默忽略。
**绝不弹框、绝不 panic。**

### 2.3 性能硬约束（v2 新增）

通知区**绝不允许拖慢任务栏**。

| 约束 | 值 | 理由 |
|---|---|---|
| 镜像路径跨进程超时 | **250 ms**（原 2000 ms） | `refresh()` 在消息线程上；`TB_BUTTONCOUNT + 每按钮 2 次`，宿主卡死时原本一次刷新可阻塞数秒 |
| 镜像采样时机 | **心跳（默认 30 s）+ 首次启动**，不在 `refresh()` 里 | 镜像值是**缓存的快照**；`refresh()` 只读缓存 |
| 替换模式跨进程调用 | **0 次** | 数据是推来的，根本不需要读别人的内存 |
| 新增线程 | **0** | `prd_2_1.md` §5.1 禁止新增 worker 抢锁 |

**可接受的代价**：镜像模式下新出现的图标最多延迟一个心跳周期（≤30 s）才显示。
这是可接受的，因为镜像模式本身只是"降级占位"（§2.1），不承诺实时。

### 2.4 目标架构（v2 新增 · 关注点唯一所有者）

```
                 ┌──────────────────────────────────────────────┐
                 │ 数据源（谁拥有通知区？有哪些图标？）          │
                 │ src/tray.rs                                  │
                 │   TrayItem  ← 唯一模型（tooltip/pid/hicon/owner）│
                 │   items()   ← 唯一入口：宿主优先 → 镜像兜底    │
                 │   (镜像读取：跨进程探测，250ms，心跳采样+缓存) │
                 └───────────────┬──────────────────────────────┘
                                 │ Vec<TrayItem>
                 ┌───────────────▼──────────────────────────────┐
                 │ 呈现与交互（条长什么样、点了算谁）            │
                 │ src/tray_bar.rs                              │
                 │   Layout     ← 唯一几何（cell/gap/宽度/命中）  │
                 │   paint()    ← 唯一绘制（真图标 / 占位）       │
                 │   forward()  ← 唯一转发（规则 2/4/5）          │
                 └───────────────▲──────────────────────────────┘
                                 │ 调用
                 ┌───────────────┴──────────────────────────────┐
                 │ src/taskbar.rs  ← 唯一调用方（画 + 命中接线）  │
                 └──────────────────────────────────────────────┘

                 ┌──────────────────────────────────────────────┐
                 │ 宿主协议（替换模式：收 Shell_NotifyIcon）      │
                 │ src/tray_host.rs                             │
                 │   install() / tray_wndproc / handles()        │
                 │   —— 只产数据，不管几何、不管绘制、不管命中    │
                 └──────────────────────────────────────────────┘
```

**收敛结果**：`HIT_CELL/HIT_GAP`（tray_host）与 `CELL/GAP`（tray_bar）**合并为一份**；
`StripLayout` 与 `cell_rect`/`strip_width` 的三份实现**合并为一份**；
命中与绘制的坐标计算**合并为一个 `Layout`**。

### 2.5 非目标（本版明确不做）

- 任何**新产品功能**（不做可浏览桌面、不做堆叠窗口组、不做主题拾色器）。
- 不改 `Cargo.toml` / 不新增 windows crate feature（沿用 0.62 冻结 feature 集）。
- 不动 explorer 夺权逻辑（`shell_launch` / `shell_swap`，v0.1.2 刚动过，本版只碰它的**错误日志与前台调用**）。
- 不引入 watchdog 守护进程（业主已明确不做）。
- 不做"伴随模式真图标 + 真转发"（§2.1）。
- **不拆 `src/taskbar.rs`**（见 §7 门控）。
- **不做全仓 `pub → pub(crate)`**（见 §5 决策 D3）。

---

## 3. 选题与切片

> 每条都给出**调研证据（file:line）**，便于开工时直接定位，不用重新找。

### P0-1 稳健性：消息线程不再"一 panic 就掉桌面" ✅ W1 已完成

**做法与结果**
1. `win_util::lock_or_recover`（含毒化单测）→ 全仓裸 `lock().unwrap()` 已清零（`grep` 只剩定义处）。`8dfa54b`
2. 19 个 FFI 回调全部包 `guard_abi`（`catch_unwind` + `error!` + 各自的默认 fallback）。`d31f22e`
3. 6 类关键静默失败补 `warn!`/`error!`；`force_foreground` 全策略被拒时也补了日志。`31e0c07`

---

### P0-2 通知区收敛：从"看得到假方块"到"真图标 + 能点" ⚠️ 本版唯一产品行为变更

**现状（调研结论）**：两套实现互不相通，任务栏只接了废的那套。

| 路径 | 模块 | 问题 |
|------|------|------|
| 镜像（在用） | `tray.rs` + `tray_bar.rs` | ① **我们是外壳时读不到**（我们那条 `Shell_TrayWnd` 是 `tray_host` 建的假窗，无 `TrayNotifyWnd` 子窗 → `find_tray_toolbar()` 返回 None）→ **替换模式下条是空的**；② `pid` 恒为 0（`tray.rs:224`）→ 永远同色；③ `tray_bar.rs:101` 画的是 `CreateSolidBrush` **彩色方块**；④ **2000ms 超时在消息线程上**（§2.3） |
| 宿主（替换模式该用的） | `tray_host.rs` | 数据齐（`TrayIconHandle{uid,hwnd,callback_msg,tooltip,hicon}`），**点击转发已实现**且整段 `#[allow(dead_code)]`；`taskbar.rs` **从不引用它** |

**做法（按 §2.4 架构，6 步）**
1. **定模型**：`tray::TrayItem { tooltip: String, pid: u32, hicon: isize, owner: Option<TrayOwner> }`，
   `TrayOwner { hwnd: isize, uid: u32, callback_msg: u32 }`。可点性 = `owner.is_some()`。
2. **定入口**：`tray::items()` 宿主优先（`tray_host::handles()` → 带 owner）→ 否则镜像兜底（`owner: None`, `hicon: 0`）。
3. **定几何**：`tray_bar::Layout`（由 `tray_host::StripLayout` 迁入，保留 i64 溢出保护与 `hit_test`），
   删 `tray_host` 的 `HIT_CELL/HIT_GAP/CellRect/to_i32` 与 `tray_bar` 的裸 `cell_rect`/`strip_width`。
4. **定绘制**：`tray_bar::paint(hdc, &layout, &items)`；有 `hicon` → `CopyImage(IMAGE_ICON)` 取本进程副本
   → `DrawIconEx` → 画完 `DestroyIcon` **副本**；无 `hicon` → 占位方块，色用 `pid_color`（且对比度降低）。
   **绝不 `DestroyIcon` 外来句柄。**
5. **定转发**：`tray_bar::forward(item: &TrayItem, event: MouseEvent) -> bool`（§2.2 规则 2/5）。
6. **接线**：`taskbar` 里 `tray_layout(client) -> Layout` 供**绘制与命中共用**；`WM_LBUTTONDOWN` /
   `WM_LBUTTONDBLCLK` / `WM_RBUTTONDOWN` / `WM_RBUTTONUP` 先试托盘命中（命中即 `return`，
   不再走任务按钮/时钟逻辑）；hover 用 `bar_tip` 显示 tooltip（规则 3）。
7. **摘掉 `#[allow(dead_code)]`**，让编译器重新把未接线的地方报出来。

**验收方式（2026-10-14 校正，仍有效）**：agent 沙箱**不能新起 GUI 进程**，但**能驱动已在运行的界面**——
`SetCursorPos` + `mouse_event` / `keybd_event` 发真实输入 → `scripts/capture.py` 截图 → `Read` 读图，
并用 ctypes 读回 `GetForegroundWindow` / `IsIconic` 做程序化佐证。**本版没有"无法自证"的条目。**
业主只在发版前做一次体感确认，不作为阻塞项。

---

### P1-1 复用收敛（基本零行为变更，纯搬家）

| 重复项 | 证据 | 收敛方向 |
|--------|------|----------|
| **托盘三份几何/常量** | `tray_bar.rs:18,20,25,39` ↔ `tray_host.rs:83,85,225,238` | 合并为 `tray_bar::Layout` 一份（§2.4） |
| **弹出菜单样板 5 处手抄** | `taskbar.rs:2174,2256,2382` / `task_list.rs:51` / `start_menu.rs:929` | 抽 `win_util::popup_menu(owner, items) -> Option<usize>` |
| **`wide()` 4 份** | `win_util.rs:62` 已有；`tray.rs:299` 另写一份；`shell_swap.rs:229` / `single_instance.rs:44` 内联 | 全改调 `win_util::wide` |
| **矩形命中谓词 6 份** | `quick_launch.rs:197`、`settings_ui.rs:320,333`、`taskbar.rs:1294,1512`、`tray_host.rs:191` | 收成 `win_util::rect_contains(r, x, y)` |
| **居中原点 2 份逐字相同** | `launcher_ui.rs:206` ↔ `run_dialog.rs:239`（**逐字相同**）；`settings_ui.rs:236` 是另一种（垂直也居中） | 合成 `win_util::centered_origin(screen, win, quarter: bool)`，两种模式都走它 |
| **锚点弹层定位 2 份** | `hover_preview.rs:83` ↔ `bar_tip.rs:68`（**连注释都同构**） | 合成 `win_util::anchored_card(anchor, work, w, h, gap)` |
| **主题访问器 3 份** | `bar_tip.rs:132` / `settings_ui.rs:351` / `taskbar.rs:204` 各自 `OnceLock<Theme>` 包同一个 config 值 | 收敛为 `config::theme()` 单一访问器 |
| **同义常量散落** | `ERROR_CLASS_ALREADY_EXISTS=1410` 三份（`bar_tip.rs:40`/`hover_preview.rs:54`/`tray_host.rs:52`）；同名多义 `GAP` = 4（`taskbar.rs:64`/`quick_launch.rs:21`/`start_menu.rs:47`）与 = 6（`bar_tip.rs:47`/`hover_preview.rs:46`）、= 8（`launcher_ui.rs:42`/`run_dialog.rs:39`） | 1410 收进 `win_util` ✅；各组 `GAP` 改语义名 ✅ `0fd2da6`——**实际名比本表的示例更贴用法**：`BTN_GAP`（taskbar）/ `PIN_GAP`（quick_launch）/ `SECTION_GAP`（start_menu）/ `TIP_GAP`（bar_tip）/ `PREVIEW_GAP`（hover_preview）/ `LIST_GAP`（launcher_ui）/ `BTN_GAP`（run_dialog）；`tray_bar::GAP` 保留，成为全仓唯一 `GAP` |
| **画刷 create/delete 手工配对 7 处** | `bar_tip.rs:277,280`、`hover_preview.rs:191,341`、`launcher_ui.rs:106`、`run_dialog.rs:88`、`start_menu.rs:484` | `win_util::Brush`（RAII `Drop` → `DeleteObject`）。**注意**：`taskbar.rs:571-577`、`settings_ui.rs:363-370,447` 是**长生命周期**画刷（存进结构体/`WNDCLASS`），**不要**套 RAII |
| **裸 `SetForegroundWindow` 9 处** | `launcher_ui.rs:235`、`run_dialog.rs:267`、`start_menu.rs:809,953`、`taskbar.rs:1563,2199,2302,2392`、`task_list.rs:83` | 统一改调 `win_util::force_foreground`（分层策略，减少"点了没反应"） |

**约束**：**每一条都必须"先有单测/characterization，再搬家"**；纯函数搬家允许零行为变更，禁止顺手改逻辑。
`force_foreground` 那条是**行为改进**（不是等价搬家），单独 commit 并在 commit message 里写明。

---

### P1-2 耦合：解掉两个循环依赖

| 环 | 证据 | 解法 |
|----|------|------|
| `taskbar` ↔ `settings_ui` | `taskbar.rs:1431`（`settings_ui::toggle()`） ↔ `settings_ui.rs:599`（`taskbar::refresh()`） | 装配层 `window.rs` 持有一个 `OnceLock<fn()>` 刷新回调：`window::set_refresh_hook(taskbar::refresh)`（在 `run()` 里注册）；`settings_ui` 改调 `window::request_refresh()`。两侧都不再互相 `use` |
| `launcher` ↔ `recents` | `launcher.rs:94,95,168` 用 `recents::` ↔ `recents.rs:11`（`use crate::launcher::Shortcut`） | 把 `Shortcut` 提到新的叶子模块 `src/model.rs`；`launcher` 与 `recents` 都只依赖它 |

顺带：`SHELL_HWND` 同名静态两份（`window.rs:38` / `window_events.rs:27`）——**明确归属**：
`window_events` 那份改为持有 `fn() -> HWND` 的注入，或直接用 `window::shell_hwnd()` 访问器。

---

### P1-3 抽象的边界：禁止绕过 `win_util`

**证据**：`win_util` 只封装了三样（`wide` / `force_foreground` / `window_exe_path`），而
`force_foreground` **被绕过 8 处**、画刷手工配对 **7 处**（见 P1-1 表）。

**做法**：① 8 处改调 `force_foreground`；② 7 处改 `Brush` RAII；
③ `tray.rs` 的跨进程读**加一层薄封装**（`remote::open/read/close`）——但**不追求**过度抽象，
因为镜像路径已降级为"心跳采样的占位条"（§2.1/§2.3），只在**超时常量、缓存、日志**三处收紧即可。

---

### P2 收尾（文档 / 可见性，低风险） ✅ 已完成 `348a9c6` `c1f9f5e`

- ✅ **`pub` → `pub(crate)` 降级（收窄版）**：`tray` / `tray_bar` / `tray_host` 与 `win_util` 全部转 `pub(crate)`。
  `win_util` 是**整文件**转的（不是只转本版新增项）——半转的文件比两端都更不诚实，且该文件本版确实动过。
  依据：模块都从 `main.rs` 声明、lib target 是个空的 `src/lib.rs`，所以 `pub` 本来就不可达 crate 之外。
  全仓 150+ 项降级并入 0.2.0（§5 决策 D3）。
- ✅ **`INTERFACE FROZEN` 标记清理**：实测 **13 个文件**（本节原写 16，是估的），逐个核对**全是完整实现**
  （最短 `task_list.rs` 98 行、最长 `start_menu.rs` 1408 行），**没有一个是 stub** → 标记全部删除。
- ✅ **注释与 v0.1.2 现状对齐**：`config.rs`（`start_menu`）与 `settings_ui.rs`（`build_rows`）原本都写
  "窗口列表按钮已覆盖找窗口"——**与事实相反**（v0.1.2 把两个入口一起退役了），已改正；`task_list.rs`
  的模块文档也不再自称那个已退役的按钮。`show_tray` 文档按 **§2.1 矩阵**重写（W5 已做，不再是
  "experimental / off-meaning"）。
- ✅ **设置界面文案**：`显示通知图标条（实验性）` → **`显示通知区域（仅作外壳时可点）`**。行文字有宽度预算
  （`body_font` 13px、可用宽约 390px，且置灰时还会追加「（需先显示任务栏）」），所以标签只写"条件 + 可点性"，
  完整两形态语义放在 §2.1 矩阵与本行源码注释里。新增单测 `the_tray_row_states_the_shape_dependent_clickability`
  把文案钉在 §2.1 上（V10）。
- ✅ **文档回写**：`prd_1_1.md` 通知区一节按 §2.1–§2.3 重写（两形态矩阵 + 事件矩阵 + 性能约束）；
  `prd_6_1.md` / `prd_4_1.md` / `prd_2_1.md` / `prd.md` / `README.md` 中已变成假话的行同步修正。
  `CHANGELOG.md` 的旧条目**不动**（那是历史记录），发版时新增 0.1.3 段。

---

## 4. 波次与并行分工

沿用 [`prd_5_1.md`](prd_5_1.md) §11：**大改动拆并行 subagent，每个只改唯一文件**；接线由主代理单点做。

> **进度（2026-10-14，W7 开工前）**：W1–W6 全部落地。`cargo test` 起点 224 → **231 passed**；
> `cargo fmt --check` 通过；`cargo clippy --all-targets -- -D warnings` 干净。
> 切片提交：`a771d60`（本计划 v2）、`927c118`（W2）、`0fddbf5`（W3）、`41033c5`（W4）、`021e533`（W5）、
> `348a9c6` + `c1f9f5e`（W6）、`0fd2da6`（P1-1 收尾：`GAP` 语义化改名，至此 §3 的 P1-1 表全部关闭）。
> **剩余：W7**——§6 的 V1–V11 验收 + 热切换目视 + 发版。
>
> **规模实测**：`src/` **36 个文件 / 14,861 行**（起点 33 / 14,282）。三巨头 `taskbar.rs` **2,995**
> （起点 2,873）、`start_menu.rs` 1,406、`settings_ui.rs` 1,321；新增叶子模块 `hub.rs` 81 + `model.rs` 18。
>
> ⚠️ **§7 门控要如实说明**：门控说"做完 P1 收敛后 `taskbar.rs` 应显著变薄"。**没有兑现**——
> 它净 **+122 行**，因为 W3 的托盘接线（4 个鼠标事件的命中→转发、`tray_layout` 与命中共用、
> hover tooltip 分支）落在这一个文件里，收益被抵掉了。同时测试只 +7（224→231），够不上门控要的
> "显著增长"。**结论：按门控顺延 0.2.0 拆分**，与 §2.5 非目标一致——本版不拆 `taskbar.rs`，
> 只把它"该去哪去哪"的部分（几何/命中/转发）搬到 `tray_bar`，留下了"接线"这一层，这本就是它的职责。

| 波次 | 切片 | 唯一负责文件 | 状态 |
|------|------|--------------|------|
| W1 | 锁恢复工具 + 单测 | `src/win_util.rs` | ✅ `8dfa54b` |
| W1 | 托盘宿主：补右键 + 摘 `dead_code` | `src/tray_host.rs` | ✅ `7c11ba5` |
| W1 | FFI 回调护栏 ×19 | 全仓 | ✅ `d31f22e` |
| W1 | 静默失败补日志 | 全仓 | ✅ `31e0c07` |
| W2 | `win_util` 扩容器：`rect_contains` / `popup_menu` / `centered_origin` / `anchored_card` / `Brush` / `ERROR_CLASS_ALREADY_EXISTS` / `wide` 兜底 | `src/win_util.rs` | ✅ `927c118` |
| W3 | **通知区重构**：模型 + `items()` + 镜像缓存（250ms/心跳） | `src/tray.rs` | ✅ `0fddbf5` |
| W3 | **通知区重构**：`Layout`/`paint`/`forward`（迁入 `StripLayout`） | `src/tray_bar.rs` | ✅ `0fddbf5` |
| W3 | 宿主瘦身：删几何/命中/转发，只留协议 | `src/tray_host.rs` | ✅ `0fddbf5` |
| W3 | **接线**（绘制 + 命中共用布局 + hover tooltip） | `src/taskbar.rs`（主代理单点） | ✅ `0fddbf5` |
| W4 | 调用点收敛：菜单 5 处 / 命中 6 处 / 居中 2 处 / 锚点 2 处 / 主题 3 处 / 画刷 7 处 / 前台 8 处 | 各调用文件 | ✅ `41033c5` |
| W5 | 解 `taskbar ↔ settings_ui` 环（新增叶子模块 `src/hub.rs`） | `src/window.rs` + 两侧 | ✅ `021e533` |
| W5 | 解 `launcher ↔ recents` 环（新增 `src/model.rs`） | `src/model.rs`、`src/launcher.rs`、`src/recents.rs` | ✅ `021e533` |
| W5 | `config` key 常量单一来源 + 一致性单测（决策 D4） | `src/config.rs` | ✅ `021e533` |
| W6 | FROZEN 清理（13 个文件全部是完整实现，标记全删）+ 注释对齐 + 文档回写 | 全仓注释 + `docs/` | ✅ `348a9c6` `c1f9f5e` |
| **W7** | 关卡 + 热切换 + **代理驱动验收** + 发版 | 无（运维） | ▶ 本波 |

**subagent 通用禁令**（照抄 PRD）：禁改 `Cargo.toml`、禁 `cargo add`、禁 `cargo fmt`、禁起 GUI；
可跑 `cargo check`；**纯逻辑必须带单测**；每个 subagent **只改一个文件**，改前先重读（有并发编辑史）。

---

## 5. 计划决策（产品设计角色已拍板）

| # | 待决问题 | 决定 | 理由 |
|---|----------|------|------|
| **D1** | 本版主题 | **「通知区收敛 + 代码健康」，不新增产品功能** | 0.1.x 是清偿线；四份调研指向同一批债 |
| **D2** | 伴随模式下条的行为 | **占位、不可点、不 tooltip、低对比度**；不做真图标/真转发 | 与 explorer 的真托盘重复，价值低；跨进程面翻倍 |
| **D3** | `pub → pub(crate)` 全仓降级 | **收窄**：只做本版动过的文件；全量并入 0.2.0 | 150+ 项、30 文件会让 diff 淹没本版真实改动；本版已有大改 |
| **D4** | 配置 setter 表驱动 | **降级**为 key 常量单一来源 + 一致性单测；真正表驱动留 0.2.0 | 15 个 key、改动频率低；一致性测试即可防住"加了 setter 忘了解析"，重构收益不抵风险 |
| **D5** | 镜像跨进程读 | **保留**但降级：250ms 超时 + 心跳采样 + 缓存 | 不删功能（可逆）；解决消息线程卡顿；产品语义干净 |
| **D6** | 命中基准 | **对"刚画的那份快照"命中**，不重读全局表 | 消除绘制/命中错位导致的点错图标 |

---

## 6. 验收基线（硬性，不满足不发版）

| # | 条目 | 判定 |
|---|------|------|
| V1 | `cargo test` **≥ 224 passed**（v0.1.3 起点基线，**只增不减**；本版是收敛版，测试数应明显上升） | `cargo test` |
| V2 | `cargo fmt --check` 通过、`cargo clippy --all-targets -- -D warnings` 干净 | CI |
| V3 | **不新增 worker 线程**；`bars()/tasks()/tray_icons()` 仅消息线程访问（`prd_2_1.md` §5.1） | 代码审查 |
| V4 | **重构切片必须行为等价**：每切片单独 commit、可独立回滚；搬家前先补 characterization 单测 | `git log` + 审查 |
| V5 | 消息线程上 **0 处** 无护栏的 `lock().unwrap()`；FFI 回调全部有 `guard_abi` | `grep` 取证 |
| V6 | **正向**：截屏见**真图标**；驱动托盘图标左键 → owner 窗口状态变化（ctypes 读回） | 驱动 + 截图 + 状态读回 |
| V7 | 热切换上线后日志 0 ERROR / 0 panic；私有字节 ≤ 20 MB、GDI 句柄持平 | 日志 + `metrics` |
| V8 | release 构建通过；打 tag 即经 `.github/workflows/release.yml` 自动发 Release（**先确认 CI 绿再打 tag**） | 实测 + `gh run list` |
| **V9** | **负向**：伴随模式（镜像）下点击条**不产生任何转发**（owner 为 None）；hover 不改变前台窗口 | 驱动 + 日志验证"无 forward" |
| **V10** | 设置项与其 config key 的**读写一致性**有单测锁住；`show_tray` 文案与 §2.1 矩阵一致 | 单测 + 文案审查 |
| **V11** | `tray` / `tray_bar` / `tray_host` 三处几何常量确认为**一份**；`HIT_CELL/HIT_GAP` 已不存在 | `grep` 取证 |

---

## 7. 风险与缓解

| 风险 | 缓解 |
|------|------|
| **重构无 UI 自动化测试兜底**（本版最大风险） | ① 每切片独立 commit + 可回滚；② 搬家前补 characterization 单测；③ 每波次后热切换目视一次 |
| **托盘 `DrawIconEx` 画外来 hicon 可能崩** | 先 `CopyImage(IMAGE_ICON)` 取本进程副本再画；**绝不 `DestroyIcon` 外来句柄**；副本用完自己 `DestroyIcon` |
| 镜像读取卡消息线程 | 250ms 超时 + 移出 `refresh()`（心跳采样 + 缓存）——见 §2.3/D5 |
| `catch_unwind` 掩盖真实 bug | 捕获后**必须** `error!` + 落日志（不许静默吞）；`metrics` 里可加计数 |
| **上帝模块未拆，改动仍集中在 `taskbar.rs`** | 本版**接受**（见门控）；做完 P1 的收敛后 `taskbar.rs` 应显著变薄，为 0.2.0 拆分铺路 |
| 托盘图标由别的进程异步投递，时序不可控 | 驱动验收时若拍到空条，先等一轮心跳 + 手动触发一次 `refresh()` 再截；仍空则查日志里 `tray_host` 是否收到 `SHELLTRAYDATA` |
| `force_foreground` 兜底 `SW_MINIMIZE→SW_RESTORE` 会让窗口闪一下 | 可接受（比"点了没反应"好）；若观察到大面积闪烁，回退该处为裸 `SetForegroundWindow` + `warn!` |

### 门控：为什么不拆 `src/taskbar.rs`（2873 行）

拆分是耦合治理里**收益最大也最危险**的一步。判断：**0.1.3 不做**。

理由：① 缺少 characterization 测试兜底（界面自绘逻辑没有自动化断言）；② 2873 行拆分回归面最大，
而本版目标是"把危险面压下去"，不宜同时引入新的不确定性；③ 先做 P0/P1 的**去重与解环**之后，
`taskbar.rs` 自然会变薄，拆分会更容易、更安全。
**门控**：若 P0/P1 全部落地且 `cargo test` 数量显著增长（有兜底了），可作为 0.1.3 的加餐；否则顺延 0.2.0。

---

## 8. 时间线

1. 开工前：本计划的**决定**部分回写对应 PRD 子文档（`prd_1_1.md` 通知区一节按 §2.1/§2.2 补齐）——随 W3 一起提交。
2. W2 → W3（重构 + 接线）→ W4 → W5 → W6 → W7 全量关卡（V1–V11）→ 真机验收 → 发 **v0.1.3**。
3. 每波次结束跑一次完整关卡，**不允许攒到最后**（本版风险在"改得多"，靠小步降低暴露面）。
4. **发版前必做**：`gh run list --limit 3` 确认最新提交 CI 为 `completed success`，再打 tag。
