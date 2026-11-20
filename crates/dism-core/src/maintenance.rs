use crate::systasks::{decode_cmd_output, decode_utf16le};
use crate::system_tools::system32_command;
use crate::win_util::CREATE_NO_WINDOW;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::io::{self, Read};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

const STREAM_LIMIT: usize = 128 * 1024;
const DISPLAY_LIMIT: usize = 12 * 1024;

#[derive(Default)]
pub struct MaintenanceProgress {
    cancelled: AtomicBool,
    state: Mutex<MaintenanceSnapshot>,
}

#[derive(Clone, Default, Serialize)]
pub struct MaintenanceSnapshot {
    program: Option<String>,
    percent: Option<f32>,
    output: String,
}

#[derive(Debug)]
pub struct MaintenanceCancelled;

impl std::fmt::Display for MaintenanceCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Maintenance task cancelled")
    }
}

impl std::error::Error for MaintenanceCancelled {}

impl MaintenanceProgress {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> MaintenanceSnapshot {
        self.state.lock().unwrap().clone()
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(MaintenanceCancelled.into())
        } else {
            Ok(())
        }
    }

    fn publish(&self, program: &str, output: &str) {
        let mut state = self.state.lock().unwrap();
        if state.program.as_deref() != Some(program) {
            state.program = Some(program.to_string());
            state.percent = None;
        }
        let offset = output.floor_char_boundary(output.len().saturating_sub(DISPLAY_LIMIT));
        let tail = &output[offset..];
        state.percent = last_percent(tail).or(state.percent);
        state.output = tail.to_string();
    }
}

fn last_percent(output: &str) -> Option<f32> {
    output.rmatch_indices('%').find_map(|(end, _)| {
        let before = output[..end].trim_end();
        let start = before
            .rfind(|ch: char| !ch.is_ascii_digit() && ch != '.')
            .map_or(0, |idx| {
                before[idx..]
                    .chars()
                    .next()
                    .map_or(idx, |ch| idx + ch.len_utf8())
            });
        before[start..]
            .parse::<f32>()
            .ok()
            .filter(|value| (0.0..=100.0).contains(value))
    })
}

pub(crate) struct CommandResult {
    pub code: i32,
    pub output: String,
}

pub(crate) fn run_system_command(
    program: &str,
    args: &[&str],
    progress: Option<&MaintenanceProgress>,
) -> Result<CommandResult> {
    let mut command = system32_command(program)?;
    command.creation_flags(CREATE_NO_WINDOW).args(args);
    let (status, output) = if let Some(progress) = progress {
        progress.check()?;
        let job =
            MaintenanceJob::new().context("Could not create the maintenance process group")?;
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to start {program}"))?;
        capture_child(child, &job, program, progress)?
    } else {
        let output = command
            .output()
            .with_context(|| format!("Failed to start {program}"))?;
        (
            output.status,
            combine_output(program, &output.stdout, &output.stderr),
        )
    };
    let code = status.code().unwrap_or(-1);
    if !status.success() && code != 3010 {
        return Err(anyhow!("{program} exited with code {code}: {output}"));
    }
    Ok(CommandResult { code, output })
}

struct RunningChild(Child);

impl Drop for RunningChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct MaintenanceJob(OwnedHandle);

impl MaintenanceJob {
    fn new() -> io::Result<Self> {
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    fn attach(&self, child: &mut Child) -> io::Result<()> {
        if unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), child.as_raw_handle()) } != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        // A very short command can exit before assignment.
        if child.try_wait()?.is_some() {
            Ok(())
        } else {
            Err(error)
        }
    }

    fn terminate(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn capture_child(
    child: Child,
    job: &MaintenanceJob,
    program: &str,
    progress: &MaintenanceProgress,
) -> Result<(ExitStatus, String)> {
    let mut child = RunningChild(child);
    job.attach(&mut child.0)
        .context("Could not group the maintenance process and its children")?;
    let (sender, receiver) = mpsc::sync_channel(16);
    let stdout = child
        .0
        .stdout
        .take()
        .context("Missing command stdout pipe")?;
    let stderr = child
        .0
        .stderr
        .take()
        .context("Missing command stderr pipe")?;
    read_pipe(stdout, 0, sender.clone())?;
    read_pipe(stderr, 1, sender)?;
    progress.publish(program, "");
    let mut streams = [Vec::new(), Vec::new()];
    let mut status = None;
    let mut drain_deadline = None;
    let mut cancelled = false;
    let mut pipes_open = true;
    let mut pending_output = false;
    let mut next_publish = Instant::now();
    loop {
        if pending_output && Instant::now() >= next_publish {
            progress.publish(program, &combine_output(program, &streams[0], &streams[1]));
            pending_output = false;
            next_publish = Instant::now() + Duration::from_millis(150);
        }
        if status.is_none() {
            if let Some(exit) = child.0.try_wait()? {
                status = Some(exit);
                // Descendants can inherit a pipe. Do not wait indefinitely once the command exits.
                drain_deadline = Some(Instant::now() + Duration::from_secs(1));
            } else if !cancelled && progress.is_cancelled() {
                match job.terminate() {
                    Ok(()) => cancelled = true,
                    Err(_) if child.0.try_wait()?.is_some() => (),
                    Err(error) => {
                        return Err(error).context("Could not stop the maintenance process")
                    }
                }
            }
        }
        if let Some(exit) = status {
            if !pipes_open || drain_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                if cancelled {
                    return Err(MaintenanceCancelled.into());
                }
                return Ok((exit, combine_output(program, &streams[0], &streams[1])));
            }
        }
        let interval = Duration::from_millis(80);
        if !pipes_open {
            std::thread::sleep(interval);
            continue;
        }
        match receiver.recv_timeout(interval) {
            Ok((stream, bytes)) => {
                let buffer = &mut streams[stream];
                buffer.extend(bytes?);
                let overflow = buffer.len().saturating_sub(STREAM_LIMIT);
                if overflow > 0 {
                    truncate_stream_tail(
                        buffer,
                        overflow,
                        stream == 0 && program.eq_ignore_ascii_case("sfc.exe"),
                    );
                }
                pending_output = true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => pipes_open = false,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
    }
}

fn truncate_stream_tail(buffer: &mut Vec<u8>, overflow: usize, utf16: bool) {
    if utf16 || buffer.starts_with(b"\xff\xfe") {
        let start = usize::from(buffer.starts_with(b"\xff\xfe")) * 2;
        let cut = start + overflow.next_multiple_of(2);
        let skip = buffer[cut..].starts_with(&[b'\n', 0])
            || buffer
                .get(cut + 1)
                .is_some_and(|high| (0xdc..=0xdf).contains(high));
        buffer.drain(start..cut + usize::from(skip) * 2);
        return;
    }
    let start = usize::from(buffer.starts_with(b"\xef\xbb\xbf")) * 3;
    let mut cut = start + overflow;
    // Prefer starting at a full line for both UTF-8 and the Windows OEM code page.
    if let Some(offset) = buffer[cut..]
        .iter()
        .take(4096)
        .position(|byte| *byte == b'\n')
    {
        cut += offset + 1;
    } else {
        cut += buffer[cut..]
            .iter()
            .take_while(|byte| **byte & 0xc0 == 0x80)
            .count();
    }
    buffer.drain(start..cut);
}

fn read_pipe(
    mut reader: impl Read + Send + 'static,
    stream: usize,
    sender: mpsc::SyncSender<(usize, io::Result<Vec<u8>>)>,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name("maintenance-output".to_string())
        .spawn(move || {
            let mut bytes = [0; 8192];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        if sender.send((stream, Ok(bytes[..count].to_vec()))).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send((stream, Err(error)));
                        break;
                    }
                }
            }
        })?;
    Ok(())
}

