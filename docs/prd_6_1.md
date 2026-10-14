# prd_6_1 — 任务栏交互策略 · 合并 × 密度

> 索引：6.1 · 树路径：`任务栏交互策略` · 上位文档：[`prd.md`](prd.md)
> 关联：[`prd_1_1.md`](prd_1_1.md)（功能明细）、[`prd_4_1.md`](prd_4_1.md)（设计语言）、[`prd_2_1.md`](prd_2_1.md)（不变量）
> 定位：把"窗口合并"从隐藏 env 提升为一等设置项，并定义"不合并 + 窗口很多"时的自适应规则。
> 状态：**一期 + 二期均已实现**——合并三态 + 合并依据 + 密度 D0/D1/D2 + 迟滞 + 设置面板 + 独立设置文件 + hover 标题气泡 + 键盘 `1..9` + 分组右键批量 + 通知区域条 + `»` 分列。（开始菜单入口已于 2026-10-14 从 UI 退役，仅留 `taskbar.menu` 隐藏开关。**通知区**于 0.1.3 收敛为"形态决定行为"——两形态矩阵与事件规格见 [`prd_1_1.md`](prd_1_1.md)「通知区 / 托盘」与 [`plan_0_1_3.md`](plan_0_1_3.md) §2.1–§2.3；本节只保留其**在任务栏布局里占多少宽**这一面。）实现对照见 §0.1，决策见 §12，API 核查见 §13。

## 0.1 实现状态（对照代码）

| 设计条目 | 状态 | 落地位置 |
|----------|------|----------|
| 合并三态 `never`/`when_full`/`always` | ✅ | `config::MergeStrategy` + `taskbar::merge_policy` |
| 合并依据 `exe`/`pid`（失败回退 pid） | ✅ | `config::GroupBy` + `taskbar::group_key_for`（`win_util::window_exe_path`，per-refresh 缓存，锁外 IO） |
| 组标签锚（不随激活漂移） | ✅ | `taskbar::choose_anchor` + `group_anchors()`（首次出现窗口锁定存活期） |
| `(+N)` 计数 | ✅ | `taskbar::group_label`（仅在合并档出现） |
| 密度阶梯 D0/D1/D2 + 迟滞 | ✅ | `taskbar::{Density, budget, density_for, layout}`（8px 死区） |
| 保底 ≥1 可见按钮且不覆盖时钟 | ✅ | `layout` 溢出分支（`k` 保底 + 溢出 rect 二次夹取） |
| 设置面板：合并三态 / 合并依据 / 显示任务栏 / 通知条 / 悬停预览 | ✅ | `settings_ui::{build_rows, row_rects, hit_row, row_action, toggle_flag}`（GDI 自绘，纯几何可单测） |
| 独立设置文件 + 优先级 + 写回保真 | ✅ | `config::{settings_path, upsert, persist, load}`（`env > settings > conf > 默认`，key-level upsert + `.bak`） |
| 即时生效 | ✅ | 命中控件 → `config::set_*` → `config::persist` → `taskbar::refresh`（主题仍重启生效） |
| 组按钮点击：已前台 → 展开成员，否则切回 | ✅ | `taskbar` 的 `WM_LBUTTONDOWN`（`members.len()>1 && contains(fg)`） |
| 分组右键批量菜单（打开组内全部/全还原/全最小化/全关闭/添加到快速启动） | ✅ | `taskbar::group_menu`（菜单 id 8..12，纯函数 `group_menu_action`）；`members.len()>1` 时替代单窗口 `window_menu` |
| D2 icon-only（含 hover 气泡 / `never` 局部折叠） | ✅ | `taskbar::Density::IconOnly` + `src/bar_tip.rs`（纯函数 `bar_tip::tip_rect`）+ `fold_needed` → `build_grouped` |
| 键盘 `1..9` 直跳 | ✅ | `taskbar::digit_index`（进入导航后按 `1..9` 移动光标，`Enter` 才激活） |
| 通知区域条开关（含条宽参与布局） | ✅ | 配置键 `taskbar.tray`（默认 `true`，env `MINICON_TRAY` 可覆盖）+ 设置复选行「显示通知区域（仅作外壳时可点）」；条宽由 `tray_bar::Layout` 唯一决定（`CELL=18`/`GAP=2`，无图标则为 0） |
| 开始菜单开关 | ⛔ 已退役 | 配置键 `taskbar.menu`（默认 `false`，env `MINICON_START_MENU` 可覆盖）**保留为隐藏开关**；任务栏入口与设置复选行均已于 2026-10-14 撤除 |
| 窄栏折叠 pins | ✅ | `taskbar::hide_pins`（带 pins 仍溢出、不带则不溢出 → 本轮隐藏 pins，pin 数据不删） |
| `»` 溢出弹层分列 | ✅ | `src/task_list.rs`（`PER_COLUMN=24` + 纯函数 `starts_new_column` + `MF_MENUBARBREAK`），`popup()` 签名不变 |

