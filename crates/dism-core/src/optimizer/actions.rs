use crate::evaluator::{
    expand_env_vars, parse_hex_bytes, parse_hex_u32, parse_hex_u64, parse_reg_path,
    reg_value_to_u32, reg_value_to_u64, wow64_sam_list,
};
use crate::snapshot::{read_tree_recursive, split_parent_leaf, write_tree_recursive, ShellRefresh};
use crate::win_util::{has_reparse_point_in_ancestors, is_unc_path, CREATE_NO_WINDOW};
use anyhow::{anyhow, Context, Result};
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::Path;
use windows_sys::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG,
};
use winreg::enums::{RegType, KEY_ALL_ACCESS, KEY_READ};
use winreg::{RegKey, RegValue};

const START_MENU_REFRESH: &str = "taskkill /f /im StartMenuExperienceHost.exe";

pub(crate) fn is_start_menu_refresh(command: &str) -> bool {
    command.trim().eq_ignore_ascii_case(START_MENU_REFRESH)
}

pub(crate) fn restore_shell_refresh(refresh: &ShellRefresh) -> Result<()> {
    match refresh {
        ShellRefresh::Associations => exec_explorer_notify("AssocChanged", None, None, None, None),
        ShellRefresh::TraySettings => {
            exec_explorer_notify("Custom", Some("1A"), None, Some("TraySettings"), None)
        }
        ShellRefresh::TrayCommand => {
            exec_explorer_notify("Custom", Some("111"), Some("#A220"), None, None)
        }
        ShellRefresh::StartMenu => {
            exec_explorer_notify("Cmd", None, None, None, Some(START_MENU_REFRESH))
        }
    }
}

pub(super) fn exec_reg_write(
    key: &str,
    value: Option<&str>,
    reg_type: Option<&str>,
    data: Option<&str>,
    operator: &str,
    wow64: Option<&str>,
    fun: bool,
) -> Result<()> {
    if fun {
        return Err(anyhow!(
            "Interactive function expression '{}' is not supported in unattended apply",
            data.unwrap_or("")
        ));
    }
    let (hive, sub) =
        parse_reg_path(key).ok_or_else(|| anyhow!("Invalid registry key: {}", key))?;
    let root = RegKey::predef(hive);

    for sam in wow64_sam_list(wow64) {
        let (k, _) = root
            .create_subkey_with_flags(&sub, KEY_ALL_ACCESS | sam)
            .with_context(|| format!("Failed to open/create registry key {}", key))?;

        let Some(vname) = value else {
            // Key-only creation
            continue;
        };
        let raw_data = data.unwrap_or("");
        let rtype = reg_type.unwrap_or("REG_SZ").trim().to_ascii_uppercase();

        match rtype.as_str() {
            "REG_DWORD" => {
                let operand = parse_hex_u32(raw_data)
                    .ok_or_else(|| anyhow!("Invalid hex REG_DWORD data '{}'", raw_data))?;
                let existing = k
                    .get_raw_value(vname)
                    .ok()
                    .and_then(|rv| reg_value_to_u32(&rv))
                    .unwrap_or(0);
                let new_val = apply_bitwise_u32(operator, existing, operand)?;
                let rv = RegValue {
                    vtype: RegType::REG_DWORD,
                    bytes: new_val.to_le_bytes().to_vec(),
                };
                k.set_raw_value(vname, &rv)?;
            }
            "REG_QWORD" => {
                let operand = parse_hex_u64(raw_data)
                    .ok_or_else(|| anyhow!("Invalid hex REG_QWORD data '{}'", raw_data))?;
                let existing = k
                    .get_raw_value(vname)
                    .ok()
                    .and_then(|rv| reg_value_to_u64(&rv))
                    .unwrap_or(0);
                let new_val = apply_bitwise_u64(operator, existing, operand)?;
                let rv = RegValue {
                    vtype: RegType::REG_QWORD,
                    bytes: new_val.to_le_bytes().to_vec(),
                };
                k.set_raw_value(vname, &rv)?;
            }
            "REG_BINARY" => {
                let bytes = parse_hex_bytes(raw_data)
                    .ok_or_else(|| anyhow!("Invalid REG_BINARY data '{}'", raw_data))?;
                let rv = RegValue {
                    vtype: RegType::REG_BINARY,
                    bytes,
                };
                k.set_raw_value(vname, &rv)?;
            }
            "REG_EXPAND_SZ" => {
                let bytes = encode_utf16_sz(raw_data);
                let rv = RegValue {
                    vtype: RegType::REG_EXPAND_SZ,
                    bytes,
                };
                k.set_raw_value(vname, &rv)?;
            }
            "REG_MULTI_SZ" => {
                let bytes = encode_utf16_multi_sz(raw_data);
                let rv = RegValue {
                    vtype: RegType::REG_MULTI_SZ,
                    bytes,
                };
                k.set_raw_value(vname, &rv)?;
            }
            _ => {
                let expanded = expand_env_vars(raw_data, None);
                let bytes = encode_utf16_sz(&expanded);
                let rv = RegValue {
                    vtype: RegType::REG_SZ,
                    bytes,
                };
                k.set_raw_value(vname, &rv)?;
            }
        }
    }
    Ok(())
}

