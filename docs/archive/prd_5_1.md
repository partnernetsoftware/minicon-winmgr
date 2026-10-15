> 历史归档（2026-10-15）：保留当时的计划、过程和证据；状态与排期以 [当前 PRD](../prd.md) 为准。

# prd_5_1 — 后续与调研 · 未做项与可行性

> 索引：5.1 · 树路径：`后续与调研` · 上位文档：[`prd.md`](../prd.md)
> 原始 `plan/plan-shell-gaps.md` 与 `plan/plan-folder-stacks.md` 已并入本文。
> 约束（贯穿）：**companion 优先**、不改系统级 Shell、不发版、不签名。

## 1. 缺口（按杠杆排序）

> 排期见 [`plan_0_2_0.md`](../plan_0_2_0.md)；本节只定**理由与优先级**，不定排期。

1. **文件夹堆栈最小切片**（见 §2）：钉住常用文件夹，点击即见其子项。工作量小、优先级最高。
   → **0.2.0 P0-1**（已排期）
2. **主菜单完善**：菜单内"最近使用"专区、搜索框、拖拽整理分类。
   → **0.2.0 P0-2**（搜索框 + 最近使用；拖拽整理仍不做）
3. **可浏览桌面**（见 §3）：工程/风险最大，排最后。
   → **0.2.0 不做**
4. 通知区**点击转发**（`tray_host` 进阶）。
   → **0.2.0 P1-1**
5. 堆叠窗口组（stacked window groups）。
   → **0.2.0 不做**

## 2. 文件夹堆栈（推荐先做）
- 定义：把文件夹钉到菜单/任务栏，点击即见其子项；价值：免开资源管理器。
- **最小切片**：`Config` 增 `folders` 字段（`folders = a;b`）；ORB 的 Places 区之后渲染为行；
  `list_dir` 用 `std::fs::read_dir`（同 `launcher.rs` 风格，**无需新增 windows feature**）；
  路径失效 → `tracing::debug` 跳过；大目录 → 限条数（如 64）。
- 切片规模：新增 1 个纯逻辑模块 + `config` 一字段 + `start_menu` 接线；**不新增线程**。

## 3. 可浏览桌面（缓做）
- 定义：不靠资源管理器，在桌面层浏览文件/图标。
- 最小做法（若做）：置顶弹窗，`read_dir` 取项 + `SHGetFileInfoW` 取图标 + `DrawIconEx` 绘制。
- 桌面层结构：`Progman → SHELLDLL_DefView → SysListView32`（切壁纸时 `SHELLDLL_DefView` 可能挂到 `WorkerW` 下）。
  自建桌面窗口会与系统桌面**重叠**——故只做弹窗，不接管桌面。

## 4. 读取方式选择
`std::fs::read_dir` + `SHGetFileInfoW` **优于** `IShellFolder`/`IShellItem`：后者需 `CoInitialize`/PIDL/`Release`，代码重；
前者零新依赖（`Win32_UI_Shell` 已启用），代价是不认虚拟文件夹（"此电脑"）、图标自行获取。

## 5. 图标
`SHGetFileInfoW(path,0,&mut si,cb,SHGFI_ICON|SHGFI_SMALLICON)` → `si.hIcon`，`DrawIconEx` 绘制；
**每个图标必须 `DestroyIcon`**，否则句柄泄漏（`metrics` 已在监控句柄/GDI）。

## 6. 明确先不做
拖放（OLE `IDropTarget`）、shell 右键菜单（`IContextMenu`）、双击默认动词解析（先用 `ShellExecuteW` 近似）——均显著增复杂度。

## 7. 复用点与护城河约束
- 复用：`launcher` 的 `read_dir` 递归、`start_menu` 的 LISTBOX 范式与 `Place` 模型、
  `task_list::popup` 的 `TrackPopupMenu`、`config` 的 `key=value` 解析、`recents` 的纯文本落盘范式。
- 护城河：失败一律 `tracing` 落日志并降级（缺图标→默认，坏目录→跳过）；浏览在消息线程、**不新增线程**；
  **文件 IO 绝不在持 `bars()`/`tasks()` 锁时做**（沿用 `refresh` 的锁外 IO 约束，避锁序隐患）。

## 8. API 清单
- `Win32::UI::Shell::ShellExecuteW`（已启用）
- `Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES}`
- `Win32::UI::WindowsAndMessaging::{DrawIconEx, DestroyIcon, HICON}`
- `std::fs::read_dir`（无需新 feature）

## 9. 风险表
| 风险 | 缓解 |
|------|------|
| 图标句柄泄漏 | 用完 `DestroyIcon`；`metrics` 监控句柄/GDI |
| 坏/慢目录卡消息线程 | 限条数、`debug` 跳过 |
| 与系统桌面冲突 | 只弹窗，不接管 |
| 持锁做 IO | IO 一律锁外 |

## 10. 模块接口草图（不实现）
`src/folder_stack.rs`：
- `pub struct StackEntry { pub label: String, pub path: String }`
- `pub fn pinned() -> Vec<StackEntry>`
- `pub fn list_dir(path: &str, limit: usize) -> Vec<StackEntry>`
- `pub fn icon_for(path: &str) -> Option<HICON>`
- `pub fn open(path: &str)`（`ShellExecuteW`，失败落日志）
- `pub fn menu_layout(rows: usize, row_h: i32) -> Vec<RECT>`（纯函数，带单测）

`config.rs` 增字段 `pub folders: Vec<String>`；`start_menu.rs` 在 Places 区之后追加 `folder_stack::pinned()` 行，双击调 `open`。

## 11. 工作方式（沿用）
- 大改动拆成**并行 subagent**，每个只改**唯一文件**；接线（`window.rs`/`taskbar.rs`）由主代理单点做。
- 每个 subagent：禁改 `Cargo.toml`、禁 `cargo add`、禁 `cargo fmt`、禁起 GUI；可跑 `cargo check`；纯逻辑必须带单测。
- 收尾：`cargo fmt --check` + `cargo clippy -D warnings` + `cargo test`，并**真跑一次新 exe** 核日志。
- 验收基线：`cargo test` **以实测为准（当前 161 passed），改动后不得低于该值**；`clippy -D warnings` 必须干净。
