# minicon-winmgr

[语言索引](readme.md) · [English](readme.en.md) · **简体中文**

轻量 Windows 外壳与会话管理器，Rust + Win32。默认作为 companion 与 Explorer 共存；受控接管模式可替换当前用户的桌面外壳。

当前版本 **[0.1.7](https://github.com/partnernetsoftware/minicon-winmgr/releases/tag/v0.1.7)**。版本事实见 [CHANGELOG](CHANGELOG.md)，需求入口见 [PRD](docs/prd.md)，本版范围与验收见 [0.1.6 计划](docs/plan_0_1_6.md)。

0.1.7 包括紧凑暖色设置、中英文切换（默认英文）、白天/黑夜/自动主题、四边任务栏停靠、设置分组、折叠桌面管理、可复制的配置详情，以及检测到自身提升权限后自动以普通权限重启。普通启动直接继续，不主动请求 UAC。实现与覆盖限制见[后续验收](docs/plan_0_1_6.md#10-后续实现与验证2026-10-16)；发布验证见 [0.1.7 验收](docs/acceptance_release_0_1_7.json)。

## 设置预览

真实 Win32 设置窗口截图，界面已包含在 0.1.7 中。默认 **English**，可在设置中即时切换 **中文**并保存；提供白天、黑夜和跟随 Windows 的自动主题。

| English · Light | English · Dark |
|---|---|
| ![English light settings](docs/images/settings_en_light.png) | ![English dark settings](docs/images/settings_en_dark.png) |

| 中文 · 白天 | 中文 · 黑夜 |
|---|---|
| ![中文白天设置](docs/images/settings_zh_light.png) | ![中文黑夜设置](docs/images/settings_zh_dark.png) |

截图直接渲染本应用窗口，没有桌面内容。设计说明见[界面设计](docs/design_0_1_6_industrial.md)，验证方法见[测试分层](docs/testing.md)。

## 当前能力

- 多屏任务栏、窗口激活/最小化、按应用或进程合并、拥挤密度调整和溢出菜单、快捷启动与文件夹入口。
- Alt+Space 启动器；Win+R 运行框（注册失败时 Ctrl+Alt+R），支持历史、路径建议、中文 IME 与启动失败反馈。
- 替换模式的真实通知图标、版本化回调、分页隐藏图标面板与应用重注册恢复。伴随模式保留 Explorer 通知宿主，不绘制不可点击的应用占位。
- 会话回收、日志和资源指标、工作区预留/恢复、显式接管与回滚。

任务栏左侧保留设置入口。开始菜单圆点与窗口列表按钮已于 0.1.2 退役；旧菜单仅保留隐藏开关，不列为默认主路径。声音/网络入口是设置快捷方式，电源/输入法状态图标尚未实现。设置面板使用原生控件，支持键盘导航、滚动、来源说明和保存失败重试。

## 下载

从 [GitHub Releases](https://github.com/partnernetsoftware/minicon-winmgr/releases/latest) 下载 Windows x64 可执行文件与 `SHA256SUMS.txt`。普通运行无需管理员权限；升级前正常退出旧实例并保留个人配置。二进制尚未代码签名。

## 使用与配置

```powershell
cargo build
New-Item -ItemType Directory -Force .\target\debug\live | Out-Null
Copy-Item .\target\debug\minicon-winmgr.exe .\target\debug\live\minicon-winmgr.exe
.\target\debug\live\minicon-winmgr.exe
```

已有实例时运行 `minicon-winmgr.exe --settings` 可再次呼出设置；无任务栏时该入口仍可用。开发时先构建，再退出旧实例、复制到 live 运行，避免锁住构建产物。

单击时间打开通知面板，面板含“时间设置”；双击时间直接打开同一系统日期/时间设置。

Alt+反引号聚焦任务栏，方向键选择、Enter 激活；Win+B 打开通知面板，支持方向键、Home/End、Tab、Enter/Space、Shift+F10、Esc。窗口贴靠使用 Win+方向键，冲突时有 Ctrl+Alt+方向键回退；实际注册结果目前主要写入日志。

配置优先级：环境变量 > exe 旁 `minicon-settings.txt` > `minicon.conf` > 默认值。`MINICON_CONFIG` / `MINICON_SETTINGS` 可指定路径；设置文件异步按键更新并备份，失败保留原文件、显示状态并可重试，手写配置不被重写。有效环境覆盖项在面板中只读并注明变量名。完整键表只维护在 [任务栏与配置规格 §6](docs/prd_6_1.md)。显示任务栏只修改下次启动偏好，面板同时显示当前状态；悬停预览支持运行时开关。Run 的目标和参数均展开已定义的 `%变量%`，未知变量保留；默认打开程序失效时提示指定编辑器。

接管会立即关闭当前会话的桌面外壳，并修改当前用户的后续登录 Shell。先读 [接管与回滚规格](docs/prd_3_1.md)，保留回滚记录；命令行修改需显式 `MINICON_CONFIRM_SHELL=yes`，设置面板保留默认“否”的确认。普通开发无需接管桌面。

## 开发与文档

- [代码与文档地图](docs/code-map.md)：模块边界、已知差距和证据入口。
- [0.1.6 计划](docs/plan_0_1_6.md)：设置日常可用与反馈闭环。
- [0.1.8 设计讨论稿](docs/plan_0_1_8.md)：候选方向，尚未承诺实现。
- [后续能力清单](docs/prd_5_1.md)：未排期需求及实施前提。
- [历史归档](docs/archive/README.md)：旧版本计划、旧路线图和整理前 README。

开发期使用 `python scripts/test.py` 快速检查；`--layer unit --filter config::tests` 只跑指定模块，`--layer appearance` / `settings` 运行对应桌面专项。准备发布时用 `--layer release` 跑全部自动化，并测试 release 产物。分层内容与环境要求见[测试说明](docs/testing.md)。Windows CI 执行 `quality` 静态检查，release workflow 另跑完整单元测试。打 tag 的 release workflow 已支持构建并发布产物；代码签名尚未纳入。构建工具链如在自定义位置，设置对应 `RUSTUP_HOME` / `CARGO_HOME` 并把 Cargo bin 加入 PATH。

0.1.7 发布验收见 [完整发布记录](docs/acceptance_release_0_1_7.json)；0.1.6 的历史自动化与桌面证据见 [版本计划](docs/plan_0_1_6.md) 和 [验收数据](docs/acceptance_0_1_6.json)。物理混合 DPI、多屏热插拔、屏幕阅读器语音、高对比度和小时级长跑未覆盖。
