# 分层测试

开发期按修改范围选择一层；默认只做格式和编译检查，不运行整套回归。每次运行会在 `target/test-reports/<层>/summary.json` 记录步骤、耗时与结果；旧的其他层结果不代表本次也执行过。

| 命令 | 内容与使用时机 |
|---|---|
| `python scripts/test.py` | `check`：格式、cargo check；日常快速反馈 |
| `python scripts/test.py --layer unit --filter config::tests` | 指定 Rust 模块或测试名；只跑相关测试 |
| `python scripts/test.py --layer unit` | 全部单元测试；跨模块调整时使用 |
| `python scripts/test.py --layer quality` | 格式、严格 Clippy；提交前与 CI 静态检查 |
| `python scripts/test.py --layer layout` | 构建 debug；默认英文/中英切换与持久化、双语明暗截图、标签与选项同行、窄宽恢复、页脚可达；只改设置布局时使用 |
| `python scripts/test.py --layer appearance` | 构建 debug；明暗 × 四边原生窗口、工作区、保存读回、环境只读与切换资源专项 |
| `python scripts/test.py --layer taskmgr` | 当前会话既有任务管理器的普通权限恢复/激活/最小化循环；无窗口则记录跳过，不自动启动或提权 |
| `python scripts/test.py --layer interaction` | 冷启动按需初始化、Run 双语候选采用与窗口复用、四边任务栏焦点返回、设置重启提示；0.1.8 专项 |
| `python scripts/test.py --layer update` | 本地 HTTP 故障、双语按钮、禁止启动联网、校验失败、暂存篡改、保存阻止重启、两版本真实交接、文件占用和启动失败回滚；独立目录，不替换用户安装 |
| `python scripts/test.py --layer settings` | 构建 debug；设置原生交互、来源、保存故障、UIA 与窗口复用；省略完整 Run 历史/IME 回归 |
| `python scripts/test.py --layer release` | 格式、严格 Clippy、release 全部单元测试、release 构建，再用该产物跑更新/回滚、按需初始化/交互、设置、完整 Run、语言/布局、主题与四边桌面回归 |

桌面层需要当前 Windows 输入桌面可用，依赖 Python Pillow（截图）、PowerShell 7（UIA）。测试先构建，再正常关闭当前会话实例，复制至 live 路径；使用独立配置，结束后恢复正常会话。不会主动申请 UAC、接管 Shell 或结束其他会话进程。不要同时启动两个桌面层，也不要在测试操作键盘时输入其他内容。

`release` 表示当前自动化全套，不代表所有物理环境已覆盖。混合 DPI、多屏热插拔、高对比度实屏、屏幕阅读器语音、特殊管理员令牌以及小时级常驻观察仍需独立环境验收。CI 没有当前交互桌面，执行 `quality` 与独立 `fault` 层，release workflow 另外执行全部单元测试；发布前在可用桌面运行 `release` 并审阅报告。仅构建优化二进制不会自动声称完成发布验收。

发布层构建目录为 `target/release-validation`，避免旧会话直接运行 `target/release` 时锁住 Cargo 硬链接产物。不会为释放构建文件而结束其他会话。桌面专项仍复制产物到 live 后运行。

更新层会将当前源码回移到隔离的 0.1.8 测试构建，以验证真实 0.1.9 目标的两版本交接；该 fixture 不是历史公开版。服务仅监听 loopback，验收报告明确记录两份二进制 SHA-256。后台助手只允许本用户/本会话，并等旧进程退出后才修改本测试安装。成功后清理本轮自建用例目录，保留事务事实与截图。

## 公开证据检查

提交及发布前运行 `python scripts/check_public_docs.py`，CI 同样检查 Git 暂存区内容，拒绝原始验收 JSON 和工作站路径。可先运行 `python scripts/check_public_docs.py --self-test` 验证检查器。不要将原始报告复制到 docs；公开结果写入人工审阅的 Markdown，截图只使用无个人路径的隔离演示。该检查不能识别图片中的文字，图片必须目视检查。

0.1.10 开发层已增加：

| 命令 | 行为 |
|---|---|
| `python scripts/test.py --layer memory --pid <test-pid> --duration 60` | 只读观察现有测试实例，记录数值；有 matplotlib 时生成本地曲线，不重启、不修改配置；默认 5 秒采样 |
| `python scripts/test.py --layer soak --pid <test-pid> --duration 28800` | 显式 8 小时观察；提前退出记为不完整，不自动判定没有泄漏 |
| `python scripts/test.py --layer resources` | 创建本测试的隐藏窗口并停滞源消息线程，验证 DWM 100 次注册/释放和 owned 图标寿命；图标 worker 在三个全新进程各验证 32 个隐藏窗口、1000 次请求及通知合并；另用 6 个全新子进程对照两种日志队列容量，不替换正常外壳 |

原始数据写入每次唯一的 observation 目录，不覆盖上次实验。曲线依赖 matplotlib；采样本身只使用 Python 标准库。独立工具 `scripts/memory_probe.py` 也要求显式 PID 与时长。Private Working Set 只在确认支持的系统尝试，缺失值保持为空。当前层通过只代表工具和执行完整，不等于 0.1.10 的长期门槛已通过。

`python scripts/test.py --layer fault` 当前覆盖 pin 文件超长/损坏/替换失败/外部修改/占用、交接清单读取边界、流式暂存故障保护，以及 loopback HTTP 大小/截断/状态码/预算与增量哈希验证。它不代表完整网络超时、跨进程交接或长跑矩阵；跨进程交接继续使用 update 层。发布长跑门禁尚待实现，现有 release 入口尚未声称覆盖新的 0.1.10 长跑要求。

