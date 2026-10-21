# 0.1.20 验证与公开交付记录

状态：已合 main 并公开为 Latest；准确候选、附件核对、真实公开 .19 → .20 更新与同版工具下载通过。范围见[计划](plan_0_1_20.md)：仅任务栏全屏让位，不涉及设置、任务管理、工具获取、更新与授权逻辑。

## 1. 风险与证据

| 风险 | 直接证据 | 结论 |
|---|---|---|
| 全屏应用被任务栏遮挡（原始缺陷） | 隔离桌面实际路径：覆盖整屏的前台窗口使任务栏退出 topmost（扩展样式位实测清除），任务栏区域命中测试返回全屏窗口 | 让位生效，全屏画面不再被遮挡 |
| 让位后不恢复 | 销毁全屏窗口后在一个时钟节拍内实测恢复 WS_EX_TOPMOST | 恢复路径通过 |
| 最大化/普通窗口误触发 | 工作区尺寸窗口置前超过一个节拍不触发让位；`covers_monitor` 正反例单元（全屏、越界无边框、最大化缺一边、部分覆盖、跨屏） | 不误判 |
| 桌面宿主误判 | `is_desktop_host_class` 单元：Progman/WorkerW 排除，mstsc 的 TscShellContainerClass 不排除 | 桌面不隐藏任务栏 |
| 重停靠覆盖让位状态 | 重停靠与 AppBar `ABM_SETPOS` 后的插入位置按让位状态选择（`bar_insert_after`）；AppBar 回调新增 `ABN_FULLSCREENAPP` 收敛到同一评估 | z 序一致 |
| 既有桌面行为回归 | 全屏层含启动置顶、外壳注册不变、原实例恢复检查；布局/合并/密度逻辑未改，复用既有单元 | 未见退化 |

`taskbar::tests` 新增 3 项单元通过；严格 Clippy 与质量门禁通过（含日志静态文案审计，让位日志改为字段化后通过）。本版未重复设置、更新、UAC、进程管理与工具获取矩阵，对应实现未改变，沿用 [0.1.19 验收](validation_0_1_19.md)。实机验证为当前单显示器会话；多屏让位有单元覆盖，实机扩样记录为限制。真实 mstsc 全屏场景依赖用户更新后在实际工作流中确认。

## 2. 正式候选验收（2026-10-21）

[候选 39620863457](https://github.com/partnernetsoftware/minicon-winmgr/actions/runs/39620863457) 成功，来源 `b04c290a4eba98793189996f49f2a3e97426c4de`。公开文档边界、质量检查与一次普通优化单元环境通过；未勾选授权、菜单与 TCP 专项矩阵（对应实现未改）。

下载的双产物及校验表逐一核对，产品源码指纹 `ac370c6c94c90ead049b000c462f69a2ab6c90acee3e7bd7820a23a42d5b821f` 与当前产品 Git blobs 相同：

- Winmgr：`340ec6a36b39f32bdb81414125021f0d9cdc07d78f2e1a2adec89f3cde7a4fcd`
- Tskmgr：`e1649676c445abc98468e05f4fe50917691d2c8e281ad3df690999ccacb7eb14`

全屏层（`test.py --layer fullscreen`）对准确 CI Winmgr 通过 6 项检查，报告记录的被测 exe 摘要与候选一致；结束后原公开 .19 外壳恢复，Shell 注册未改变。原始报告留在忽略目录。

接受记录保存在 [v0.1.20.artifact.toml](releases/v0.1.20.artifact.toml)，发布须原样晋升，禁止重新构建或替换已验收文件。

## 3. 公开交付（2026-10-21）

代码及接受记录已合入 main，发布标签 `v0.1.20` 指向 `b0e017b`。[main CI 39622320033](https://github.com/partnernetsoftware/minicon-winmgr/actions/runs/39622320033) 与 [发布 39622370843](https://github.com/partnernetsoftware/minicon-winmgr/actions/runs/39622370843) 成功。发布工作流按接受清单原样晋升四个文件，未重新构建；完整下载草稿后，两份 exe、BUILDINFO、SHA256SUMS 与候选逐字节相同。

[公开 v0.1.20](https://github.com/partnernetsoftware/minicon-winmgr/releases/tag/v0.1.20) 于 **2026-10-21T15:13:12Z** 发布，非草稿、非预发布，并设为 Latest。匿名 GitHub Latest API 核实版本、公开状态及四个附件。

`public_update_probe.py` 使用正式 .19 更新器连接真实 GitHub Latest，检查、下载校验、重启至 .20 的 11 项检查全部通过；实际运行文件为新的 .20 官方版本化名称，内容为上述准确候选，旧名移除、旧版备份及设置保留，令牌仍为原用户/会话普通权限。结束后原桌面恢复，未替换用户安装文件。

`tool_entry_probe.py --public-download` 从 .20 实际 My tasks 入口确认下载同 tag 的公开 Tskmgr；核对摘要与准确候选一致（Winmgr `340ec6a3…`、Tskmgr `e1649676…`），工具正常打开退出，原桌面恢复。

物理多屏让位实机扩样与真实 mstsc 工作流确认仍待用户环境反馈。已替代的 .19 计划归档，旧标签及公开附件不改写。