**已知偏差（接受）**：
1. `when_full` 的"满"判定用**首个有效栏宽**（无则主屏宽）作参考，且不计 pins 宽度——略保守但跨栏一致、不抖；真实判据仍是"密度降到底仍 `overflow.is_some()`"。
2. UWP / `ApplicationFrameHost` 宿主窗口可能解析到同一 exe 而**过度合并**（`exe` 策略的固有取舍，见 §13）。
3. **「折叠最久未用」未实现**：会破坏按钮位置稳定性（每次激活换可见集），当前保留**尾部折叠**（`visible..n`；零状态、与 `stable_order` 一致），`»` 菜单按列表顺序列出被折叠者（见 §3.3）。

## 0. 结论摘要

1. 任务栏拥挤拆成**两条正交轴**：**合并**（改按钮**数量**）× **密度**（改单按钮**形态**），互不反推，保证可预测、可单测。
2. 合并三态 `never`（默认）/ `when_full` / `always`；依据由 pid 改为 **exe 路径**（解析失败回退 pid）——pid 会把 Chrome/Electron 的一窗口一进程拆成一堆按钮。
3. 自适应阶梯：**先收紧内边距 → 再缩标题 → 最后才丢标题（图标方块）→ 溢出**；**时钟区永不被覆盖**，组标签**不随激活漂移**。
4. 设置项写入**独立文件** `minicon-settings.txt`，**不改写**用户手写的 `minicon.conf`。

## 1. 问题与目标

- **现状**：任务按钮默认每窗口一个；按 pid 分组是 opt-in 环境变量 `MINICON_GROUP_TASKS=1`（≡ `taskbar.group=always`）。用户无法从 UI 得知或修改。
- **拥挤现状**：`layout(start_left, client_w, n, tray_w, prev)` 在所有按钮都放不下时，显示尽量多的按钮 + 一个 `»`（`OVERFLOW_W=44`），其余进 `task_list::popup` 菜单，且时钟区永不被覆盖。进入完整档的阈值常量是 **`D0_ENTRY`（=120）**。
- **目标**：① 合并与否由用户在设置里决定；② 不合并时窗口变多仍保持"看一眼就知道系统在干什么、点一下就能切过去"；③ 所有新逻辑为纯函数、可单测、不新增线程。

## 2. 轴一：合并策略

### 2.1 取值与默认
| 存储值 | 中文文案 | 行为 | 默认 |
|--------|----------|------|------|
| `never` | 从不合并 | 每窗口一按钮（对齐 Alt+Tab） | ✅ |
| `when_full` | 任务栏放不下时合并 | 放得下就逐窗口；**密度降到底仍溢出**才合并 | |
| `always` | 始终合并 | 每应用一按钮 | |

**"满"的精确定义**（避免抖动）：= 密度阶梯降到最低档后 `layout()` 仍产生溢出按钮（`overflow.is_some()`）。不引入新阈值。

