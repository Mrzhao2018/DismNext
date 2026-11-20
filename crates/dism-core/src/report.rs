use crate::catalog::Catalog;
use crate::cleaner::{scan_all_with_summary, CleanupBatchScan, CleanupScanResult};
use crate::drivers::list_drivers;
use crate::fmt_bytes;
use crate::startup::{list_scheduled_tasks, list_startup_items, list_third_party_services};
use crate::sysinfo::SystemInfo;
use crate::systasks::get_system_task_status;
use crate::uninstaller::list_win32_apps;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRuleSummary {
    pub id: String,
    pub name_zh: String,
    pub name_en: String,
    pub cleanable_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthReportPayload {
    pub generated_at: String,
    pub health_score: u8,
    pub score_notes: Vec<String>,
    pub os_summary: String,
    pub disk_root: String,
    pub disk_total_bytes: u64,
    pub disk_free_bytes: u64,
    pub disk_used_bytes: u64,
    pub reclaimable_bytes: u64,
    pub outdated_drivers_count: usize,
    pub outdated_drivers_bytes: u64,
    pub orphaned_installer_count: usize,
    pub orphaned_installer_bytes: u64,
    pub enabled_startups: usize,
    pub auto_services: usize,
    pub enabled_tasks: usize,
    pub installed_win32_apps: usize,
    pub compact_os_enabled: bool,
    pub top_cleanup_rules: Vec<ReportRuleSummary>,
    pub markdown: String,
    pub html: String,
}

#[derive(Clone, Copy, Default)]
pub enum ReportLanguage {
    #[default]
    Chinese,
    English,
}

impl ReportLanguage {
    fn text<'a>(self, zh: &'a str, en: &'a str) -> &'a str {
        match self {
            Self::Chinese => zh,
            Self::English => en,
        }
    }

    fn rule_name(self, rule: &ReportRuleSummary) -> &str {
        let translated = self.text(&rule.name_zh, &rule.name_en);
        if translated.is_empty() {
            self.text(&rule.name_en, &rule.name_zh)
        } else {
            translated
        }
    }
}

/// Computes a comprehensive Windows health audit and renders Markdown + HTML reports.
/// Pass `cached_scans` when the server already holds a fresh complete batch scan.
pub fn generate_health_report(
    catalog: &Catalog,
    cached_scans: Option<&CleanupBatchScan>,
) -> HealthReportPayload {
    generate_localized_health_report(catalog, cached_scans, ReportLanguage::default())
}

pub fn generate_localized_health_report(
    catalog: &Catalog,
    cached_scans: Option<&CleanupBatchScan>,
    language: ReportLanguage,
) -> HealthReportPayload {
    let label = |zh, en| language.text(zh, en);
    let sys = SystemInfo::collect();
    let owned_scans;
    let scans = match cached_scans {
        Some(s) => s,
        None => {
            owned_scans = scan_all_with_summary(&catalog.cleanup_rules, &sys);
            &owned_scans
        }
    };

    let task_status = get_system_task_status();
    let drivers = list_drivers();
    let outdated_drivers_count = drivers.iter().filter(|d| d.is_outdated_duplicate).count();
    let outdated_drivers_bytes: u64 = drivers
        .iter()
        .filter(|d| d.is_outdated_duplicate)
        .map(|d| d.size_bytes)
        .sum();

    let startups = list_startup_items();
    let enabled_startups = startups.iter().filter(|s| s.enabled).count();
    let services = list_third_party_services();
    let auto_services = services.iter().filter(|s| s.start_mode == "auto").count();
    let tasks = list_scheduled_tasks();
    let enabled_tasks = tasks.iter().filter(|t| t.enabled).count();
    let win32_apps = list_win32_apps();

    let reclaimable_bytes = scans.summary.cleanable_bytes;

    let mut ranked: Vec<&CleanupScanResult> = scans
        .items
        .iter()
        .filter(|r| r.cleanable_bytes > 0)
        .collect();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.cleanable_bytes));
    let top_cleanup_rules: Vec<ReportRuleSummary> = ranked
        .into_iter()
        .take(10)
        .map(|r| ReportRuleSummary {
            id: r.id.clone(),
            name_zh: r.name.zh.clone(),
            name_en: r.name.en.clone(),
            cleanable_bytes: r.cleanable_bytes,
        })
        .collect();

    let (health_score, score_notes) = compute_health_score(
        &sys,
        reclaimable_bytes,
        outdated_drivers_count,
        task_status.orphaned_installer_count,
        enabled_startups,
        auto_services,
        language,
    );

    let generated_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let os_summary = format!(
        "{} {} ({}, {})",
        sys.product_name, sys.display_version, sys.version_string, sys.arch
    );

    use std::fmt::Write;
    let title = label(
        "DismNext 系统健康诊断与空间体检报告",
        "DismNext System Health Report",
    );
    let mut md = format!("# {title}\n\n");
    writeln!(
        md,
        "- **{}**: `{generated_at}`",
        label("生成时间", "Generated")
    )
    .unwrap();
    writeln!(md, "- **{}**: {os_summary}", label("系统环境", "System")).unwrap();
    writeln!(
        md,
        "- **{}**: `{}` ({} {})",
        label("规则库基线", "Catalog baseline"),
        catalog.base_version,
        catalog.cleanup_rules.len(),
        label("条规则", "rules")
    )
    .unwrap();
    writeln!(
        md,
        "- **{}**: **{health_score} / 100**\n",
        label("综合健康评分", "Health score")
    )
    .unwrap();

    writeln!(
        md,
        "## 1. {}\n",
        label("磁盘与可释放空间概览", "Disk and reclaimable space")
    )
    .unwrap();
    writeln!(
        md,
        "- **{} `{}`**: {} {} / {} ({} **{}**)",
        label("系统盘", "System drive"),
        sys.system_disk.root,
        label("已用", "used"),
        fmt_bytes(sys.system_disk.used_bytes),
        fmt_bytes(sys.system_disk.total_bytes),
        label("可用", "free"),
        fmt_bytes(sys.system_disk.free_bytes)
    )
    .unwrap();
    writeln!(
        md,
        "- **{}**: **{}**",
        label("可安全回收垃圾", "Reclaimable space"),
        fmt_bytes(reclaimable_bytes)
    )
    .unwrap();
    let compact = if task_status.compact_os_enabled {
        label("已开启", "Enabled")
    } else {
        label("未开启", "Disabled")
    };
    writeln!(
        md,
        "- **{}**: {compact}",
        label("CompactOS 系统压缩", "CompactOS compression")
    )
    .unwrap();
    writeln!(
        md,
        "- **{}**: {outdated_drivers_count} ({})",
        label("过期重复驱动包", "Superseded driver packages"),
        fmt_bytes(outdated_drivers_bytes)
    )
    .unwrap();
    writeln!(
        md,
        "- **{}**: {} ({})\n",
        label(
            "孤立 Windows Installer 缓存",
            "Orphaned Windows Installer cache"
        ),
        task_status.orphaned_installer_count,
        fmt_bytes(task_status.orphaned_installer_bytes)
    )
    .unwrap();

    writeln!(
        md,
        "## 2. {}\n",
        label("启动项、后台服务与应用分布", "Startup, services and apps")
    )
    .unwrap();
    writeln!(
        md,
        "- **{}**: {enabled_startups} / {}",
        label("开机自启项（启用中）", "Enabled startup items"),
        startups.len()
    )
    .unwrap();
    writeln!(
        md,
        "- **{}**: {auto_services} / {}",
        label("第三方自启动服务", "Automatic third-party services"),
        services.len()
    )
    .unwrap();
    writeln!(
        md,
        "- **{}**: {enabled_tasks} / {}",
        label("已启用非系统计划任务", "Enabled non-system scheduled tasks"),
        tasks.len()
    )
    .unwrap();
    writeln!(
        md,
        "- **{}**: {}\n",
        label(
            "已安装桌面软件（Win32/MSI）",
            "Installed desktop apps (Win32/MSI)"
        ),
        win32_apps.len()
    )
    .unwrap();

    if !top_cleanup_rules.is_empty() {
        writeln!(md, "## 3. {}\n", label("主要清理项目", "Top cleanup items")).unwrap();
        writeln!(
            md,
            "| {} | {} | {} |\n|---|---|---|",
            label("规则 ID", "Rule ID"),
            label("项目名称", "Name"),
            label("可清理体积", "Reclaimable size")
        )
        .unwrap();
        for item in &top_cleanup_rules {
            writeln!(
                md,
                "| {} | {} | {} |",
                markdown_cell(&item.id),
                markdown_cell(language.rule_name(item)),
                fmt_bytes(item.cleanable_bytes)
            )
            .unwrap();
        }
        md.push('\n');
    }

    if !score_notes.is_empty() {
        writeln!(
            md,
            "## 4. {}\n",
            label("健康扣分项与优化建议", "Findings and recommendations")
        )
        .unwrap();
        for note in &score_notes {
            writeln!(md, "- {note}").unwrap();
        }
    }

    let html = render_html_report(
        language,
        &generated_at,
        health_score,
        &os_summary,
        &sys,
        reclaimable_bytes,
        outdated_drivers_count,
        outdated_drivers_bytes,
        task_status.orphaned_installer_count,
        task_status.orphaned_installer_bytes,
        enabled_startups,
        startups.len(),
        auto_services,
        services.len(),
        enabled_tasks,
        tasks.len(),
        win32_apps.len(),
        compact,
        &top_cleanup_rules,
        &score_notes,
    );

    HealthReportPayload {
        generated_at,
        health_score,
        score_notes,
        os_summary,
        disk_root: sys.system_disk.root.clone(),
        disk_total_bytes: sys.system_disk.total_bytes,
        disk_free_bytes: sys.system_disk.free_bytes,
        disk_used_bytes: sys.system_disk.used_bytes,
        reclaimable_bytes,
        outdated_drivers_count,
        outdated_drivers_bytes,
        orphaned_installer_count: task_status.orphaned_installer_count,
        orphaned_installer_bytes: task_status.orphaned_installer_bytes,
        enabled_startups,
        auto_services,
        enabled_tasks,
        installed_win32_apps: win32_apps.len(),
        compact_os_enabled: task_status.compact_os_enabled,
        top_cleanup_rules,
        markdown: md,
        html,
    }
}

