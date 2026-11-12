// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Supervise FFmpeg/FFprobe processes, diagnostics and cancellation.
use crate::{config, worker};
use anyhow::{Context, Result, anyhow};
use rife_core::CancellationToken;
use std::{
    collections::VecDeque,
    fmt,
    io::{BufRead, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
};

struct ProcessExit {
    status: ExitStatus,
    terminated_on_cancel: bool,
}

#[derive(Debug)]
pub(crate) struct ProcessFailure {
    label: &'static str,
    status: ExitStatus,
    diagnostics: String,
    terminated_on_cancel: bool,
}

impl fmt::Display for ProcessFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed ({})", self.label, self.status)?;
        if !self.diagnostics.is_empty() {
            write!(formatter, "\n{}", self.diagnostics)?;
        }
        Ok(())
    }
}

impl std::error::Error for ProcessFailure {}

impl ProcessFailure {
    pub(crate) fn report(&self) -> crate::diagnostics::Report {
        crate::diagnostics::Report {
            reason: format!("{} failed ({})", self.label, self.status),
            details: self.diagnostics.lines().map(str::to_owned).collect(),
        }
    }
}

pub(crate) fn attach_cleanup_failure(
    error: anyhow::Error,
    diagnostic: anyhow::Error,
) -> anyhow::Error {
    // A native failure stops the pipeline and closes/kills both FFmpeg pipes.
    // Their shutdown status and buffered warnings are secondary to that failure.
    if error.downcast_ref::<rife_core::InferenceError>().is_some()
        || error
            .downcast_ref::<worker::Failure>()
            .is_some_and(worker::Failure::is_native)
    {
        return error;
    }

    if let Some(failure) = diagnostic.downcast_ref::<ProcessFailure>() {
        let original = error.downcast_ref::<ProcessFailure>();
        if original.is_some_and(|failure| !failure.terminated_on_cancel)
            || (failure.terminated_on_cancel
                && (failure.diagnostics.is_empty() || original.is_some()))
        {
            // Keep an independently failed backend ahead of children killed
            // during cleanup, even when those children logged ordinary warnings.
            return error;
        }
        // Its actual backend diagnosis replaces a secondary broken-pipe error.
        return diagnostic;
    }
    error.context(diagnostic)
}

/// Reaps children on every exit path. A watchdog can terminate blocked FFmpeg
/// reads/writes on cancellation while Rust waits for in-flight GPU work to finish.
pub(crate) struct ManagedChild {
    child: Arc<Mutex<Child>>,
    stdout: Option<ChildStdout>,
    monitor: Option<JoinHandle<Result<ProcessExit>>>,
    diagnostics: Option<JoinHandle<VecDeque<String>>>,
    label: &'static str,
}

fn read_diagnostics(
    stderr: impl std::io::Read,
    progress: Option<&AtomicU64>,
    encoding: bool,
) -> VecDeque<String> {
    let mut lines = VecDeque::with_capacity(config::DIAGNOSTIC_LINES);
    for line in BufReader::new(stderr).lines() {
        match line {
            Ok(line) => {
                if encoding && read_encoding_progress(&line, progress) {
                    continue;
                }
                if lines.len() == config::DIAGNOSTIC_LINES {
                    lines.pop_front();
                }
                lines.push_back(line.chars().take(config::DIAGNOSTIC_LINE_CHARS).collect());
            }
            Err(error) => {
                lines.push_back(format!("could not read diagnostics: {error}"));
                break;
            }
        }
    }
    lines
}

fn watch_process(child: Arc<Mutex<Child>>, cancel: CancellationToken) -> Result<ProcessExit> {
    let mut terminated_on_cancel = false;
    loop {
        {
            let mut child = child
                .lock()
                .map_err(|_| anyhow!("child process lock poisoned"))?;
            if let Some(status) = child.try_wait().context("could not poll FFmpeg")? {
                return Ok(ProcessExit {
                    status,
                    terminated_on_cancel,
                });
            }
            if cancel.is_cancelled() && !terminated_on_cancel {
                terminated_on_cancel = child.kill().is_ok();
            }
        }
        thread::sleep(config::PROCESS_POLL_INTERVAL);
    }
}

impl ManagedChild {
    pub(crate) fn spawn(
        command: Command,
        label: &'static str,
        cancel: CancellationToken,
    ) -> Result<Self> {
        Self::start(command, label, cancel, None, false)
    }

    pub(crate) fn encoder(
        command: Command,
        cancel: CancellationToken,
        frames: Option<Arc<AtomicU64>>,
    ) -> Result<Self> {
        Self::start(command, "FFmpeg encoding", cancel, frames, true)
    }