### 2.2 合并依据
| 候选 | 同进程多窗口（终端多标签） | 不同进程同应用（Chrome/Electron） | 结论 |
|------|--------------------------|--------------------------------|------|
| **exe 路径**（小写归一化） | 合并 ✓ | 合并 ✓ | **采用（默认）** |
| pid（现状） | 合并 ✓ | 拆开 ✗ | 设为回退 |
| 窗口类 | 粒度不准 | 粒度不准 | 弃 |

- 取值：`taskbar.group_by` = `exe` | `pid`（见 §6）。
- 实现：复用 `win_util::window_exe_path`（`QueryFullProcessImageNameW`），**解析失败回退 pid**，失败落 `tracing::debug`。
- 成本控制：exe 解析是跨进程调用，**只在合并开启时做**，且 `pid → exe` 结果在内存缓存（消息线程持有，无新锁）；解析发生在取 `bars()/tasks()` 锁**之前**（沿用"锁外 IO"约束）。

### 2.3 组内行为
| 项 | 规则 | 理由 |
|----|------|------|
| **标签锚** | 组内**首次出现**的存活窗口标题 + `(+N)`；锚窗口关闭则接续"次早出现的存活窗口" | 修缺陷：标签取 Z 序首窗口 → 激活即置顶 → **标签漂移** |
| **点组按钮** | 若组内**任一**成员是前台 → `task_list::popup` 展开组内列表；否则激活锚窗口（最小化先 `SW_RESTORE`） | 最常用动作"切回去"一次点击；二次点击才展开，对齐 Windows |
| **组内顺序** | 按首次出现固定，新窗口追加组尾；**不按 Z 序** | 与 `stable_order` 同哲学 |
| `(+N)` | 保留。分组必藏窗口，它是"数量入 UI"的唯一正当场景；`never` 下恒不出现 | |
| **组右键菜单** ✅ | 打开组内全部窗口 / 全部还原 / 全部最小化 / 全部关闭 / 添加到快速启动 | 纯函数 `group_menu_action`（id 8..12）；单窗口沿用 `window_menu` |

> **不做「从组中分离」**：与自动分组冲突，且窗口关闭即失效、难以持久化。用全局"合并依据"设置替代。

## 3. 轴二：密度自适应

### 3.1 密度阶梯
单按钮**预算宽** `budget = (avail - GAP*(n-1)) / n`；`avail` 沿用现有公式（扣 `PAD` / `RIGHT_W` / tray）。

| 档 | 形态 | 按钮宽 | 进入（budget ≥） | 停留地板 |
|----|------|--------|-----------------|---------|
| **D0** full | 图标 16 + 标题 | `[112, 200]` | 120（`D0_ENTRY`） | 112（`D0_FLOOR`） |
| **D1** compact | 图标 16 + 标题（内边距 8/7 → 6/5，标题更早省略） | `[80, 119]` | 自 D0 下探 80（`D1_FLOOR`）；自 D2 回升 88（`D1_ENTRY`） | 80 |
| **D2** icon-only | 仅图标方块（图标 20px，常量 `ICON_ONLY_SIZE`），**不画标题** | `[40, 48]` | 40（`D2_FLOOR`） | 40 |
| **D3** 溢出 | 尽量多方块 + `»`（44） | — | D2 也放不下 | — |

> `D1_ENTRY=88` **仅用于从更密档（D2）回升**到 D1；从 D0 下探到 D1 的阈值是 `D1_FLOOR=80`，二者间距即 8px 死区。

设计意图：**先缩内边距 → 再缩标题 → 最后才丢标题**——"图标定身份、标题定区分"（[`prd_4_1.md`](prd_4_1.md) §1），丢标题是最后手段。

**去抖动（迟滞）**：向下阈值低于向上阈值，死区 8px，防止加/删一个窗口导致整条栏跳档。
```
prev=D0: budget>=112 → D0 ; >=80  → D1 ; else D2
prev=D1: budget>=120 → D0 ; >=80  → D1 ; else D2
prev=D2: budget>=120 → D0 ; >=88  → D1 ; else D2
```
首次布局 `prev=D0`。因为"停留"要求 `budget ≥ floor`，且 `btn_w = budget.clamp(floor, ceil)`，停留档永不越界 → "时钟永不覆盖"自动成立。

