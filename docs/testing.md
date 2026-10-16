# 分层测试

开发期按修改范围选择一层；默认只做格式和编译检查，不运行整套回归。每次运行会在 `target/test-reports/<层>/summary.json` 记录步骤、耗时与结果；旧的其他层结果不代表本次也执行过。

| 命令 | 内容与使用时机 |
|---|---|
| `python scripts/test.py` | `check`：格式、cargo check；日常快速反馈 |
| `python scripts/test.py --layer unit --filter config::tests` | 指定 Rust 模块或测试名；只跑相关测试 |
| `python scripts/test.py --layer unit` | 全部单元测试；跨模块调整时使用 |
| `python scripts/test.py --layer quality` | 格式、严格 Clippy；提交前与 CI 静态检查 |
| `python scripts/test.py --layer layout --restore-exe <normal-running-exe>` | 构建 debug；默认英文/中英切换与持久化、双语明暗截图、标签与选项同行、窄宽恢复、页脚可达；只改设置布局时使用 |
| `python scripts/test.py --layer appearance --restore-exe <normal-running-exe>` | 构建 debug；明暗 × 四边原生窗口、工作区、保存读回、环境只读与切换资源专项 |
| `python scripts/test.py --layer taskmgr --restore-exe <normal-running-exe>` | 当前会话既有任务管理器的普通权限恢复/激活/最小化循环；无窗口则记录跳过，不自动启动或提权 |
| `python scripts/test.py --layer interaction --restore-exe <normal-running-exe>` | 冷启动按需初始化、Run 双语候选采用与窗口复用、四边任务栏焦点返回、设置重启提示；0.1.8 专项 |
| `python scripts/test.py --layer update --restore-exe <normal-running-exe>` | 本地 HTTP 故障、双语按钮、禁止启动联网、校验失败、暂存篡改、保存阻止重启、两版本真实交接、文件占用和启动失败回滚；独立目录，不替换用户安装 |
| `python scripts/test.py --layer settings --restore-exe <normal-running-exe>` | 构建 debug；设置原生交互、来源、保存故障、UIA 与窗口复用；省略完整 Run 历史/引号/中文参数回归 |
| `python scripts/test.py --layer resource-gate --release-binary <candidate> --baseline <baseline> --campaign <local-summary> ...` | 只读核对三轮对照和先 2h 后 8h 原始证据；不等于完整验收 |
| `python scripts/test.py --layer release --release-binary <candidate> --restore-exe <normal-running-exe> --legacy-exe <official-v0.1.4-exe> --baseline <baseline>` | 格式、严格 Clippy、release 全部单元、组件资源/更新内存、诊断 fixture 及实际候选的侧栏/更新/交互/设置/Run/布局/主题/换边/回收站回归 |

会替换实例的桌面层均要求显式 `--restore-exe`，指向当前实际运行文件，并由隔离上下文先核对同用户/会话、文件路径和摘要；依赖当前可用的 Windows 输入桌面、Python Pillow（截图）和 PowerShell 7（UIA）。开发层默认构建到 `target/desktop-validation`，只在忽略目录内的副本上运行，正常退出后从原路径恢复并核对工作区及 Shell。不会主动申请 UAC、接管 Shell 或结束其他会话进程。不要同时启动两个桌面层，也不要在测试操作键盘时输入其他内容。

`release` 表示当前自动化全套，不代表所有物理环境已覆盖。混合 DPI、多屏热插拔、高对比度实屏、屏幕阅读器语音、特殊管理员令牌以及小时级常驻观察仍需独立环境验收。CI 没有当前交互桌面，执行 `quality` 与独立 `fault` 层，release workflow 另外执行全部单元测试；发布前在可用桌面运行 `release` 并审阅报告。仅构建优化二进制不会自动声称完成发布验收。

发布层构建目录为 `target/release-validation`，避免旧会话直接运行 `target/release` 时锁住 Cargo 硬链接产物。实际候选来自显式指定的 `--release-binary`（如已下载的草稿资产），不会用重建结果替换它。桌面探针的 live 路径隔离在忽略目录，结束恢复显式指定的原运行路径、摘要、工作区与 Shell 登记，不覆盖原外壳文件，也不结束其他会话。

