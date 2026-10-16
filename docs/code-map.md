# 代码与文档地图

> 2026-10-16 · 公开版 0.1.10；验证见[验收摘要](validation.md)，本版见[0.1.10 设计](plan_0_1_10.md)。

| 用户路径 | 主要实现 | 规格/证据 |
|---|---|---|
| 启动、消息循环、单实例、事件分发 | `main`、`window`、`single_instance`、`instance_switch`、`hub`、`window_events` | [可靠性约束](prd_2_1.md) |
| 主题与四边布局 | `appearance`（原生主题）、`config`（调色板/来源）、`dock`（纯几何）、`taskbar`（横纵布局） | [工业风设计](design.md)、[测试分层](testing.md) |
| 枚举窗口、分组与任务切换 | `window_snapshot`、`model`、`grouping`、`taskbar`、`task_list`、`monitor` | [任务栏策略](prd_6_1.md) |
| 设置、配置来源与持久化 | `settings_ui`（桌面管理门面）、`settings_panel`（原生窗口）、`config`（状态/来源）、`settings_store`（串行 IO） | [0.1.6 需求](prd_6_1_1.md) |
| 启动、Run、历史、路径建议 | `launcher`、`launcher_ui`、`run_dialog`、`run_history`、`shell_launch` | [Run 规格](prd_1_1_1.md) |
| 通知宿主、图标、面板 | `tray_host`、`tray`、`tray_bar`、`tray_popup` | [通知规格](prd_1_1_2.md)、[验收数据](validation.md) |
| 悬停标题/缩略图、图标 | `bar_tip`、`hover_preview`、`window_icon`、`window_icon_worker` | [资源审计与开发状态](resources_0_1_10.md) |
| 兼容的旧菜单、快捷固定项、文件夹 | `start_menu`、`quick_launch`、`folder_stack`、`recents` | [后续清单](prd_5_1.md)；默认 UI 入口已退役 |
| 外壳接管、回滚、会话与观测 | `shell_registry`、`shell_swap`、`session`、`logging`、`log_store`、`metrics` | [外壳规格](prd_3_1.md) |

## 已交付基础与覆盖边界

- 原生控件、对话框键盘路由、滚动视口和固定反馈页脚替代设置行命中区域；窗口处理 DPI/工作区变化并释放替换字体。
- 独立串行 IO worker 合并最新键值；读取、备份、临时文件与替换有分阶段错误；旧结果不清除新请求，失败可重试。
- 当前任务栏状态、下次启动偏好和配置来源分别展示；有效环境覆盖只读。
- `--settings` 使用固定消息，在同一用户会话内呼出已有实例；无任务栏同样可用。
- 0.1.10 预览改用一个 DWM 缩略图关系，隐藏/换目标/销毁释放；图标查询改用一个固定 worker 和 owned 快照。隐藏窗口资源验证见[开发验收](validation_0_1_10.md)，准确 CI 候选的实际诊断/预览 27 项已通过，正式资源场景与长跑仍在执行。
- Run 展开目标与参数环境变量；既有文件关联的编辑器缺失时显示可操作错误。

物理混合 DPI/热插拔、屏幕阅读器语音和高对比度未覆盖；短轮次资源检查不能替代小时级稳定性验收。

## 保持的边界

UI 消息线程拥有窗口；模块间通过 hub 请求刷新，避免设置/任务栏反向耦合。文件 IO 不持 `bars/tasks` 锁；耗时启动与 Run 历史已有有界工作线程。HICON/窗口/计时器资源需明确所有权与清理。接管/回滚不因调整设置界面改变权限、确认或备份语义。

CI 与 release workflow 分别负责质量检查和产物发布。历史测试数量是版本证据，不代表此次已运行。实际物理多屏、混合 DPI、长跑覆盖必须单独记录，不能由单测替代。

- `src/i18n.rs`：English/中文界面目录、默认语言和自有窗口标题/控件本地化；不转换命令或用户输入。

文档入口：[语言索引](../readme.md)、[English](../readme.en.md)、[简体中文](../readme.zh.md)。历史：[0.1.8 设计与交付](archive/plan_0_1_8.md)；历史：[0.1.9 设置更新](plan_0_1_9.md)；当前公开版：[0.1.10 资源与稳健性](plan_0_1_10.md)。

0.1.8：Run 快捷键路由在 `window`，窗口与 worker 在首次 `run_dialog::show` 创建；`run_history` 提供有界本地应用发现与路径/环境变量建议。主题化原生候选行保留完整 LB 字符串。`win_util::force_foreground` 处理跨权限 restore，`taskbar` 按点击坐标命中任务；专项脚本 `interaction_probe.py` / `taskmgr_probe.py` 纳入发布层。

0.1.9：`update` 管理手动状态与一次性 worker；`update_http` 封装有界 WinHTTP/CNG；`update_install` 负责暂存、同版本助手、进程退出等待、备份替换、启动回执与回滚；`settings_panel` 呈现三按钮和双语状态，不在渲染路径联网。`scripts/update_probe.py` 为隔离真实交接层，并纳入 release。

0.1.10：`memory_probe.py` 提供外部有界采样；`window_icon_worker` 合并查询、拒绝过期结果，UI 只绘制 owned 图像；`log_store` 固定日志保留预算；`settings_store` 默认持久化用户目录偏好并兼容迁移。完整范围与待办以[版本计划](plan_0_1_10.md)为准。

0.1.10 更新路径：`update_http::get_into` 有界分块交付；`update_stream` 负责增量 CNG 哈希、PE 文件校验和复制；`update_install` 只持小清单与固定缓冲，不再同时保留新旧 exe 的完整 Vec。`update-memory` 为独立组件对照层，update 层继续验证真实交接与回滚。


0.1.10 发布路径：candidate 工作流构建一次，`release_artifact.py` 记录仅含公开身份的构建信息；`scenario_probe.py` 与 `resource_gate.py` 分别执行场景和只读核验，`native_feature_probe.py` 检查实际诊断入口及 DWM 预览，`instance_switch_probe.py` 检查历史实例交接。最终 tag 的 release 工作流复核产品源码指纹与 exe 摘要，原样晋升经测 CI 文件为草稿，经结果审阅后公开。

更新兼容分开保留证据：`update_probe.main()` 使用当前更新器回移构建验证状态/故障；发布层另显式传入 `previous_artifact`，以真正公开的旧版更新器操作同一准确候选。两组结果各存独立忽略目录，不能用回移构建冒充历史公开版。