fn apply_bitwise_u32(operator: &str, existing: u32, operand: u32) -> Result<u32> {
    match operator.trim() {
        "" | "=" => Ok(operand),
        "|" => Ok(existing | operand),
        "&" => Ok(existing & operand),
        "^" => Ok(existing ^ operand),
        other => Err(anyhow!("Unsupported registry bitwise operator '{}'", other)),
    }
}

fn apply_bitwise_u64(operator: &str, existing: u64, operand: u64) -> Result<u64> {
    match operator.trim() {
        "" | "=" => Ok(operand),
        "|" => Ok(existing | operand),
        "&" => Ok(existing & operand),
        "^" => Ok(existing ^ operand),
        other => Err(anyhow!("Unsupported registry bitwise operator '{}'", other)),
    }
}

fn encode_utf16_sz(s: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((s.len() + 1) * 2);
    for w in OsStr::new(s).encode_wide().chain(Some(0)) {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes
}

fn encode_utf16_multi_sz(s: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    let parts: Vec<&str> = if s.contains('\0') {
        s.split('\0').filter(|p| !p.is_empty()).collect()
    } else {
        s.split('\n')
            .map(|p| p.trim_end_matches('\r'))
            .filter(|p| !p.is_empty())
            .collect()
    };
    for part in parts {
        bytes.extend_from_slice(&encode_utf16_sz(part));
    }
    // Terminate REG_MULTI_SZ list with an extra NUL code unit (or double NUL if empty).
    bytes.extend_from_slice(&0u16.to_le_bytes());
    if bytes.len() == 2 {
        bytes.extend_from_slice(&0u16.to_le_bytes());
    }
    bytes
}

fn is_skippable_reg_error(e: &std::io::Error, skip_error: Option<i32>) -> bool {
    e.kind() == ErrorKind::NotFound || skip_error.is_some_and(|code| e.raw_os_error() == Some(code))
}

pub(super) fn exec_reg_delete(
    key: &str,
    value: Option<&str>,
    skip_error: Option<i32>,
    wow64: Option<&str>,
) -> Result<()> {
    let (hive, sub) =
        parse_reg_path(key).ok_or_else(|| anyhow!("Invalid registry key: {}", key))?;
    let root = RegKey::predef(hive);

    for sam in wow64_sam_list(wow64) {
        match value {
            Some(vname) => match root.open_subkey_with_flags(&sub, KEY_ALL_ACCESS | sam) {
                Ok(k) => {
                    if let Err(e) = k.delete_value(vname) {
                        if !is_skippable_reg_error(&e, skip_error) {
                            return Err(e.into());
                        }
                    }
                }
                Err(e) => {
                    if !is_skippable_reg_error(&e, skip_error) {
                        return Err(e.into());
                    }
                }
            },
            None => {
                if let Some((parent_sub, leaf)) = split_parent_leaf(&sub) {
                    match root.open_subkey_with_flags(&parent_sub, KEY_ALL_ACCESS | sam) {
                        Ok(pk) => {
                            if let Err(e) = pk.delete_subkey_all(&leaf) {
                                if !is_skippable_reg_error(&e, skip_error) {
                                    return Err(e.into());
                                }
                            }
                        }
                        Err(e) => {
                            if !is_skippable_reg_error(&e, skip_error) {
                                return Err(e.into());
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub(super) fn exec_reg_move(
    key: &str,
    new_key: &str,
    value: Option<&str>,
    new_value: Option<&str>,
    skip_error: Option<i32>,
    wow64: Option<&str>,
) -> Result<()> {
    let (src_hive, src_sub) =
        parse_reg_path(key).ok_or_else(|| anyhow!("Invalid source key: {}", key))?;
    let (dst_hive, dst_sub) =
        parse_reg_path(new_key).ok_or_else(|| anyhow!("Invalid dest key: {}", new_key))?;

    for sam in wow64_sam_list(wow64) {
        let src_root = RegKey::predef(src_hive);
        let dst_root = RegKey::predef(dst_hive);
        match value {
            Some(vname) => {
                let dst_vname = new_value.unwrap_or(vname);
                match src_root.open_subkey_with_flags(&src_sub, KEY_ALL_ACCESS | sam) {
                    Ok(sk) => match sk.get_raw_value(vname) {
                        Ok(rv) => {
                            let (dk, _) = dst_root
                                .create_subkey_with_flags(&dst_sub, KEY_ALL_ACCESS | sam)?;
                            dk.set_raw_value(dst_vname, &rv)?;
                            if let Err(e) = sk.delete_value(vname) {
                                if !is_skippable_reg_error(&e, skip_error) {
                                    return Err(e.into());
                                }
                            }
                        }
                        Err(e) if is_skippable_reg_error(&e, skip_error) => {}
                        Err(e) => return Err(e.into()),
                    },
                    Err(e) if is_skippable_reg_error(&e, skip_error) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            None => match src_root.open_subkey_with_flags(&src_sub, KEY_READ | sam) {
                Ok(sk) => {
                    let tree = read_tree_recursive(&sk, sam)?;
                    drop(sk);
                    let (dk, _) =
                        dst_root.create_subkey_with_flags(&dst_sub, KEY_ALL_ACCESS | sam)?;
                    write_tree_recursive(&dk, &tree, sam)?;
                    if let Some((parent_sub, leaf)) = split_parent_leaf(&src_sub) {
                        match src_root.open_subkey_with_flags(&parent_sub, KEY_ALL_ACCESS | sam) {
                            Ok(pk) => {
                                if let Err(e) = pk.delete_subkey_all(&leaf) {
                                    if !is_skippable_reg_error(&e, skip_error) {
                                        return Err(e.into());
                                    }
                                }
                            }
                            Err(e) if is_skippable_reg_error(&e, skip_error) => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                }
                Err(e) if is_skippable_reg_error(&e, skip_error) => {}
                Err(e) => return Err(e.into()),
            },
        }
    }
    Ok(())
}

pub(super) fn exec_explorer_notify(
    notify_type: &str,
    msg: Option<&str>,
    w_param: Option<&str>,
    l_param: Option<&str>,
    cmd: Option<&str>,
) -> Result<()> {
    match notify_type.trim() {
        "AssocChanged" => {
            unsafe {
                SHChangeNotify(
                    SHCNE_ASSOCCHANGED as i32,
                    SHCNF_IDLIST,
                    std::ptr::null(),
                    std::ptr::null(),
                );
            }
            Ok(())
        }
        "Custom" => {
            let msg_num = msg
                .and_then(parse_hex_u32)
                .ok_or_else(|| anyhow!("Invalid ExplorerNotify msg"))?;
            let wp = w_param
                .and_then(|s| s.strip_prefix('#').and_then(parse_hex_u32))
                .unwrap_or(0) as usize;
            let lp_wide: Option<Vec<u16>> = l_param.and_then(|s| {
                if s.starts_with('#') || s.eq_ignore_ascii_case("NULL") {
                    None
                } else {
                    Some(OsStr::new(s).encode_wide().chain(Some(0)).collect())
                }
            });
            let lp_val: isize = match (&lp_wide, l_param) {
                (Some(buf), _) => buf.as_ptr() as isize,
                (None, Some(s)) if s.starts_with('#') => parse_hex_u32(s).unwrap_or(0) as isize,
                _ => 0,
            };
            let mut result: usize = 0;
            unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    msg_num,
                    wp,
                    lp_val,
                    SMTO_ABORTIFHUNG,
                    300,
                    &mut result,
                );
            }
            Ok(())
        }
        "Cmd" => {
            if let Some(c) = cmd {
                let expanded = expand_env_vars(c, None);
                let (exe, args): (&str, &[&str]) =
                    match expanded.trim().to_ascii_lowercase().as_str() {
                        "taskkill /f /im startmenuexperiencehost.exe" => (
                            "taskkill.exe",
                            &["/f", "/im", "StartMenuExperienceHost.exe"],
                        ),
                        "powercfg -hibernate on" => ("powercfg.exe", &["-hibernate", "on"]),
                        "powercfg -hibernate off" => ("powercfg.exe", &["-hibernate", "off"]),
                        "netsh wfp set options netevents=on" => {
                            ("netsh.exe", &["wfp", "set", "options", "netevents=on"])
                        }
                        "netsh wfp set options netevents=off" => {
                            ("netsh.exe", &["wfp", "set", "options", "netevents=off"])
                        }
                        _ => {
                            return Err(anyhow!(
                                "Unsupported optimization command '{}'",
                                expanded.trim()
                            ));
                        }
                    };
                let output = crate::system_tools::system32_command(exe)?
                    .args(args)
                    .creation_flags(CREATE_NO_WINDOW)
                    .output()
                    .with_context(|| format!("Could not start {exe}"))?;
                let code = output.status.code().unwrap_or(-1);
                // There is nothing to refresh if the Start menu process is already stopped.
                if !output.status.success() && !(is_start_menu_refresh(&expanded) && code == 128) {
                    let stderr = crate::systasks::decode_cmd_output(&output.stderr);
                    let stdout = crate::systasks::decode_cmd_output(&output.stdout);
                    return Err(anyhow!(
                        "{exe} exited with code {code}: {}",
                        format!("{stdout}\n{stderr}").trim()
                    ));
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Generates a minimal valid 16x16 transparent ICO file at target path (replaces Blank.ico extraction).
pub(super) fn exec_create_blank_ico(path_tmpl: &str) -> Result<()> {
    let expanded = expand_env_vars(path_tmpl, None);
    let target = Path::new(&expanded);
    if is_unc_path(target) {
        return Err(anyhow!(
            "Refusing to create Blank.ico on UNC path: {}",
            target.display()
        ));
    }
    if has_reparse_point_in_ancestors(target, true) {
        return Err(anyhow!(
            "Refusing to write Blank.ico across reparse point: {}",
            target.display()
        ));
    }
    let abs = std::path::absolute(target).unwrap_or_else(|_| target.to_path_buf());
    if crate::diskmap::is_protected_recycle_target(&abs) {
        return Err(anyhow!(
            "Refusing to write Blank.ico to protected target: {}",
            abs.display()
        ));
    }
    if target.symlink_metadata().is_ok() {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    // Minimal 16x16 32-bpp transparent ICO (ICONDIR + ICONDIRENTRY + BITMAPINFOHEADER + XOR/AND masks)
    let mut ico = Vec::with_capacity(22 + 40 + 16 * 16 * 4 + 16 * 4);
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // ICONDIR: 1 icon
    ico.extend_from_slice(&[16, 16, 0, 0, 1, 0, 32, 0]); // 16x16, 32bpp
    let img_size: u32 = 40 + 16 * 16 * 4 + 16 * 4;
    ico.extend_from_slice(&img_size.to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes()); // offset
                                                 // BITMAPINFOHEADER
    ico.extend_from_slice(&40u32.to_le_bytes());
    ico.extend_from_slice(&16i32.to_le_bytes());
    ico.extend_from_slice(&32i32.to_le_bytes()); // 2x height
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&32u16.to_le_bytes());
    ico.extend_from_slice(&[0u8; 24]);
    ico.extend_from_slice(&vec![0u8; 16 * 16 * 4]); // transparent RGBA
    ico.extend_from_slice(&[0xFFu8; 16 * 4]); // AND mask = all transparent
    std::fs::write(target, ico)?;
    Ok(())
}