更新层会将当前源码回移到上一补丁的隔离测试构建（0.1.10 候选对应 0.1.9 fixture），以验证当前实际候选的两版本交接；该 fixture 不是历史公开版。服务仅监听 loopback，验收报告明确记录两份二进制 SHA-256。后台助手只允许本用户/本会话，并等旧进程退出后才修改本测试安装。成功后清理本轮自建用例目录，保留事务事实与截图。

## 公开证据检查

提交及发布前运行 `python scripts/check_public_docs.py`，CI 同样检查 Git 暂存区内容，拒绝原始验收 JSON 和工作站路径。可先运行 `python scripts/check_public_docs.py --self-test` 验证检查器。不要将原始报告复制到 docs；公开结果写入人工审阅的 Markdown，截图只使用无个人路径的隔离演示。该检查不能识别图片中的文字，图片必须目视检查。

0.1.10 开发层已增加：

| 命令 | 行为 |
|---|---|
| `python scripts/test.py --layer memory --pid <test-pid> --duration 60` | 只读观察现有测试实例，记录数值；有 matplotlib 时生成本地曲线，不重启、不修改配置；默认 5 秒采样 |
| `python scripts/test.py --layer soak --pid <test-pid> --duration 28800` | 显式 8 小时观察；提前退出记为不完整，不自动判定没有泄漏 |
| `python scripts/test.py --layer resources` | 创建本测试的隐藏窗口并停滞源消息线程，验证 DWM 100 次注册/释放和 owned 图标寿命；图标 worker 在三个全新进程各验证 32 个隐藏窗口、1000 次请求及通知合并；另用 6 个全新子进程对照两种日志队列容量，不替换正常外壳 |

原始数据写入每次唯一的 observation 目录，不覆盖上次实验。曲线依赖 matplotlib；采样本身只使用 Python 标准库。独立工具 `scripts/memory_probe.py` 也要求显式 PID 与时长。Private Working Set 只在确认支持的系统尝试，缺失值保持为空。当前层通过只代表工具和执行完整，不等于 0.1.10 的长期门槛已通过。

`python scripts/test.py --layer fault` 当前覆盖 pin 文件超长/损坏/替换失败/外部修改/占用、交接清单读取边界、流式暂存故障保护，以及 loopback HTTP 大小/截断/状态码/预算与增量哈希验证。它不代表完整网络超时、跨进程交接或长跑矩阵；跨进程交接继续使用 update 层。扩展资源认证另用 resource-gate 的真实证据；校验器自身使用合成数据测试，不能代替长跑。

`python scripts/test.py --layer side-resize` 构建 debug，使用隔离偏好操作本会话任务栏内边缘，检查左右拖宽、取消、提交与重启持久化；finally 恢复原 live 二进制与环境。该层使用真实鼠标输入，不能与其他桌面层或长跑同时执行。

`python scripts/memory_probe.py --export-summary --output <local-observation-directory>` 只把白名单数值、场景代号和二进制摘要写成本地 Markdown 草稿，未知字段不导出，字段中出现路径字符串会拒绝；经人工检查后才提交公开摘要。

无 matplotlib 的环境可先完成采样，再用带该模块的 Python 执行 `--plot-only`。采样报告明确记录是否已生成曲线，不把缺失曲线隐藏为已生成。

`python scripts/test.py --layer update-memory` 独立比较 32 MiB 文件的旧双缓冲模式与实际流式暂存；每种模式各三个全新进程，不操作正常外壳，不联网。`python scripts/update_memory_probe.py --release` 在隔离优化构建目录重复同一实验。局部私有提交差值不当作整个应用峰值，采样未观察到增量也不代表零分配。原始日志和数值报告只留在 target。

0.1.10 的 update 层会根据 Cargo 当前版本构造上一补丁的回移 fixture，替换/回滚仍在自建 case 目录；finally 恢复测试开始时备份的原 live 二进制，并检查其摘要，避免把开发候选误留为正常运行版本。错误报告显式按 UTF-8 读取。