fn compute_health_score(
    sys: &SystemInfo,
    reclaimable_bytes: u64,
    outdated_drivers_count: usize,
    orphaned_installer_count: usize,
    enabled_startups: usize,
    auto_services: usize,
    language: ReportLanguage,
) -> (u8, Vec<String>) {
    let mut score: i32 = 100;
    let mut notes = Vec::new();
    let label = |zh, en| language.text(zh, en);
    let english = matches!(language, ReportLanguage::English);

    if sys.system_disk.total_bytes > 0 {
        let free_ratio = sys.system_disk.free_bytes as f64 / sys.system_disk.total_bytes as f64;
        if free_ratio < 0.10 {
            score -= 20;
            notes.push(
                label(
                    "系统盘可用空间低于 10%，建议立即释放空间。",
                    "The system drive has less than 10% free space. Free up space now.",
                )
                .into(),
            );
        } else if free_ratio < 0.20 {
            score -= 10;
            notes.push(label("系统盘可用空间低于 20%，建议定期清理缓存。",
                "The system drive has less than 20% free space. Consider clearing caches regularly.").into());
        }
    }

    let reclaimable = fmt_bytes(reclaimable_bytes);
    if reclaimable_bytes >= 10 * 1024 * 1024 * 1024 {
        score -= 15;
        notes.push(if english {
            format!(
                "Cleanup found {reclaimable} of reclaimable files. Review them in Space Reclaim."
            )
        } else {
            format!("检测到 {reclaimable} 可清理文件，可在「空间回收」中核对。")
        });
    } else if reclaimable_bytes >= 2 * 1024 * 1024 * 1024 {
        score -= 8;
        notes.push(if english {
            format!("{reclaimable} of cache files can be cleaned up as needed.")
        } else {
            format!("当前共有 {reclaimable} 可释放缓存，可按需清理。")
        });
    }

    if outdated_drivers_count > 5 {
        score -= 8;
        notes.push(if english {
            format!("{outdated_drivers_count} driver packages have been superseded. Back them up before removing them in Driver Manager.")
        } else {
            format!("有 {outdated_drivers_count} 个旧驱动包已被新版取代，可在「驱动管理」中备份后清理。")
        });
    } else if outdated_drivers_count > 0 {
        score -= 3;
    }

    if orphaned_installer_count > 5 {
        score -= 5;
        notes.push(if english {
            format!("Found {orphaned_installer_count} orphaned Windows Installer packages.")
        } else {
            format!("发现 {orphaned_installer_count} 个无主 Windows Installer 安装包缓存。")
        });
    }
    if enabled_startups > 12 {
        score -= 7;
        notes.push(if english {
            format!("{enabled_startups} startup items are enabled. Review which ones you need in Startup Manager.")
        } else {
            format!("有 {enabled_startups} 个开机自启项，可在「启动项管理」中关闭非必要项目。")
        });
    }
    if auto_services > 20 {
        score -= 5;
        notes.push(if english {
            format!("{auto_services} third-party services start automatically. Review which ones can be switched to manual startup.")
        } else {
            format!("有 {auto_services} 个第三方服务随系统启动，可将非必需服务改为手动启动。")
        });
    }

    if notes.is_empty() {
        notes.push(
            label(
                "系统整体健康状态良好，暂无高风险瓶颈。",
                "The system is healthy. No high-risk issues were identified.",
            )
            .into(),
        );
    }

    (score.clamp(0, 100) as u8, notes)
}