`python scripts/test.py --layer side-resize` 构建 debug，使用隔离偏好操作本会话任务栏内边缘，检查左右拖宽、取消、提交与重启持久化；finally 恢复原 live 二进制与环境。该层使用真实鼠标输入，不能与其他桌面层或长跑同时执行。

`python scripts/memory_probe.py --export-summary --output <local-observation-directory>` 只把白名单数值、场景代号和二进制摘要写成本地 Markdown 草稿，未知字段不导出，字段中出现路径字符串会拒绝；经人工检查后才提交公开摘要。

无 matplotlib 的环境可先完成采样，再用带该模块的 Python 执行 `--plot-only`。采样报告明确记录是否已生成曲线，不把缺失曲线隐藏为已生成。

`python scripts/test.py --layer update-memory` 独立比较 32 MiB 文件的旧双缓冲模式与实际流式暂存；每种模式各三个全新进程，不操作正常外壳，不联网。`python scripts/update_memory_probe.py --release` 在隔离优化构建目录重复同一实验。局部私有提交差值不当作整个应用峰值，采样未观察到增量也不代表零分配。原始日志和数值报告只留在 target。

0.1.10 的 update 层会根据 Cargo 当前版本构造上一补丁的回移 fixture，替换/回滚仍在自建 case 目录；finally 恢复测试开始时备份的原 live 二进制，并检查其摘要，避免把开发候选误留为正常运行版本。错误报告显式按 UTF-8 读取。

`python scripts/test.py --layer diagnostics` 运行有界数据/导出测试，再从真实“关于”入口打开独立原生诊断窗口，覆盖中英 × 明暗主题、默认不采样、停止/关闭/重开、新记录确认、原生保存取消和实际导出，以及窄宽布局。不替换 live、不使用全局输入，也不修改 Shell；依赖当前可用桌面和 Pillow。`python scripts/diagnostics_probe.py --case zh-dark` 可仅复查一个组合。该专项也纳入 release 入口；原生 fixture 当前使用 debug 测试构建，不能冒充优化版全应用内存验收。真实长跑和整应用资源对照仍是另外的发布要求。

`quality` 层另跑 `logging::tests`，用 AST 约束无插值的静态日志文案，并在独立子进程验证文件、stderr、span 及 panic 的文本保护；不需要桌面输入，不跑完整桌面套件。`unit --filter logging::tests` 可单独运行。原生数值与布尔值保留，文本、Debug/Display 和错误链在格式化前屏蔽，即使设置 trace 也不输出；显式 CLI 查询结果和用户错误对话框不属于后台日志。该组测试使用系统临时目录，不修改正常会话日志。

`fault` 另覆盖 WinHTTP 原生异步请求在静默/慢速响应头、停滞/慢速正文和断流时的取消、context/读取缓冲寿命及成功重试，以及更新 worker panic 与进度队列满后的状态恢复。仅使用 loopback 和本测试文件，不替换正常外壳；公网 DNS、代理/TLS 故障及跨进程交接另行验证。完整交接仍使用 update 层，不能因组件测试通过而省略。

整应用场景入口：

```powershell
python scripts/scenario_probe.py --candidate <candidate-exe> --restore-exe <normal-running-exe> --scenario idle
python scripts/scenario_probe.py --candidate <candidate-exe> --restore-exe <normal-running-exe> --scenario settings
python scripts/scenario_probe.py --candidate <candidate-exe> --restore-exe <normal-running-exe> --scenario windows
python scripts/scenario_probe.py --candidate <candidate-exe> --restore-exe <normal-running-exe> --scenario soak --observe 28800 --repetitions 1
```

默认基线为原正常 exe，也可显式指定 `--baseline`；须自行确认两份产物的构建配置，工具记录二进制摘要，不从文件名推断优化级别。默认三次新进程对照，交替先后顺序；5 分钟预热、15 分钟空闲操作期、10 分钟恢复。设置默认 200 个完整循环，窗口默认 300 次、最多同时 8 个本工具窗口；这些动作期间可有不等距采样，使用实际时间，不伪造每 5 秒都有样本。`--warmup 0 --observe 1 --recovery 0 --repetitions 1` 只用于 idle 驱动短测，不满足协议前提；settings/windows 还应指定很小的 --iterations。soak 最少两小时，示例八小时每个被测进程执行一次，基线和候选合计仍需十六小时以上；长跑结束不自动等于通过。

该入口会暂时正常关闭当前桌面的 winmgr，并在 finally 从原路径恢复，要求输入桌面可访问及普通用户权限，不使用全局鼠标/键盘，不修改注册或覆盖原二进制。当前真实生命周期尚待实机验证；先运行短测，再安排长观察，不能与其他桌面测试并发。控制器被强制结束时须参考本轮忽略目录内的恢复记录人工启动原实例。原始报告仅在 target；其中恢复记录有本地路径，禁止复制到公共文档。

`python scripts/test_scenario_probe.py` 的 10 项模拟生命周期检查不需要桌面，也纳入 quality 和 release。模拟通过只证明驱动控制流，不能证明 native 恢复成功或应用资源稳定。曲线可用现有 memory_probe 的 `--plot-only` 从每个 run 目录生成；缺少 matplotlib 时保留数据后再生成。

fault 层现运行整个 update 状态机模块，覆盖进度合并后隐藏 UI 的终态交付、退出取消、幂等 shutdown 与拒绝新 worker；HTTP 覆盖取消中的原生回调寿命，安装模块逐个检查 7 个取消边界。它们不依赖输入桌面，不关闭正常会话。每次操作的取消由应用退出触发；隐藏设置继续既有下载。完整交接、桌面和长跑仍需各自的验收。
