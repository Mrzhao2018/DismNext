use std::{ffi::c_void, io, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        SECURITY_ATTRIBUTES,
    },
};

pub(crate) fn with_security<T>(
    sddl: &str,
    create: impl FnOnce(&SECURITY_ATTRIBUTES) -> io::Result<T>,
) -> io::Result<T> {
    struct Descriptor(*mut c_void);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    let sddl: Vec<_> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut raw = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut raw,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let security = Descriptor(raw);
    create(&SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0,
        bInheritHandle: 0,
    })
}
