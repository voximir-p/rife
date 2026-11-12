// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Supervise the isolated native backend without owning any native handles.
mod protocol;
mod raw_frames;
mod runtime;
use crate::{config, diagnostics::Report as BackendReport};
use anyhow::{Context, Result};
pub(crate) use protocol::Config;
use protocol::Event;
use rife_core::CancellationToken;
pub(crate) use runtime::entry;
use std::{
    collections::VecDeque,
    env, fmt, fs,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[cfg(test)]
use {
    crate::cli::Args,
    protocol::send_event,
    raw_frames::RawFrames,
    rife_core::{FrameSchedule, PlaybackOptions},
    runtime::configure_fault_reporting,
};

const WORKER_FLAG: &str = "--__rife-native-worker";
const EVENT_PREFIX: &str = "RIFE_WORKER ";
const CANCEL_FILE: &str = "native.cancel";
const CANCEL_GRACE: Duration = Duration::from_secs(5);
const PROCESSING_FAILURE: &str = "native backend failed";

#[derive(Debug)]
pub(crate) struct Failure {
    message: String,
    report: Option<BackendReport>,
}
impl Failure {
    pub(crate) fn is_native(&self) -> bool {
        self.report.is_some()
    }
    pub(crate) fn report(&self) -> Option<&BackendReport> {
        self.report.as_ref()
    }
}
impl fmt::Display for Failure {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}

#[derive(Default)]
struct Diagnostics {
    lines: VecDeque<String>,
    shutdown_error: Option<String>,
}

pub(crate) struct NativeWorker {
    child: Child,
    stdout: Option<ChildStdout>,
    events: Option<Receiver<Event>>,
    reader: Option<JoinHandle<io::Result<Diagnostics>>>,
    cancel_file: PathBuf,
    cancel: CancellationToken,
    expected_frames: u64,
    loaded: bool,
}

impl NativeWorker {
    pub(crate) fn spawn(
        config: &Config,
        model: &Path,
        staging: &Path,
        input: ChildStdout,
        cancel: CancellationToken,
    ) -> Result<Self> {
        cancel.check()?;
        let path = staging.join("native-worker.json");
        fs::write(&path, serde_json::to_vec(config)?)
            .context("could not prepare native worker configuration")?;
        let mut command =
            Command::new(env::current_exe().context("could not locate native worker executable")?);
        command
            .arg(WORKER_FLAG)
            .arg(&path)
            .arg(model)
            .env("RIFE_NATIVE_WORKER_DIAGNOSTICS", "1")
            .stdin(Stdio::from(input))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self::start(
            command,
            staging.join(CANCEL_FILE),
            cancel,
            config.schedule()?.output_frames(),
        )
    }

    fn start(
        mut command: Command,
        cancel_file: PathBuf,
        cancel: CancellationToken,
        expected_frames: u64,
    ) -> Result<Self> {
        let mut child = command
            .spawn()
            .context("could not launch native processing worker")?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take().expect("configured worker status pipe");
        // Bound both queued status events and the native diagnostic tail.
        // Drop disconnects the receiver before joining a blocked sender.
        let (sender, events) = mpsc::sync_channel(16);
        let reader = thread::spawn(move || {
            let mut diagnostics = Diagnostics::default();
            for line in BufReader::new(stderr).split(b'\n') {
                let line = line?;
                if let Some(json) = line.strip_prefix(EVENT_PREFIX.as_bytes())
                    && let Ok(event) = serde_json::from_slice(json)
                {
                    if sender.send(event).is_err() {
                        break;
                    }
                } else {
                    if diagnostics.lines.len() == config::DIAGNOSTIC_LINES {
                        diagnostics.lines.pop_front();
                    }
                    let line: String = String::from_utf8_lossy(&line)
                        .chars()
                        .take(config::DIAGNOSTIC_LINE_CHARS)
                        .collect();
                    if line.starts_with("model destruction failed:") {
                        diagnostics.shutdown_error = Some(line.clone());
                    }
                    diagnostics.lines.push_back(line);
                }
            }
            Ok(diagnostics)
        });
        Ok(Self {
            child,
            stdout,
            events: Some(events),
            reader: Some(reader),
            cancel_file,
            cancel,
            expected_frames,
            loaded: false,
        })
    }

    fn failure(&self, reason: impl Into<String>, details: Vec<String>) -> Failure {
        let reason = reason.into();
        let mut unique = Vec::new();
        for detail in details {
            if !detail.trim().is_empty()
                && !reason.contains(detail.trim())
                && !unique.contains(&detail)
            {
                unique.push(detail);
            }
        }
        Failure {
            message: PROCESSING_FAILURE.into(),
            report: Some(BackendReport {
                reason,
                details: unique,
            }),
        }
    }

    fn next_event(&mut self) -> Result<Event> {
        loop {
            self.cancel.check()?;
            match self
                .events
                .as_ref()
                .expect("active worker receiver")
                .recv_timeout(config::PROCESS_POLL_INTERVAL)
            {
                Ok(Event::Error {
                    message,
                    native,
                    details,
                }) => {
                    return Err(if native || !self.loaded {
                        let (status, mut diagnostics) = self.wait_for_exit()?;
                        diagnostics.lines.extend(details);
                        if !status.success() && status.code() != Some(1) {
                            diagnostics.lines.push_back(exit_description(status));
                        }
                        if let Some(error) = diagnostics.shutdown_error {
                            diagnostics.lines.push_back(error);
                        }
                        self.failure(message, diagnostics.lines.into_iter().collect())
                    } else {
                        Failure {
                            message,
                            report: None,
                        }
                    }
                    .into());
                }
                Ok(event) => return Ok(event),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    self.check_exit(false)?;
                    return Err(self
                        .failure("backend exited without completing the video", Vec::new())
                        .into());
                }
            }
        }
    }

    fn wait_for_exit(&mut self) -> Result<(ExitStatus, Diagnostics)> {
        // Wait for the status pipe to close so cancellation remains responsive
        // even if native shutdown stalls after reporting a terminal event.
        loop {
            self.cancel.check()?;
            match self
                .events
                .as_ref()
                .expect("active worker receiver")
                .recv_timeout(config::PROCESS_POLL_INTERVAL)
            {
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => continue,
                Ok(_) => {
                    return Err(self
                        .failure("unexpected backend status after completion", Vec::new())
                        .into());
                }
            }
        }
        let status = self
            .child
            .wait()
            .context("could not wait for native worker")?;
        let details = match self
            .reader
            .take()
            .expect("worker status reader joined once")
            .join()
        {
            Ok(Ok(diagnostics)) => diagnostics,
            Ok(Err(error)) => {
                return Err(self
                    .failure("could not read backend details", vec![error.to_string()])
                    .into());
            }
            Err(_) => {
                return Err(self
                    .failure("backend status reader failed", Vec::new())
                    .into());
            }
        };
        Ok((status, details))
    }

    fn check_exit(&mut self, completed: bool) -> Result<()> {
        let (status, diagnostics) = self.wait_for_exit()?;
        let mut details: Vec<_> = diagnostics.lines.into_iter().collect();
        if let Some(error) = &diagnostics.shutdown_error {
            details.push(error.clone());
        }
        let reason = if !status.success() {
            exit_description(status)
        } else if let Some(error) = diagnostics.shutdown_error {
            error
        } else if !completed {
            "backend exited without completing the video".into()
        } else {
            return Ok(());
        };
        Err(self.failure(reason, details).into())
    }

    pub(crate) fn loaded(&mut self) -> Result<Vec<String>> {
        match self.next_event()? {
            Event::Loaded { devices } => {
                self.loaded = true;
                Ok(devices)
            }
            _ => Err(self
                .failure("unexpected backend status before model loading", Vec::new())
                .into()),
        }
    }

    pub(crate) fn take_stdout(&mut self) -> ChildStdout {
        self.stdout.take().expect("native output pipe taken once")
    }

    pub(crate) fn process(&mut self, mut progress: impl FnMut(u64)) -> Result<u64> {
        loop {
            match self.next_event()? {
                Event::Progress { frames } => {
                    if frames > self.expected_frames {
                        return Err(self
                            .failure("backend reported an invalid frame count", Vec::new())
                            .into());
                    }
                    progress(frames);
                }
                Event::Finished { frames } => {
                    self.check_exit(true)?;
                    if frames != self.expected_frames {
                        return Err(self
                            .failure("backend reported an invalid frame count", Vec::new())
                            .into());
                    }
                    return Ok(frames);
                }
                _ => {
                    return Err(self
                        .failure("unexpected backend status during interpolation", Vec::new())
                        .into());
                }
            }
        }
    }

    pub(crate) fn request_cancel(&self) {
        let _ = fs::write(&self.cancel_file, []);
    }
}