`python scripts/test.py --layer diagnostics` 运行有界数据/导出测试，再从真实“关于”入口打开独立原生诊断窗口，覆盖中英 × 明暗主题、默认不采样、停止/关闭/重开、新记录确认、原生保存取消和实际导出，以及窄宽布局。不替换 live、不使用全局输入，也不修改 Shell；依赖当前可用桌面和 Pillow。`python scripts/diagnostics_probe.py --case zh-dark` 可仅复查一个组合。该专项也纳入 release 入口；原生 fixture 当前使用 debug 测试构建，不能冒充优化版全应用内存验收。真实长跑和整应用资源对照独立跟踪，不阻塞普通修复发布。

`quality` 层另跑 `logging::tests`，用 AST 约束无插值的静态日志文案，并在独立子进程验证文件、stderr、span 及 panic 的文本保护；不需要桌面输入，不跑完整桌面套件。`unit --filter logging::tests` 可单独运行。原生数值与布尔值保留，文本、Debug/Display 和错误链在格式化前屏蔽，即使设置 trace 也不输出；显式 CLI 查询结果和用户错误对话框不属于后台日志。该组测试使用系统临时目录，不修改正常会话日志。

`fault` 另覆盖 WinHTTP 原生异步请求在静默/慢速响应头、停滞/慢速正文和断流时的取消、context/读取缓冲寿命及成功重试，以及更新 worker panic 与进度队列满后的状态恢复。仅使用 loopback 和本测试文件，不替换正常外壳；公网 DNS、代理/TLS 故障及跨进程交接另行验证。完整交接仍使用 update 层，不能因组件测试通过而省略。

整应用场景入口：

```powershell
python scripts/scenario_probe.py --candidate <candidate-exe> --restore-exe <normal-running-exe> --scenario idle
python scripts/scenario_probe.py --candidate <candidate-exe> --restore-exe <normal-running-exe> --scenario settings
python scripts/scenario_probe.py --candidate <candidate-exe> --restore-exe <normal-running-exe> --scenario windows
python scripts/scenario_probe.py --candidate <candidate-exe> --restore-exe <normal-running-exe> --scenario soak --subject candidate --observe 7200 --repetitions 1
python scripts/scenario_probe.py --candidate <candidate-exe> --restore-exe <normal-running-exe> --scenario soak --subject candidate --observe 28800 --repetitions 1
```

默认基线为原正常 exe，也可显式指定 `--baseline`；须自行确认两份产物的构建配置，工具记录二进制摘要，不从文件名推断优化级别。默认三次新进程对照，交替先后顺序；5 分钟预热、15 分钟空闲操作期、10 分钟恢复。设置默认 200 个完整循环，窗口默认 300 次、最多同时 8 个本工具窗口；这些动作期间可有不等距采样，使用实际时间，不伪造每 5 秒都有样本。`--warmup 0 --observe 1 --recovery 0 --repetitions 1` 用于驱动短测，不满足资源验收前提；settings/windows 还应指定很小的 --iterations。soak 最少两小时，先完成 2h 再独立进行 8h；指定 `--subject candidate` 避免重复基线长跑，结束不自动等于通过。

该入口会暂时正常关闭当前桌面的 winmgr，并在 finally 从原路径恢复，要求输入桌面可访问及普通用户权限，不使用全局鼠标/键盘，不修改注册或覆盖原二进制。真实短测恢复、四边最大化及中英文回收站打开已验证；正式资源对照与长观察仍待完成。不能与其他桌面测试并发。控制器被强制结束时须参考本轮忽略目录内的恢复记录人工启动原实例。原始报告仅在 target；其中恢复记录有本地路径，禁止复制到公共文档。

`python scripts/test_scenario_probe.py` 的 10 项模拟生命周期检查不需要桌面，也纳入 quality 和 release。模拟通过只证明驱动控制流，不能证明 native 恢复成功或应用资源稳定。曲线可用现有 memory_probe 的 `--plot-only` 从每个 run 目录生成；缺少 matplotlib 时保留数据后再生成。

fault 层现运行整个 update 状态机模块，覆盖进度合并后隐藏 UI 的终态交付、退出取消、幂等 shutdown 与拒绝新 worker；HTTP 覆盖取消中的原生回调寿命，安装模块逐个检查 7 个取消边界。它们不依赖输入桌面，不关闭正常会话。每次操作的取消由应用退出触发；隐藏设置继续既有下载。完整交接、桌面和长跑仍需各自的验收。


跨版本手动启动交接另有显式桌面探针（不得与其他桌面探针并行）：

