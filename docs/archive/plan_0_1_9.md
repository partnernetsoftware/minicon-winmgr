# 0.1.9 需求：设置中的版本更新

> 历史记录：本版已交付并由后续公开版本替代，保留当时设计及验收边界。当前公开版见[0.1.11](plan_0_1_11.md)，后续讨论见[文档索引](../README.md)。

状态：[v0.1.9 已公开发布](https://github.com/partnernetsoftware/minicon-winmgr/releases/tag/v0.1.9)，完整验收、公开附件校验及实际下载重启交接已完成。详见[公开验收摘要](../validation.md)。下一版见[0.1.10 设计](plan_0_1_10.md)。

设置提供 **Check / Update / Restart**（中文：检查更新 / 更新 / 重启），不增加任务栏常驻入口。参考本地 `../minicon/src/update.rs`、`host_ui.rs` 的三按钮状态映射，以及 `main.rs::restart_into_staged`。

参考实现已核对：用户点击后才联网，无启动扫描/后台定时器；网络与替换在 worker；下载内容根据发布的 SHA-256 校验；更新与重启分开，重启是独立显式操作。winmgr 需要使用自己的 GitHub 仓库与 exe + SHA256SUMS 附件，不能照搬 minicon 的归档/跨平台包规则。

实施状态（校验包含在下载步骤）：Idle → Checking → UpToDate / Available → Downloading → Verifying → ReadyToRestart → Restarting；错误保留当前运行版本，可重新检查/重试。按钮随状态启用、显示当前/目标版本和进度，双语并沿用现有主题。

实施约束：运行中文件的 Windows 替换方案、备份/回滚、下载大小与超时限制、校验不匹配、离线/限流、文件占用、保存失败/未保存设置、已有实例与工作区恢复。重启不能关闭用户应用或主动请求 UAC；先构建/下载并校验，再正常退出并有界交接，失败保持可用外壳。具体机制与故障注入验收见下。


## 本轮收敛设计

- 设置在任务与窗口卡片下新增紧凑版本区：当前版本/状态一行，Check / Update / Restart 三按钮一行；默认窗口适度增高约 64 px，窄窗自然换行并继续滚动，保存页脚固定。沿用明暗主题、原生键盘/UIA，中英文即时切换。主界面不显示 API 地址、环境变量或内部路径。
- 不在启动、打开设置或定时器中联网；只有检查和更新按钮启动一个短生命周期 worker。WinHTTP 使用系统 TLS 校验与代理，限制重定向、响应大小、各段超时与总预算；SHA-256 使用 Windows CNG。不引入浏览器引擎或常驻网络线程。
- 仅接受本仓库稳定正式版本与精确 x64 exe / SHA256SUMS 文件名；版本按数字比较，不降级。下载上限 64 MiB，发布信息 512 KiB，校验表 64 KiB；校验及 x64 PE 检查通过才标记可重启。
- 文件先放到可写安装目录下唯一临时子目录。重启按钮在设置保存完成后才启用交接：启动同版本临时助手并等待就绪，再正常关闭当前实例；助手等旧进程真正退出后，将旧 exe 留为备份、替换并启动新版本。新实例在窗口与单实例初始化完成后回执，失败回滚并启动旧版本。全程普通权限，安装目录不可写时提示手动更新。
- 故障时保留当前运行能力；检查/下载可重试。已下载内容在重启前和助手安装前再次校验。仅操作本实例创建的临时目录和子进程，不结束用户应用或其他会话。
- 新增 update 单元/本地 HTTP 故障层和隔离目录真实交接层，开发期按层运行；公开发布前完整 release 层，发布后验证 exe、SHA256SUMS 与 GitHub 摘要。

实现说明：网络总预算在 WinHTTP I/O 边界检查；正在阻塞的原生调用由分段超时约束，并非精确到毫秒的强制中断。每次下载保留独立暂存/备份目录，不在普通启动时扫描清理；历史备份可在确认新版本稳定后手动删除。特殊文件系统/权限也可能阻止回滚，保留原文件和错误记录并不宣称任何存储故障都能自动恢复。

API 依据：[WinHTTP 超时](https://learn.microsoft.com/en-us/windows/win32/api/winhttp/nf-winhttp-winhttpsettimeouts)、[Windows CNG](https://learn.microsoft.com/en-us/windows/win32/api/bcrypt/nf-bcrypt-bcryptopenalgorithmprovider)。

## 发布验收

完整 release 层通过：268 项单元测试、15 项更新/回滚、36 项设置、22 项 Run 回归、44 项按需交互、16 项布局、27 项主题/停靠和 8 项任务管理器检查。详见[验收摘要](../validation.md)。

内部交接协议：`--apply-update plan.json` 运行暂存助手，`--after-update started` 启动新版并确认回执。后续版本须保留兼容；缺少回执或构建版本与目标版本不同会触发回滚。
