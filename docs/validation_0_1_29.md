# 0.1.29 验收记录

本版完成图标、程序资源与签名发布链路。计划见 [plan_0_1_29.md](plan_0_1_29.md)。本文件在开发中逐项补充，发布后定稿。

## 1. 本地构建与资源（2026-10-29）

- `cargo build --release --locked --bin minicon-winmgr` 通过，版本 0.1.29。
- 构建产物的文件属性：产品名 `Minicon Winmgr`，公司 `PARTNERNET SOFTWARE PTY LTD`，文件说明 `Winmgr shell and taskbar`，原始文件名 `minicon-winmgr.exe`，文件版本 0.1.29；图标可从 exe 提取（32×32）。

## 2. 两阶段绑定演练（本地，未签名）

- `build_candidate.py --tool-only` 输出 Tskmgr 摘要 `60b879ad…`。
- 以该 Tskmgr 作为 `--signed-tool` 输入编译 Winmgr，输出的绑定摘要与 Tskmgr 摘要一致。
- 这只证明构建顺序与绑定正确，不是签名证据，也不是发布候选。

## 3. 签名身份（2026-10-29）

- 仓库专属 Entra 应用、联合凭据（精确 GitHub 主体）、`minicon-public` 证书配置上的签名者角色，以及 `release-signing` 环境中的三个 secret，均已创建。
- 只读核对：GitHub 侧 READY；Azure 侧 READY（四个签名者各有一个精确主体）。

## 4. 签名运行、发布路径与公开发布

待补充。
