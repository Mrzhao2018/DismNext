use super::StartupSignature;
use std::fmt::Write;
use std::fs::{File, OpenOptions};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    CERT_E_CHAINING, CERT_E_EXPIRED, CERT_E_REVOKED, CERT_E_UNTRUSTEDROOT, HANDLE,
    TRUST_E_BAD_DIGEST, TRUST_E_EXPLICIT_DISTRUST, TRUST_E_NOSIGNATURE, TRUST_E_PROVIDER_UNKNOWN,
    TRUST_E_SUBJECT_FORM_UNKNOWN, TRUST_E_SUBJECT_NOT_TRUSTED,
};
use windows_sys::Win32::Security::Cryptography::Catalog::{
    CryptCATAdminAcquireContext2, CryptCATAdminCalcHashFromFileHandle2,
    CryptCATAdminEnumCatalogFromHash, CryptCATAdminReleaseCatalogContext,
    CryptCATAdminReleaseContext, CryptCATCatalogInfoFromContext, CATALOG_INFO,
};
use windows_sys::Win32::Security::Cryptography::{
    CertGetNameStringW, CERT_NAME_SIMPLE_DISPLAY_TYPE,
};
use windows_sys::Win32::Security::WinTrust::{
    WTHelperGetProvCertFromChain, WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData,
    WinVerifyTrust, DRIVER_ACTION_VERIFY, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_CATALOG_INFO,
    WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL,
    WTD_CHOICE_CATALOG, WTD_CHOICE_FILE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UI_NONE,
};
use windows_sys::Win32::Storage::FileSystem::{VerQueryValueW, FILE_SHARE_READ};

pub(super) fn inspect(path: &Path) -> (StartupSignature, Option<String>) {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
    else {
        return (StartupSignature::Unavailable, None);
    };
    let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide.as_ptr(),
        hFile: file.as_raw_handle(),
        pgKnownSubject: null_mut(),
    };
    let (code, signer) = verify(WTD_CHOICE_FILE, WINTRUST_DATA_0 { pFile: &mut info });
    if matches!(
        code,
        TRUST_E_NOSIGNATURE | TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN
    ) {
        let mut best = None;
        for algorithm in ["SHA256", "SHA1"] {
            if let Some((catalog_code, catalog_signer)) = catalog_signature(&file, &wide, algorithm)
            {
                if catalog_code == 0 {
                    return (StartupSignature::Valid, catalog_signer);
                }
                if best.is_none() {
                    best = Some((
                        signature_status(catalog_code, catalog_signer.is_some()),
                        catalog_signer,
                    ));
                }
            }
        }
        if let Some(catalog_result) = best {
            return catalog_result;
        }
    }
    (signature_status(code, signer.is_some()), signer)
}

fn signature_status(code: i32, has_signer: bool) -> StartupSignature {
    match code {
        0 => StartupSignature::Valid,
        TRUST_E_NOSIGNATURE => StartupSignature::Unsigned,
        TRUST_E_SUBJECT_FORM_UNKNOWN | TRUST_E_PROVIDER_UNKNOWN => StartupSignature::Unsupported,
        TRUST_E_BAD_DIGEST => StartupSignature::HashMismatch,
        CERT_E_CHAINING
        | CERT_E_EXPIRED
        | CERT_E_REVOKED
        | CERT_E_UNTRUSTEDROOT
        | TRUST_E_EXPLICIT_DISTRUST
        | TRUST_E_SUBJECT_NOT_TRUSTED => StartupSignature::Untrusted,
        _ if has_signer => StartupSignature::Untrusted,
        _ => StartupSignature::Unavailable,
    }
}

fn verify(choice: u32, subject: WINTRUST_DATA_0) -> (i32, Option<String>) {
    let mut data: WINTRUST_DATA = unsafe { zeroed() };
    data.cbStruct = size_of::<WINTRUST_DATA>() as u32;
    data.dwUnionChoice = choice;
    data.Anonymous = subject;
    data.dwUIChoice = WTD_UI_NONE;
    data.fdwRevocationChecks = WTD_REVOKE_NONE;
    data.dwProvFlags = WTD_CACHE_ONLY_URL_RETRIEVAL;
    data.dwStateAction = WTD_STATEACTION_VERIFY;
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    unsafe {
        let code = WinVerifyTrust(
            null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        );
        let signer = signer_name(data.hWVTStateData);
        data.dwStateAction = WTD_STATEACTION_CLOSE;
        WinVerifyTrust(
            null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        );
        (code, signer)
    }
}

unsafe fn signer_name(state: HANDLE) -> Option<String> {
    if state.is_null() {
        return None;
    }
    let provider = unsafe { WTHelperProvDataFromStateData(state).as_mut()? };
    let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, 0, 0).as_mut()? };
    let certificate = unsafe { WTHelperGetProvCertFromChain(signer, 0).as_ref()? }.pCert;
    if certificate.is_null() {
        return None;
    }
    let size = unsafe {
        CertGetNameStringW(
            certificate,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            null(),
            null_mut(),
            0,
        )
    };
    if size <= 1 {
        return None;
    }
    let mut name = vec![0; size as usize];
    let written = unsafe {
        CertGetNameStringW(
            certificate,
            CERT_NAME_SIMPLE_DISPLAY_TYPE,
            0,
            null(),
            name.as_mut_ptr(),
            size,
        )
    };
    (written > 1).then(|| String::from_utf16_lossy(&name[..written as usize - 1]))
}

