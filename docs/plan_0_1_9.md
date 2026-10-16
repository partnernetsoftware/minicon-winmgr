# 0.1.9 需求：设置中的版本更新

状态：已记录，后续设计；0.1.8 继续按当前范围交付。

设置提供 **Check / Update / Restart**（中文：检查更新 / 更新 / 重启），不增加任务栏常驻入口。参考本地 `../minicon/src/update.rs`、`host_ui.rs` 的三按钮状态映射，以及 `main.rs::restart_into_staged`。

参考实现已核对：用户点击后才联网，无启动扫描/后台定时器；网络与替换在 worker；下载内容根据发布的 SHA-256 校验；更新与重启分开，重启是独立显式操作。winmgr 需要使用自己的 GitHub 仓库与 exe + SHA256SUMS 附件，不能照搬 minicon 的归档/跨平台包规则。

候选状态：Idle → Checking → UpToDate / Available → Downloading → Verifying → ReadyToRestart → Restarting；错误保留当前运行版本，可重新检查/重试。按钮随状态启用、显示当前/目标版本和进度，双语并沿用现有主题。

设计前必须明确：运行中文件的 Windows 替换方案、备份/回滚、下载大小与超时限制、校验不匹配、离线/限流、文件占用、保存失败/未保存设置、已有实例与工作区恢复。重启不能关闭用户应用或主动请求 UAC；先构建/下载并校验，再正常退出并有界交接，失败保持可用外壳。实际机制与故障注入验收在 0.1.9 实施前确定。