#[allow(clippy::too_many_arguments)]
fn render_html_report(
    language: ReportLanguage,
    generated_at: &str,
    health_score: u8,
    os_summary: &str,
    sys: &SystemInfo,
    reclaimable_bytes: u64,
    outdated_drivers_count: usize,
    outdated_drivers_bytes: u64,
    orphaned_installer_count: usize,
    orphaned_installer_bytes: u64,
    enabled_startups: usize,
    total_startups: usize,
    auto_services: usize,
    total_services: usize,
    enabled_tasks: usize,
    total_tasks: usize,
    installed_win32_apps: usize,
    compact: &str,
    top_cleanup_rules: &[ReportRuleSummary],
    score_notes: &[String],
) -> String {
    let label = |zh, en| language.text(zh, en);
    let mut rows_html = String::new();
    for r in top_cleanup_rules {
        rows_html.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td></tr>\n",
            escape_html(&r.id),
            escape_html(language.rule_name(r)),
            fmt_bytes(r.cleanable_bytes)
        ));
    }
    if rows_html.is_empty() {
        rows_html.push_str(&format!(
            "<tr><td colspan=\"3\">{}</td></tr>",
            label(
                "暂无显著垃圾占用的规则项",
                "No significant cleanup items found."
            )
        ));
    }

    let mut notes_html = String::new();
    for n in score_notes {
        notes_html.push_str(&format!("<li>{}</li>\n", escape_html(n)));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="{html_lang}">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>{title}</title>
<style>
  body {{ font-family: "Segoe UI", "Microsoft YaHei UI", sans-serif; margin: 32px auto; max-width: 920px; color: #1f2328; background: #f6f8fa; line-height: 1.6; padding: 0 16px; }}
  .card {{ background: #fff; border: 1px solid #d0d7de; border-radius: 12px; padding: 20px 24px; margin-bottom: 18px; box-shadow: 0 2px 6px rgba(0,0,0,0.03); }}
  .header {{ display: flex; justify-content: space-between; align-items: center; gap: 16px; flex-wrap: wrap; }}
  .score {{ font-size: 40px; font-weight: 700; color: {score_color}; }}
  .grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 12px; margin-top: 12px; }}
  .metric {{ background: #f8fafc; border: 1px solid #e5e7eb; border-radius: 8px; padding: 12px; }}
  .metric .label {{ font-size: 12px; color: #57606a; }}
  .metric .val {{ font-size: 18px; font-weight: 600; margin-top: 4px; }}
  table {{ width: 100%; border-collapse: collapse; margin-top: 10px; }}
  th, td {{ text-align: left; padding: 9px 10px; border-bottom: 1px solid #e5e7eb; font-size: 14px; }}
  th {{ background: #f3f4f6; }}
  code {{ background: #eff1f3; padding: 2px 6px; border-radius: 4px; font-family: Consolas, monospace; }}
</style>
</head>
<body>
  <div class="card header">
    <div>
      <h1 style="margin:0 0 6px 0;">{title}</h1>
      <div style="color:#57606a;font-size:14px;">{os} · {generated}: {time}</div>
    </div>
    <div style="text-align:right;">
      <div style="font-size:12px;color:#57606a;">{score_label}</div>
      <div class="score">{score} <span style="font-size:16px;color:#57606a;">/ 100</span></div>
    </div>
  </div>

  <div class="card">
    <h2 style="margin-top:0;">{metrics}</h2>
    <div class="grid">
      <div class="metric"><div class="label">{disk_label} ({root}) {free_total}</div><div class="val">{free} / {total}</div></div>
      <div class="metric"><div class="label">{scan_label}</div><div class="val">{reclaimable}</div></div>
      <div class="metric"><div class="label">{drivers_label}</div><div class="val">{outdated_drv} ({outdated_drv_bytes})</div></div>
      <div class="metric"><div class="label">{installer_label}</div><div class="val">{orphan_msi} ({orphan_msi_bytes})</div></div>
      <div class="metric"><div class="label">{startup_label}</div><div class="val">{st_on}/{st_tot} · {sv_on}/{sv_tot} · {tk_on}/{tk_tot}</div></div>
      <div class="metric"><div class="label">{apps_label}</div><div class="val">{apps} · {compact}</div></div>
    </div>
  </div>

  <div class="card">
    <h2 style="margin-top:0;">{top_label}</h2>
    <table>
      <thead><tr><th>{rule_label}</th><th>{name_label}</th><th>{size_label}</th></tr></thead>
      <tbody>{rows}</tbody>
    </table>
  </div>

  <div class="card">
    <h2 style="margin-top:0;">{findings}</h2>
    <ul>{notes}</ul>
  </div>
</body>
</html>"#,
        html_lang = label("zh-CN", "en"),
        title = label("DismNext 系统健康体检报告", "DismNext System Health Report"),
        generated = label("生成时间", "Generated"),
        score_label = label("健康评分", "Health score"),
        metrics = label("核心指标概览", "Key metrics"),
        disk_label = label("系统盘", "System drive"),
        free_total = label("可用 / 总计", "free / total"),
        scan_label = label("扫描可安全回收垃圾", "Reclaimable space"),
        drivers_label = label("过期重复驱动包", "Superseded driver packages"),
        installer_label = label("孤立 Installer 缓存", "Orphaned Installer cache"),
        startup_label = label(
            "开机自启 / 后台服务 / 任务",
            "Startup items / services / tasks"
        ),
        apps_label = label("桌面软件 · CompactOS", "Desktop apps · CompactOS"),
        top_label = label("主要清理项目", "Top cleanup items"),
        rule_label = label("规则 ID", "Rule ID"),
        name_label = label("项目名称", "Name"),
        size_label = label("可清理体积", "Reclaimable size"),
        findings = label("诊断结论与建议", "Findings and recommendations"),
        score_color = if health_score >= 85 {
            "#1a7f37"
        } else if health_score >= 65 {
            "#9a6700"
        } else {
            "#cf222e"
        },
        os = escape_html(os_summary),
        time = escape_html(generated_at),
        score = health_score,
        root = escape_html(&sys.system_disk.root),
        free = fmt_bytes(sys.system_disk.free_bytes),
        total = fmt_bytes(sys.system_disk.total_bytes),
        reclaimable = fmt_bytes(reclaimable_bytes),
        outdated_drv = outdated_drivers_count,
        outdated_drv_bytes = fmt_bytes(outdated_drivers_bytes),
        orphan_msi = orphaned_installer_count,
        orphan_msi_bytes = fmt_bytes(orphaned_installer_bytes),
        st_on = enabled_startups,
        st_tot = total_startups,
        sv_on = auto_services,
        sv_tot = total_services,
        tk_on = enabled_tasks,
        tk_tot = total_tasks,
        apps = installed_win32_apps,
        rows = rows_html,
        notes = notes_html,
    )
}

fn markdown_cell(value: &str) -> String {
    escape_html(value)
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace('`', "\\`")
        .replace(['\r', '\n'], " ")
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_health_report_markdown_and_html() {
        let catalog = Catalog::load(None).expect("catalog should load");
        let mut scan = CleanupBatchScan::default();
        scan.summary.cleanable_bytes = 42;
        let report = generate_health_report(&catalog, Some(&scan));
        assert!(report.health_score <= 100);
        assert!(report.disk_total_bytes > 0);
        assert_eq!(report.reclaimable_bytes, 42);
        assert!(report
            .markdown
            .contains("DismNext 系统健康诊断与空间体检报告"));
        assert!(report.html.contains("<!DOCTYPE html>"));
        assert!(report.html.contains("DismNext 系统健康体检报告"));
        let english =
            generate_localized_health_report(&catalog, Some(&scan), ReportLanguage::English);
        assert!(english.markdown.contains("# DismNext System Health Report"));
        assert!(english.html.contains("<html lang=\"en\">"));
        assert_eq!(english.health_score, report.health_score);
        assert_eq!(english.reclaimable_bytes, report.reclaimable_bytes);
        assert!(english.score_notes.iter().all(|note| note.is_ascii()));
    }

    #[test]
    fn markdown_rule_names_cannot_add_a_column_or_row() {
        assert_eq!(
            markdown_cell("Browser | cache\n<old>"),
            "Browser \\| cache &lt;old&gt;"
        );
    }
}