### 3.2 纯函数与单测要点
```rust
pub enum Density { Full, Compact, IconOnly }
pub fn budget(avail: i32, n: usize, gap: i32) -> i32;
pub fn density_for(budget: i32, prev: Density) -> Density;
fn layout(start_left: i32, client_w: i32, n: usize, tray_w: i32, prev: Density) -> ButtonLayout;
fn fold_needed(avail: i32, n: usize, gap: i32) -> bool;   // budget < D1_FLOOR
fn hide_pins(avail_with: i32, avail_without: i32, n: usize, gap: i32) -> bool;
fn digit_index(vk: u16, visible: usize) -> Option<usize>;
```
> 早期草案里的 `Plan` / `plan()` / `fold_set()` **未落地**（代码中不存在）。实际折叠 = `visible..n`，由 `layout` 直接产出，无独立 `fold_set`。

单测：① `density_for` 幂等（`f(b,f(b,p)) == f(b,p)`）；② budget 单调增 ⇒ 档位不可变密；③ **迟滞**：`n` 序列 `7→8→7→8` 档位跳变 ≤1；④ 边界（112/111/119/120/80/88/40/48）；⑤ `visible` 随 n 单调不增，`overflow == (visible < n)`；⑥ 各档停留时 rects/overflow 不越 `client_w - PAD - RIGHT_W - tray`。

**保底规则**：`visible` 至少 1（`k.max(1)`）——否则任务栏只剩一个 `»`；对 `overflow` 输出前二次 clamp，并补 `k=0` 与超窄栏用例。

### 3.3 溢出弹层
| 项 | 实现 |
|----|------|
| 内容 | 单列菜单（复用 `task_list::popup`，`TrackPopupMenu`，零新依赖） |
| 每行 | 应用图标 + 标题（合并时带 `(+N)`） |
| 上限 | 条目 > `PER_COLUMN`(=`24`) 时用 `MF_MENUBARBREAK` 显式**分列**（纯函数 `starts_new_column`），避免 30+ 项超屏；`popup()` 签名不变 |
| 折叠取舍 | **尾部折叠**（`visible..n`，即最右/最新出现者）——零额外状态，与 `stable_order`（旧窗口留左、新窗口追加右）天然一致；`»` 菜单按**列表顺序**列出被折叠者。**「折叠最久未用」明确不做**（见 §12） |

### 3.4 极端情形退化顺序
| 情形 | 行为 |
|------|------|
| 窄栏（~800px 屏） | 优先级：**时钟 > 任务区 > 快速启动 > 通知图标条**。先由 `taskbar.tray` 决定通知条是否绘制；再由纯函数 `hide_pins(with, without, n, gap)`（带 pins 仍溢出、不带则不溢出 → 本轮隐藏 pins，pin 数据不删）；任务区降到 icon-only |
| 50+ 窗口 | icon-only 方块(40) 放不下就尾部折叠进 `»`；**禁止**把方块压到 < 40（20px 图标不可读） |
| 极窄（< 固定区总宽） | `avail=0` → 1 个可见按钮（保底）+ `»` |
| 多显示器 | 每屏一栏按**本屏宽度独立定档**；任务列表**全镜像**（现状即规范） |

## 4. 两轴组合与自动纠正

| 合并 \ 密度 | D0 full | D1 compact | D2 icon-only |
|------------|---------|-----------|--------------|
| `never` | 理想 | 允许 | 该应用**局部折叠**并带计数角标（`fold_needed` ⟺ `budget < D1_FLOOR` → 改走 `build_grouped`，保证"一个方块 = 一个应用"） |
| `when_full` | 未触发 | 允许 | 允许（合并后 n 已降） |
| `always` | 理想 | 允许 | **最佳组合**（图标 = 应用，天然去重） |

