use anyhow::{ensure, Result};
use clap::{Subcommand, ValueEnum};
use dism_core::cleaner::CleanupLease;
use dism_core::diskmap::{find_duplicate_files, find_orphaned_app_dirs};
use dism_core::report::{generate_localized_health_report, ReportLanguage};
use dism_core::rescue::{create_restore_point, list_restore_points, run_rescue_action};
use dism_core::startup::{
    list_scheduled_tasks, list_third_party_services, set_scheduled_task_enabled,
    set_service_start_mode,
};
use dism_core::uninstaller::{launch_win32_uninstaller, list_win32_apps, UninstallState};
use dism_core::Catalog;
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Subcommand)]
pub(crate) enum Tools {
    /// Find files with identical contents
    Duplicates {
        path: PathBuf,
        #[arg(long, default_value_t = 512 * 1024)]
        min_size_bytes: u64,
    },
    /// List possible orphaned application directories
    Orphans,
    /// Generate the health report
    Report {
        #[arg(long, value_enum, default_value = "markdown")]
        format: ReportFormat,
        #[arg(long, value_enum, default_value = "zh")]
        lang: Language,
    },
    /// Windows system rescue actions
    Rescue {
        #[command(subcommand)]
        action: Rescue,
    },
    /// List or uninstall registered Win32 applications
    Uninstall {
        #[command(subcommand)]
        action: Uninstall,
    },
    /// List or configure third-party service startup
    Services {
        #[command(subcommand)]
        action: Services,
    },
    /// List or enable/disable scheduled tasks
    Tasks {
        #[command(subcommand)]
        action: Tasks,
    },
}

#[derive(Clone, ValueEnum)]
pub(crate) enum ReportFormat {
    Markdown,
    Html,
    Json,
}

#[derive(Clone, ValueEnum)]
pub(crate) enum Language {
    Zh,
    En,
}

#[derive(Subcommand)]
pub(crate) enum Rescue {
    ListRestorePoints,
    CreateRestorePoint { description: String },
    Run { action_id: String },
}

#[derive(Subcommand)]
pub(crate) enum Uninstall {
    List,
    Run {
        id: String,
        #[arg(long)]
        quiet: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum Services {
    List,
    Set {
        name: String,
        #[arg(value_enum)]
        mode: ServiceMode,
    },
}

#[derive(Clone, ValueEnum)]
pub(crate) enum ServiceMode {
    Auto,
    Manual,
    Disabled,
}

#[derive(Subcommand)]
pub(crate) enum Tasks {
    List,
    Enable { path: String },
    Disable { path: String },
}

impl Tools {
    pub(crate) fn run(self, catalog: &Catalog) -> Result<()> {
        match self {
            Self::Duplicates {
                path,
                min_size_bytes,
            } => print_json(find_duplicate_files(&path, min_size_bytes)?),
            Self::Orphans => print_json(find_orphaned_app_dirs()),
            Self::Report { format, lang } => {
                let lang = match lang {
                    Language::Zh => ReportLanguage::Chinese,
                    Language::En => ReportLanguage::English,
                };
                let report = generate_localized_health_report(catalog, None, lang);
                match format {
                    ReportFormat::Markdown => print!("{}", report.markdown),
                    ReportFormat::Html => print!("{}", report.html),
                    ReportFormat::Json => return print_json(report),
                }
                Ok(())
            }
            Self::Rescue { action } => match action {
                Rescue::ListRestorePoints => print_json(list_restore_points()),
                Rescue::CreateRestorePoint { description } => {
                    let _lease = CleanupLease::try_acquire()?;
                    print_json(create_restore_point(&description)?)
                }
                Rescue::Run { action_id } => {
                    let _lease = CleanupLease::try_acquire()?;
                    print_json(run_rescue_action(&action_id)?)
                }
            },
            Self::Uninstall { action } => match action {
                Uninstall::List => print_json(list_win32_apps()),
                Uninstall::Run { id, quiet } => {
                    let _lease = CleanupLease::try_acquire()?;
                    let mut job = launch_win32_uninstaller(&id, quiet)?;
                    let state = loop {
                        let status = job.status()?;
                        if status.state != UninstallState::Running {
                            let state = status.state;
                            print_json(status)?;
                            break state;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    };
                    ensure!(
                        matches!(
                            state,
                            UninstallState::Completed | UninstallState::RestartRequired
                        ),
                        "Uninstaller returned {state:?}"
                    );
                    Ok(())
                }
            },
            Self::Services { action } => match action {
                Services::List => print_json(list_third_party_services()),
                Services::Set { name, mode } => {
                    let _lease = CleanupLease::try_acquire()?;
                    let mode = match mode {
                        ServiceMode::Auto => "auto",
                        ServiceMode::Manual => "manual",
                        ServiceMode::Disabled => "disabled",
                    };
                    print_json(set_service_start_mode(&name, mode)?)
                }
            },
            Self::Tasks { action } => match action {
                Tasks::List => print_json(list_scheduled_tasks()),
                Tasks::Enable { path } => set_task(&path, true),
                Tasks::Disable { path } => set_task(&path, false),
            },
        }
    }
}

fn set_task(path: &str, enabled: bool) -> Result<()> {
    let _lease = CleanupLease::try_acquire()?;
    set_scheduled_task_enabled(path, enabled)?;
    print_json(serde_json::json!({ "path": path, "enabled": enabled }))
}

fn print_json(value: impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