    fn start(
        mut command: Command,
        label: &'static str,
        cancel: CancellationToken,
        frames: Option<Arc<AtomicU64>>,
        encoding: bool,
    ) -> Result<Self> {
        cancel.check()?;
        command.stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("could not launch {label}"))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take().expect("configured stderr pipe");
        let diagnostics =
            thread::spawn(move || read_diagnostics(stderr, frames.as_deref(), encoding));
        let child = Arc::new(Mutex::new(child));
        let watched = Arc::clone(&child);
        let monitor = thread::spawn(move || watch_process(watched, cancel));
        Ok(Self {
            child,
            stdout,
            monitor: Some(monitor),
            diagnostics: Some(diagnostics),
            label,
        })
    }

    pub(crate) fn finish(&mut self) -> Result<()> {
        self.stdout.take();
        let exit = self
            .monitor
            .take()
            .expect("child is finished once")
            .join()
            .map_err(|_| anyhow!("process watchdog panicked"))??;
        let lines = self
            .diagnostics
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow!("diagnostic reader panicked"))?;
        if !exit.status.success() {
            return Err(ProcessFailure {
                label: self.label,
                status: exit.status,
                diagnostics: lines
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_owned(),
                terminated_on_cancel: exit.terminated_on_cancel,
            }
            .into());
        }
        Ok(())
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.stdout.take();
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(monitor) = self.monitor.take() {
            let _ = monitor.join();
        }
        if let Some(diagnostics) = self.diagnostics.take() {
            let _ = diagnostics.join();
        }
    }
}

impl ManagedChild {
    pub(crate) fn is_finished(&self) -> bool {
        self.monitor.is_none()
    }

    pub(crate) fn take_stdin(&mut self) -> Result<ChildStdin> {
        self.child
            .lock()
            .map_err(|_| anyhow!("child process lock poisoned"))?
            .stdin
            .take()
            .context("child stdin is not configured as a pipe")
    }

    pub(crate) fn take_stdout(&mut self) -> ChildStdout {
        self.stdout
            .take()
            .expect("child stdout is configured as a pipe")
    }
}

