use anyhow::Result;
use std::ffi::c_void;
use std::io;
use std::marker::PhantomData;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::rc::Rc;
use windows_sys::Win32::Foundation::{LocalFree, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Threading::{
    CreateMutexExW, ReleaseMutex, WaitForSingleObject, MUTEX_MODIFY_STATE,
    SYNCHRONIZATION_SYNCHRONIZE,
};

#[derive(Debug)]
pub struct CleanupBusy;

impl std::fmt::Display for CleanupBusy {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(
            "Another DismNext process is modifying Windows. Try again after it finishes.",
        )
    }
}

impl std::error::Error for CleanupBusy {}

/// A Windows mutex is released by the same thread that acquired it.
pub struct CleanupLease(OwnedHandle, PhantomData<Rc<()>>);

impl CleanupLease {
    pub fn try_acquire() -> Result<Self> {
        Self::acquire_named(r"Global\DismNext-Cleanup-v1")
    }

    fn acquire_named(name: &str) -> Result<Self> {
        // Medium integrity allows standard and elevated processes to coordinate.
        // Authenticated users only need to wait and release.
        let descriptor: Vec<_> = "D:P(A;;0x00100001;;;AU)(A;;GA;;;SY)(A;;GA;;;BA)S:(ML;;NW;;;ME)"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut security: *mut c_void = null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                descriptor.as_ptr(),
                SDDL_REVISION_1,
                &mut security,
                null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security,
            bInheritHandle: 0,
        };
        let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe {
            CreateMutexExW(
                &attributes,
                name.as_ptr(),
                0,
                MUTEX_MODIFY_STATE | SYNCHRONIZATION_SYNCHRONIZE,
            )
        };
        let error = io::Error::last_os_error();
        unsafe { LocalFree(security) };
        if raw.is_null() {
            return Err(error.into());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        match unsafe { WaitForSingleObject(raw, 0) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle, PhantomData)),
            WAIT_TIMEOUT => Err(CleanupBusy.into()),
            _ => Err(io::Error::last_os_error().into()),
        }
    }
}

impl Drop for CleanupLease {
    fn drop(&mut self) {
        unsafe { ReleaseMutex(self.0.as_raw_handle()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_lease_excludes_other_threads_until_release() {
        let name = format!(r"Global\DismNext-LeaseTest-{}", std::process::id());
        let acquired = CleanupLease::acquire_named(&name).unwrap();
        std::thread::scope(|scope| {
            scope
                .spawn(|| match CleanupLease::acquire_named(&name) {
                    Err(error) => assert!(error.is::<CleanupBusy>()),
                    Ok(_) => panic!("The mutation lease was granted to two threads"),
                })
                .join()
                .unwrap();
        });
        drop(acquired);
        CleanupLease::acquire_named(&name).unwrap();
    }

    #[test]
    fn named_lease_excludes_another_process() {
        const FIXTURE: &str = "DISMNEXT_TEST_LEASE_NAME";
        if let Ok(name) = std::env::var(FIXTURE) {
            assert!(
                matches!(CleanupLease::acquire_named(&name), Err(error) if error.is::<CleanupBusy>())
            );
            return;
        }
        let name = format!(r"Global\DismNext-LeaseProcessTest-{}", std::process::id());
        let _lease = CleanupLease::acquire_named(&name).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cleaner::lease::tests::named_lease_excludes_another_process",
            ])
            .env(FIXTURE, name)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}",
            String::from_utf8_lossy(&child.stdout)
        );
    }
}
