use windows::core::HRESULT;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

const RPC_E_CHANGED_MODE_HR: HRESULT = HRESULT(0x8001_0106u32 as i32);

pub(crate) struct Apartment {
    should_uninitialize: bool,
}

impl Apartment {
    /// Initializes COM in STA mode. Returns `Err(RPC_E_CHANGED_MODE)` if this thread is already MTA.
    pub(crate) fn initialize() -> windows::core::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
        Ok(Self {
            should_uninitialize: true,
        })
    }

    /// Ensures COM is initialized (preferring STA, or reusing an existing MTA apartment).
    pub(crate) fn initialize_any() -> windows::core::Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if hr.is_ok() {
            Ok(Self {
                should_uninitialize: true,
            })
        } else if hr == RPC_E_CHANGED_MODE_HR {
            Ok(Self {
                should_uninitialize: false,
            })
        } else {
            Err(hr.into())
        }
    }

    pub(crate) fn is_changed_mode(error: &windows::core::Error) -> bool {
        error.code() == RPC_E_CHANGED_MODE_HR
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.should_uninitialize {
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::COINIT_MULTITHREADED;

    #[test]
    fn initialize_any_succeeds_inside_mta_thread_and_sta_detects_changed_mode() {
        std::thread::spawn(|| {
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            assert!(hr.is_ok());
            let err = Apartment::initialize()
                .err()
                .expect("STA on MTA should fail");
            assert!(Apartment::is_changed_mode(&err));
            let any =
                Apartment::initialize_any().expect("MTA should be accepted by initialize_any");
            assert!(!any.should_uninitialize);
            drop(any);
            unsafe { CoUninitialize() };
        })
        .join()
        .unwrap();
    }
}
