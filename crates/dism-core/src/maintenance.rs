use crate::systasks::{decode_cmd_output, decode_utf16le};
use crate::system_tools::system32_command;
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::io::{self, Read};
use std::os::windows::process::CommandExt;
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
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
        let child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to start {program}"))?;
        capture_child(child, program, progress)?
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

fn capture_child(
    child: Child,
    program: &str,
    progress: &MaintenanceProgress,
) -> Result<(ExitStatus, String)> {
    let mut child = RunningChild(child);
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
                match child.0.kill() {
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
                    // Drain complete UTF-16 units and retain a BOM if the stream supplied one.
                    let start = usize::from(buffer.starts_with(b"\xff\xfe")) * 2;
                    buffer.drain(start..start + overflow.next_multiple_of(2));
                }
                pending_output = true;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => pipes_open = false,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
    }
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
}
