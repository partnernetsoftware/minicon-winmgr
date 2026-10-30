# 文档索引

产品方向（2026-10-19）：Winmgr 保持轻量外壳；Tskmgr（进程与网络）和 Sysmgr（整机内存/磁盘/硬件）作为独立工具，通过统一入口集成并共享基础代码，暂不建设插件系统。长期预留 macOS/Linux 扩展与 computer-use / mind-use 服务能力，近期不启动完整移植，见[跨平台原则](prd_7_2.md)。

当前公开版：[v0.1.29](https://github.com/partnernetsoftware/minicon-winmgr/releases/tag/v0.1.29)。使用入口：[English](../readme.en.md) / [中文](../readme.zh.md)。

## 当前

| 需要了解 | 文档 |
|---|---|
| 当前版本计划与验收 | [0.1.29 计划](plan_0_1_29.md)、[0.1.29 验收](validation_0_1_29.md) |
| 最近版本验收 | [0.1.28](validation_0_1_28.md)、[0.1.27](validation_0_1_27.md)、[0.1.26 之后的验收](validation_0_1_26.md) |
| 产品需求与模块规格 | [PRD](prd.md) |
| 工具分工与后续顺序 | [工具 PRD](prd_7_1.md)、[后续路线](roadmap_0_1.md) |
| 跨平台和自动化服务方向 | [能力分层与服务契约](prd_7_2.md)、[macOS/Linux 讨论稿](plan_port_macos_linux.md) |
| 界面和交互规则 | [当前设计规范](design.md) |
| 代码入口与边界 | [代码地图](code-map.md) |
| 验证范围与分层 | [分层测试](testing.md)、[公开验收摘要](validation.md) |
| 发布记录与附件清单 | [releases/](releases/)（每版的说明与 `.artifact.toml` 清单） |
| 尚未交付的候选能力 | [后续清单](prd_5_1.md) |

## 历史

0.1.26 及更早版本的计划、验收和设计讨论已移入 [archive/](archive/README.md)，包括 0.1.10 至 0.1.22 的验收记录。归档内容只用于追溯，其中的“当前”“未实现”不代表现状。

历史材料只用于追溯，不自动构成当前任务。原始工作报告不进入版本库，公开证据统一记录于[验收摘要](validation.md)；测试原始输出留在忽略的本地目录。图片必须单独审阅。
