use anyhow::{ensure, Context, Result};
use roxmltree::{Document, Node, ParsingOptions};
use serde::Serialize;
use std::ffi::{c_void, CStr};
use std::fs;
use std::io;
use std::mem::{size_of, transmute};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{
    FreeLibrary, LocalFree, BOOL, ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, FARPROC,
    GENERIC_READ, HANDLE, HMODULE, INVALID_HANDLE_VALUE, MAX_PATH,
};
use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

const MAX_XML_BYTES: usize = 16 * 1024 * 1024;
const MOUNT_INFO_LEVEL1: u32 = 1;
const WIM_MOUNTED: u32 = 0x01;
const WIM_MOUNTING: u32 = 0x02;
const WIM_REMOUNTABLE: u32 = 0x04;
const WIM_INVALID_FLAGS: u32 = 0x08 | 0x10 | 0x20 | 0x40;
const WIM_READ_WRITE: u32 = 0x100;

type CreateWim = unsafe extern "system" fn(*const u16, u32, u32, u32, u32, *mut u32) -> HANDLE;
type CloseWim = unsafe extern "system" fn(HANDLE) -> BOOL;
type GetImageInformation = unsafe extern "system" fn(HANDLE, *mut *mut c_void, *mut u32) -> BOOL;
type GetMountedImageInfo =
    unsafe extern "system" fn(u32, *mut u32, *mut c_void, u32, *mut u32) -> BOOL;

#[derive(Debug, Serialize)]
pub struct WimInspection {
    pub path: String,
    pub images: Vec<WimImage>,
    pub mounts: WimMounts,
}

#[derive(Debug, Serialize)]
pub struct WimImage {
    pub index: u32,
    pub name: Option<String>,
    pub description: Option<String>,
    pub edition: Option<String>,
    pub architecture: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WimMounts {
    Available { entries: Vec<MountedWim> },
    AccessDenied,
    Unavailable { error: String },
}

#[derive(Debug, Serialize)]
pub struct MountedWim {
    pub image_path: String,
    pub mount_path: String,
    pub index: u32,
    pub state: WimMountState,
    pub read_write: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WimMountState {
    Mounted,
    Mounting,
    Remountable,
    Invalid,
    Unknown,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct NativeMountInfo {
    image_path: [u16; MAX_PATH as usize],
    mount_path: [u16; MAX_PATH as usize],
    index: u32,
    flags: u32,
}

impl From<NativeMountInfo> for MountedWim {
    fn from(mount: NativeMountInfo) -> Self {
        let state = match mount.flags {
            flags if flags & WIM_INVALID_FLAGS != 0 => WimMountState::Invalid,
            flags if flags & WIM_MOUNTING != 0 => WimMountState::Mounting,
            flags if flags & WIM_MOUNTED != 0 => WimMountState::Mounted,
            flags if flags & WIM_REMOUNTABLE != 0 => WimMountState::Remountable,
            _ => WimMountState::Unknown,
        };
        Self {
            image_path: null_terminated_string(&mount.image_path),
            mount_path: null_terminated_string(&mount.mount_path),
            index: mount.index,
            state,
            read_write: mount.flags & WIM_READ_WRITE != 0,
        }
    }
}

struct ImagingLibrary(HMODULE);

impl ImagingLibrary {
    fn load() -> Result<Self> {
        let name: Vec<_> = "wimgapi.dll".encode_utf16().chain(Some(0)).collect();
        let module =
            unsafe { LoadLibraryExW(name.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        if module.is_null() {
            return Err(io::Error::last_os_error()).context("Loading Windows Imaging API");
        }
        Ok(Self(module))
    }

    fn procedure(&self, name: &CStr) -> Result<unsafe extern "system" fn() -> isize> {
        let procedure: FARPROC = unsafe { GetProcAddress(self.0, name.as_ptr().cast()) };
        procedure
            .with_context(|| format!("Windows Imaging API is missing {}", name.to_string_lossy()))
    }
}

impl Drop for ImagingLibrary {
    fn drop(&mut self) {
        unsafe { FreeLibrary(self.0) };
    }
}

struct WimHandle {
    value: HANDLE,
    close: CloseWim,
}

impl Drop for WimHandle {
    fn drop(&mut self) {
        unsafe { (self.close)(self.value) };
    }
}

struct ImagingBuffer(*mut c_void);

impl Drop for ImagingBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Inspects the image catalog and Windows' current WIM mount inventory.
pub fn inspect_wim(path: &Path) -> Result<WimInspection> {
    ensure!(
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("wim")),
        "Select a .wim image file"
    );
    let path = std::path::absolute(path)?;
    ensure!(
        fs::metadata(&path)
            .with_context(|| format!("Reading image file '{}'", path.display()))?
            .is_file(),
        "The image path must be a file"
    );
    let library = ImagingLibrary::load()?;
    let images = read_images(&library, &path)?;
    let mounts = match read_mounts(&library) {
        Ok(entries) => WimMounts::Available { entries },
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|cause| cause.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)) =>
        {
            WimMounts::AccessDenied
        }
        Err(error) => WimMounts::Unavailable {
            error: format!("{error:#}"),
        },
    };
    Ok(WimInspection {
        path: path.to_string_lossy().into_owned(),
        images,
        mounts,
    })
}

fn read_images(library: &ImagingLibrary, path: &Path) -> Result<Vec<WimImage>> {
    let create: CreateWim = unsafe { transmute(library.procedure(c"WIMCreateFile")?) };
    let close: CloseWim = unsafe { transmute(library.procedure(c"WIMCloseHandle")?) };
    let get_info: GetImageInformation =
        unsafe { transmute(library.procedure(c"WIMGetImageInformation")?) };
    let mut name: Vec<_> = path.as_os_str().encode_wide().collect();
    ensure!(
        !name.contains(&0),
        "The image path contains a null character"
    );
    name.push(0);
    let value = unsafe { create(name.as_ptr(), GENERIC_READ, OPEN_EXISTING, 0, 0, null_mut()) };
    if value.is_null() || value == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error()).context("Opening the WIM image for reading");
    }
    let handle = WimHandle { value, close };
    let mut buffer = ImagingBuffer(null_mut());
    let mut length = 0;
    if unsafe { get_info(handle.value, &mut buffer.0, &mut length) } == 0 {
        return Err(io::Error::last_os_error()).context("Reading WIM image metadata");
    }
    let length = length as usize;
    ensure!(
        !buffer.0.is_null() && (2..=MAX_XML_BYTES).contains(&length) && length % 2 == 0,
        "Windows returned invalid WIM metadata"
    );
    let utf16 = unsafe { std::slice::from_raw_parts(buffer.0.cast::<u16>(), length / 2) };
    let metadata = String::from_utf16(utf16).context("Decoding WIM metadata")?;
    parse_images(
        metadata
            .trim_start_matches('\u{feff}')
            .trim_end_matches('\0'),
    )
}