```powershell
python scripts/instance_switch_probe.py --candidate <candidate-exe> --legacy <official-v0.1.4-exe> --restore <normal-running-exe> --language en
python scripts/instance_switch_probe.py --candidate <candidate-exe> --legacy <official-v0.1.9-exe> --restore <normal-running-exe> --language zh
```

只使用忽略目录内的副本，测试默认取消、Esc、明确确认、原进程正常退出、新进程就绪、相同构建重复启动。外层恢复原程序并核对工作区与 Shell。历史版本无需支持新就绪消息；只在候选上检查该消息。它不覆盖进程异常终止时的恢复或拒绝退出故障注入。


发布候选由 `gh workflow run candidate.yml --ref main` 显式构建，成功后下载 `windows-x64-candidate` artifact。正式场景及 full release 使用其中原样 exe。全部验证与审阅后，将 BUILDINFO.toml 的五个公开字段保存为相应版本的 `.artifact.toml` 发布清单，提交文档再打 tag；release 工作流从成功的生产 run 下载并逐项复核，原样晋升这份文件。不可用本机或后续 CI 重建文件替代已测候选。`quality` 同时运行 10 项晋升门槛测试；它们是受控校验器测试，不是实际文件发布证据。

全发布桌面层另包含 `native_feature_probe.main()`：实际程序“关于 → 诊断”中英/明暗四组，以及实际 DWM hover、源窗口最小化/销毁。回收站场景覆盖中英/明暗、原生 Tab、Enter/Space 八组组合。开发时可通过 `desktop_session.isolated` 独立运行，不能与其他桌面动作并发。


0.1.10 完整更新验证区分两组：默认 `update_probe.main()` 的当前代码回移 fixture，以及发布层 `previous_artifact=--baseline` 的真正历史公开文件。前者便于验证当前故障处理，后者验证旧更新器与新文件的实际协议兼容。报告分别位于 `target/update-acceptance/current-backport` 和 `historical-public`，摘要记录两份准确文件 SHA；两组都必须成功，不能相互顶替。资源任务执行期间不得并行运行这些桌面动作。


发布层显式以 `release=True` 执行资源所有权/停滞源实验、流式更新准备内存对照和诊断 fixture，记录对应 profile；开发层继续默认 debug，可单独选优化构建。诊断 fixture 只说明专项窗口行为，实际候选入口仍由 native_feature_probe 另验。全发布已有优化 Rust 单元，诊断专项不再次重复跑同一组普通单元。


开发桌面层可加 `--candidate-exe <existing-exe>` 跳过构建，直接测试已有文件；记录实际摘要，不推断预构建文件一定来自当前源码。示例：

```powershell
python scripts/test.py --layer settings --restore-exe <normal-running-exe> --candidate-exe <debug-or-candidate-exe>
```

各旧桌面探针脚本的直接 CLI 已改为指向此受保护入口；模块函数仍供隔离上下文调用，interaction 的独立测试窗口子进程入口保留。缺少恢复路径或非法参数时，在构建和窗口操作前退出。6 项无桌面模拟检查验证缺少/无效恢复参数、独立构建目录、预构建跳过编译、测试异常后的恢复与报告，以及禁止预构建参数绕过发布资源门槛；它们不是实际桌面验收。


发布功能层与扩展资源认证分离：`release` 不要求等待小时级 campaign，但仍要求准确候选、公开基线（用于真实历史更新）、官方 0.1.4 及原实例恢复路径，并跑全部自动化功能项。不传 `--campaign` 时报告明确记录 `long_observation_completed=false`；不等于长期资源认证。若提供 campaign 则仍调用原严格资源门槛，缺失/不完整证据照常拒绝。`resource-gate` 的三轮、2h/8h、原始 CSV 核对标准未降低。公开内存结论只引用实际完成的场景与时长。

Run 的既有 22 项桌面回归现由 `scripts/run_probe.py`、`run_probe_input.py` 和版本化 Rust 小程序 fixture 提供，所有输出进入忽略的 target；发布层强制调用，不再因本机忽略目录缺脚本而静默跳过。它验证 Unicode 参数，不等同于真实 IME 组合输入矩阵。路径候选按有限超时等候异步就绪，不以固定短睡眠代替条件检查。
