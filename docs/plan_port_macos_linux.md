# macOS / Linux 版 winmgr 策划（讨论稿）

状态：讨论稿（2026-10-28），未排期、未开工。依据用户提出的方向：做 macOS 与 Linux 版本，权限受限时能做多少算多少；目的是功能增强与 Minicon 系列的品牌扩张，并作为产品背书。与之前的边界关系：[prd_7_2.md](prd_7_2.md) §4 写的是"当前不启动完整移植"，本稿是对该边界的一次受控推进，需要用户确认后才改变 7.2 的状态。

## 0. 参照 minicon 的已有选择（2026-10-28）

minicon（同一公司的跨平台终端 GUI，Windows、macOS、Linux）已经走过这条路，下面这些选择直接沿用，除非有明确理由偏离：

| 方面 | minicon 的做法（来源：`minicon` 仓库） | 对 winmgr 的含义 |
|---|---|---|
| 平台结构 | `minicon-platform` 把平台中立的契约放在 `contract/`，编译期用 `selected.rs` 选择 `adapters/windows`、`adapters/linux`、`adapters/unix` 等适配层 | 采用同样结构：先定契约，再按系统选实现。Windows 现有代码作为 `adapters/windows` 逐步迁入，不做大搬迁 |
| Linux 显示协议 | 只用 X11（`x11rb`）。归档记录写明当时没有 Wayland 后端 | 阶段 A 只做 X11，与 minicon 一致，不另起 Wayland 路线 |
| 窗口与绘制 | 可选 `winit` + `softbuffer` 的像素窗口，`minicon-ui-core` 承担与宿主无关的 UI 机制 | winmgr 的任务条需要 dock 类型窗口和预留边，`winit` 本身不提供这些，需要 X11 层补充；绘制可评估复用 `minicon-ui-core`（需确认其许可与依赖边界） |
| 签名与公证 | Windows 为 Azure Trusted Signing 的公开信任 Authenticode 签名；macOS 为 Developer ID 签名加公证；`release-policy.json` 中 `signing.mode=required`，缺少签名配置则阻断发布，不回退为未签名产物 | macOS 版直接复用同一个 Apple Developer 身份与公证流程，不需要另外开户。Windows 版也可沿用同一签名链路，这会去掉 winmgr 当前“尚未代码签名”的说明 |
| 验证矩阵 | 发布前有“六格”运行时验证（三系统 × 两种架构的组合） | winmgr 的候选验收也按格组织，先覆盖 Windows x64，再按需补 macOS 与 Linux 各一格 |
| 单文件理念 | “one file · no install” | 与 winmgr 的单 exe 一致。macOS 版仍需 `.app` 包结构，这一点要向用户说明 |

由此，原策划中的几项决策会变化（见 §7）：Apple 开发者账户不需要另外决定，签名基础设施可以复用。

## 1. 先说清楚的前提

- **Winmgr 在 Windows 上的核心能力是"替换外壳"**：接管登录 Shell、关闭 Explorer 的任务栏与桌面。这一能力在 macOS 上**不存在**（Dock、菜单栏、Finder 由系统持有，且受 SIP 保护），在 Linux 上只存在于特定窗口管理器或桌面环境中。
- 因此 macOS/Linux 版不是"移植外壳"，而是**同一产品形态的最大可实现子集**：任务栏/菜单栏条、窗口列表与切换、启动器（Run）、时钟与设置、主题。接管桌面、桌面管理、Shell 注册表相关能力全部不在范围内。
- 能力是否可用取决于系统权限与桌面环境，每项功能都要能表达"受限、需授权、不可用"，并给出原因（沿用 prd_7_2 §2 的能力分层）。

## 2. 平台能力速查（需逐项实测确认）

### macOS

| 能力 | 所需接口 | 权限 |
|---|---|---|
| 菜单栏条（时钟、入口） | NSStatusItem / NSPanel | 无 |
| 运行中的应用列表、激活应用 | NSWorkspace | 无 |
| 窗口列表（标题需授权） | CGWindowListCopyWindowInfo | 屏幕录制（读取他应用窗口标题） |
| 激活、最小化、关闭他应用窗口 | Accessibility（AXUIElement） | 辅助功能授权 |
| 启动器、Run | 自绘面板 + NSWorkspace 打开 | 无 |
| 接管 Dock / 菜单栏 | 不可能 | — |

分发：未经签名公证的应用会被 Gatekeeper 拦截，需要 Developer ID 签名与公证（Apple Developer Program，年费）。架构需同时覆盖 arm64 与 x86_64（universal）。

### Linux（X11）

| 能力 | 所需接口 | 权限 |
|---|---|---|
| 窗口列表、标题、所属桌面 | EWMH：_NET_CLIENT_LIST、_NET_WM_NAME、_NET_WM_DESKTOP | 无 |
| 激活、最小化、关闭 | _NET_ACTIVE_WINDOW、_NET_WM_STATE、_NET_CLOSE_WINDOW | 无 |
| 预留任务栏区域 | _NET_WM_WINDOW_TYPE_DOCK、_NET_WM_STRUT_PARTIAL | 无 |
| 系统托盘图标 | StatusNotifierItem（D-Bus） | 无 |
| 启动器 | .desktop 文件 | 无 |

