//! English/Chinese UI catalog. Identifiers, paths and user input are never localized.
use std::sync::atomic::{AtomicU8, Ordering};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    En,
    Zh,
}
impl Language {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "en" | "en-us" => Some(Self::En),
            "zh" | "zh-cn" => Some(Self::Zh),
            _ => None,
        }
    }
    pub fn as_key(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Zh => "zh",
        }
    }
}
static LANGUAGE: AtomicU8 = AtomicU8::new(0);
pub fn set(language: Language) {
    LANGUAGE.store(u8::from(language == Language::Zh), Ordering::Relaxed);
}
pub fn language() -> Language {
    if LANGUAGE.load(Ordering::Relaxed) == 1 {
        Language::Zh
    } else {
        Language::En
    }
}
pub fn text(value: &str) -> &str {
    render(language(), value)
}
fn render(language: Language, value: &str) -> &str {
    for &(zh, en) in CATALOG {
        if value == zh || value == en {
            return if language == Language::En { en } else { zh };
        }
    }
    value
}
pub(crate) fn localize_window(hwnd: windows::Win32::Foundation::HWND) {
    use windows::{Win32::UI::WindowsAndMessaging::*, core::PCWSTR};
    unsafe {
        let mut kind = [0u16; 128];
        let n = GetClassNameW(hwnd, &mut kind);
        let kind = String::from_utf16_lossy(&kind[..n.max(0) as usize]);
        if !(kind.starts_with("minicon_winmgr_")
            || kind.eq_ignore_ascii_case("STATIC")
            || kind.eq_ignore_ascii_case("BUTTON"))
        {
            return;
        }
        let mut title = vec![0u16; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
        let n = GetWindowTextW(hwnd, &mut title);
        let title = String::from_utf16_lossy(&title[..n.max(0) as usize]);
        let translated = text(&title);
        if translated != title {
            let _ = SetWindowTextW(hwnd, PCWSTR(crate::win_util::wide(translated).as_ptr()));
        }
    }
}
#[macro_export]
macro_rules! ui_format {
    ($zh:literal,$en:literal $(,$($args:tt)*)?)=>{if $crate::i18n::language()==$crate::i18n::Language::En {format!($en $(,$($args)*)?)}else{format!($zh $(,$($args)*)?)}};
}
const CATALOG: &[(&str, &str)] = &[
    (
        "无法打开保存窗口，请重试。",
        "Could not open the Save dialog. Try again.",
    ),
    ("诊断…", "Diagnostics…"),
    ("资源观察", "Resource observation"),
    ("场景", "Scenario"),
    ("日常使用", "Daily use"),
    ("空闲", "Idle"),
    ("窗口操作", "Window activity"),
    ("版本更新", "Version update"),
    ("开始记录", "Start"),
    ("新记录", "New"),
    ("停止", "Stop"),
    ("导出摘要…", "Export…"),
    ("准备就绪，尚未记录", "Ready — no recording yet"),
    (
        "导出仅含版本、时间、场景和资源数值。请先查看上方摘要；保存时可取消。",
        "Export includes version, times, scenario and counters. Review above; you can cancel Save.",
    ),
    ("正在保存摘要…", "Saving summary…"),
    ("上次摘要已保存", "Last summary saved"),
    (
        "保存失败，请选择可写位置后重试。",
        "Save failed. Choose another location.",
    ),
    (
        "无法保存摘要，请稍后重试。",
        "Could not start export. Try again.",
    ),
    (
        "无法开始记录，请重试。",
        "Could not start recording. Try again.",
    ),
    (
        "开始新记录会替换当前记录。继续？",
        "A new recording replaces the current one. Continue?",
    ),
    (
        "点击“开始记录”后，每 5 秒采样一次，最长 8 小时。\n关闭此窗口会停止记录，已有数据保留至新记录或程序退出。\n\n记录包含诊断自身开销；资源变化不等于泄漏，也不自动代表稳定性通过。\n缺失指标显示为暂不可用，不填成零。",
        "Press Start to sample every 5 seconds, for up to 8 hours.\nClosing this window stops recording. Data stays until a new recording or application exit.\n\nMeasurements include diagnostic overhead. Resource changes are not an automatic leak or stability verdict.\nMissing metrics stay unavailable, rather than becoming zero.",
    ),
    ("预览暂不可用", "Preview unavailable"),
    (
        "此处无法更新，请下载发布文件后手动替换。",
        "Cannot update here. Download the release and replace manually.",
    ),
    (
        "已恢复旧版本，可重新检查更新。",
        "Previous version restored. Check to retry.",
    ),
    ("检查更新", "Check"),
    ("更新", "Update"),
    ("版本 {}", "Version {}"),
    ("正在检查更新…", "Checking for updates…"),
    ("{} · 已是最新版本", "{} · Up to date"),
    ("正在下载… {:.1} MiB", "Downloading… {:.1} MiB"),
    ("{} 已就绪，重启后更新", "{} ready to restart"),
    ("正在重启…", "Restarting…"),
    (
        "重启未完成，请重试。",
        "Restart did not complete. Please retry.",
    ),
    (
        "更新未完成，请重试。",
        "Update did not complete. Please retry.",
    ),
    ("应用", "App"),
    ("采用", "Use"),
    (
        "↑↓ 选择 · Enter 采用 · Esc 收起候选",
        "↑↓ Select · Enter Use · Esc Dismiss",
    ),
    (
        "运行窗口初始化失败，请重试。详情见日志。",
        "Cannot initialize Run. Please retry; details are in the log.",
    ),
    ("读取", "read"),
    ("备份", "backup"),
    ("写入临时文件", "temporary write"),
    (
        "检查外部修改并替换",
        "external change check and replacement",
    ),
    (
        "无法确定设置文件路径",
        "Cannot determine the settings file path",
    ),
    (
        "保存线程处理失败，请重试",
        "Settings writer failed; please retry",
    ),
    ("保存线程不可用", "Settings writer is unavailable"),
    (
        "文件已被外部修改，请重试",
        "The file was changed externally; please retry",
    ),
    ("设置", "Settings"),
    ("设置 — minicon-winmgr", "Settings — minicon-winmgr"),
    ("外观与位置", "Appearance"),
    ("外观与日常行为", "Appearance and everyday behavior"),
    ("任务与窗口", "Tasks & windows"),
    ("语言", "Language"),
    ("颜色主题", "Theme"),
    ("任务栏位置", "Position"),
    ("白天", "Light"),
    ("黑夜", "Dark"),
    ("自动", "Auto"),
    ("上方", "Top"),
    ("下方", "Bottom"),
    ("左侧", "Left"),
    ("右侧", "Right"),
    ("显示任务栏", "Show taskbar"),
    ("显示通知区域", "Show notification area"),
    ("合并窗口", "Combine"),
    ("窗口合并", "Window grouping"),
    ("合并依据", "Group by"),
    ("从不合并", "Never combine"),
    ("任务栏放不下时合并", "When the taskbar is crowded"),
    ("拥挤时合并", "When crowded"),
    ("始终合并", "Always combine"),
    ("按应用", "Application"),
    ("按进程", "Process"),
    ("按应用（可执行文件）", "Application (executable)"),
    ("按进程（pid）", "Process (PID)"),
    ("悬停显示窗口缩略图", "Show window previews on hover"),
    ("悬停预览", "Window previews"),
    ("配置与关于…", "Configuration & about…"),
    (
        "配置与关于 — minicon-winmgr",
        "Configuration & about — minicon-winmgr",
    ),
    ("展开桌面管理…", "Desktop management…"),
    ("收起桌面管理", "Hide desktop management"),
    (
        "更改 Windows 桌面外壳，操作前会再次确认。",
        "Change the Windows desktop shell. Confirmation is required.",
    ),
    ("接管桌面…", "Take over desktop…"),
    ("还原系统桌面…", "Restore desktop…"),
    ("关闭", "Close"),
    ("取消", "Cancel"),
    ("查看详情", "Details"),
    ("重试保存", "Retry save"),
    ("更改自动保存", "Changes save automatically"),
    ("已保存", "Saved"),
    ("开启", "On"),
    ("本次无任务栏", "No taskbar in this session"),
    (
        "已保存，任务栏显示设置重启后生效。",
        "Saved. Taskbar visibility changes after restart.",
    ),
    (
        "本次修改已保留，正在保存…",
        "Applied to this session. Saving…",
    ),
    (
        "重启后隐藏；本次仍显示任务栏",
        "Hidden after restart; visible in this session.",
    ),
    (
        "重启后显示；本次未显示任务栏",
        "Shown after restart; hidden in this session.",
    ),
    ("设置尚未保存", "Unsaved settings"),
    (
        "仍有设置尚未保存。\n选择“是”放弃未保存修改并退出；选择“否”返回设置重试。",
        "Some settings have not been saved.\nChoose Yes to discard unsaved changes and exit, or No to return and retry.",
    ),
    ("任务栏显示（下次启动）", "Taskbar visibility (next start)"),
    ("通知区域", "Notification area"),
    ("设置文件", "Settings file"),
    ("内置默认", "Built-in default"),
    ("运行", "Run"),
    ("打开(&O)：", "&Open:"),
    (
        "输入程序、文件夹、文档或网址",
        "Enter an application, folder, document, or URL",
    ),
    ("清空历史", "Clear history"),
    ("历史已清空", "History cleared"),
    ("最近", "Recent"),
    ("路径", "Path"),
    ("路径建议暂不可用", "Path suggestions are unavailable"),
    ("正在打开…", "Opening…"),
    ("正在清空历史…", "Clearing history…"),
    (
        "历史保存失败，本次仍可使用，重启后可能丢失。",
        "History could not be saved. It remains available for this session.",
    ),
    (
        "本次历史已清空，磁盘历史清除失败，重启后可能恢复。请再次清空重试。",
        "Session history cleared, but disk history could not be removed. Retry to keep it from returning after restart.",
    ),
    (
        "启动服务初始化失败，请重启后重试。",
        "The launch service could not start. Restart and try again.",
    ),
    ("上页", "Previous"),
    ("下页", "Next"),
    (
        "应用图标由 Windows 通知区域管理",
        "Application icons are managed by Windows.",
    ),
    ("应用通知图标", "Application notification icon"),
    ("无法打开系统设置", "Unable to open system settings"),
    (
        "无法打开系统设置，请重试。",
        "Unable to open system settings. Please try again.",
    ),
    ("时间设置", "Date & time"),
    ("暂无应用通知图标", "No application notification icons"),
    ("网络设置", "Network settings"),
    ("音量设置", "Sound settings"),
    ("从快速启动移除", "Unpin"),
    ("添加到快速启动", "Pin to quick launch"),
    ("全部关闭", "Close all"),
    ("全部最小化", "Minimize all"),
    ("全部还原", "Restore all"),
    ("打开组内全部窗口", "Open all windows in group"),
    ("最大化", "Maximize"),
    ("最小化", "Minimize"),
    ("还原", "Restore"),
    ("下载", "Downloads"),
    ("图片", "Pictures"),
    ("文档", "Documents"),
    ("打开此文件夹", "Open this folder"),
    ("计算机", "This PC"),
    ("关机", "Shut down"),
    ("注销", "Sign out"),
    ("重启", "Restart"),
    ("电源确认", "Confirm power action"),
    (
        "请输入程序、路径或网址。",
        "Enter an application, path, or URL.",
    ),
    (
        "输入包含无效字符，请修改后重试。",
        "The input contains invalid characters. Edit it and try again.",
    ),
    (
        "引号未闭合，请补全目标路径的引号。",
        "Close the quotation marks around the target path.",
    ),
    (
        "目标与参数之间需要空格。",
        "Add a space between the target and its arguments.",
    ),
    (
        "用户目录不可用，无法打开相对路径。请使用完整路径。",
        "The user directory is unavailable. Use an absolute path.",
    ),
    (
        "盘符相对路径没有用户目录基准，请使用完整路径（例如 C:\\目录\\文件）。",
        "A drive-relative path has no user-directory base. Use an absolute path, such as C:\\folder\\file.",
    ),
    (
        "找不到目标。请检查名称或路径后重试。",
        "Target not found. Check the name or path and try again.",
    ),
    (
        "路径不可用。请检查目录或设备连接。",
        "The path is unavailable. Check the folder or device connection.",
    ),
    (
        "权限不足。请检查目标的访问权限。",
        "Access denied. Check your permissions for the target.",
    ),
    (
        "打开操作已取消。可以修改输入后重试。",
        "Opening was cancelled. Edit the input and try again.",
    ),
    (
        "目标不是可运行的程序，或与当前系统不兼容。请检查文件。",
        "This file is not an executable or is incompatible with this system.",
    ),
    (
        "文件存在，但默认打开程序不可用。请在文件路径前指定程序（例如 notepad），或修复系统默认打开方式。",
        "The file exists, but its default application is unavailable. Specify an application (for example, notepad) before the path, or repair the file association.",
    ),
    (
        "没有默认打开方式。请为此文件设置关联应用。",
        "No default application is configured. Set a file association.",
    ),
    (
        "关联的应用程序不可用。请指定程序或修复系统默认打开方式。",
        "The associated application is unavailable. Specify an application or repair the file association.",
    ),
    (
        "目标或所需组件不可用。请关闭占用程序或修复应用后重试。",
        "The target or a required component is unavailable. Close blocking applications or repair it and try again.",
    ),
    (
        "无法与关联应用通信。请关闭其阻塞窗口后重试。",
        "Cannot communicate with the associated application. Close its blocking dialogs and try again.",
    ),
    (
        "关联应用响应超时。请确认应用可用后重试。",
        "The associated application timed out. Check it and try again.",
    ),
    (
        " 请检查未定义的环境变量。",
        " Check any undefined environment variables.",
    ),
    ("普通权限", "Standard user"),
    ("管理员权限", "Administrator"),
    ("权限状态未知", "Unknown privilege status"),
    ("无法以普通权限启动", "Unable to start as a standard user"),
    (
        "读取启动令牌信息失败：{e}",
        "Cannot read startup token information: {e}",
    ),
    (
        "读取启动令牌失败：{e}",
        "Cannot read the startup token: {e}",
    ),
    (
        "无法读取启动令牌信息长度",
        "Cannot read the startup token information size",
    ),
    ("进程完整性级别无效", "Invalid process integrity level"),
    (
        "系统未能生成普通权限令牌；请从普通用户会话启动。",
        "Windows could not create a standard-user token. Start from a standard-user session.",
    ),
    (
        "无法继续普通权限进程，已终止该子进程。",
        "The standard-user process could not be resumed and was terminated.",
    ),
    (
        "普通权限进程未能在 15 秒内完成启动。请从普通用户会话重试。",
        "The standard-user process did not become ready within 15 seconds. Retry from a standard-user session.",
    ),
    (
        "普通权限令牌不属于当前用户会话",
        "The standard-user token does not belong to the current user and session",
    ),
    (
        "重新启动的权限或用户会话不符合要求，已终止该子进程。",
        "The relaunched process failed privilege or identity checks and was terminated.",
    ),
    ("接管桌面", "Take over desktop"),
    ("还原系统桌面", "Restore desktop"),
    (
        "当前已经是 minicon 在充当桌面外壳，无需接管。",
        "Minicon is already the desktop shell.",
    ),
    (
        "已接管桌面（立即生效）。\n",
        "Desktop takeover completed.\n",
    ),
    (
        "本会话没有正在运行的桌面外壳，无需关闭。\n",
        "No desktop shell was running in this session.\n",
    ),
    (
        "本会话已有桌面外壳在运行。",
        "A desktop shell is already running in this session.",
    ),
    (
        "\n可用「还原系统桌面（立即生效）」一键恢复。",
        "\nUse Restore desktop to restore the previous shell.",
    ),
    ("已还原系统桌面（立即生效）。\n", "Desktop restored.\n"),
    (
        "已还原系统桌面设置。\n",
        "Desktop shell settings restored.\n",
    ),
    ("已重新启动 explorer。", "Explorer restarted."),
    (
        "下次登录将由 explorer 接管桌面。",
        "Explorer will be the desktop shell at the next sign-in.",
    ),
    (
        "当前正是本程序在充当桌面外壳，切换将在下次登录时完成。",
        "Minicon is the current shell. The change takes effect at the next sign-in.",
    ),
    ("将立即重新启动 explorer。", "Explorer will restart now."),
    (
        "\n若桌面外观未恢复，注销重登即可。",
        "\nIf the desktop has not recovered, sign out and back in.",
    ),
    (
        "没有记录到接管前的桌面外壳（本程序未曾接管过桌面），无法还原。",
        "No previous desktop shell was recorded, so restoration is unavailable.",
    ),
    (
        "无法创建设置窗口：{error}",
        "Unable to create the settings window: {error}",
    ),
    ("无法开启预览：{e}", "Unable to enable previews: {e}"),
    (
        "无法打开配置详情：{error}",
        "Unable to open configuration details: {error}",
    ),
    (
        "minicon-winmgr {} · {}\n\n任务栏唤出配置：{}（实际注册结果见日志）\n\n配置来源（启动时）\n",
        "minicon-winmgr {} · {}\n\nTaskbar hotkey: {} (registration results are recorded in the log)\n\nConfiguration sources at startup\n",
    ),
    (
        "{title}：{value}\n来源：{}\n\n",
        "{title}: {value}\nSource: {}\n\n",
    ),
    ("设置保存位置：{}", "Settings file: {}"),
    (
        "本次修改已保留，{} 项保存失败；下次启动尚未保存。请重试。",
        "Applied to this session. {} setting(s) could not be saved for next start. Please retry.",
    ),
    (
        "由环境变量 {name} 控制",
        "Controlled by environment variable {name}",
    ),
    ("此项由启动配置固定", "Fixed by startup configuration"),
    ("启动来源：{}", "Startup source: {}"),
    (
        "启动失败（错误码 {code}），请修改输入后重试。",
        "Launch failed (error {code}). Edit the input and try again.",
    ),
    ("打开 {label}", "Open {label}"),
    ("确定要{}吗？", "Are you sure you want to {}?"),
    (
        "已关闭 {} 个桌面外壳进程。\n",
        "Stopped {} desktop shell process(es).\n",
    ),
    ("接管桌面失败：{err}", "Desktop takeover failed: {err}"),
    ("还原失败：{err}", "Desktop restoration failed: {err}"),
    (
        "还原系统桌面：把外壳还原为\n{v}\n{tail}\n\n确定？",
        "Restore the desktop shell to:\n{v}\n{tail}\n\nContinue?",
    ),
    (
        "还原系统桌面：把外壳还原为系统默认（explorer）\n{tail}\n\n确定？",
        "Restore the default desktop shell (Explorer).\n{tail}\n\nContinue?",
    ),
    (
        "无法以普通权限重新启动：{first}；备用方式：{second}。请从普通用户会话启动。",
        "Unable to relaunch as a standard user: {first}; fallback: {second}. Start from a standard-user session.",
    ),
    (
        "无法检查普通权限进程：{e}",
        "Cannot inspect the standard-user process: {e}",
    ),
    (
        "无法检查进程权限：{e}",
        "Cannot inspect process privileges: {e}",
    ),
    (
        "无法生成普通完整性级别：{e}",
        "Cannot create a medium-integrity level: {e}",
    ),
    (
        "无法生成普通权限令牌：{e}",
        "Cannot create a standard-user token: {e}",
    ),
    (
        "无法读取启动环境：{}",
        "Cannot read the startup environment: {}",
    ),
    (
        "无法降低进程完整性级别：{e}",
        "Cannot lower the process integrity level: {e}",
    ),
    (
        "普通权限进程启动失败（退出码 {code}）",
        "The standard-user process failed to start (exit code {code})",
    ),
];
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn language_defaults_to_english_and_accepts_supported_aliases() {
        assert_eq!(Language::default(), Language::En);
        assert_eq!(Language::parse("ZH-cn"), Some(Language::Zh));
        assert_eq!(Language::parse("auto"), None);
    }
    #[test]
    fn catalog_round_trips_ui_and_preserves_external_text() {
        for &(zh, en) in CATALOG {
            assert_eq!(render(Language::En, zh), en);
            assert_eq!(render(Language::Zh, en), zh);
        }
        for input in [
            "D:\\用户\\models.json",
            "%USERPROFILE%",
            "minicon_winmgr_settings",
            "taskbar.position",
        ] {
            assert_eq!(render(Language::En, input), input);
        }
    }
}
