# plan-shell-gaps — 外壳功能补全（叙述性）

> 目的：给后续会话 / 分身接手用。接力前先读 `docs/prd.md`（反漂移入口）与本文。
> 约束（贯穿）：**companion 模式**，不改 Winlogon Shell，不发版、不签名。

## 现状（已落地）

可靠性底座（护城河）已完成：M0–M8 —— 日志/panic、单实例、消息泵、窗口感知、
自绘任务栏（AppBar 停靠、多显示器每屏一栏、键盘导航 Alt+`）、会话生命周期、
受控 Shell 开关、托盘镜像/宿主、启动器（Alt+Space）、主菜单（ORB：应用 + Places + 电源）、
运行对话框（Win+R / 回退 Ctrl+Alt+R）、Snap 热键、稳定性指标。

**已验证**（自测，非仅编译）：新 exe 常驻无 panic/ERROR；句柄 ~382 / GDI 20 在
数十秒内稳定（无泄漏）；各热键在被占用时优雅降级并落日志。

## 缺口（按杠杆排序）

1. **热键在 companion 下的可用性**：`Win+←/→/↑` 与 `Win+R` 常被默认外壳占用。
   运行对话框已加 `Ctrl+Alt+R` 回退；**Snap 热键仍无回退**——应仿照 run_dialog，
   在 `hotkeys.rs` 里对每个动作尝试 `Win+*` → 回退 `Ctrl+Alt+*`，并记录胜出的组合。
2. **任务栏快速启动 / 列表按钮**：品牌点左侧加固定快捷方式；右区加"窗口总览"按钮
   （列出全部分组窗口，点击切换）。复用 `grouping` + `window_snapshot`。
3. **悬停窗口预览**：鼠标悬停任务按钮时弹一个缩略图（`PrintWindow`/DWM 缩略图）。
   较重，先做纯几何 + 缩略图抓取的单测，再做悬浮窗。
4. **主菜单完善**：最近使用、搜索框、拖拽整理分类（可后续）。
5. **可浏览桌面 / 文件夹堆栈**：价值高但工程量最大，排在最后。

## 工作方式（沿用）

- 大改动拆成**并行 subagent**，每个只改**唯一文件**；接线（`window.rs` / `taskbar.rs`）
  由主代理单点做，避免冲突。
- 每个 subagent：禁改 `Cargo.toml`、禁 `cargo add`、禁 `cargo fmt`、禁起 GUI；
  可跑 `cargo check`。纯逻辑必须带单测。
- 收尾统一 `cargo fmt` + `cargo clippy -D warnings` + `cargo test`，并**真跑一次新 exe**核对日志。

## 验收基线

`cargo test` 目前 76 passed。改动后不得低于此数；`clippy -D warnings` 必须干净。
