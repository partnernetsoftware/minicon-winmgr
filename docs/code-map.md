# 代码与文档地图

> 2026-10-15 · 0.1.6 实现地图；基线 0.1.5 / `2f1a470`。验收见 [版本计划](plan_0_1_6.md)。

| 用户路径 | 主要实现 | 规格/证据 |
|---|---|---|
| 启动、消息循环、单实例、事件分发 | `main`、`window`、`single_instance`、`hub`、`window_events` | [可靠性约束](prd_2_1.md) |
| 主题与四边布局 | `appearance`（原生主题）、`config`（调色板/来源）、`dock`（纯几何）、`taskbar`（横纵布局） | [工业风设计](design_0_1_6_industrial.md)、[测试分层](testing.md) |
| 枚举窗口、分组与任务切换 | `window_snapshot`、`model`、`grouping`、`taskbar`、`task_list`、`monitor` | [任务栏策略](prd_6_1.md) |
| 设置、配置来源与持久化 | `settings_ui`（桌面管理门面）、`settings_panel`（原生窗口）、`config`（状态/来源）、`settings_store`（串行 IO） | [0.1.6 需求](prd_6_1_1.md) |
| 启动、Run、历史、路径建议 | `launcher`、`launcher_ui`、`run_dialog`、`run_history`、`shell_launch` | [Run 规格](prd_1_1_1.md) |
| 通知宿主、图标、面板 | `tray_host`、`tray`、`tray_bar`、`tray_popup` | [通知规格](prd_1_1_2.md)、[验收数据](acceptance_0_1_5.json) |
| 悬停标题/缩略图、图标 | `bar_tip`、`hover_preview`、`window_icon` | [当前限制与计划](plan_0_1_6.md) |
| 兼容的旧菜单、快捷固定项、文件夹 | `start_menu`、`quick_launch`、`folder_stack`、`recents` | [后续清单](prd_5_1.md)；默认 UI 入口已退役 |
| 外壳接管、回滚、会话与观测 | `shell_registry`、`shell_swap`、`session`、`logging`、`metrics` | [外壳规格](prd_3_1.md) |

## 0.1.6 落地与剩余边界

- 原生控件、对话框键盘路由、滚动视口和固定反馈页脚替代设置行命中区域；窗口处理 DPI/工作区变化并释放替换字体。
- 独立串行 IO worker 合并最新键值；读取、备份、临时文件与替换有分阶段错误；旧结果不清除新请求，失败可重试。
- 当前任务栏状态、下次启动偏好和配置来源分别展示；有效环境覆盖只读。
- `--settings` 使用固定消息，在同一用户会话内呼出已有实例；无任务栏同样可用。
- 预览可从关闭启动热开启；关闭取消悬停计时并清理位图。同步 PrintWindow 仍有慢目标风险，见 FUT-07。
- Run 展开目标与参数环境变量；既有文件关联的编辑器缺失时显示可操作错误。

物理混合 DPI/热插拔、屏幕阅读器语音和高对比度未覆盖；短轮次资源检查不能替代小时级稳定性验收。

## 保持的边界

UI 消息线程拥有窗口；模块间通过 hub 请求刷新，避免设置/任务栏反向耦合。文件 IO 不持 `bars/tasks` 锁；耗时启动与 Run 历史已有有界工作线程。HICON/窗口/计时器资源需明确所有权与清理。接管/回滚不因调整设置界面改变权限、确认或备份语义。

CI 与 release workflow 分别负责质量检查和产物发布。历史测试数量是版本证据，不代表此次已运行。实际物理多屏、混合 DPI、长跑覆盖必须单独记录，不能由单测替代。

- `src/i18n.rs`：English/中文界面目录、默认语言和自有窗口标题/控件本地化；不转换命令或用户输入。

文档入口：[语言索引](../readme.md)、[English](../readme.en.md)、[简体中文](../readme.zh.md)。下一轮：[0.1.8 产品设计讨论稿](plan_0_1_8.md)。