X11 上大多数窗口管理器（Openbox、Xfwm、i3 等）遵循 EWMH，这是最易验证的路线。

### Linux（Wayland）

- 普通应用没有"列出全部窗口 / 激活他应用窗口"的通用协议。
- wlroots 系（sway、Hyprland 等）提供 wlr-foreign-toplevel-management；KDE Plasma 有自有接口；GNOME 目前没有对应的通用接口，需要扩展。
- 结论：Wayland 只能按合成器分别适配，属于第二阶段，且在 GNOME 上可能完全做不了任务列表。

## 3. 建议的产品范围（能做多少算多少）

| 层级 | 内容 | 平台 |
|---|---|---|
| T0 共享核心 | 配置文件（winmgr.cnf 格式）、主题色板、启动项数据、设置面板的内容模型、更新检查（GitHub zip） | 三端 |
| T1 任务条/菜单栏条 | 时钟、窗口或应用列表、切换、启动器入口、设置入口 | macOS 菜单栏形态；X11 底部/顶部条 |
| T2 增强 | 托盘图标（SNI）、Run 对话框、多显示器工作区 | X11；macOS 视权限 |
| 明确不做 | 替换系统 Shell、桌面图标与壁纸管理、Dock 替代、接管系统菜单栏 | — |

每项能力都标注支持范围与缺失原因，界面不伪造数据或放置无效按钮（沿用 prd_7_2 §2 的原则）。

## 4. 代码结构

- 把窗口/进程/通知等平台相关的逻辑收敛到适配层：`platform::windows`、`platform::macos`、`platform::x11`，共享 T0 的数据模型和规则。
- Windows 现有代码不做大规模搬迁，先用"适配层薄包装"的方式逐步隔离，避免影响 0.1.x 的稳定性。
- UI 技术选择需单独决定（原生 AppKit / GTK / 自绘），不预设统一工具包。

## 5. 分阶段（建议，待确认）

1. **阶段 A：Linux X11 原型（选它先行的理由见 §6）**
   - 窗口列表、切换、DOCK 预留区域、时钟；只读验证 EWMH 数据与权限。
   - 验收：在 2–3 种常见窗口管理器上（实测记录）列出窗口并正确激活。
2. **阶段 B：macOS 菜单栏原型**
   - 状态栏条、应用列表、Run 入口、无权限下的最小可用集；再逐项引入屏幕录制与辅助功能授权，并说明每项授权的用途。
   - 验收：未授权与授权两种状态下的功能差异表；签名公证后的安装与更新流程。
3. **阶段 C：共享核心抽取与发布**
   - 把 T0 抽成共享 crate，Windows 版改为使用它；分别发布 macOS 与 Linux 产物。
4. **阶段 D：Wayland 按合成器适配**（不承诺覆盖全部）。

每个阶段独立版本号，不要求与 Windows 版本同步（roadmap §1 已允许分别发布）。

## 6. 风险与取舍

- **权限墙**：macOS 的窗口标题与窗口操作都需要用户授权，首次使用体验会打折扣；需要在产品里解释每项授权的用途，并允许不授权使用。
- **签名与发布成本**：需要 Apple Developer 账户与签名公证流程（CI 中的密钥管理同 0.1.24 讨论的签名问题，需要同样的决策）。
- **Linux 覆盖面窄**：X11 实际用户占比在下降，Wayland 为主流方向但无统一接口。先做 X11 的理由是"最容易验证公共模型"，它不代表市场优先级。
- **维护成本**：三端同时维护会把 Windows 的稳定性与回归压力放大。建议阶段 A 不替换 Windows 的任何现有路径。
- **品牌风险**：如果 macOS/Linux 版本体验明显弱于 Windows，反而会损害背书；因此应在"受限但可靠"的范围内发布，对未覆盖的能力直接写明。

## 7. 需要用户确认的事项

1. 是否改变 prd_7_2 的边界，开始阶段 A（Linux X11 原型）。
2. 第一个平台：建议 Linux X11 先行（无权限墙，便于验证公共模型）；如果你更看重用户规模，可以把 macOS 菜单栏提前，但需接受签名与授权的成本。
3. 是否复用 minicon 已有的 Apple Developer 身份与公证流程（建议：复用，不另开户）。
4. Linux 支持矩阵：先支持哪些窗口管理器（建议以 2–3 个常见的 X11 WM 为准）。
5. 是否允许新建独立代码目录或仓库（例如 `minicon-winmgr-linux`），还是在本仓库内用 cargo feature 隔离。
6. 是否让 winmgr 依赖 minicon 的共享 crate（`minicon-platform`、`minicon-ui-core`）。这会带来跨仓库的版本耦合，需要单独决定依赖方式（路径依赖、发布版本或 git 依赖）。
7. 是否把 Windows 版也接入 minicon 的签名链路（建议：是，并以此替代当前“尚未代码签名”的状态）。