fn read_mounts(library: &ImagingLibrary) -> Result<Vec<MountedWim>> {
    let get_mounts: GetMountedImageInfo =
        unsafe { transmute(library.procedure(c"WIMGetMountedImageInfo")?) };
    let mut entries = Vec::<NativeMountInfo>::new();
    let mut count = 0;
    let mut required = 0;
    let blank = NativeMountInfo {
        image_path: [0; MAX_PATH as usize],
        mount_path: [0; MAX_PATH as usize],
        index: 0,
        flags: 0,
    };
    for _ in 0..3 {
        let capacity = (entries.len() * size_of::<NativeMountInfo>()) as u32;
        let buffer = if entries.is_empty() {
            null_mut()
        } else {
            entries.as_mut_ptr().cast()
        };
        let ok = unsafe {
            get_mounts(
                MOUNT_INFO_LEVEL1,
                &mut count,
                buffer,
                capacity,
                &mut required,
            )
        };
        if ok != 0 {
            ensure!(
                count as usize <= entries.len(),
                "Windows returned an invalid WIM mount count"
            );
            entries.truncate(count as usize);
            return Ok(entries.into_iter().map(MountedWim::from).collect());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(error).context("Reading the Windows WIM mount list");
        }
        ensure!(
            required > capacity && required <= 4 * 1024 * 1024,
            "Windows returned an invalid WIM mount buffer size"
        );
        entries.resize(
            (required as usize).div_ceil(size_of::<NativeMountInfo>()),
            blank,
        );
    }
    anyhow::bail!("WIM mount list changed during inspection")
}