fn combine_output(program: &str, stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = if program.eq_ignore_ascii_case("sfc.exe") {
        decode_utf16le(stdout.strip_prefix(b"\xff\xfe").unwrap_or(stdout))
    } else {
        decode_cmd_output(stdout)
    };
    let stderr = decode_cmd_output(stderr);
    format!("{}\n{}", stdout.trim(), stderr.trim())
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streams_output_and_cancels_a_running_child() {
        let progress = MaintenanceProgress::default();
        std::thread::scope(|scope| {
            let job = scope
                .spawn(|| run_system_command("ping.exe", &["-t", "127.0.0.1"], Some(&progress)));
            let deadline = Instant::now() + Duration::from_secs(5);
            while progress.snapshot().output.is_empty()
                && !job.is_finished()
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            progress.cancel();
            assert!(!progress.snapshot().output.is_empty());
            assert!(job
                .join()
                .unwrap()
                .err()
                .unwrap()
                .is::<MaintenanceCancelled>());
        });
        assert_eq!(last_percent("[ 12.5% ]\r[ 54.2% ]"), Some(54.2));
    }

    #[test]
    fn output_tail_begins_at_a_character_boundary() {
        let mut utf8 = "€后续".as_bytes().to_vec();
        truncate_stream_tail(&mut utf8, 1, false);
        assert_eq!(String::from_utf8(utf8).unwrap(), "后续");
        let mut utf16: Vec<u8> = "🙂后续".encode_utf16().flat_map(u16::to_le_bytes).collect();
        truncate_stream_tail(&mut utf16, 2, true);
        assert_eq!(decode_utf16le(&utf16), "后续");
    }

    #[test]
    fn cancelling_a_job_stops_its_descendants() {
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE,
        };

        let progress = MaintenanceProgress::default();
        let program = r"WindowsPowerShell\v1.0\powershell.exe";
        let script = r#"$child = Start-Process "$env:SystemRoot\System32\ping.exe" -ArgumentList '-t','127.0.0.1' -WindowStyle Hidden -PassThru; $child.Id; Wait-Process -Id $child.Id"#;
        std::thread::scope(|scope| {
            let task = scope.spawn(|| {
                run_system_command(
                    program,
                    &["-NoProfile", "-Command", script],
                    Some(&progress),
                )
            });
            let deadline = Instant::now() + Duration::from_secs(10);
            let descendant = loop {
                if let Ok(pid) = progress.snapshot().output.trim().parse::<u32>() {
                    let handle = unsafe { OpenProcess(SYNCHRONIZATION_SYNCHRONIZE, 0, pid) };
                    if !handle.is_null() {
                        break Some(unsafe { OwnedHandle::from_raw_handle(handle) });
                    }
                }
                if task.is_finished() || Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            progress.cancel();
            assert!(
                matches!(task.join().unwrap(), Err(error) if error.is::<MaintenanceCancelled>())
            );
            let descendant =
                descendant.expect("maintenance command should report the running descendant");
            let result = unsafe { WaitForSingleObject(descendant.as_raw_handle(), 5_000) };
            assert_ne!(
                result, WAIT_TIMEOUT,
                "descendant still running after cancelling maintenance"
            );
            assert_eq!(result, WAIT_OBJECT_0);
        });
    }
}
