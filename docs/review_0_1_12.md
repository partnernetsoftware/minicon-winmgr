# 0.1.12 前代码与文档梳理

核对日期：2026-10-17。对象：基于 `1df19ee` 的当前工作树，Cargo 版本 0.1.11，包含本轮尚未提交的权限、时钟和 Run 调整。仓库文档记录的上一公开版为 0.1.10；本轮没有向远端重新核验发布状态。

这是结构、关键调用链与检查基线的审阅，不是逐个 Win32 边界的安全认证或完整桌面验收。本轮不修改产品运行逻辑、不升级包版本、不切换运行中的外壳。0.1.12 的范围仍是[讨论草案](archive/plan_0_1_12.md)。已有暂存/未暂存修改保持原状，包括 `CHANGELOG.md` 的既有删除。

后续规划更新（2026-10-17；2026-10-19 补记）：以下保留发现时的证据与建议；调整后的 [0.1.12](archive/plan_0_1_12.md) 聚焦接管、就绪和基线，并将 C07 与接管修复同版处理。响应性、Run 体验和旧启动路径分别顺延 [原性能备选草案](archive/plan_0_1_13_performance_draft.md)、[0.1.14](archive/plan_0_1_14.md)、[原维护备选](archive/plan_0_1_15_maintenance_draft.md)。当前 [0.1.15](archive/plan_0_1_15.md)另以更新文件名修复为主，旧维护设计不自动实施。0.1.11 独立发布过程中完成的门槛修复，后续按实际提交与证据接收，不重复列为未做。