**不变量**：密度**不得**改 `n`；合并**不得**改 `btn_w`（单向串联、无环）。

## 5. 设置面板

### 5.1 信息架构（`src/settings_ui.rs`）
| 分组 | 设置项 | 控件 | 默认 |
|------|--------|------|------|
| 任务栏 | 显示任务栏 | 复选（重启后生效） | 开 |
| 任务栏 | 显示通知区域（仅作外壳时可点） | 复选 | 开 |
| 任务栏 | ~~显示开始菜单~~ | ~~复选~~ | 已于 2026-10-14 撤除（入口退役，不再有行） |
| 窗口合并 | 合并窗口 | 三态单选（3 行） | 从不合并 |
| 窗口合并 | 合并依据 | 单选（按应用 / 按进程），仅在合并 ≠ 从不时可用 | 按应用 |
| 行为 | 悬停预览 | 复选 | 关 |
| 外观 / 快捷键 / 关于 | 主题色 / 唤出键 / 版本 | 只读 | — |

### 5.2 控件落地（只有 GDI 自绘，无现成控件）
照搬现成"纯几何 + 命中"范式，行高 `ROW_H = 28`：
```rust
fn row_rects(client: RECT, n: usize) -> Vec<RECT>;        // 等行高、不重叠
fn hit_row(rows: &[Row], rects: &[RECT], x: i32, y: i32) -> Option<usize>;
fn row_action(rows: &[Row], i: usize) -> Option<SettingAction>;
fn toggle_flag(v: bool) -> bool;                          // 复选取反
```
绘制：左侧 12px 圆点（选中 = accent 实心，未选 = accent 描边）+ 标签；复选为 16px 方框 + 勾。**选单选而非下拉**：下拉需自绘弹出层/滚轮/键盘，单选直接可见，契合"一屏一焦点"且实现最小。

### 5.3 生效与回滚
| 设置 | 生效时机 |
|------|---------|
| 合并策略 / 依据 / 显示任务栏 / 通知区域条 / 悬停预览 | **即时**：命中控件 → 更新内存 Config → 写回设置文件（锁外 IO） → 重建任务栏（经 `hub::request_taskbar_refresh()`，避免 `settings_ui ↔ taskbar` 互相依赖） |
| 主题色 / 圆角 / 亚克力 | **重启**（`config::theme()` 走 `OnceLock` 永久缓存）——UI 上标注"重启后生效" |

## 6. 配置与持久化（配置键唯一真值表）

> 本节是全仓**配置键的唯一真值表**；[`prd_1_1.md`](prd_1_1.md)、[`README.md`](../README.md) 只引用，不另列。

| 键 | 取值 | 默认 |
|----|------|------|
| `show_taskbar`（别名 `taskbar.show`） | `true` \| `false` | `true` |
| `taskbar.tray` | `true` \| `false` | `true`（通知区域条；两形态下图标与可点性不同，见 [`prd_1_1.md`](prd_1_1.md)「通知区 / 托盘」） |
| `taskbar.group` | `never` \| `when_full` \| `always` | `never` |
| `taskbar.group_by` | `exe` \| `pid` | `exe` |
| `behavior.hover_preview` | `true` \| `false` | `false` |
| `theme.bg` / `surface` / `hover` / `accent` / `text` / `muted` | `#RRGGBB` 或 `0xRRGGBB`（默认值见 [`prd_4_1.md`](prd_4_1.md) §2） | — |
| `hotkey` | 热键串，如 `alt+`` | `alt+`` |

- **文件**：手写配置 `<exe 旁>/minicon.conf`（`MINICON_CONFIG` 可覆盖）；UI 独占写 `<exe 旁>/minicon-settings.txt`（`MINICON_SETTINGS` 可覆盖）。
- **为什么不用 `minicon.conf`**：它读侧会丢弃注释与未知键、且全项目**无写回器**；UI 一旦整文件重写，用户手写的主题注释与未来新增键全部丢失。
- **写回保真**：key-level upsert（只替换目标行，保留其余原行）；写前备份 `minicon-settings.txt.bak`。
- **优先级**：`环境变量 > minicon-settings.txt > minicon.conf > 内置默认`。保留 `MINICON_GROUP_TASKS`（`=1` ≡ `group=always`）作为兼容与调试入口，优先于设置面板。
- 环境变量完整清单以 [`prd_1_1.md`](prd_1_1.md)「环境变量 / 开关」为准。

