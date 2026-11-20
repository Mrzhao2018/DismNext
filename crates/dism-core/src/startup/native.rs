use super::{ScheduledTaskItem, StartupSignature, StartupTargetInspection};
use crate::com::Apartment;
use crate::evaluator::expand_env_vars;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf, Prefix};
use windows::core::{Interface, BSTR, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ,
};
use windows::Win32::System::TaskScheduler::{
    IRegisteredTask, ITaskFolder, ITaskService, TaskScheduler, TASK_ENUM_HIDDEN,
    TASK_STATE_DISABLED, TASK_STATE_QUEUED, TASK_STATE_READY, TASK_STATE_RUNNING,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, SLGP_RAWPATH};
use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;

mod signature;

pub(super) fn inspect_launch_file(path: String) -> StartupTargetInspection {
    let mut inspection = StartupTargetInspection::without_signature(
        Some(path.clone()),
        StartupSignature::Unavailable,
    );
    let mut target = PathBuf::from(&path);
    let mut canonical = match local_target(&target) {
        Ok(path) => path,
        Err(status) => {
            inspection.signature = status;
            return inspection;
        }
    };
    if target
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
    {
        let Some(link_target) = read_shortcut_target(&canonical) else {
            inspection.path = None;
            inspection.signature = StartupSignature::UnresolvedTarget;
            return inspection;
        };
        inspection.path = Some(link_target.clone());
        target = PathBuf::from(link_target);
        canonical = match local_target(&target) {
            Ok(path) => path,
            Err(status) => {
                inspection.signature = status;
                return inspection;
            }
        };
    }
    if !canonical.is_file() {
        inspection.signature = StartupSignature::Unsupported;
        return inspection;
    }
    inspection.company = signature::file_company(&canonical);
    (inspection.signature, inspection.signer) = signature::inspect(&canonical);
    inspection
}

fn local_target(path: &Path) -> Result<PathBuf, StartupSignature> {
    if !is_local_drive_path(path) {
        return Err(StartupSignature::UnresolvedTarget);
    }
    let canonical = path.canonicalize().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => StartupSignature::MissingTarget,
        _ => StartupSignature::Unavailable,
    })?;
    if !is_local_drive_path(&canonical) {
        return Err(StartupSignature::UnresolvedTarget);
    }
    Ok(canonical)
}

fn is_local_drive_path(path: &Path) -> bool {
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return false;
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) if path.has_root() => drive,
        _ => return false,
    };
    const DRIVE_REMOTE: u32 = 4;
    let root = [u16::from(drive), u16::from(b':'), u16::from(b'\\'), 0];
    unsafe { GetDriveTypeW(root.as_ptr()) != DRIVE_REMOTE }
}

fn read_shortcut_target(path: &Path) -> Option<String> {
    let _apartment = Apartment::initialize().ok()?;
    let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut target = vec![0; 32_768];
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let persist: IPersistFile = link.cast().ok()?;
        persist.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;
        link.GetPath(&mut target, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
            .ok()?;
    }
    let end = target.iter().position(|&ch| ch == 0)?;
    (end > 0).then(|| expand_env_vars(&String::from_utf16_lossy(&target[..end]), None))
}

pub(super) fn compute_scheduled_tasks_uncached() -> Vec<ScheduledTaskItem> {
    let loaded = || -> windows::core::Result<Vec<ScheduledTaskItem>> {
        let _apartment = Apartment::initialize()?;
        let empty = VARIANT::default();
        unsafe {
            let service: ITaskService =
                CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
            service.Connect(&empty, &empty, &empty, &empty)?;
            let folder = service.GetFolder(&BSTR::from("\\"))?;
            let mut items = Vec::new();
            collect_tasks(folder, &mut items);
            items.sort_by(|a, b| {
                b.enabled.cmp(&a.enabled).then_with(|| {
                    a.name
                        .to_ascii_lowercase()
                        .cmp(&b.name.to_ascii_lowercase())
                })
            });
            Ok(items)
        }
    };
    loaded().unwrap_or_default()
}

