use crate::com::Apartment;
use crate::diskmap::is_protected_recycle_target;
use anyhow::{anyhow, Result};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use windows::core::{implement, Ref, HRESULT, PCWSTR};
use windows::Win32::Foundation::E_ABORT;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, IFileOperationProgressSink, IFileOperationProgressSink_Impl,
    IShellItem, SHCreateItemFromParsingName, FOFX_RECYCLEONDELETE, FOF_NOCONFIRMATION,
    FOF_NOERRORUI, FOF_NO_CONNECTED_ELEMENTS, FOF_SILENT, TSF_DELETE_RECYCLE_IF_POSSIBLE,
};

// Keep the number of Shell items and verification handles bounded.
const SHELL_BATCH_SIZE: usize = 64;

pub(crate) struct Verified<T> {
    pub value: T,
    pub hold: Option<File>,
}

pub(crate) struct RecycleRequest<T> {
    pub path: PathBuf,
    pub verify: Box<dyn FnOnce() -> Result<Verified<T>> + Send>,
}

struct ShellEntry<T> {
    path: PathBuf,
    state: Mutex<EntryState<T>>,
}

enum EntryState<T> {
    Pending(Box<dyn FnOnce() -> Result<Verified<T>> + Send>),
    Verified(Verified<T>),
    Rejected(anyhow::Error),
    Finished(Result<T>),
    Empty,
}

#[implement(IFileOperationProgressSink)]
struct DeleteSink<T>(Arc<ShellEntry<T>>)
where
    T: Send + 'static;

impl<T: Send + 'static> IFileOperationProgressSink_Impl for DeleteSink_Impl<T> {
    fn PreDeleteItem(&self, flags: u32, _item: Ref<IShellItem>) -> windows::core::Result<()> {
        let mut state = self.0.state.lock().unwrap();
        if !matches!(*state, EntryState::Pending(_)) {
            return Err(E_ABORT.into());
        }
        let EntryState::Pending(verify) = std::mem::replace(&mut *state, EntryState::Empty) else {
            return Err(E_ABORT.into());
        };
        let verified = catch_unwind(AssertUnwindSafe(verify))
            .unwrap_or_else(|_| Err(anyhow!("Recycle target verification stopped unexpectedly")))
            .and_then(|verified| {
                if flags & TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32 == 0
                    || is_protected_recycle_target(&self.0.path)
                {
                    return Err(anyhow!(
                        "Windows cannot safely recycle '{}'",
                        self.0.path.display()
                    ));
                }
                Ok(verified)
            });
        match verified {
            Ok(verified) => {
                *state = EntryState::Verified(verified);
                Ok(())
            }
            Err(error) => {
                *state = EntryState::Rejected(error);
                Err(E_ABORT.into())
            }
        }
    }

    fn PostDeleteItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        result: HRESULT,
        recycled: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        let mut state = self.0.state.lock().unwrap();
        if let EntryState::Verified(_) = &*state {
            if let EntryState::Verified(Verified { value, hold }) =
                std::mem::replace(&mut *state, EntryState::Empty)
            {
                drop(hold);
                let moved = self
                    .0
                    .path
                    .symlink_metadata()
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound);
                let outcome = if result.is_err() {
                    Err(windows::core::Error::from(result).into())
                } else if recycled.is_null() || !moved {
                    Err(anyhow!(
                        "Windows did not complete the move to the Recycle Bin"
                    ))
                } else {
                    Ok(value)
                };
                *state = EntryState::Finished(outcome);
            }
        }
        Ok(())
    }

    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

pub(crate) fn move_to_recycle_bin(path: &Path) -> Result<()> {
    move_batch(vec![RecycleRequest {
        path: path.to_path_buf(),
        verify: Box::new(|| {
            Ok(Verified {
                value: (),
                hold: None,
            })
        }),
    }])
    .pop()
    .unwrap()
}

