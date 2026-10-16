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

`release` 表示当前自动化全套，不代表所有物理环境已覆盖。混合 DPI、多屏热插拔、高对比度实屏、屏幕阅读器语音、特殊管理员令牌以及小时级常驻观察仍需独立环境验收。CI 没有当前交互桌面，执行 `quality`，release workflow 另外执行全部单元测试；发布前在可用桌面运行 `release` 并审阅报告。仅构建优化二进制不会自动声称完成发布验收。

发布层构建目录为 `target/release-validation`，避免旧会话直接运行 `target/release` 时锁住 Cargo 硬链接产物。不会为释放构建文件而结束其他会话。桌面专项仍复制产物到 live 后运行。

更新层会将当前源码回移到隔离的 0.1.8 测试构建，以验证真实 0.1.9 目标的两版本交接；该 fixture 不是历史公开版。服务仅监听 loopback，验收报告明确记录两份二进制 SHA-256。后台助手只允许本用户/本会话，并等旧进程退出后才修改本测试安装。成功后清理本轮自建用例目录，保留事务事实与截图。

## 公开证据检查

提交及发布前运行 `python scripts/check_public_docs.py`，CI 同样检查 Git 暂存区内容，拒绝原始验收 JSON 和工作站路径。可先运行 `python scripts/check_public_docs.py --self-test` 验证检查器。不要将原始报告复制到 docs；公开结果写入人工审阅的 Markdown，截图只使用无个人路径的隔离演示。该检查不能识别图片中的文字，图片必须目视检查。

0.1.10 计划新增 memory/resources/fault/soak 层，当前尚未实现，不属于现有命令。
