use crate::catalog::LocalizedText;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::process::Command;
use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};
use winreg::RegKey;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CBS_FEATURES_KEY: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\Notifications\OptionalFeatures";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureCategory {
    Virtualization,
    Developer,
    SecurityAi,
    NetworkSmb,
    PrintMedia,
    IisWeb,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowsFeatureInfo {
    pub name: String,
    pub display_name: LocalizedText,
    pub description: LocalizedText,
    pub category: FeatureCategory,
    pub enabled: bool,
    pub high_impact: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureToggleOutcome {
    pub name: String,
    pub enabled: bool,
    pub restart_required: bool,
    pub output: String,
}

/// Lists Windows Optional Features directly from CBS OptionalFeatures registry (~5ms, works non-elevated).
pub fn list_optional_features() -> Vec<WindowsFeatureInfo> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let Ok(root) = hklm.open_subkey_with_flags(CBS_FEATURES_KEY, KEY_READ) else {
        return Vec::new();
    };

    let mut features = Vec::new();
    for name in root.enum_keys().flatten() {
        let Ok(sk) = root.open_subkey_with_flags(&name, KEY_READ) else {
            continue;
        };
        let selection: u32 = sk.get_value("Selection").unwrap_or(0);
        let enabled = selection != 0;
        let (display_name, description, category, high_impact, priority) = feature_meta(&name);
        features.push((
            priority,
            WindowsFeatureInfo {
                name,
                display_name,
                description,
                category,
                enabled,
                high_impact,
            },
        ));
    }

    features.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| cat_order(a.1.category).cmp(&cat_order(b.1.category)))
            .then_with(|| b.1.enabled.cmp(&a.1.enabled))
            .then_with(|| a.1.name.to_ascii_lowercase().cmp(&b.1.name.to_ascii_lowercase()))
    });

    features.into_iter().map(|(_, f)| f).collect()
}

/// Enables or disables a Windows Optional Feature via DISM (`dism.exe /Online`).
/// Requires Administrator privileges.
pub fn set_feature_enabled(feature_name: &str, enabled: bool) -> Result<FeatureToggleOutcome> {
    let trimmed = feature_name.trim();
    if trimmed.is_empty()
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(anyhow!("Invalid Windows feature name: '{}'", trimmed));
    }

    if !crate::sysinfo::SystemInfo::collect().is_admin {
        return Err(anyhow!(
            "Changing Windows Optional Features requires Administrator privileges. Please click '一键提权为管理员' first."
        ));
    }

    let feature_arg = format!("/FeatureName:{}", trimmed);
    let mut args = vec!["/Online", "/NoRestart"];
    if enabled {
        args.push("/Enable-Feature");
        args.push(&feature_arg);
        args.push("/All");
    } else {
        args.push("/Disable-Feature");
        args.push(&feature_arg);
    }

    let output = Command::new("dism.exe")
        .creation_flags(CREATE_NO_WINDOW)
        .args(&args)
        .output()
        .context("Failed to execute dism.exe")?;

    let stdout = crate::systasks::decode_cmd_output(&output.stdout);
    let stderr = crate::systasks::decode_cmd_output(&output.stderr);
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim()).trim().to_string();
    let code = output.status.code().unwrap_or(-1);

    // DISM returns 0 (success) or 3010 (ERROR_SUCCESS_REBOOT_REQUIRED)
    if code != 0 && code != 3010 {
        return Err(anyhow!(
            "dism.exe exited with code {}: {}",
            code,
            combined
        ));
    }

    Ok(FeatureToggleOutcome {
        name: trimmed.to_string(),
        enabled,
        restart_required: code == 3010,
        output: combined,
    })
}

fn cat_order(c: FeatureCategory) -> u8 {
    match c {
        FeatureCategory::Virtualization => 0,
        FeatureCategory::Developer => 1,
        FeatureCategory::SecurityAi => 2,
        FeatureCategory::NetworkSmb => 3,
        FeatureCategory::PrintMedia => 4,
        FeatureCategory::IisWeb => 5,
    }
}

