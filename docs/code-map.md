# 代码与文档地图

> 2026-10-20 · 当前公开版 0.1.16；实际范围见[实施计划](plan_0_1_16.md)，证据与限制见[验收记录](validation_0_1_16.md)。[本轮梳理](review_0_1_12.md)保留开发前发现。

| 用户路径 | 主要实现 | 规格/证据 |
|---|---|---|
| 启动、权限、消息与生命周期 | `main`、`window`、`privilege`、`single_instance`、`instance_switch`、`hub`、`window_events`、`session` | [可靠性约束](prd_2_1.md)、[0.1.11](archive/plan_0_1_11.md) |
| 设置、配置、语言与主题 | `config`、`settings_store`、`settings_ui`、`settings_panel`、`settings_info`、`i18n`、`appearance` | [设置规格](prd_6_1_1.md)、[当前设计](design.md) |
| 几何、枚举窗口与任务切换 | `taskbar`、`dock`、`monitor`、`grouping`、`model`、`task_list`、`window_snapshot`、`hotkeys`、`win_util`、`dwm` | [任务栏策略](prd_6_1.md)、[测试分层](testing.md) |
| 启动、Run、历史、路径建议 | `launcher`、`launcher_ui`、`run_dialog`、`run_history`、`shell_launch` | [Run 规格](prd_1_1_1.md) |
| 通知宿主、图标、面板 | `tray_host`、`tray`、`tray_bar`、`tray_popup` | [通知规格](prd_1_1_2.md)、[验收数据](validation.md) |
| 悬停标题/缩略图、图标 | `bar_tip`、`hover_preview`、`window_icon`、`window_icon_worker` | [资源审计与开发状态](resources_0_1_10.md) |
| 兼容的旧菜单、快捷固定项、文件夹 | `start_menu`、`quick_launch`、`folder_stack`、`recents` | [后续清单](prd_5_1.md)；默认 UI 入口已退役 |
| 外壳接管与回滚 | `shell_registry`、`shell_recovery`、`shell_swap` | [外壳规格](prd_3_1.md)、[验收边界](validation_0_1_12.md) |
| 手动更新与交接 | `update`、`update_cancel`、`update_http`、`update_install`、`update_stream` | [更新设计](archive/plan_0_1_9.md) |
| 独立任务工具与定位 | `bin/minicon-tskmgr`、`task_service` / `task_service/windows`、`task_ui` / `task_ui/view`、`tool_manager`、`palette` | [工具 PRD](prd_7_1.md)、[.16 验收](validation_0_1_16.md) |
| 诊断、导出、指标与日志 | `diagnostics`、`diagnostic_ui`、`diagnostic_export`、`metrics`、`logging`、`log_store` | [资源审计](resources_0_1_10.md)、[已验范围](validation_0_1_10.md) |

`main.rs` 组装轻量外壳；`bin/minicon-tskmgr.rs` 是独立工具入口。`lib.rs` 导出共享色板、平台无关任务模型及 Windows 查询/动作和原生工具 UI；`win_util` 共享源码，各宿主只使用所需部分。重叠职责以调用链判断，不把文件数量当作重构目标。

## 已交付基础与覆盖边界

- 原生控件、对话框键盘路由、滚动视口和固定反馈页脚替代设置行命中区域；窗口处理 DPI/工作区变化并释放替换字体。
- 独立串行 IO worker 合并最新键值；读取、备份、临时文件与替换有分阶段错误；旧结果不清除新请求，失败可重试。
- 当前任务栏状态、下次启动偏好和配置来源分别展示；有效环境覆盖只读。
- `--settings` 使用固定消息，在同一用户会话内呼出已有实例；无任务栏同样可用。
- 0.1.10 预览改用一个 DWM 缩略图关系，隐藏/换目标/销毁释放；窗口图标查询使用一个固定 worker 和 owned 快照。准确候选诊断/预览 27 项、完整功能层及窗口资源三轮已通过；空闲/设置整应用对照与长跑仍未完成，见[正式验收](validation_0_1_10.md)。路径图标和旧启动器仍有 UI 同步 I/O，不能把后台约束推广为全部路径。
- Run 展开目标与参数环境变量；既有文件关联的编辑器缺失时显示可操作错误。

物理混合 DPI/热插拔、屏幕阅读器语音和高对比度未覆盖；短轮次资源检查不能替代小时级稳定性验收。

## 保持的边界

UI 消息线程拥有窗口；模块间通过 hub 请求刷新，避免设置/任务栏反向耦合。文件 IO 不持 `bars/tasks` 锁；耗时启动与 Run 历史已有有界工作线程。HICON/窗口/计时器资源需明确所有权与清理。接管/回滚不因调整设置界面改变权限、确认或备份语义。