## 7. 可发现性与文案

| 问题 | 处置 | 优先级 |
|------|------|--------|
| 右键是主入口但零提示 | 首次运行一次性可关闭提示；齿轮与 `»` 加 tooltip | 应该 |
| "外壳"是黑话 | 文案改白话：`接管桌面（立即生效）` / `还原系统桌面（立即生效）` | 应该 |
| 设置项命名 | 统一动词短语、去翻译腔；文案与枚举映射集中在 `config::MergeStrategy::label()` / `config::GroupBy::label()` **一处** | 必须 |
| 多显示器"显示哪些窗口"未定义 | 写明"现状即规范"：所有栏镜像同一任务列表；"仅本屏窗口"留后续 | 应该 |
| 空状态 / 错误态 | 无窗口→留空静默；pin 文件损坏→整条静默消失仅落日志；启动失败不弹窗；图标缺失→回退首字 | 应该 |

## 8. 红队评审要点（处置见对应节，均已落地 ✅）

① pid→exe（§2.2）；② 修组标签锚（§2.3）；③ `when_full` 用 `overflow.is_some()` + 8px 迟滞（§3.1）；④ 独立设置文件 + key-level upsert + `.bak`（§6）；⑤ 生效时机/优先级（§5.3）；⑥ D2 配 hover 气泡 + `never` 局部折叠（§3.3–4）；⑦ 保底 1 个可见按钮 + 溢出 rect 二次 clamp（§3.2）；⑧ `>24` 用 `MF_MENUBARBREAK` 分列（§3.3）；⑨ 导航中 `1..9` 直跳；⑩ 通知区域条可关（§5.1/6）——原"标实验性"的处置已由 0.1.3 的形态化文案取代。

## 9. 盲区清单（默认已冻结）

| 问题 | 默认 | 优先级 |
|------|------|--------|
| 设置项完整清单与默认值 | §5.1 / §6 冻结 | 必须 |
| 零配置 vs 可自定义 | 代码默认即最优开箱；只暴露"改变心智模型"的开关，不暴露实现细节 | 必须 |
| 快速启动上限 | **8**（`quick_launch::MAX_PINS`，见 [`prd_1_1.md`](prd_1_1.md)）；满了的菜单项置灰并写明原因 | 应该 |
| 设置项依赖 | 关"显示任务栏"后，合并相关项置灰并说明依赖 | 应该 |
| 时钟区承载边界 | 只放时钟；不塞 CPU/电量（对齐"噪声最小"） | 可选 |

## 10. 分期

| 期 | 内容 |
|----|------|
| **一期** ✅ | ① 修组标签漂移；② 设置面板真控件：合并窗口（三态单选）+ 合并依据；③ 配置 schema + 独立设置文件 + 优先级 + 写回保真；④ 密度 D0/D1 + 迟滞 + 保底 1 个可见按钮 + 边界用例；⑤ 文案与 tooltip |
| **二期** ✅ | ① D2 icon-only + hover 标题气泡（`bar_tip`）+ `never` 局部折叠；② `»` 弹层 `>24` 分列；③ 导航中 `1..9` 直跳；④ 通知条开关（`taskbar.tray`）；⑤ 窄栏折叠 pins（`hide_pins`）；⑥ 分组右键批量菜单 |
| **明确不做** | 折叠最久未用（破坏位置稳定）；从组中分离；多列溢出网格；pin 拖拽重排；主题拾色器；每屏窗口过滤 |