fn collect_tasks(root: ITaskFolder, items: &mut Vec<ScheduledTaskItem>) {
    let mut folders = vec![root];
    while let Some(folder) = folders.pop() {
        unsafe {
            let Ok(path) = folder.Path() else { continue };
            if path.to_string().eq_ignore_ascii_case(r"\Microsoft\Windows") {
                continue;
            }
            if let Ok(tasks) = folder.GetTasks(TASK_ENUM_HIDDEN.0) {
                for index in 1..=tasks.Count().unwrap_or(0) {
                    if let Ok(task) = tasks.get_Item(&VARIANT::from(index)) {
                        if let Some(item) = describe_task(&task) {
                            items.push(item);
                        }
                    }
                }
            }
            if let Ok(children) = folder.GetFolders(0) {
                for index in 1..=children.Count().unwrap_or(0) {
                    if let Ok(child) = children.get_Item(&VARIANT::from(index)) {
                        folders.push(child);
                    }
                }
            }
        }
    }
}

fn describe_task(task: &IRegisteredTask) -> Option<ScheduledTaskItem> {
    unsafe {
        let state = match task.State().ok() {
            Some(TASK_STATE_DISABLED) => "Disabled",
            Some(TASK_STATE_QUEUED) => "Queued",
            Some(TASK_STATE_READY) => "Ready",
            Some(TASK_STATE_RUNNING) => "Running",
            _ => "Unknown",
        };
        let command = task
            .Xml()
            .map(|xml| task_commands(&xml.to_string()))
            .unwrap_or_default();
        Some(ScheduledTaskItem {
            path: task.Path().ok()?.to_string(),
            name: task.Name().ok()?.to_string(),
            enabled: task.Enabled().ok()?.as_bool(),
            state: state.to_string(),
            command,
        })
    }
}

fn task_commands(xml: &str) -> String {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return String::new();
    };
    let Some(actions) = doc
        .root_element()
        .children()
        .find(|node| node.has_tag_name("Actions"))
    else {
        return String::new();
    };
    actions
        .children()
        .filter(|node| node.has_tag_name("Exec"))
        .filter_map(|node| {
            let command = node
                .children()
                .find(|child| child.has_tag_name("Command"))?
                .text()?
                .trim();
            let arguments = node
                .children()
                .find(|child| child.has_tag_name("Arguments"))
                .and_then(|child| child.text())
                .unwrap_or("")
                .trim();
            if command.is_empty() {
                return None;
            }
            Some(if arguments.is_empty() {
                command.to_string()
            } else {
                format!("{command} {arguments}")
            })
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_preserves_entities_and_all_executable_actions() {
        let xml = r#"<Task xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
          <Actions><Exec><Command>C:\A&amp;B\task.exe</Command><Arguments>&quot;X &amp; Y&quot;</Arguments></Exec>
          <ComHandler><ClassId>handler</ClassId></ComHandler><Exec><Command>second.exe</Command></Exec></Actions>
        </Task>"#;
        assert_eq!(
            task_commands(xml),
            r#"C:\A&B\task.exe "X & Y" · second.exe"#
        );
        assert!(task_commands("<invalid>").is_empty());
    }

    #[test]
    fn native_inspection_resolves_a_shortcut_and_reports_signer_and_company() {
        let binary = crate::system_tools::system32_dir().unwrap().join("cmd.exe");
        let shortcut =
            std::env::temp_dir().join(format!("dismnext-signature-{}.lnk", std::process::id()));
        let binary_wide: Vec<_> = binary.as_os_str().encode_wide().chain(Some(0)).collect();
        let shortcut_wide: Vec<_> = shortcut.as_os_str().encode_wide().chain(Some(0)).collect();
        {
            let _apartment = Apartment::initialize().unwrap();
            unsafe {
                let link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                link.SetPath(PCWSTR(binary_wide.as_ptr())).unwrap();
                let persist: IPersistFile = link.cast().unwrap();
                persist.Save(PCWSTR(shortcut_wide.as_ptr()), true).unwrap();
            }
        }
        let inspection = inspect_launch_file(shortcut.to_string_lossy().into_owned());
        std::fs::remove_file(shortcut).unwrap();
        assert!(inspection
            .path
            .as_deref()
            .is_some_and(|path| Path::new(path).file_name() == binary.file_name()));
        assert!(
            matches!(inspection.signature, StartupSignature::Valid),
            "{inspection:?}"
        );
        assert!(inspection
            .signer
            .as_ref()
            .is_some_and(|name| !name.is_empty()));
        assert!(inspection
            .company
            .as_ref()
            .is_some_and(|name| !name.is_empty()));
    }
}
