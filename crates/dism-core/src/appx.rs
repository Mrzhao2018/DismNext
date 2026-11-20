use crate::catalog::LocalizedText;
use crate::evaluator::reg_value_to_string;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::Path;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
use winreg::RegKey;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PACKAGES_SUBKEY: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppxCategory {
    Bloatware,
    UserApp,
    Framework,
    SystemCore,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppxPackageInfo {
    pub package_full_name: String,
    pub name: String,
    pub display_name: LocalizedText,
    pub description: LocalizedText,
    pub version: String,
    pub arch: String,
    pub publisher_id: String,
    pub install_location: String,
    pub category: AppxCategory,
    pub protected: bool,
}

/// Enumerates installed AppX/MSIX packages directly from the AppModel registry store (~10-20ms).
pub fn list_appx_packages() -> Vec<AppxPackageInfo> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(root) = hkcu.open_subkey_with_flags(PACKAGES_SUBKEY, KEY_READ) else {
        return Vec::new();
    };

    let mut by_key: HashMap<(String, String), (Vec<u32>, AppxPackageInfo)> = HashMap::new();
    for full_name in root.enum_keys().flatten() {
        let Ok(sk) = root.open_subkey_with_flags(&full_name, KEY_READ) else {
            continue;
        };
        let raw_display = sk
            .get_raw_value("DisplayName")
            .ok()
            .map(|rv| reg_value_to_string(&rv))
            .unwrap_or_default();
        let install_location = sk
            .get_raw_value("PackageRootFolder")
            .ok()
            .map(|rv| reg_value_to_string(&rv))
            .unwrap_or_default();

        if install_location.is_empty() || !Path::new(&install_location).exists() {
            continue;
        }

        let (name, version, arch, publisher_id) = parse_package_full_name(&full_name);
        if name.is_empty() {
            continue;
        }
        let category = classify_appx(&name, &install_location);
        let protected = matches!(category, AppxCategory::SystemCore | AppxCategory::Framework);
        let (display_name, description) = resolve_friendly_meta(&name, &raw_display, category);

        let ver_parts: Vec<u32> = version
            .split('.')
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect();
        let dedup_key = (name.to_ascii_lowercase(), arch.to_ascii_lowercase());
        let info = AppxPackageInfo {
            package_full_name: full_name,
            name,
            display_name,
            description,
            version,
            arch,
            publisher_id,
            install_location,
            category,
            protected,
        };

        let replace = match by_key.get(&dedup_key) {
            None => true,
            Some((prev_ver, _)) => ver_parts > *prev_ver,
        };
        if replace {
            by_key.insert(dedup_key, (ver_parts, info));
        }
    }

    let mut pkgs: Vec<AppxPackageInfo> = by_key.into_values().map(|(_, p)| p).collect();

    pkgs.sort_by(|a, b| {
        cat_order(a.category)
            .cmp(&cat_order(b.category))
            .then_with(|| {
                a.display_name
                    .zh
                    .to_ascii_lowercase()
                    .cmp(&b.display_name.zh.to_ascii_lowercase())
            })
    });
    pkgs
}

/// Safely removes a non-protected AppX/MSIX package for the current user via PowerShell `Remove-AppxPackage`.
pub fn remove_appx_package(package_full_name: &str) -> Result<()> {
    let trimmed = package_full_name.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Empty package_full_name"));
    }
    // Validate characters to guard against shell injection
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~'))
    {
        return Err(anyhow!("Invalid characters in package name '{}'", trimmed));
    }

    let (name, _, _, _) = parse_package_full_name(trimmed);
    let pkgs = list_appx_packages();
    if let Some(found) = pkgs.iter().find(|p| p.package_full_name == trimmed) {
        if found.protected {
            return Err(anyhow!(
                "Package '{}' ({}) is marked as protected system/framework component",
                found.display_name.zh,
                trimmed
            ));
        }
    } else if matches!(
        classify_appx(&name, ""),
        AppxCategory::SystemCore | AppxCategory::Framework
    ) {
        return Err(anyhow!(
            "Package '{}' is a protected system/framework component",
            trimmed
        ));
    }

    let ps_cmd = format!(
        "Remove-AppxPackage -Package '{}' -ErrorAction Stop",
        trimmed
    );
    let out = crate::system_tools::powershell_command()?
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;

    if !out.status.success() {
        let stderr = crate::systasks::decode_cmd_output(&out.stderr);
        let stdout = crate::systasks::decode_cmd_output(&out.stdout);
        let (stderr, stdout) = (stderr.trim(), stdout.trim());
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(anyhow!("Remove-AppxPackage failed: {}", detail));
    }
    Ok(())
}