struct CatalogAdmin(isize);

impl Drop for CatalogAdmin {
    fn drop(&mut self) {
        unsafe {
            CryptCATAdminReleaseContext(self.0, 0);
        }
    }
}

struct CatalogEntry<'a>(&'a CatalogAdmin, isize);

impl Drop for CatalogEntry<'_> {
    fn drop(&mut self) {
        unsafe {
            CryptCATAdminReleaseCatalogContext(self.0 .0, self.1, 0);
        }
    }
}

fn catalog_signature(file: &File, path: &[u16], algorithm: &str) -> Option<(i32, Option<String>)> {
    let algorithm: Vec<_> = algorithm.encode_utf16().chain(Some(0)).collect();
    let mut admin = 0;
    if unsafe {
        CryptCATAdminAcquireContext2(
            &mut admin,
            &DRIVER_ACTION_VERIFY,
            algorithm.as_ptr(),
            null(),
            0,
        )
    } == 0
    {
        return None;
    }
    let admin = CatalogAdmin(admin);
    let mut hash = [0u8; 64];
    let mut size = hash.len() as u32;
    if unsafe {
        CryptCATAdminCalcHashFromFileHandle2(
            admin.0,
            file.as_raw_handle(),
            &mut size,
            hash.as_mut_ptr(),
            0,
        )
    } == 0
    {
        return None;
    }
    let catalog =
        unsafe { CryptCATAdminEnumCatalogFromHash(admin.0, hash.as_ptr(), size, 0, null_mut()) };
    if catalog == 0 {
        return None;
    }
    let catalog = CatalogEntry(&admin, catalog);
    let mut info: CATALOG_INFO = unsafe { zeroed() };
    info.cbStruct = size_of::<CATALOG_INFO>() as u32;
    if unsafe { CryptCATCatalogInfoFromContext(catalog.1, &mut info, 0) } == 0 {
        return None;
    }
    let mut tag = String::with_capacity(size as usize * 2);
    for byte in hash.get(..size as usize)? {
        write!(tag, "{byte:02X}").ok()?;
    }
    let tag: Vec<_> = tag.encode_utf16().chain(Some(0)).collect();
    let mut subject = WINTRUST_CATALOG_INFO {
        cbStruct: size_of::<WINTRUST_CATALOG_INFO>() as u32,
        dwCatalogVersion: 0,
        pcwszCatalogFilePath: info.wszCatalogFile.as_ptr(),
        pcwszMemberTag: tag.as_ptr(),
        pcwszMemberFilePath: path.as_ptr(),
        hMemberFile: file.as_raw_handle(),
        pbCalculatedFileHash: hash.as_mut_ptr(),
        cbCalculatedFileHash: size,
        pcCatalogContext: null_mut(),
        hCatAdmin: admin.0,
    };
    Some(verify(
        WTD_CHOICE_CATALOG,
        WINTRUST_DATA_0 {
            pCatalog: &mut subject,
        },
    ))
}

pub(super) fn file_company(path: &Path) -> Option<String> {
    let bytes = crate::evaluator::read_version_resource(path)?;
    let translations = query_version::<u8>(&bytes, r"\VarFileInfo\Translation").unwrap_or_default();
    let languages = translations.as_chunks::<4>().0.iter().map(|language| {
        (
            u16::from_le_bytes([language[0], language[1]]),
            u16::from_le_bytes([language[2], language[3]]),
        )
    });
    languages
        .chain([(0x0409, 0x04b0), (0x0409, 0x04e4)])
        .find_map(|(language, codepage)| {
            let key = format!(r"\StringFileInfo\{language:04x}{codepage:04x}\CompanyName");
            let wide = query_version::<u16>(&bytes, &key)?;
            let text = String::from_utf16_lossy(wide.split(|&letter| letter == 0).next()?);
            (!text.trim().is_empty()).then_some(text)
        })
}

fn query_version<'a, T>(bytes: &'a [u8], key: &str) -> Option<&'a [T]> {
    let key: Vec<_> = key.encode_utf16().chain(Some(0)).collect();
    let mut value = null_mut();
    let mut len = 0;
    let found =
        unsafe { VerQueryValueW(bytes.as_ptr().cast(), key.as_ptr(), &mut value, &mut len) };
    if found == 0 || value.is_null() {
        return None;
    }
    let offset = (value as usize).checked_sub(bytes.as_ptr() as usize)?;
    let byte_len = (len as usize).checked_mul(size_of::<T>())?;
    if offset.checked_add(byte_len)? > bytes.len() || !value.cast::<T>().is_aligned() {
        return None;
    }
    // Only u8 and u16 are queried. All their bit patterns are valid.
    Some(unsafe { std::slice::from_raw_parts(value.cast::<T>(), len as usize) })
}