fn lt(zh: &str, en: &str) -> LocalizedText {
    LocalizedText {
        zh: zh.to_string(),
        en: en.to_string(),
    }
}

fn feature_meta(name: &str) -> (LocalizedText, LocalizedText, FeatureCategory, bool, u8) {
    match name {
        "Microsoft-Windows-Subsystem-Linux" => (
            lt("适用于 Linux 的 Windows 子系统 (WSL)", "Windows Subsystem for Linux (WSL)"),
            lt("允许直接在 Windows 上原生运行 Linux 发行版与开发工具链。", "Provides native compatibility to run Linux distributions on Windows."),
            FeatureCategory::Virtualization,
            false,
            0,
        ),
        "VirtualMachinePlatform" => (
            lt("虚拟机平台 (Virtual Machine Platform)", "Virtual Machine Platform"),
            lt("为 WSL 2、Docker Desktop 与安卓子系统提供轻量级虚拟化平台支持。", "Enables lightweight hypervisor platform support required by WSL 2 and containers."),
            FeatureCategory::Virtualization,
            false,
            0,
        ),
        "HypervisorPlatform" => (
            lt("Windows 虚拟机监控程序平台", "Windows Hypervisor Platform"),
            lt("允许 QEMU、Android Emulator 等第三方虚拟化软件调用 Windows Hypervisor。", "Allows third-party virtualization apps (QEMU, Android Emulator) to use Windows Hypervisor."),
            FeatureCategory::Virtualization,
            false,
            0,
        ),
        "Containers-DisposableClientVM" => (
            lt("Windows 沙盒 (Windows Sandbox)", "Windows Sandbox"),
            lt("创建轻量级一次性隔离桌面环境，安全测试未知程序，关闭后自动销毁。", "Provides a lightweight isolated desktop environment to safely run untrusted software."),
            FeatureCategory::Virtualization,
            false,
            0,
        ),
        "Containers" => (
            lt("Windows 容器 (Containers)", "Windows Containers"),
            lt("提供创建和管理 Windows 容器所需的核心服务与命名空间隔离。", "Provides services and isolation support for native Windows Containers."),
            FeatureCategory::Virtualization,
            false,
            0,
        ),
        "Client-ProjFS" => (
            lt("Windows 投影文件系统 (ProjFS)", "Windows Projected File System"),
            lt("允许用户态文件系统提供商将虚拟树（如大型 VFS 仓库）投影为本地文件。", "Enables user-mode providers such as VFS for Git to project virtual files."),
            FeatureCategory::Developer,
            false,
            1,
        ),
        "NetFx3" => (
            lt(".NET Framework 3.5 (含 2.0/3.0)", ".NET Framework 3.5 (includes 2.0 & 3.0)"),
            lt("大量经典专业软件、工控应用和老游戏运行所需的 .NET 3.5 运行环境。", "Runtime required by many classic desktop tools and legacy enterprise applications."),
            FeatureCategory::Developer,
            false,
            1,
        ),
        "NetFx4Extended-ASPNET45" => (
            lt(".NET Framework 4.8 高级服务 / ASP.NET 4.8", ".NET Framework 4.8 Advanced Services"),
            lt("提供 WCF 服务激活与 ASP.NET 4.8 运行支持。", "Provides ASP.NET 4.8 and WCF HTTP/TCP activation capabilities."),
            FeatureCategory::Developer,
            false,
            2,
        ),
        "TelnetClient" => (
            lt("Telnet 客户端", "Telnet Client"),
            lt("用于端口连通性排障与老旧网络设备远程命令行调试的经典工具。", "Classic CLI utility for TCP port diagnostics and legacy device connections."),
            FeatureCategory::Developer,
            false,
            1,
        ),
        "TFTP" => (
            lt("TFTP 客户端", "TFTP Client"),
            lt("用于路由器、嵌入式固件刷写与 PXE 网络传输的简单文件传输客户端。", "Trivial File Transfer Protocol utility commonly used in embedded & network workflows."),
            FeatureCategory::Developer,
            false,
            2,
        ),
        "SimpleTCP" => (
            lt("简单 TCPIP 服务", "Simple TCPIP Services"),
            lt("包含 Echo、Daytime、Chargen 等经典网络测试服务。", "Provides classic TCP/IP test responders (Echo, Daytime, Discard, Quote)."),
            FeatureCategory::Developer,
            false,
            3,
        ),
        "Sysmon" | "Sysmon-Service" => (
            lt("Windows 内置 Sysmon 遥测支持", "Windows Native Sysmon"),
            lt("Win11 新特性：原生系统监控工具与事件日志驱动。", "Win11 built-in System Monitor telemetry feature for security auditing."),
            FeatureCategory::SecurityAi,
            true,
            1,
        ),
        "Recall" => (
            lt("Windows Recall (回顾快照功能)", "Windows Recall"),
            lt("Win11 AI 屏幕定时截屏检索功能；非 Copilot+ 设备或隐私场景建议关闭。", "Win11 AI snapshot feature; consider keeping disabled on non-Copilot+ PCs or for privacy."),
            FeatureCategory::SecurityAi,
            true,
            0,
        ),
        "Windows-Defender-Default-Definitions" => (
            lt("Microsoft Defender 默认离线定义包", "Defender Default Definitions"),
            lt("随系统镜像预置的 Defender 基础病毒特征库模块。", "Built-in baseline security definitions component."),
            FeatureCategory::SecurityAi,
            true,
            3,
        ),
        "Windows-Identity-Foundation" => (
            lt("Windows Identity Foundation 3.5", "Windows Identity Foundation 3.5"),
            lt("部分旧版企业级 .NET 身份认证应用依赖的声明模型组件。", "Claims-based identity library required by older .NET applications."),
            FeatureCategory::SecurityAi,
            false,
            3,
        ),
        "SMB1Protocol" => (
            lt("SMB 1.0/CIFS 文件共享支持（总开关）", "SMB 1.0/CIFS File Sharing Support"),
            lt("安全风险提示：SMB1 存在勒索软件漏洞风险，除非连接老旧 NAS/打印机，否则强烈建议关闭。", "Security caution: Legacy SMB1 protocol has known vulnerabilities; disable unless older NAS requires it."),
            FeatureCategory::NetworkSmb,
            true,
            1,
        ),
        "SMB1Protocol-Client" => (
            lt("SMB 1.0/CIFS 客户端", "SMB 1.0/CIFS Client"),
            lt("仅用于访问仅支持 SMBv1 协议的老旧网络共享设备。", "Client component to connect to legacy SMBv1-only NAS shares."),
            FeatureCategory::NetworkSmb,
            true,
            2,
        ),
        "SMB1Protocol-Server" => (
            lt("SMB 1.0/CIFS 服务器", "SMB 1.0/CIFS Server"),
            lt("高风险老旧服务端组件，允许其他设备以 SMB1 访问本机共享，建议关闭。", "Legacy SMB1 server listener; recommended disabled for security."),
            FeatureCategory::NetworkSmb,
            true,
            2,
        ),
        "WorkFolders-Client" => (
            lt("工作文件夹客户端", "Work Folders Client"),
            lt("与企业文件服务器同步工作文档的 Windows 客户端功能。", "Synchronizes corporate work files with organization servers."),
            FeatureCategory::NetworkSmb,
            false,
            3,
        ),
        "Printing-XPSServices-Features" => (
            lt("Microsoft XPS 文档写入程序", "Microsoft XPS Document Writer"),
            lt("内置虚拟 XPS 打印机驱动，允许将文档输出为 .xps 格式。", "Virtual printer driver allowing applications to export documents as XPS."),
            FeatureCategory::PrintMedia,
            false,
            2,
        ),
        "Printing-Foundation-LPDPrintService" => (
            lt("LPD 打印服务", "LPD Print Service"),
            lt("接收来自 Linux/UNIX 系统的 Line Printer Daemon 打印任务。", "Allows Windows to receive print jobs from UNIX/Linux LPR clients."),
            FeatureCategory::PrintMedia,
            false,
            3,
        ),
        "Printing-Foundation-LPRPortMonitor" => (
            lt("LPR 端口监视器", "LPR Port Monitor"),
            lt("允许 Windows 向连接到 UNIX/网络打印服务器的打印机发送任务。", "Allows printing directly to LPR/LPD network print servers."),
            FeatureCategory::PrintMedia,
            false,
            3,
        ),
        "TIFFIFilter" => (
            lt("Windows TIFF IFilter (OCR 索引)", "Windows TIFF IFilter"),
            lt("对 TIFF 扫描图片中的文字执行 OCR 以便 Windows 搜索索引检索。", "Enables OCR indexing of text inside TIFF documents for Windows Search."),
            FeatureCategory::PrintMedia,
            false,
            3,
        ),
        "DirectPlay" | "LegacyComponents" => (
            lt(
                if name == "DirectPlay" { "DirectPlay（旧版 DirectX 联机组件）" } else { "旧版组件 (Legacy Components)" },
                if name == "DirectPlay" { "DirectPlay (Legacy DirectX)" } else { "Legacy Components" },
            ),
            lt("运行 2000 年代经典老游戏（如早期局域网游戏）时可能需要启用。", "Required when running classic older Windows games using legacy DirectPlay APIs."),
            FeatureCategory::PrintMedia,
            false,
            2,
        ),
        "IIS-WebServerRole" => (
            lt("Internet Information Services (IIS) Web 角色", "IIS Web Server Role"),
            lt("Windows 内置 Web 服务器角色主开关，支持静态站点与 ASP.NET 托管。", "Microsoft built-in Web Server role for local web & ASP.NET hosting."),
            FeatureCategory::IisWeb,
            false,
            4,
        ),
        "IIS-ManagementConsole" => (
            lt("IIS 管理控制台", "IIS Management Console"),
            lt("用于配置本地 IIS 站点与应用池的图形管理界面。", "GUI snap-in for configuring local IIS sites and application pools."),
            FeatureCategory::IisWeb,
            false,
            4,
        ),
        _ => {
            let cat = if name.starts_with("IIS-")
                || name.starts_with("WAS-")
                || name.starts_with("WCF-")
                || name.starts_with("MSMQ-")
            {
                FeatureCategory::IisWeb
            } else if name.contains("Printing") {
                FeatureCategory::PrintMedia
            } else if name.contains("SMB") {
                FeatureCategory::NetworkSmb
            } else {
                FeatureCategory::Developer
            };
            let zh = match name {
                _ if name.starts_with("IIS-") => format!("IIS 模块 · {}", &name[4..]),
                _ if name.starts_with("WCF-") => format!("WCF 激活 · {}", &name[4..]),
                _ if name.starts_with("WAS-") => format!("进程激活服务 · {}", &name[4..]),
                _ if name.starts_with("MSMQ-") => format!("消息队列 · {}", &name[5..]),
                _ => name.to_string(),
            };
            let desc = match cat {
                FeatureCategory::IisWeb => lt("IIS / WCF / 消息队列子组件，用于本地 Web 或服务宿主。", "IIS / WCF / Messaging sub-component for local web service hosting."),
                _ => lt("Windows 内置系统可选组件。", "Windows built-in optional system component."),
            };
            (lt(&zh, name), desc, cat, false, 5)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_optional_features_from_cbs_registry() {
        let list = list_optional_features();
        assert!(!list.is_empty(), "Expected CBS OptionalFeatures keys on Windows");
        assert!(list.iter().any(|f| f.name == "NetFx3" || f.name == "TelnetClient"));
    }

    #[test]
    fn test_set_feature_enabled_validates_name() {
        assert!(set_feature_enabled("bad;name&calc", true).is_err());
    }
}