> 本表原有的"**通知点击转发**"已从"不做"移出——0.1.3 改为**替换模式下交付**（宿主路径转发四个鼠标事件，
  伴随模式仍不转发）。规格见 [`prd_1_1.md`](prd_1_1.md)「通知区 / 托盘」与 [`plan_0_1_3.md`](plan_0_1_3.md) §2.2。

## 11. 验收标准

| # | 条目 | 判定 |
|---|------|------|
| A1 | 三态解析正确，非法值回落 `never` | 单测 |
| A2 | 优先级 `env > settings > conf > 默认`（三组合） | 单测 |
| A3 | `when_full`：无溢出→逐窗口；密度降到底仍溢出→合并 | 单测 |
| A4 | **标签稳定**：同组交替激活不同窗口，标签与按钮位置均不变 | 单测 |
| A5 | `(+N)`：组内 k 窗口 → `(+{k-1})`；`never` 下恒无 | 单测 |
| A6 | `density_for` 幂等 + 单调 + 边界 + **迟滞跳变 ≤1** | 单测 |
| A7 | 各档位、各 `client_w` 下时钟区不被覆盖；`visible ≥ 1` | 单测 |
| A8 | 单选/复选几何不重叠、`hit_row` 边界（gap/越界） | 单测 |
| A9 | `fold_needed` / `hide_pins` / `digit_index` / `group_menu_action` / `starts_new_column` / `tip_rect` 纯函数边界 | 单测 |
| A10 | 改设置 → `refresh`：合并/拆分即时变化，无需重启 | 手测 |
| A11 | **以 `cargo test` 实测为准（当前 231 passed，v0.1.3 收敛后），改动后不得低于该值**；`clippy -D warnings` 干净 | `cargo test` / `cargo clippy` |
| A12 | 无新 worker 线程；`bars()/tasks()` 仅消息线程访问 | 代码审查 |

## 12. 已决定（决策表）

| # | 议题 | 决定 |
|---|------|------|
| 1 | 合并策略 | **保留三态** `never`/`when_full`/`always`（`never` 默认） |
| 2 | D2 纯图标档 | **实现**（icon-only 方块 + hover 气泡 + `never` 下局部折叠） |
| 3 | `never` × D2 的纠正方式 | **局部折叠**（`fold_needed` → `build_grouped`，带 `(+N)`），不整体回退 D1 |
| 4 | 设置是否写回文件 | **独立 `minicon-settings.txt`** + key-level upsert + `.bak`（不改写 `minicon.conf`） |
| 5 | 通知区域条 | **保留 + 可关**（`taskbar.tray`，默认 `true`）。0.1.3 起不再标"实验性"：替换模式给真图标 + 可点转发，伴随模式为降级占位不可点，文案与矩阵见 [`prd_1_1.md`](prd_1_1.md)「通知区 / 托盘」 |
| 6 | 多显示器折叠集合 | **各栏独立**（按本屏宽度独立定档；任务列表全镜像） |
| 7 | 折叠策略 | **尾部折叠**（`visible..n`）；**不做**「折叠最久未用」 |

## 13. 已核查结论（原"待验证 API"）

1. **`WM_MOUSEWHEEL`**：经典语义发给**焦点窗口**；Win10+ 可经系统设置（`HKCU\Control Panel\Desktop\MouseWheelRouting`，默认开）按**光标**路由到非活动窗口 → 滚轮翻页"**可行但依赖该设置**"，且需在 `wndproc` 加该分支（**当前无**）。本项目**未实现**滚轮翻页。
2. **`TrackPopupMenu`**：不保证自动滚动箭头，30+ 项可能超出屏幕 → 已改用 `MF_MENUBARBREAK` **显式分列**（每 `PER_COLUMN=24` 行，纯函数 `starts_new_column`）。
3. **UWP / `ApplicationFrameHost`**：`QueryFullProcessImageNameW` 会让多个 UWP 应用解析到同一 `ApplicationFrameHost.exe` → `exe` 策略会**过度合并**；属已知取舍（已记入 §0.1 偏差 2）。
