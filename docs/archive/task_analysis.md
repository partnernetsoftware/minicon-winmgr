# Tskmgr 任务分析（0.1.18）

默认进程树按真实父子关系组织；点击箭头展开或折叠，取消“进程树”可回到平面排序。搜索会保留匹配进程的祖先路径，并临时展开，不改变原有折叠状态。不同账户、会话或不可信的父进程身份不强行挂接。

选中进程后点击“笔记 / 标识…”。默认只分析**本次运行**（账户、PID、创建时间）；可切换为**整个应用**（账户与完整程序路径），让分析在应用重启后继续积累。应用范围只在程序路径可取得时提供；同一路径的文件被升级或替换仍归入同一应用，不代表文件内容受到信任。

笔记追加保存作者类型、作者、时间、实际运行身份、正文、依据和状态历史。可标为未分析、正常、关注中、异常 / 待核实、已处理；仅改变状态也可以保存。正文中请区分事实与推测，依据栏填写观察来源。`Ctrl+S` 追加，`Ctrl+A` 全选文本，`Esc` 关闭；有草稿时确认放弃。

状态在进程名后显示：`[?]` 关注、`[!]` 异常待核实、`[done]` 已处理、`[normal]` 正常、`[notes]` 有笔记但未设置状态。搜索 `status:suspicious`、`status:watching`、`status:resolved` 或 `has:notes` 可筛选，树中仍显示祖先。本次运行的明确状态优先于应用状态；清除本次状态后恢复应用状态。两个范围的历史分别保留。

笔记默认在**执行账户**的 `%LOCALAPPDATA%/minicon-tskmgr/analysis/task-notes.json`。其他凭据启动的管理员工具使用那个执行账户的存储，不冒充原账户，也不自动跨账户同步。笔记不上传、不自动抄录命令行或连接信息。异常标识不等于恶意判定，不自动结束进程、提权或阻断网络。

## Agent 的本地接口

普通权限调用 `minicon-tskmgr --notes-json`，从 stdin 读取一条 UTF-8 JSON 请求，stdout 输出一条 JSON 响应后退出。与原生界面共享存储；显式测试目录可用 `--store ABSOLUTE_DIRECTORY`（界面对应 `--notes-store`）。接口拒绝提权运行，不提供进程管理或自动授权操作。

先用 `--snapshot-json OUTPUT_PATH` 取得当前普通权限快照中的 `identity`（PID 与 `created_100ns`），避免只凭 PID。以下数字是示例，须换成实际身份：

```json
{"op":"read","target":{"pid":1234,"created_100ns":133000000000000000},"scope":"run"}
```

`scope` 默认为 `run`；`application` 选择该进程的账户与完整路径，路径未知则拒绝。新对象必须通过仍在运行、身份校验通过的 `target` 建立。读取返回 `subject`、`revision`、`flag`、`entries`。追加时传回该范围最新的 `revision`：

```json
{"op":"append","target":{"pid":1234,"created_100ns":133000000000000000},"scope":"run","expected_revision":0,"author":"my-agent","text":"推测：需要进一步核实用途。","evidence":"说明采样来源或人工提供的证据","flag":"watching"}
```

可用状态：`unreviewed`、`normal`、`watching`、`suspicious`、`resolved`；省略 `flag` 保持当前状态。`author` 是调用方声明的来源名称，不是认证或操作权限。人工作者记录实际执行账户，agent 作者固定标为 agent。

`{"op":"list"}` 列出已有分析对象和版本；以返回的完整 `subject` 替代 `target` 可继续读取或追加已有历史，即使进程已退出。`subject` 与 `target` 互斥，使用 `subject` 时不要传 `scope`。不允许凭任意历史对象创建新线程。

成功格式为 `{"schema":1,"ok":true,"data":...}`，退出码 0。失败为 `{"schema":1,"ok":false,"error":{"code":"...","message":"..."}}`，退出码 1。`conflict` 表示其他人已追加：先重读、复核后再提交，不能盲目覆盖。界面冲突时保留草稿，点击“重载历史”后复核再保存。

请求上限 64 KiB，正文 16 KiB、依据 8 KiB、作者 128 字节；界面输入另限正文 4096、依据 2048 UTF-16 单元。存储上限 8 MiB / 10000 条，满时提示归档，不静默删除。存储使用锁、同步写入和原子替换；损坏时拒绝追加并保留原文件。它是本地协作记录，不是防篡改审计，也不自动修复或清理历史。
