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

## 4. 签名运行（2026-10-29）

- 运行 `winmgr-signed-candidate.yml`，源提交 `ffb809b`，run `40846854862`，六个 job 全部成功（guard、build-companion、sign-companion、build-shell、sign-shell、package）。
- 产物 `windows-x64-signed-candidate` 中两个 exe 都经 `Get-AuthenticodeSignature` 核对为 Valid，发行者 PARTNERNET SOFTWARE PTY LTD，带 RFC 3161 时间戳。
- 摘要：Winmgr `61c80340…`，Tskmgr `e430e5af…`，zip `23611388…`；BUILDINFO 与 SHA256SUMS 一致。清单 `docs/releases/v0.1.29.artifact.toml` 原样取自该 BUILDINFO。

## 5. 发布路径

- `scripts/release_artifact.py`：从 0.1.29 起读取 `release-policy.json`；`required` 时只接受签名工作流的 run 与 `windows-x64-signed-candidate`，并校验两个 exe 的签名；所有晋升都校验 zip 只含已审阅的 exe。策略缺失或取值非法直接失败。
- 单元测试 29 项通过，覆盖签名路径、候选 run 被拒绝、未签名 exe 被拒绝、策略缺失、`off` 模式、zip 内容与签名判定的正反例。
- 本机对真实签名 run 执行 `promote` 成功，输出的五个文件与签名产物一致。

## 6. 公开发布（2026-10-29）

- 第一次发布运行（run `40848519271`，标签指向 `c7f71fc`）在晋升步骤失败：Windows PowerShell 5.1 继承了 pwsh 的 `PSModulePath`，`Get-AuthenticodeSignature` 模块加载失败。没有生成草稿，也没有公开任何产物。
- 修复（提交 `b724097`）：签名校验调用 PowerShell 时去掉继承的 `PSModulePath`，单元测试锁定该行为（30 项通过）。已在 PS 7 模块路径下复现失败并确认修复后可运行。标签移动到 `b724097`（之前的标签未发布）。
- 第二次发布运行（run `40848824915`）成功，生成草稿。草稿的五个资产与签名运行产物逐字节一致（SHA-256 与大小均相同）。
- 公开：`gh release edit v0.1.29 --draft=false --latest`；匿名 API 的 `releases/latest` 返回 `v0.1.29`，`releases/tags/v0.1.29` 返回 200。
- readme 中英文条目已加入 0.1.29。

## 7. 未完成的验证

- 公开更新实机探针（`public_update_probe.py`，从已发布的 0.1.28 更新到 0.1.29）没有完成。该探针会关闭并重新启动运行中的外壳；运行时没有 Winmgr 进程，探针在任何改动前就拒绝了（“原外壳身份与恢复文件不一致”）。未启动或关闭任何外壳。需要在有人值守、并有一个运行中的外壳时补跑。
- 已知问题：左右任务条时“显示桌面”把最小化窗口的标题条放到屏幕底部（0.1.28 同样存在）。未修，待单独处理。
