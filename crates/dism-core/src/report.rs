use crate::catalog::Catalog;
use crate::cleaner::{scan_all, CleanupScanResult};
use crate::drivers::list_drivers;
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

/// Computes a comprehensive Windows health audit and renders Markdown + HTML reports.
/// Pass `cached_scans` when the server already holds fresh `scan_all` results to avoid rescanning.
pub fn generate_health_report(
    catalog: &Catalog,
    cached_scans: Option<&[CleanupScanResult]>,
) -> HealthReportPayload {
    let sys = SystemInfo::collect();
    let owned_scans;
    let scans: &[CleanupScanResult] = match cached_scans {
        Some(s) => s,
        None => {
            owned_scans = scan_all(&catalog.cleanup_rules, &sys);
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

    let reclaimable_bytes: u64 = scans.iter().map(|r| r.cleanable_bytes).sum();

    let mut ranked: Vec<&CleanupScanResult> = scans
        .iter()
        .filter(|r| r.cleanable_bytes > 0)
        .collect();
    ranked.sort_by(|a, b| b.cleanable_bytes.cmp(&a.cleanable_bytes));
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
    );

    let generated_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let os_summary = format!(
        "{} {} ({}, {})",
        sys.product_name, sys.display_version, sys.version_string, sys.arch
    );

    let mut md = String::new();
    md.push_str("# DismNext 系统健康诊断与空间体检报告\n\n");
    md.push_str(&format!("- **生成时间**：`{}`\n", generated_at));
    md.push_str(&format!("- **系统环境**：{}\n", os_summary));
    md.push_str(&format!("- **规则库基线**：`{}`（{} 条规则）\n", catalog.base_version, catalog.cleanup_rules.len()));
    md.push_str(&format!("- **综合健康评分**：**{} / 100**\n\n", health_score));

    md.push_str("## 1. 磁盘与可释放空间概览\n\n");
    md.push_str(&format!(
        "- **系统盘 `{}`**：已用 {} / 共 {}（可用 **{}**）\n",
        sys.system_disk.root,
        fmt_bytes(sys.system_disk.used_bytes),
        fmt_bytes(sys.system_disk.total_bytes),
        fmt_bytes(sys.system_disk.free_bytes)
    ));
    md.push_str(&format!("- **可安全回收垃圾**：**{}**\n", fmt_bytes(reclaimable_bytes)));
    md.push_str(&format!(
        "- **CompactOS 系统压缩**：{}\n",
        if task_status.compact_os_enabled { "已开启" } else { "未开启" }
    ));
    md.push_str(&format!(
        "- **过期重复驱动包**：{} 个（{}）\n",
        outdated_drivers_count,
        fmt_bytes(outdated_drivers_bytes)
    ));
    md.push_str(&format!(
        "- **孤立 Windows Installer 缓存**：{} 个（{}）\n\n",
        task_status.orphaned_installer_count,
        fmt_bytes(task_status.orphaned_installer_bytes)
    ));

    md.push_str("## 2. 启动项、后台服务与应用分布\n\n");
    md.push_str(&format!("- **开机自启项（启用中）**：{} / {}\n", enabled_startups, startups.len()));
    md.push_str(&format!("- **第三方自启动服务**：{} / {}\n", auto_services, services.len()));
    md.push_str(&format!("- **已启用非系统计划任务**：{} / {}\n", enabled_tasks, tasks.len()));
    md.push_str(&format!("- **已安装桌面软件（Win32/MSI）**：{} 款\n\n", win32_apps.len()));

    if !top_cleanup_rules.is_empty() {
        md.push_str("## 3. Top 可清理大项明细\n\n");
        md.push_str("| 规则 ID | 项目名称 | 可清理体积 |\n|---|---|---|\n");
        for item in &top_cleanup_rules {
            md.push_str(&format!(
                "| `{}` | {} | {} |\n",
                item.id,
                item.name_zh,
                fmt_bytes(item.cleanable_bytes)
            ));
        }
        md.push('\n');
    }

    if !score_notes.is_empty() {
        md.push_str("## 4. 健康扣分项与优化建议\n\n");
        for note in &score_notes {
            md.push_str(&format!("- {}\n", note));
        }
    }

    let html = render_html_report(
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
        task_status.compact_os_enabled,
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
) -> (u8, Vec<String>) {
    let mut score: i32 = 100;
    let mut notes = Vec::new();

    if sys.system_disk.total_bytes > 0 {
        let free_ratio = sys.system_disk.free_bytes as f64 / sys.system_disk.total_bytes as f64;
        if free_ratio < 0.10 {
            score -= 20;
            notes.push("系统盘可用空间低于 10%，强烈建议立即执行空间回收或清理大文件。".to_string());
        } else if free_ratio < 0.20 {
            score -= 10;
            notes.push("系统盘可用空间偏低（< 20%），建议定期释放缓存空间。".to_string());
        }
    }

    if reclaimable_bytes >= 10 * 1024 * 1024 * 1024 {
        score -= 15;
        notes.push(format!(
            "检测到 {} 可清理垃圾文件，建议在「空间回收」中执行清理。",
            fmt_bytes(reclaimable_bytes)
        ));
    } else if reclaimable_bytes >= 2 * 1024 * 1024 * 1024 {
        score -= 8;
        notes.push(format!(
            "当前共有 {} 可释放缓存，可按需清理。",
            fmt_bytes(reclaimable_bytes)
        ));
    }

    if outdated_drivers_count > 5 {
        score -= 8;
        notes.push(format!(
            "存在 {} 个被新版取代的旧驱动包，可在「驱动管理」中备份后去重。",
            outdated_drivers_count
        ));
    } else if outdated_drivers_count > 0 {
        score -= 3;
    }

    if orphaned_installer_count > 5 {
        score -= 5;
        notes.push(format!(
            "存在 {} 个无主 Windows Installer 安装包缓存。",
            orphaned_installer_count
        ));
    }

    if enabled_startups > 12 {
        score -= 7;
        notes.push(format!(
            "开机自启项较多（{} 个），可在「启动项管理」中关闭非必要项目。",
            enabled_startups
        ));
    }
    if auto_services > 20 {
        score -= 5;
        notes.push(format!(
            "第三方随系统启动的后台服务较多（{} 个），建议将非必需服务改为手动触发。",
            auto_services
        ));
    }

    if notes.is_empty() {
        notes.push("系统整体健康状态良好，暂无高风险瓶颈。".to_string());
    }

    (score.clamp(0, 100) as u8, notes)
}

#[allow(clippy::too_many_arguments)]
fn render_html_report(
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
    compact_os_enabled: bool,
    top_cleanup_rules: &[ReportRuleSummary],
    score_notes: &[String],
) -> String {
    let mut rows_html = String::new();
    for r in top_cleanup_rules {
        rows_html.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td></tr>\n",
            escape_html(&r.id),
            escape_html(&r.name_zh),
            fmt_bytes(r.cleanable_bytes)
        ));
    }
    if rows_html.is_empty() {
        rows_html.push_str("<tr><td colspan=\"3\">暂无显著垃圾占用的规则项</td></tr>");
    }

    let mut notes_html = String::new();
    for n in score_notes {
        notes_html.push_str(&format!("<li>{}</li>\n", escape_html(n)));
    }

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>DismNext 系统健康体检报告</title>
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
      <h1 style="margin:0 0 6px 0;">DismNext 系统健康体检报告</h1>
      <div style="color:#57606a;font-size:14px;">{os} · 生成时间：{time}</div>
    </div>
    <div style="text-align:right;">
      <div style="font-size:12px;color:#57606a;">健康评分</div>
      <div class="score">{score} <span style="font-size:16px;color:#57606a;">/ 100</span></div>
    </div>
  </div>

  <div class="card">
    <h2 style="margin-top:0;">核心指标概览</h2>
    <div class="grid">
      <div class="metric"><div class="label">系统盘 ({root}) 可用 / 总计</div><div class="val">{free} / {total}</div></div>
      <div class="metric"><div class="label">扫描可安全回收垃圾</div><div class="val">{reclaimable}</div></div>
      <div class="metric"><div class="label">过期重复驱动包</div><div class="val">{outdated_drv} 个 ({outdated_drv_bytes})</div></div>
      <div class="metric"><div class="label">孤立 Installer 缓存</div><div class="val">{orphan_msi} 个 ({orphan_msi_bytes})</div></div>
      <div class="metric"><div class="label">开机自启 / 后台服务 / 任务</div><div class="val">{st_on}/{st_tot} · {sv_on}/{sv_tot} · {tk_on}/{tk_tot}</div></div>
      <div class="metric"><div class="label">桌面软件 · CompactOS</div><div class="val">{apps} 款 · {compact}</div></div>
    </div>
  </div>

  <div class="card">
    <h2 style="margin-top:0;">Top 可清理项目明细</h2>
    <table>
      <thead><tr><th>规则 ID</th><th>项目名称</th><th>可清理体积</th></tr></thead>
      <tbody>{rows}</tbody>
    </table>
  </div>

  <div class="card">
    <h2 style="margin-top:0;">诊断结论与建议</h2>
    <ul>{notes}</ul>
  </div>
</body>
</html>"#,
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
        compact = if compact_os_enabled { "已开启" } else { "未开启" },
        rows = rows_html,
        notes = notes_html,
    )
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn fmt_bytes(b: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let f = b as f64;
    if f >= GB {
        format!("{:.2} GB", f / GB)
    } else if f >= MB {
        format!("{:.2} MB", f / MB)
    } else if f >= KB {
        format!("{:.1} KB", f / KB)
    } else {
        format!("{} B", b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_health_report_markdown_and_html() {
        let catalog = Catalog::load(None).expect("catalog should load");
        let report = generate_health_report(&catalog, Some(&[]));
        assert!(report.health_score <= 100);
        assert!(report.disk_total_bytes > 0);
        assert!(report.markdown.contains("DismNext 系统健康诊断与空间体检报告"));
        assert!(report.html.contains("<!DOCTYPE html>"));
        assert!(report.html.contains("DismNext 系统健康体检报告"));
    }
}