fn null_terminated_string(value: &[u16]) -> String {
    let end = value
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..end])
}

fn parse_images(xml: &str) -> Result<Vec<WimImage>> {
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            nodes_limit: 250_000,
            ..ParsingOptions::default()
        },
    )
    .context("Parsing WIM metadata")?;
    let root = document.root_element();
    ensure!(
        root.has_tag_name("WIM"),
        "WIM metadata has an unexpected root element"
    );
    let mut images = Vec::new();
    for image in root.children().filter(|node| node.has_tag_name("IMAGE")) {
        let index: u32 = image
            .attribute("INDEX")
            .context("WIM image is missing its index")?
            .parse()
            .context("WIM image index is invalid")?;
        ensure!(index != 0, "WIM image indices start at 1");
        let windows = child(image, "WINDOWS");
        let version = windows
            .and_then(|node| child(node, "VERSION"))
            .and_then(|node| {
                let major = text(node, "MAJOR")?;
                let minor = text(node, "MINOR")?;
                let build = text(node, "BUILD")?;
                Some(match text(node, "SPBUILD") {
                    Some(revision) => format!("{major}.{minor}.{build}.{revision}"),
                    None => format!("{major}.{minor}.{build}"),
                })
            });
        let architecture = windows.and_then(|node| text(node, "ARCH")).map(|code| {
            match code {
                "0" => "x86",
                "5" => "ARM",
                "6" => "IA-64",
                "9" => "x64",
                "12" => "ARM64",
                other => other,
            }
            .to_string()
        });
        images.push(WimImage {
            index,
            name: text(image, "DISPLAYNAME")
                .or_else(|| text(image, "NAME"))
                .map(str::to_string),
            description: text(image, "DISPLAYDESCRIPTION")
                .or_else(|| text(image, "DESCRIPTION"))
                .map(str::to_string),
            edition: windows
                .and_then(|node| text(node, "EDITIONID"))
                .map(str::to_string),
            architecture,
            version,
        });
    }
    images.sort_by_key(|image| image.index);
    ensure!(
        images.windows(2).all(|pair| pair[0].index != pair[1].index),
        "WIM metadata contains duplicate image indices"
    );
    Ok(images)
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|child| child.has_tag_name(name))
}

fn text<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    let value = child(node, name)?.text()?.trim();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_multiple_wim_indices_and_optional_windows_metadata() {
        let xml = r#"<WIM>
          <IMAGE INDEX="2"><NAME>Recovery &amp; Tools</NAME></IMAGE>
          <IMAGE INDEX="1"><DISPLAYNAME>Windows 11 Pro</DISPLAYNAME><WINDOWS>
            <ARCH>9</ARCH><EDITIONID>Professional</EDITIONID>
            <VERSION><MAJOR>10</MAJOR><MINOR>0</MINOR><BUILD>26100</BUILD><SPBUILD>1000</SPBUILD></VERSION>
          </WINDOWS></IMAGE>
        </WIM>"#;
        let images = parse_images(xml).unwrap();
        assert_eq!(images[0].index, 1);
        assert_eq!(images[0].name.as_deref(), Some("Windows 11 Pro"));
        assert_eq!(images[0].edition.as_deref(), Some("Professional"));
        assert_eq!(images[0].architecture.as_deref(), Some("x64"));
        assert_eq!(images[0].version.as_deref(), Some("10.0.26100.1000"));
        assert_eq!(images[1].index, 2);
        assert_eq!(images[1].name.as_deref(), Some("Recovery & Tools"));
        assert!(images[1].version.is_none());
        assert!(parse_images("<WIM><IMAGE INDEX=\"0\"/></WIM>").is_err());
        assert!(parse_images("<WIM><IMAGE INDEX=\"1\"/><IMAGE INDEX=\"1\"/></WIM>").is_err());
    }

    #[test]
    fn mount_problem_flags_take_precedence_over_mounted_flag() {
        let entry = NativeMountInfo {
            image_path: [0; MAX_PATH as usize],
            mount_path: [0; MAX_PATH as usize],
            index: 2,
            flags: WIM_MOUNTED | 0x10 | WIM_READ_WRITE,
        };
        let mount = MountedWim::from(entry);
        assert_eq!(mount.state, WimMountState::Invalid);
        assert!(mount.read_write);
    }
}