fn cat_order(c: AppxCategory) -> u8 {
    match c {
        AppxCategory::Bloatware => 0,
        AppxCategory::UserApp => 1,
        AppxCategory::Framework => 2,
        AppxCategory::SystemCore => 3,
    }
}

pub fn parse_package_full_name(full: &str) -> (String, String, String, String) {
    let parts: Vec<&str> = full.split('_').collect();
    if parts.len() >= 5 {
        (
            parts[0].to_string(),
            parts[1].to_string(),
            parts[2].to_string(),
            parts[parts.len() - 1].to_string(),
        )
    } else if parts.len() >= 2 {
        (
            parts[0].to_string(),
            parts[1].to_string(),
            String::new(),
            parts.last().copied().unwrap_or("").to_string(),
        )
    } else {
        (
            full.to_string(),
            String::new(),
            String::new(),
            String::new(),
        )
    }
}

pub fn classify_appx(name: &str, install_location: &str) -> AppxCategory {
    let n = name.to_ascii_lowercase();
    let loc = install_location.to_ascii_lowercase();

    if n.starts_with("microsoft.vclibs")
        || n.starts_with("microsoft.net.native")
        || n.starts_with("microsoft.ui.xaml")
        || n.starts_with("microsoft.windowsappruntime")
        || n.starts_with("microsoft.services.store.engagement")
        || n.contains("directxruntime")
    {
        return AppxCategory::Framework;
    }

    if loc.contains(r"\windows\systemapps\")
        || loc.contains(r"\windows\immersivecontrolpanel")
        || loc.contains(r"\windows\printdialog")
        || matches!(
            n.as_str(),
            "microsoft.windowsstore"
                | "microsoft.desktopappinstaller"
                | "microsoft.sechealthui"
                | "microsoft.windowsterminal"
                | "microsoft.storepurchaseapp"
                | "microsoft.aad.brokerplugin"
                | "microsoft.accountscontrol"
                | "microsoft.asynctextservice"
                | "microsoft.bioenrollment"
                | "microsoft.creddialoghost"
                | "microsoft.ecapp"
                | "microsoft.lockapp"
                | "microsoft.win32webviewhost"
                | "microsoft.windowscalculator"
                | "microsoft.windowsnotepad"
                | "microsoft.screensketch"
                | "microsoft.paint"
                | "microsoft.webmediaextensions"
                | "microsoft.vp9videoextensions"
                | "microsoft.heifimageextension"
                | "microsoft.hevcvideoextension"
                | "microsoft.av1videoextension"
                | "microsoft.rawimageextension"
                | "microsoft.webpimageextension"
                | "microsoft.mpeg2videoextension"
                | "windows.immersivecontrolpanel"
                | "windows.cbspreview"
                | "ncsiuwpapp"
        )
        || n.starts_with("microsoftwindows.")
        || n.starts_with("microsoft.windows.")
            && !matches!(
                n.as_str(),
                "microsoft.windows.devhome"
                    | "microsoft.windows.photos"
                    | "microsoft.windows.ai.copilot.provider"
            )
    {
        return AppxCategory::SystemCore;
    }

    const BLOATWARE_NAMES: &[&str] = &[
        "clipchamp.clipchamp",
        "microsoft.bingnews",
        "microsoft.bingweather",
        "microsoft.bingsearch",
        "microsoft.bingfinance",
        "microsoft.bingsports",
        "microsoft.gethelp",
        "microsoft.getstarted",
        "microsoft.microsoftofficehub",
        "microsoft.microsoftsolitairecollection",
        "microsoft.people",
        "microsoft.powerautomatedesktop",
        "microsoft.todos",
        "microsoft.windowsfeedbackhub",
        "microsoft.windowsmaps",
        "microsoft.yourphone",
        "microsoft.zunemusic",
        "microsoft.zunevideo",
        "microsoft.gamingapp",
        "microsoft.xboxapp",
        "microsoft.xboxgamingoverlay",
        "microsoft.xboxgameoverlay",
        "microsoft.xboxidentityprovider",
        "microsoft.xboxspeechtotextoverlay",
        "microsoft.xbox.tcui",
        "microsoft.549981c3f5f10",
        "microsoftteams",
        "msteams",
        "microsoft.outlookforwindows",
        "microsoft.windows.devhome",
        "microsoft.windowscommunicationsapps",
        "microsoft.mixedreality.portal",
        "microsoft.microsoft3dviewer",
        "microsoft.3dbuilder",
        "microsoft.skypeapp",
        "microsoft.oneconnect",
        "microsoft.wallet",
        "microsoft.messaging",
        "microsoft.windowsalarms",
        "microsoft.windowssoundrecorder",
        "microsoft.windowscamera",
        "microsoftcorporationii.quickassist",
        "microsoft.microsoftstickynotes",
        "microsoft.copilot",
        "microsoft.windows.ai.copilot.provider",
    ];

    if BLOATWARE_NAMES.iter().any(|&b| b == n) {
        return AppxCategory::Bloatware;
    }

    AppxCategory::UserApp
}

fn resolve_friendly_meta(
    name: &str,
    raw_display: &str,
    cat: AppxCategory,
) -> (LocalizedText, LocalizedText) {
    let known = match name.to_ascii_lowercase().as_str() {
        "clipchamp.clipchamp" => Some((
            "Clipchamp 视频编辑器",
            "Microsoft Clipchamp",
            "微软预装视频剪辑应用",
            "Preinstalled video editor",
        )),
        "microsoft.bingnews" => Some((
            "Microsoft 资讯",
            "Microsoft News",
            "微软新闻资讯聚合",
            "Bing News aggregator",
        )),
        "microsoft.bingweather" => Some((
            "MSN 天气",
            "MSN Weather",
            "预装天气应用",
            "Preinstalled weather app",
        )),
        "microsoft.bingsearch" => Some((
            "Bing 搜索组件",
            "Microsoft Bing Search",
            "系统内嵌 Bing 搜索组件",
            "Integrated Bing Search component",
        )),
        "microsoft.gethelp" => Some((
            "获取帮助",
            "Get Help",
            "微软在线客服与排障助手",
            "Microsoft support assistant",
        )),
        "microsoft.getstarted" => Some((
            "入门提示 (Tips)",
            "Microsoft Tips",
            "Windows 新手引导应用",
            "Windows getting started tips",
        )),
        "microsoft.microsoftofficehub" => Some((
            "Microsoft 365 (Office) 入口",
            "Microsoft 365 (Office)",
            "Office 在线推广与启动器",
            "Microsoft 365 hub launcher",
        )),
        "microsoft.microsoftsolitairecollection" => Some((
            "微软纸牌合集",
            "Microsoft Solitaire Collection",
            "预装休闲纸牌游戏",
            "Preinstalled solitaire game",
        )),
        "microsoft.people" => Some((
            "人脉 (People)",
            "Microsoft People",
            "已停止主推的联系人应用",
            "Legacy contacts app",
        )),
        "microsoft.powerautomatedesktop" => Some((
            "Power Automate 桌面版",
            "Power Automate Desktop",
            "预装 RPA 自动化工具",
            "Preinstalled automation tool",
        )),
        "microsoft.todos" => Some((
            "Microsoft To Do",
            "Microsoft To Do",
            "微软待办事项应用",
            "To Do task manager",
        )),
        "microsoft.windowsfeedbackhub" => Some((
            "反馈中心",
            "Feedback Hub",
            "Windows Insider 反馈工具",
            "Windows Feedback Hub",
        )),
        "microsoft.windowsmaps" => Some((
            "Windows 地图",
            "Windows Maps",
            "离线/在线地图应用",
            "Preinstalled maps app",
        )),
        "microsoft.yourphone" => Some((
            "手机连接 (Phone Link)",
            "Phone Link",
            "手机与电脑互联同步应用",
            "Android/iOS sync companion",
        )),
        "microsoft.zunemusic" => Some((
            "媒体播放器 (Media Player)",
            "Windows Media Player",
            "音乐与媒体播放器",
            "Preinstalled media player",
        )),
        "microsoft.zunevideo" => Some((
            "电影和电视",
            "Movies & TV",
            "视频播放与商店入口",
            "Preinstalled video app",
        )),
        "microsoft.gamingapp" => Some((
            "Xbox 主应用",
            "Xbox App",
            "Xbox Game Pass 游戏中心",
            "Xbox gaming launcher",
        )),
        "microsoft.xboxgamingoverlay" | "microsoft.xboxgameoverlay" => Some((
            "Xbox Game Bar 覆盖层",
            "Xbox Game Bar",
            "游戏录屏与性能浮窗",
            "Gaming overlay & capture",
        )),
        "microsoft.xboxidentityprovider"
        | "microsoft.xboxspeechtotextoverlay"
        | "microsoft.xbox.tcui" => Some((
            "Xbox 平台服务组件",
            "Xbox Platform Component",
            "Xbox 登录与界面依赖组件",
            "Xbox sign-in/UI component",
        )),
        "microsoft.549981c3f5f10" => Some((
            "Cortana (小娜)",
            "Cortana",
            "已退役的旧版语音助手",
            "Deprecated Cortana assistant",
        )),
        "microsoftteams" | "msteams" => Some((
            "Microsoft Teams (个人版)",
            "Microsoft Teams",
            "预装聊天与视频会议工具",
            "Preinstalled chat client",
        )),
        "microsoft.outlookforwindows" => Some((
            "新版 Outlook",
            "Outlook for Windows",
            "基于 Web 的新版邮件客户端",
            "New web-based Outlook client",
        )),
        "microsoft.windows.devhome" => Some((
            "Dev Home (开发者主页)",
            "Dev Home",
            "开发者仪表盘与配置工具",
            "Developer dashboard app",
        )),
        "microsoft.windowscommunicationsapps" => Some((
            "邮件和日历 (旧版)",
            "Mail and Calendar",
            "旧版 UWP 邮件与日历",
            "Legacy UWP Mail & Calendar",
        )),
        "microsoft.mixedreality.portal" => Some((
            "混合现实门户",
            "Mixed Reality Portal",
            "WMR 头显管理（微软已停更）",
            "Windows Mixed Reality portal",
        )),
        "microsoft.microsoft3dviewer" | "microsoft.3dbuilder" => Some((
            "3D 查看器 / 建模",
            "3D Viewer",
            "预装 3D 模型查看器",
            "Preinstalled 3D viewer",
        )),
        "microsoftcorporationii.quickassist" => Some((
            "快速助手",
            "Quick Assist",
            "远程桌面协助工具",
            "Remote assistance tool",
        )),
        "microsoft.microsoftstickynotes" => Some((
            "便笺 (Sticky Notes)",
            "Sticky Notes",
            "桌面便签工具",
            "Desktop sticky notes",
        )),
        "microsoft.copilot" | "microsoft.windows.ai.copilot.provider" => Some((
            "Microsoft Copilot",
            "Microsoft Copilot",
            "预装 AI 助手组件",
            "Preinstalled AI assistant",
        )),
        "microsoft.windowssoundrecorder" => Some((
            "录音机 (Sound Recorder)",
            "Windows Sound Recorder",
            "预装音频录制应用",
            "Preinstalled audio recorder",
        )),
        "microsoft.windowsalarms" => Some((
            "时钟 (Alarms & Clock)",
            "Windows Clock",
            "预装闹钟、计时器与世界时钟",
            "Preinstalled clock & alarms",
        )),
        "microsoft.windowscamera" => Some((
            "相机 (Windows Camera)",
            "Windows Camera",
            "预装摄像头拍照与录像应用",
            "Preinstalled camera app",
        )),
        "microsoft.windowsstore" => Some((
            "Microsoft Store (应用商店)",
            "Microsoft Store",
            "核心应用商店（受保护）",
            "Core Windows Store (Protected)",
        )),
        "microsoft.desktopappinstaller" => Some((
            "应用安装程序 (winget)",
            "App Installer (winget)",
            "MSIX 安装器与 winget CLI（受保护）",
            "Core MSIX & winget installer",
        )),
        "microsoft.sechealthui" => Some((
            "Windows 安全中心",
            "Windows Security",
            "系统防病毒与安全界面（受保护）",
            "Core Windows Security UI",
        )),
        "microsoft.windowsterminal" => Some((
            "Windows 终端",
            "Windows Terminal",
            "现代命令行终端（受保护）",
            "Core Windows Terminal",
        )),
        "microsoft.windowscalculator" => Some((
            "计算器",
            "Windows Calculator",
            "系统基础工具（受保护）",
            "System Calculator",
        )),
        "microsoft.windowsnotepad" => Some((
            "记事本",
            "Windows Notepad",
            "系统基础工具（受保护）",
            "System Notepad",
        )),
        "microsoft.paint" => Some((
            "画图",
            "Microsoft Paint",
            "系统基础工具（受保护）",
            "System Paint",
        )),
        "microsoft.screensketch" => Some((
            "截图工具",
            "Snipping Tool",
            "系统截图与录屏工具（受保护）",
            "System Snipping Tool",
        )),
        _ => None,
    };

    if let Some((zh, en, dzh, den)) = known {
        return (
            LocalizedText {
                zh: zh.to_string(),
                en: en.to_string(),
            },
            LocalizedText {
                zh: dzh.to_string(),
                en: den.to_string(),
            },
        );
    }

    let clean_name = if !raw_display.is_empty()
        && !raw_display.starts_with("@{")
        && !raw_display.to_ascii_lowercase().starts_with("ms-resource:")
    {
        raw_display.to_string()
    } else {
        name.split('.').next_back().unwrap_or(name).to_string()
    };

    let (dzh, den) = match cat {
        AppxCategory::Bloatware => ("可安全精简的预装组件", "Optional preinstalled component"),
        AppxCategory::UserApp => ("用户安装或第三方现代应用", "User / Store application"),
        AppxCategory::Framework => (
            "应用运行时依赖框架（建议保留）",
            "Shared runtime framework (Protected)",
        ),
        AppxCategory::SystemCore => (
            "Windows 系统核心体验组件（受保护）",
            "Core Windows system component (Protected)",
        ),
    };

    (
        LocalizedText {
            zh: clean_name.clone(),
            en: clean_name,
        },
        LocalizedText {
            zh: dzh.to_string(),
            en: den.to_string(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_appx_parsing_and_classification() {
        let (name, ver, arch, pub_id) =
            parse_package_full_name("Clipchamp.Clipchamp_4.7.10240.0_x64__yxz26nhyzhsrt");
        assert_eq!(name, "Clipchamp.Clipchamp");
        assert_eq!(ver, "4.7.10240.0");
        assert_eq!(arch, "x64");
        assert_eq!(pub_id, "yxz26nhyzhsrt");
        assert_eq!(
            classify_appx(&name, r"C:\Program Files\WindowsApps\Clipchamp"),
            AppxCategory::Bloatware
        );
        assert_eq!(
            classify_appx(
                "Microsoft.WindowsStore",
                r"C:\Program Files\WindowsApps\Store"
            ),
            AppxCategory::SystemCore
        );
        assert_eq!(
            classify_appx(
                "Microsoft.VCLibs.140.00",
                r"C:\Program Files\WindowsApps\VCLibs"
            ),
            AppxCategory::Framework
        );
    }
}
