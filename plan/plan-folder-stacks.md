# plan-folder-stacks — 文件夹堆栈 & 可浏览桌面（调研）

## 结论先行
先做**文件夹堆栈的最小切片**：把常用文件夹钉进 ORB 主菜单的 Places 区（复用现有弹窗，**不新建窗口、不引 COM**），点击用 `ShellExecuteW` 打开；第二切片才做"点开在弹窗里列出内容"。**可浏览桌面暂不做**：companion 下系统桌面仍在，自建桌面窗口必然与 `Progman` 桌面重叠、争焦点/壁纸，风险最高、工程最大。切片规模：新增 1 个纯逻辑模块 + `config` 一字段 + `start_menu` 接线；**不新增线程**。

## 一、文件夹堆栈
- 定义：把文件夹钉到任务栏/菜单，点击即见其子项。价值：免开资源管理器。
- 最小做法：`Config` 增 `folders`；ORB 里渲染为 Places 行；`list_dir` 用 `std::fs::read_dir`（同 `launcher.rs` 风格，**无需新增 windows feature**）。
- 风险：路径失效→`tracing::debug` 跳过；大目录→限条数（如 64）。工作量小，**优先级最高**。

## 二、可浏览桌面
- 定义：不靠资源管理器，在桌面层浏览文件/图标。
- 最小做法（若做）：置顶弹窗，`read_dir` 取项 + `SHGetFileInfoW` 取图标 + `DrawIconEx` 绘制。
- 桌面层结构：`Progman`→`SHELLDLL_DefView`→`SysListView32`（切壁纸时 `SHELLDLL_DefView` 可能挂到 `WorkerW` 下）。自建桌面窗口会与系统桌面**重叠**。
- 风险：OLE 拖放、shell 右键菜单。工作量大，**优先级最低**。

## 读取方式选择
`std::fs::read_dir` + `SHGetFileInfoW` 优于 `IShellFolder`/`IShellItem`：后者需 `CoInitialize`/PIDL/`Release`，代码重；前者零新依赖（`Win32_UI_Shell` 已启用），代价是不认虚拟文件夹（"此电脑"）、图标自行获取。

## 图标可行性
`SHGetFileInfoW(path,0,&mut si,cb,SHGFI_ICON|SHGFI_SMALLICON)`→`si.hIcon`，`DrawIconEx` 绘制；**每个图标必须 `DestroyIcon`**，否则句柄泄漏（`metrics` 已在监控句柄/GDI）。

## 先不做
拖放（OLE `IDropTarget`）、shell 右键菜单（`IContextMenu`）、双击默认动词解析（先用 `ShellExecuteW` 近似）——均显著增复杂度。

## 复用点与护城河
复用：`launcher` 的 `read_dir` 递归、`start_menu` 的 LISTBOX 范式与 `Place` 模型、`task_list::popup` 的 `TrackPopupMenu`、`config` 的 `key=value` 解析、`recents` 的纯文本落盘范式。
护城河：失败一律 `tracing` 落日志并降级（缺图标→默认，坏目录→跳过）；浏览在消息线程、不新增线程；**文件 IO 绝不在持 `bars()`/`tasks()` 锁时做**，沿用 `refresh` 锁外 IO 约束，避锁序隐患。

## API 清单
- `Win32::UI::Shell::ShellExecuteW`（已启用）
- `Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES}`
- `Win32::UI::WindowsAndMessaging::{DrawIconEx, DestroyIcon, HICON}`
- `std::fs::read_dir`（无需新 feature）

## 风险表
| 风险 | 缓解 |
|--|--|
| 图标句柄泄漏 | 用完 `DestroyIcon`，`metrics` 监控 |
| 坏/慢目录卡消息线程 | 限条数、`debug` 跳过 |
| 与系统桌面冲突 | 只弹窗，不接管 |
| 持锁做 IO | IO 一律锁外 |

## 模块接口草图（不实现）
`src/folder_stack.rs`：
- `pub struct StackEntry { pub label: String, pub path: String }`
- `pub fn pinned() -> Vec<StackEntry>`
- `pub fn list_dir(path: &str, limit: usize) -> Vec<StackEntry>`
- `pub fn icon_for(path: &str) -> Option<HICON>`
- `pub fn open(path: &str)`（`ShellExecuteW`，失败落日志）
- `pub fn menu_layout(rows: usize, row_h: i32) -> Vec<RECT>`（纯函数，带单测）

`config.rs` 增字段 `pub folders: Vec<String>`（`folders = a;b` 解析）。
`start_menu.rs`：Places 区之后追加 `folder_stack::pinned()` 行，双击调 `open`。