以上是维护边界，不代表所有旧路径均已满足。0.1.12 已修复接管恢复记录、必要界面就绪与测试隔离；伴随镜像、pin 图标重取等仍在后续备选范围，历史问题与当前去向见[审阅发现](review_0_1_12.md)和[实施计划](archive/plan_0_1_12.md)。

CI 与 release workflow 分别负责质量检查和产物发布。历史测试数量是版本证据，不代表此次已运行。实际物理多屏、混合 DPI、长跑覆盖必须单独记录，不能由单测替代。

- `src/i18n.rs`：English/中文界面目录、默认语言和自有窗口标题/控件本地化；不转换命令或用户输入。

文档入口：[语言索引](../readme.md)、[English](../readme.en.md)、[简体中文](../readme.zh.md)。历史：[0.1.8 设计与交付](archive/plan_0_1_8.md)；历史：[0.1.9 设置更新](archive/plan_0_1_9.md)；历史：[0.1.10 资源与稳健性](archive/plan_0_1_10.md)、[0.1.11](archive/plan_0_1_11.md)；当前公开版：[0.1.12](archive/plan_0_1_12.md)。

0.1.8：Run 快捷键路由在 `window`，窗口与 worker 在首次 `run_dialog::show` 创建；`run_history` 提供有界本地应用发现与路径/环境变量建议。主题化原生候选行保留完整 LB 字符串。`win_util::force_foreground` 处理跨权限 restore，`taskbar` 按点击坐标命中任务；专项脚本 `interaction_probe.py` / `taskmgr_probe.py` 纳入发布层。

0.1.9：`update` 管理手动状态与一次性 worker；`update_http` 封装有界 WinHTTP/CNG；`update_install` 负责暂存、同版本助手、进程退出等待、备份替换、启动回执与回滚；`settings_panel` 呈现三按钮和双语状态，不在渲染路径联网。`scripts/update_probe.py` 为隔离真实交接层，并纳入 release。

0.1.10：`memory_probe.py` 提供外部有界采样；`window_icon_worker` 合并查询、拒绝过期结果，UI 只绘制 owned 图像；`log_store` 固定日志保留预算；`settings_store` 默认持久化用户目录偏好并兼容迁移。完整范围与待办以[版本计划](archive/plan_0_1_10.md)为准。

0.1.10 更新路径：`update_http::get_into` 有界分块交付；`update_stream` 负责增量 CNG 哈希、PE 文件校验和复制；`update_install` 只持小清单与固定缓冲，不再同时保留新旧 exe 的完整 Vec。`update-memory` 为独立组件对照层，update 层继续验证真实交接与回滚。


0.1.10 发布路径：candidate 工作流构建一次，`release_artifact.py` 记录仅含公开身份的构建信息；`scenario_probe.py` 与 `resource_gate.py` 分别执行场景和只读核验，`native_feature_probe.py` 检查实际诊断入口及 DWM 预览，`instance_switch_probe.py` 检查历史实例交接。最终 tag 的 release 工作流复核产品源码指纹与 exe 摘要，原样晋升经测 CI 文件为草稿，经结果审阅后公开。

更新兼容分开保留证据：`update_probe.main()` 使用当前更新器回移构建验证状态/故障；发布层另显式传入 `previous_artifact`，以真正公开的旧版更新器操作同一准确候选。两组结果各存独立忽略目录，不能用回移构建冒充历史公开版。

0.1.11 已公开：管理员启动必须通过真实普通权限令牌完成自重启；失败不进入外壳主路径。任务栏时钟为 `Www yyyy-mm-dd HH:MM`，Run 增加上下键文本预览及草稿恢复；检查与覆盖限制见[验收记录](validation_0_1_11.md)。

0.1.12 实施：`shell_recovery` 负责账户操作互斥与身份绑定的原始登记记录；`shell_registry` 保留原始类型/字节并复读写入结果；`settings_ui` 的后台交接通过 `window` 消息让 UI 线程安装/释放通知宿主。`window::complete_startup` 统一成功信号，失败不写更新回执。

0.1.16：`tool_manager` 负责同版工具校验、显式后台下载和版本缓存；主程序更新协议保持单 exe，发布清单和校验表同时绑定两份 exe。`task_service` 提供 PID/创建时间身份、指标与端点；`task_ui` 使用有界 worker，结束进程默认取消。外壳退出在销毁消息窗口前结束当前线程的原生菜单，避免菜单仍等待输入。相关测试按[影响路径](testing.md)选择。