impl Drop for NativeWorker {
    fn drop(&mut self) {
        self.stdout.take();
        // Release a potentially blocked event sender before waiting for it.
        self.events.take();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            self.request_cancel();
            let deadline = Instant::now() + CANCEL_GRACE;
            loop {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    break;
                }
                if Instant::now() >= deadline {
                    let _ = self.child.kill();
                    break;
                }
                thread::sleep(config::PROCESS_POLL_INTERVAL);
            }
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn exit_description(status: ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return match signal {
                libc::SIGSEGV => "segmentation fault (SIGSEGV)".into(),
                libc::SIGABRT => "aborted (SIGABRT)".into(),
                libc::SIGBUS => "bus error (SIGBUS)".into(),
                libc::SIGILL => "illegal instruction (SIGILL)".into(),
                libc::SIGFPE => "arithmetic exception (SIGFPE)".into(),
                libc::SIGKILL => "killed (SIGKILL)".into(),
                _ => format!("terminated by signal {signal}"),
            };
        }
    }
    #[cfg(windows)]
    if let Some(code) = status.code() {
        let code = code as u32;
        let description = match code {
            0xc0000005 => "access violation",
            0xc000001d => "illegal instruction",
            0xc00000fd => "stack overflow",
            0xc0000409 => "fail-fast exception",
            _ => "exit status",
        };
        return format!("{description} (0x{code:08x})");
    }
    format!("backend exited with code {}", status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn simulated_command(mode: &str, staging: &Path) -> Command {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args(["--exact", "worker::tests::worker_child", "--nocapture"])
            .env("RIFE_TEST_WORKER_MODE", mode)
            .env("RIFE_NATIVE_WORKER_DIAGNOSTICS", "1")
            .env("RIFE_TEST_WORKER_CANCEL", staging.join(CANCEL_FILE))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn crash_for_test() -> ! {
        #[cfg(unix)]
        // SAFETY: explicitly signal this disposable test child; no invalid
        // memory accesses or recovery from corrupted process state are used.
        unsafe {
            libc::raise(libc::SIGSEGV);
            libc::raise(libc::SIGSEGV);
        }
        #[cfg(windows)]
        // SAFETY: deliberately raise an unhandled exception in this child,
        // with no arguments and no memory pointers to dereference.
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::RaiseException(
                0xc0000005,
                1,
                0,
                std::ptr::null(),
            );
        }
        std::process::abort()
    }

    #[test]
    fn worker_child() {
        let Ok(mode) = env::var("RIFE_TEST_WORKER_MODE") else {
            return;
        };
        configure_fault_reporting();
        if mode == "loading_crash" {
            crash_for_test();
        }
        send_event(&Event::Loaded {
            devices: vec!["test device".into()],
        })
        .unwrap();
        match mode.as_str() {
            "crash" => crash_for_test(),
            "logged_crash" => {
                unsafe extern "C" {
                    fn rife_record_native_error(format: *const std::ffi::c_char, ...);
                }
                // SAFETY: a static NUL-terminated format with no substitutions.
                unsafe {
                    rife_record_native_error(
                        c"vkAllocateMemory failed: VK_ERROR_OUT_OF_DEVICE_MEMORY".as_ptr(),
                    );
                }
                crash_for_test();
            }
            "abort" => std::process::abort(),
            "wait" | "finish_wait" => {
                if mode == "finish_wait" {
                    send_event(&Event::Finished { frames: 2 }).unwrap();
                }
                let path = PathBuf::from(env::var_os("RIFE_TEST_WORKER_CANCEL").unwrap());
                let deadline = Instant::now() + Duration::from_secs(30);
                while !path.exists() {
                    assert!(
                        Instant::now() < deadline,
                        "test parent did not cancel the worker"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                fs::write(path.with_extension("ack"), []).unwrap();
            }
            "incomplete" => {}
            "reported" => {
                send_event(&Event::Error {
                    message: "native allocation failed".into(),
                    native: true,
                    details: Vec::new(),
                })
                .unwrap();
            }
            "success" | "finish_crash" | "shutdown_error" => {
                if mode == "shutdown_error" {
                    unsafe extern "C" {
                        fn rife_record_native_error(format: *const std::ffi::c_char, ...);
                    }
                    // SAFETY: static format, no substitutions or borrowed buffers.
                    unsafe {
                        rife_record_native_error(
                            c"model destruction failed: test backend exception".as_ptr(),
                        );
                    }
                }
                for frames in [1, 2] {
                    send_event(&Event::Progress { frames }).unwrap();
                }
                send_event(&Event::Finished { frames: 2 }).unwrap();
                if mode == "finish_crash" {
                    crash_for_test();
                }
            }
            _ => panic!("unknown test mode"),
        }
    }

    #[test]
    fn native_faults_release_owned_resources_and_preserve_output() {
        for mode in [
            "loading_crash",
            "crash",
            "logged_crash",
            "shutdown_error",
            "abort",
            "finish_crash",
            "incomplete",
            "reported",
        ] {
            let root = tempfile::tempdir().unwrap();
            let output = root.path().join("existing.mp4");
            fs::write(&output, b"previous output").unwrap();
            let staging_path;
            {
                let cancel = CancellationToken::default();
                let _lock =
                    crate::output_lock::OutputLock::acquire(&output, true, &cancel, || {}).unwrap();
                let staging = crate::paths::staging_directory(root.path()).unwrap();
                staging_path = staging.path().to_path_buf();
                fs::write(staging.path().join("encoded.mp4"), b"partial output").unwrap();
                let mut worker = NativeWorker::start(
                    simulated_command(mode, staging.path()),
                    staging.path().join(CANCEL_FILE),
                    cancel,
                    2,
                )
                .unwrap();
                let error = if mode == "loading_crash" {
                    worker.loaded().unwrap_err()
                } else {
                    assert_eq!(worker.loaded().unwrap(), ["test device"]);
                    worker.process(|_| {}).unwrap_err()
                };
                assert!(
                    error.downcast_ref::<Failure>().unwrap().is_native(),
                    "{mode}: {error:#}"
                );
                assert_eq!(error.to_string(), PROCESSING_FAILURE, "{mode}");
                let report = error.downcast_ref::<Failure>().unwrap().report().unwrap();
                if mode == "logged_crash" {
                    assert!(
                        report
                            .details
                            .iter()
                            .any(|line| line.contains("VK_ERROR_OUT_OF_DEVICE_MEMORY"))
                    );
                }
                if mode == "shutdown_error" {
                    assert_eq!(
                        report.reason,
                        "model destruction failed: test backend exception"
                    );
                }
                if mode == "reported" {
                    assert_eq!(report.reason, "native allocation failed");
                }
                #[cfg(unix)]
                if matches!(
                    mode,
                    "loading_crash" | "crash" | "logged_crash" | "finish_crash"
                ) {
                    assert_eq!(report.reason, "segmentation fault (SIGSEGV)");
                }
            }
            assert_eq!(fs::read(&output).unwrap(), b"previous output");
            assert!(
                !output.with_extension("mp4.lock").exists(),
                "{mode}: lock leaked"
            );
            assert!(!staging_path.exists(), "{mode}: staging leaked");
        }
    }

    #[test]
    fn native_worker_success_and_cooperative_cancellation() {
        for cancelled in [false, true] {
            let staging = tempfile::tempdir().unwrap();
            let cancel = CancellationToken::default();
            let mut worker = NativeWorker::start(
                simulated_command(if cancelled { "wait" } else { "success" }, staging.path()),
                staging.path().join(CANCEL_FILE),
                cancel.clone(),
                2,
            )
            .unwrap();
            worker.loaded().unwrap();
            if cancelled {
                cancel.cancel();
                assert!(
                    worker
                        .process(|_| {})
                        .unwrap_err()
                        .to_string()
                        .contains("cancelled")
                );
            } else {
                let mut frames = Vec::new();
                assert_eq!(worker.process(|frame| frames.push(frame)).unwrap(), 2);
                assert_eq!(frames, [1, 2]);
            }
            drop(worker);
            if cancelled {
                assert!(
                    staging
                        .path()
                        .join(CANCEL_FILE)
                        .with_extension("ack")
                        .exists(),
                    "worker did not acknowledge cooperative cancellation"
                );
            }
        }
    }

    #[test]
    fn stalled_native_shutdown_remains_cancellable() {
        let staging = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::default();
        let mut worker = NativeWorker::start(
            simulated_command("finish_wait", staging.path()),
            staging.path().join(CANCEL_FILE),
            cancel.clone(),
            2,
        )
        .unwrap();
        worker.loaded().unwrap();
        assert!(matches!(
            worker.next_event().unwrap(),
            Event::Finished { frames: 2 }
        ));
        let (waiting, started) = mpsc::channel();
        thread::scope(|scope| {
            let completion = scope.spawn(|| {
                waiting.send(()).unwrap();
                worker.check_exit(true)
            });
            started.recv_timeout(Duration::from_secs(10)).unwrap();
            cancel.cancel();
            assert!(
                completion
                    .join()
                    .unwrap()
                    .unwrap_err()
                    .to_string()
                    .contains("cancelled")
            );
        });
        drop(worker);
        assert!(
            staging
                .path()
                .join(CANCEL_FILE)
                .with_extension("ack")
                .exists()
        );
    }

    #[test]
    fn worker_config_preserves_exact_playback_factors() {
        use clap::Parser;
        for factor in ["0.3", "1.5", "0.10000000000000002"] {
            let args =
                Args::try_parse_from(["rife", "-i", "input.mp4", "-s", factor, "out.mp4"]).unwrap();
            let config = Config::new(&args, (64, 64), 34);
            let decoded: Config =
                serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
            assert_eq!(
                decoded.speed.parse::<f64>().unwrap().to_bits(),
                args.speed.to_bits()
            );
            assert_eq!(
                decoded.schedule().unwrap().output_frames(),
                FrameSchedule::for_playback(
                    34,
                    PlaybackOptions {
                        fps_scale: args.fps_scale,
                        speed: args.speed
                    }
                )
                .unwrap()
                .output_frames()
            );
        }
    }

    #[test]
    fn raw_reader_distinguishes_clean_eof_from_truncated_frames() {
        let mut reader = RawFrames::new([0u8; 24].as_slice(), 2, 2).unwrap();
        assert!(reader.next().unwrap().is_ok());
        assert!(reader.next().unwrap().is_ok());
        assert!(reader.next().is_none());
        let mut reader = RawFrames::new([0u8; 13].as_slice(), 2, 2).unwrap();
        assert!(reader.next().unwrap().is_ok());
        assert!(reader.next().unwrap().is_err());
        assert!(reader.next().is_none());
    }
}