/// Recognize only FFmpeg progress keys; retain interleaved backend diagnostics.
fn read_encoding_progress(line: &str, frames: Option<&AtomicU64>) -> bool {
    let Some((key, value)) = line.split_once('=') else {
        return false;
    };
    if key == "frame"
        && let (Some(frames), Ok(count)) = (frames, value.trim().parse())
    {
        frames.fetch_max(count, Ordering::Relaxed);
    }
    matches!(
        key,
        "frame"
            | "fps"
            | "bitrate"
            | "total_size"
            | "out_time_us"
            | "out_time_ms"
            | "out_time"
            | "dup_frames"
            | "drop_frames"
            | "speed"
            | "progress"
    ) || key
        .strip_prefix("stream_")
        .and_then(|rest| rest.strip_suffix("_q"))
        .is_some_and(|indexes| {
            indexes
                .split('_')
                .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

pub(crate) fn capture_output(command: Command, cancel: &CancellationToken) -> Result<Vec<u8>> {
    capture_output_named(command, "FFprobe", cancel)
}

pub(crate) fn capture_output_named(
    command: Command,
    label: &'static str,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    read_output(command, label, cancel, |reader| {
        let mut output = Vec::new();
        reader
            .read_to_end(&mut output)
            .with_context(|| format!("could not read {label} output"))?;
        Ok(output)
    })
}

pub(crate) fn read_output<T>(
    mut command: Command,
    label: &'static str,
    cancel: &CancellationToken,
    read: impl FnOnce(&mut dyn BufRead) -> Result<T>,
) -> Result<T> {
    command.stdin(Stdio::null()).stdout(Stdio::piped());
    let mut child = ManagedChild::spawn(command, label, cancel.clone())?;
    // On a read/parser error, Drop closes the pipe and reaps the child rather
    // than waiting for a writer whose output is no longer being consumed.
    let output = read(&mut BufReader::new(child.take_stdout()))?;
    let result = child.finish();
    cancel.check()?;
    result?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::ensure;
    use std::{env, io::Write};
    #[test]
    fn stdout_reader_child() {
        if env::var_os("RIFE_TEST_STDOUT_CHILD").is_none() {
            return;
        }
        println!("RIFE_STDOUT_READY");
        std::io::stdout().flush().unwrap();
        loop {
            thread::sleep(std::time::Duration::from_secs(1));
        }
    }

    #[test]
    fn streaming_reader_reaps_children_on_parse_error_and_cancellation() {
        for cancelled in [false, true] {
            let mut command = Command::new(env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "process::tests::stdout_reader_child",
                    "--nocapture",
                ])
                .env("RIFE_TEST_STDOUT_CHILD", "1");
            let cancel = CancellationToken::default();
            let result: Result<()> = read_output(command, "test child", &cancel, |reader| {
                let mut line = String::new();
                loop {
                    line.clear();
                    ensure!(
                        reader.read_line(&mut line)? > 0,
                        "child exited before ready"
                    );
                    if line.contains("RIFE_STDOUT_READY") {
                        break;
                    }
                }
                if cancelled {
                    cancel.cancel();
                    std::io::copy(reader, &mut std::io::sink())?;
                    Ok(())
                } else {
                    Err(anyhow!("packet parser failed"))
                }
            });
            let message = result.unwrap_err().to_string();
            assert_eq!(
                message,
                if cancelled {
                    "cancelled"
                } else {
                    "packet parser failed"
                }
            );
        }
    }

    #[test]
    fn encoder_progress_tracks_reported_frames_monotonically() {
        let frames = AtomicU64::new(0);
        for line in "frame=   3\nfps=1.5\nframe=bad\nprogress=continue\nframe=136\nframe=4\nprogress=end\nstream_0_0_q=23\n".lines() {
            assert!(read_encoding_progress(line, Some(&frames)));
        }
        assert!(!read_encoding_progress(
            "[encoder] error=bad codec",
            Some(&frames)
        ));
        assert!(!read_encoding_progress("stream_failure_q=failed", None));
        assert!(!read_encoding_progress("stream_q=failed", None));
        assert_eq!(frames.load(Ordering::Relaxed), 136);
        let diagnostics = read_diagnostics(
            &b"frame=140\n[encoder] error=bad codec\nstream_0_0_q=-1\nprogress=end\nvk warning\n"[..],
            Some(&frames),
            true,
        );
        assert_eq!(frames.load(Ordering::Relaxed), 140);
        assert_eq!(diagnostics, ["[encoder] error=bad codec", "vk warning"]);
    }
    fn failure(
        label: &'static str,
        diagnostics: &str,
        terminated_on_cancel: bool,
    ) -> anyhow::Error {
        #[cfg(unix)]
        use std::os::unix::process::ExitStatusExt;
        #[cfg(windows)]
        use std::os::windows::process::ExitStatusExt;

        ProcessFailure {
            label,
            status: ExitStatus::from_raw(1),
            diagnostics: diagnostics.into(),
            terminated_on_cancel,
        }
        .into()
    }

    #[test]
    fn native_failure_stays_primary_when_ffmpeg_reports_shutdown_errors() {
        let inference = rife_core::InferenceError {
            device: rife_core::Device {
                id: 1,
                name: "Test GPU".into(),
            },
            width: 1920,
            height: 1080,
            status: -3,
            diagnostic: "vkWaitForFences failed -4".into(),
        };
        let original = anyhow::Error::new(inference).context("input frame pair 10 -> 11");
        let expected = format!("{original:#}");
        let error = attach_cleanup_failure(
            original,
            failure("FFmpeg decoding", "Error muxing a packet", false),
        );
        let error = attach_cleanup_failure(
            error,
            failure("FFmpeg encoding", "UDTA parsing failed retrying raw", true),
        );
        assert_eq!(format!("{error:#}"), expected);
    }

    #[test]
    fn cleanup_ignores_silent_kills_but_preserves_backend_failures() {
        let original = anyhow!("pipe closed");
        let error = attach_cleanup_failure(original, failure("FFmpeg decoding", "", true));
        assert_eq!(error.to_string(), "pipe closed");
        for (diagnostics, killed) in [("", false), ("Unknown encoder", true)] {
            let backend = failure("FFmpeg encoding", diagnostics, killed);
            let expected = backend.to_string();
            let error = attach_cleanup_failure(anyhow!("pipe closed"), backend);
            assert_eq!(format!("{error:#}"), expected);
            assert_eq!(expected.contains('\n'), !diagnostics.is_empty());
            let error = attach_cleanup_failure(error, failure("FFmpeg decoding", "", true));
            assert_eq!(format!("{error:#}"), expected);
        }
    }

    #[test]
    fn real_backend_error_takes_priority_over_cleanup_warnings_and_broken_pipes() {
        for encoder_first in [false, true] {
            let encoder = failure("FFmpeg encoding", "Unknown encoder", false);
            let heading = encoder.to_string();
            let cleanup = failure("FFmpeg decoding", "deprecated pixel format", true);
            let (first, second) = if encoder_first {
                (encoder, cleanup)
            } else {
                (cleanup, encoder)
            };
            let error = attach_cleanup_failure(anyhow!("Broken pipe"), first);
            let error = attach_cleanup_failure(error, second);
            assert_eq!(format!("{error:#}"), heading);
            let report = error.downcast_ref::<ProcessFailure>().unwrap().report();
            assert!(report.reason.starts_with("FFmpeg encoding failed ("));
            assert_eq!(report.details, ["Unknown encoder"]);
        }
    }
}