pub(crate) fn move_batch<T: Send + 'static>(requests: Vec<RecycleRequest<T>>) -> Vec<Result<T>> {
    if requests.is_empty() {
        return Vec::new();
    }
    let _apartment = match Apartment::initialize() {
        Ok(apartment) => apartment,
        Err(error) => {
            return requests
                .into_iter()
                .map(|_| Err(error.clone().into()))
                .collect()
        }
    };
    let mut entries = Vec::new();
    let mut results = Vec::new();
    for request in requests {
        let index = results.len();
        results.push(None);
        match std::path::absolute(&request.path) {
            Ok(path) => entries.push((
                index,
                Arc::new(ShellEntry {
                    path,
                    state: Mutex::new(EntryState::Pending(request.verify)),
                }),
            )),
            Err(error) => results[index] = Some(Err(error.into())),
        }
    }
    for batch in entries.chunks(SHELL_BATCH_SIZE) {
        perform_batch(batch, &mut results);
    }
    results
        .into_iter()
        .map(|item| item.expect("Shell batch should report each item"))
        .collect()
}

fn perform_batch<T: Send + 'static>(
    batch: &[(usize, Arc<ShellEntry<T>>)],
    results: &mut [Option<Result<T>>],
) {
    let mut pending: Vec<_> = batch.iter().collect();
    while !pending.is_empty() {
        let operation = match open_operation() {
            Ok(operation) => operation,
            Err(error) => {
                for (index, _) in pending {
                    results[*index] = Some(Err(error.clone().into()));
                }
                return;
            }
        };
        for (index, entry) in &pending {
            let queued = queue_item(&operation, entry);
            if let Err(error) = queued {
                // A missing or changed target can fail while the Shell item is being created.
                let mut state = entry.state.lock().unwrap();
                let previous = std::mem::replace(&mut *state, EntryState::Empty);
                let outcome = match previous {
                    EntryState::Pending(check) => check().err().unwrap_or(error),
                    _ => error,
                };
                results[*index] = Some(Err(outcome));
            }
        }
        pending.retain(|(index, _)| results[*index].is_none());
        if pending.is_empty() {
            return;
        }
        let performed = unsafe { operation.PerformOperations() };
        let rejected = pending
            .iter()
            .any(|(_, entry)| matches!(*entry.state.lock().unwrap(), EntryState::Rejected(_)));
        let remaining = std::mem::take(&mut pending);
        for item in remaining {
            let (index, entry) = item;
            let mut state = entry.state.lock().unwrap();
            if rejected && matches!(*state, EntryState::Pending(_)) {
                // A PreDeleteItem rejection aborts the remaining queue. Retry only untouched items.
                pending.push(item);
                continue;
            }
            results[*index] = Some(match std::mem::replace(&mut *state, EntryState::Empty) {
                EntryState::Finished(result) => result,
                EntryState::Rejected(error) => Err(error),
                _ => Err(performed
                    .as_ref()
                    .err()
                    .cloned()
                    .map(anyhow::Error::from)
                    .unwrap_or_else(|| {
                        anyhow!("Windows did not complete the move to the Recycle Bin")
                    })),
            });
        }
    }
}

fn open_operation() -> windows::core::Result<IFileOperation> {
    unsafe {
        let operation: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOperationFlags(
            FOFX_RECYCLEONDELETE
                | FOF_SILENT
                | FOF_NOERRORUI
                | FOF_NOCONFIRMATION
                | FOF_NO_CONNECTED_ELEMENTS,
        )?;
        Ok(operation)
    }
}

fn queue_item<T: Send + 'static>(
    operation: &IFileOperation,
    entry: &Arc<ShellEntry<T>>,
) -> Result<()> {
    let path: Vec<_> = entry
        .path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None)?;
        let sink: IFileOperationProgressSink = DeleteSink(Arc::clone(entry)).into();
        operation.DeleteItem(&item, &sink)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn test_move_batch<T>(
    requests: Vec<RecycleRequest<T>>,
    mut move_item: impl FnMut(&Path) -> Result<()>,
) -> Vec<Result<T>> {
    requests
        .into_iter()
        .map(|request| {
            let verified = (request.verify)()?;
            move_item(&request.path)?;
            Ok(verified.value)
        })
        .collect()
}