发布后补记（2026-10-18）：0.1.11 已合入 main 并[公开发布](https://github.com/partnernetsoftware/minicon-winmgr/releases/tag/v0.1.11)，下列检查表仍保留审阅当时的结果。C03 涉及的格式、语言隔离和公开路径检查已在发布分支修复；完整检查、准确候选及公开旧版更新结果以[0.1.11 验收记录](validation_0_1_11.md)为准，不再按本次历史失败重复排期。

0.1.12 实施补记（2026-10-18）：C01、C02、C07、C06 的 Shell 记录部分及 C03 基线已按[选定范围](archive/plan_0_1_12.md)实现；准确候选与发布状态见[验收记录](validation_0_1_12.md)。下文仍保留发现时的历史证据，源码链接固定到 0.1.11 发布提交，不把已修复问题继续当作当前待办。

## 1. 项目现状

- 当前纳入版本控制的 Rust 源文件 56 个、约 2.93 万行，含测试；正式 Python 脚本 26 个、约 4283 行。忽略目录中的个人工具不计作可复现交付。
- 主体是单个 Rust/Win32 GUI 可执行文件；`lib.rs` 目前为空。窗口、消息和绘制由 UI 线程拥有，跨模块刷新通过 `hub`，设置写入、Run 查询/历史/启动、窗口图标、更新与导出各有相应后台责任。
- 核心日常路径已具备：任务栏、窗口切换、Run、设置保存、通知图标、手动更新及回滚。默认关闭的旧菜单仍有实现，不能把“无默认入口”误写成“未开发”。
- 代码规模主要集中于 `taskbar`（3777 行）、`settings_panel`（1968）、`run_dialog`（1621）、`config`（1454）、`start_menu`（1417）。这些数字含测试，只说明审阅和变更范围，不能直接证明应当拆文件。

模块入口和边界更新于[代码地图](code-map.md)。

## 2. 本轮实际检查

| 检查 | 实际结果 | 结论边界 |
|---|---|---|
| `cargo clippy --all-targets -- -D warnings` | 通过 | 静态检查通过不代表桌面流程无问题。 |
| `cargo fmt --all -- --check` | 未通过；`logging.rs` 与 `shell_registry.rs` 有格式差异 | 现有 CI quality/candidate/release 的格式步骤会受影响；本轮未顺手格式化源文件。 |
| 全量 debug Rust，单线程 | 312 通过、4 失败、7 ignored | 四个失败都在 `shell_launch` 的英文文案断言。 |
| 单独 `shell_launch::tests` | 23 通过 | 排除“该模块单独也必然失败”的判断。 |
| 全量 debug，跳过新增 Run 原生控件测试 | 311 通过、同样 4 失败、7 ignored | 不能把失败单独归咎于新增 Run 测试。 |
| 同一全量 suite，子进程环境固定 `MINICON_LANGUAGE=en` | 316 通过、0 失败、7 ignored | 证明测试基线受语言环境影响；英文覆盖通过不等于中英文隔离问题已修复。 |
| Python 合成/控制流测试 | memory 7、scenario 10、resource-gate 13、release-artifact 10、desktop-layers 8，均通过 | 共 48 项，不是产品原生桌面、真实网络或长跑通过证据。 |
| 已暂存内容的公开文档检查 | 1 项失败：`shell_registry` 测试常量包含工作站路径 | 该工具检查 Git index；工作树说明或格式调整不会自动修复已暂存内容。公开报告不复制路径原文。 |
| 本地 Markdown 文件链接 | 梳理前缺失 9 处，其中当前文档 5、归档 4；均指向已删除的 CHANGELOG | 本轮改为已有版本/验收入口，历史引用保留为文字，不恢复被删除文件。 |

原始输出留在忽略的 `target/review-0.1.12/`。未运行优化版完整单元、实际桌面专项、管理员令牌矩阵、设备矩阵、真实历史版本更新或 2h/8h 长跑；不能据本次检查宣称 0.1.11 已完成发布验收。

## 3. 关键发现与建议

优先级针对后续讨论：P0 是先恢复可回滚、可验证的基础；P1 是高频路径和响应性；P2 是结构及覆盖扩展。下列代码问题尚未在本轮修复。

### C01 · P0：接管没有把成功备份作为前置条件

已确认调用链：[`shell_swap::record_backup`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/shell_swap.rs#L283) 返回 `()`，路径取得失败、已有路径或写入失败都不向调用者报告。`take_over` 读取旧 Shell 失败也只记警告，随后仍可能 `replace` 登记并 `stop_shell`。

具体风险场景：程序目录只读、备份文件不可写，而 HKCU Shell 可写时，接管可能成功但没有可用回滚记录。此后 `give_back` 又因缺少记录拒绝恢复。这里与“受控接管、可回滚”的产品目标不一致；本轮未在用户会话执行该故障。

建议：先读取并持久化、校验原状态，任何备份错误都在写注册表和关闭外壳之前返回；已有记录不能仅凭路径存在就视为可用。采用独占创建和明确的记录格式，保留“未登记”和“记录缺失”的区别。测试用临时目录及依赖注入，禁止拿实际桌面作失败实验。

### C02 · P0：成功回执与“必要界面就绪”不是同一门槛

已确认顺序：[`window::run`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/window.rs#L236) 在设置 owner 存在时就调用 `confirm_startup`，随后才执行 `on_ready(taskbar_ready && settings_owner)` 并设置 `STARTUP_READY`。[`Session::ready`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/instance_switch.rs#L48) 又只在存在旧实例回退记录时拒绝不可用界面。

因此存在应注入验证的场景：任务栏应开启但创建失败、设置窗口成功时，更新启动回执可能已经写出；更新助手看到回执便可认定成功，统一就绪检查却尚未完成。普通首次启动与不同入口的失败策略也不完全一致。正常环境通过不能排除这条分支。

建议：定义唯一的必要能力判定；区分用户关闭任务栏与任务栏创建失败。设置、任务栏、跨实例交接、降权父进程等待及更新回执都消费同一结果，通过之后才发成功信号。故障用 fixture 验证，保留旧版更新协议。

### C03 · P0：测试与交付检查不能稳定脱离个人环境

本轮可复现全量 suite 的 4 个语言相关失败；固定英文后全量通过。[`config::runtime`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/config.rs#L582) 会加载实际偏好并设置进程级语言，若测试初始化真实 UI/主题便可能触达它；多个错误测试则断言英文子串。具体哪条测试最先初始化依赖仍须继续定位，不能仅凭失败顺序下结论。

此外，格式检查失败、公开材料检查命中测试常量；当前 quality 层只挑选日志和 Python 检查，普通 CI 的 fault 层也不能代替完整 unit suite。

建议：用独立子进程和临时配置隔离环境；让错误分类/代码成为主断言，文案测试显式选择语言。普通 CI 增加可复现的全量单元，保留 ignore 专项的独立执行方式。同步消除格式与路径检查失败，不以禁用检查或忽略断言解决问题。

### C04 · P1：不显示的伴随托盘镜像仍在 UI 心跳读取

已确认：[`window`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/window.rs#L350) 启动和心跳调用 `tray::sample_mirror`；非宿主时执行跨进程 toolbar 读取。[`tray::read_buttons`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/tray.rs#L295) 的项目没有 owner，而 [`notification_parts`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/taskbar.rs#L777) 和面板入口只保留带 owner 的项目。这些镜像不会成为当前可交互的应用图标。

读取仍在消息线程，单条消息 250ms 超时，按 toolbar 返回的数量遍历，未设置整轮时间或数量上限。慢宿主下有累加延迟风险；本轮未测得一次真实卡顿，因此不提供卡顿毫秒数。

建议：优先评估移除无消费方的镜像采样，保持伴随模式由 Windows 管理图标的现有语义。若另有实际用途，再明确该用途并放到有界后台任务；不能以绘制不可点击占位来证明功能恢复。

### C05 · P1：快捷启动刷新和旧启动器仍有同步 I/O

已确认：[`taskbar::refresh_inner`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/taskbar.rs#L1630) 每次清路径图标缓存，再读取 pin 文件和调用 [`window_icon::for_path`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/window_icon.rs#L60) 的 `ExtractIconExW`。窗口事件驱动刷新时，即使 pin 没变也会重复做这些工作。

[`launcher_ui::refresh_results`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/launcher_ui.rs#L254) 还会在首次调用 `items()` 时同步递归扫描快捷方式；空查询排名读 recents，启动前同步记录 recents 并打开目标。Run 的应用发现已经有独立 worker 和检查预算，两套实现的约束不一致。以上是调用链事实；实际慢盘延迟待测。

建议：先选一个高频路径做窄改动，优先保留未变 pin 的 owned 图标快照、使文件变更独立失效；随后再统一应用发现的后台边界。保持原有外部 pin 文件变更和 Explorer 保护语义。不要一次重写全部菜单/启动器。

### C06 · P1：持久化状态的账户/会话边界尚未统一

用户偏好已迁至用户目录、日志已按账户分目录；但 [`shell_swap::backup_path`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/shell_swap.rs#L276)、[`taskbar::workarea_state_path`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/taskbar.rs#L530)、quick-launch 和 recents 的默认状态仍在 exe 旁。同一安装路径可供多个账户/会话共用。

外壳备份尤其依赖“只记录一次”：其他账户留下的同名文件可能被当作本用户的回滚依据。工作区记录没有用户/会话标识，只比较矩形，目录不可写时也可能无法保留恢复信息。这是存储隔离缺口，不等于本轮已重现跨账户错误恢复。

建议：先处理 Shell/工作区恢复状态的身份和写入边界，再讨论 pin/recents 的迁移。旧记录来源不明时不得静默套用；升级和手工便携配置兼容需单独设计。

### C07 · P1：运行中接管后，通知宿主的初始化没有形成闭环

[`tray_host::install`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/tray_host.rs#L144) 仅在 `window::run` 启动时调用。伴随启动因 Explorer 已存在而跳过宿主后，设置里的 `take_over` 可关闭 Explorer，但当前调用链没有重新安装通知宿主和广播图标重注册。

建议：用隔离 fixture 复核“伴随 → 接管 → 图标重注册”的生命周期，设计同一 UI 线程上的明确转换。不要根据启动时的宿主状态推断整场会话。是否随 0.1.12 实施，取决于接管功能的本版范围。

### C08 · P1：Run 体验尚未形成用户认可的交互模型

已实现上下键文本预览、取消恢复和编辑后重查；用户反馈为“暂时能接受”，不能记作 UX 已完成。现有 [`run_dialog::key/run/adopt`](https://github.com/partnernetsoftware/minicon-winmgr/blob/43b3d35078346afac303580e0fa8f88ddfab2e6d/src/run_dialog.rs) 保留第一次 Enter 采纳、第二次 Enter 执行，输入框、列表、按钮和展开状态共同决定动作；这可能是体验不直接的原因之一，尚不能代替具体使用观察。

建议：只围绕 5 个真实任务走查：运行短命令、重用带参数历史、选择后追加参数、进入文件夹继续补全、失败后修正。比较保留两次确认与单次 Enter 执行选中项两种方案，由用户讨论决定；不顺手增加浏览、提权、联网搜索等入口。用户已要求暂时接受当前行为，本轮没有继续改 UX。

### C09 · P2：可诊断性和总量边界还有缺项

- 日志的文本脱敏规则有测试，但若原生错误只以 `%error` 记录，原因会被屏蔽；例如关联令牌复制失败不一定保留可区分的数值错误码。可补固定阶段码与数值 Win32/HRESULT，继续保护路径、命令及个人内容。
- Run 使用单槽请求和最新 pending，回复通道是普通 `mpsc::channel`；它的实际积压受生产路径约束，不能仅因通道类型就认定内存泄漏。托盘注册表也没有硬性图标总数上限。应先定义并测量峰值，再决定容量/退化策略。
- `taskbar` 和 `settings_panel` 责任较多，可围绕布局、命中、绘制、外部动作逐步划界；仅拆文件或机械消除 `unsafe` 没有独立产品收益。

## 4. 文档整理结果与维护原则

本轮纠正主 PRD 的版本状态、已交付菜单最近使用/文件夹堆栈标记、已具备的发布流程，以及仍指向旧 0.1.4 排期的“当前优先级”。同时更新设计说明、代码地图、可靠性说明及 0.1.10 资源审计的过期状态。

入口按职责保留：README 说明怎么用；PRD/子规格定义行为；代码地图定位实现；版本计划定义范围；validation 保存实际证据；archive 只追溯。新增[0.1.12 草案](archive/plan_0_1_12.md)明确待讨论，不把候选项写成已承诺需求。

此前未完成的 2h/8h、空闲/设置对照、物理多屏/DPI、高对比度、屏幕阅读器、特殊令牌及公网故障矩阵继续引用 [0.1.11 遗留项](archive/plan_0_1_11.md)。不重复创建第二套长清单，也不恢复已归档的 0.2.0 排期。
