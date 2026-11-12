// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Disposable native process: cancellation, model ownership and streaming.
use super::{
    CANCEL_FILE, WORKER_FLAG,
    protocol::{Config, Event, EventWriter},
    raw_frames::RawFrames,
};
use crate::config;
use anyhow::{Context, Result, ensure};
use rife_core::{CancellationToken, Engine, StreamOptions};
use std::{
    env,
    ffi::OsStr,
    fs,
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

/// Coalesce worker status at the display cadence, without limiting frame I/O.
#[derive(Default)]
struct ProgressUpdates {
    last: Option<Instant>,
}
impl ProgressUpdates {
    fn is_due(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < config::PROGRESS_INTERVAL)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

struct CancelMonitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl CancelMonitor {
    fn start(path: PathBuf, cancel: CancellationToken) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let thread = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if path.exists() {
                    cancel.cancel();
                    break;
                }
                thread::park_timeout(config::PROCESS_POLL_INTERVAL);
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for CancelMonitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// Private self-exec mode; model paths remain OS strings, not JSON strings.
pub(crate) fn entry() -> Option<i32> {
    let mut args = env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(WORKER_FLAG)) {
        return None;
    }
    configure_fault_reporting();
    let mut events = EventWriter::default();
    let result = (|| {
        let path = args.next().context("missing worker configuration")?;
        let model = args.next().context("missing worker model directory")?;
        ensure!(args.next().is_none(), "unexpected native worker argument");
        run(Path::new(&path), Path::new(&model), &mut events)
    })();
    Some(match result {
        Ok(()) => 0,
        Err(error) => {
            let (message, native, details) =
                if let Some(inference) = error.downcast_ref::<rife_core::InferenceError>() {
                    (
                        format!("interpolation failed (code {})", inference.status),
                        true,
                        inference.diagnostic.lines().map(str::to_owned).collect(),
                    )
                } else {
                    (format!("{error:#}"), false, Vec::new())
                };
            let _ = events.send(&Event::Error {
                message,
                native,
                details,
            });
            1
        }
    })
}

fn run(path: &Path, model: &Path, events: &mut EventWriter) -> Result<()> {
    let config: Config = serde_json::from_slice(&fs::read(path)?)?;
    let cancel = CancellationToken::default();
    let signal = cancel.clone();
    ctrlc::set_handler(move || signal.cancel())
        .context("could not register native worker cancellation")?;
    let _monitor = CancelMonitor::start(
        path.parent()
            .context("missing worker staging directory")?
            .join(CANCEL_FILE),
        cancel.clone(),
    );
    let stats = {
        let engine = Engine::new_with_cancellation(
            model,
            rife_core::EngineOptions {
                devices: config.devices.parse()?,
                jobs: Some(config.jobs.parse()?),
            },
            &cancel,
        )?;
        cancel.check()?;
        events.send(&Event::Loaded {
            devices: engine
                .selected_devices()
                .iter()
                .map(|device| device.name.clone())
                .collect(),
        })?;
        stream(&engine, &config, events, &cancel)?
    }; // Drop every native model before reporting success to the owner.
    cancel.check()?;
    events.send(&Event::Finished {
        frames: stats.output_frames,
    })
}

fn stream(
    engine: &Engine,
    config: &Config,
    events: &mut EventWriter,
    cancel: &CancellationToken,
) -> Result<rife_core::StreamStats> {
    let input = io::stdin();
    let frames = RawFrames::new(input, config.width, config.height)?;
    let mut output = BufWriter::with_capacity(64 * 1024, io::stdout().lock());
    let mut report_error = None;
    let mut updates = ProgressUpdates::default();
    let result = engine.interpolate_stream(
        frames,
        StreamOptions {
            threshold: config.threshold.parse()?,
            schedule: Some(config.schedule()?),
            ..StreamOptions::default()
        },
        cancel,
        |frame| {
            output
                .write_all(frame.pixels())
                .context("could not stream native output to FFmpeg")
        },
        |frames| {
            if report_error.is_none()
                && updates.is_due(Instant::now())
                && let Err(error) = events.send(&Event::Progress { frames })
            {
                report_error = Some(error);
                cancel.cancel();
            }
        },
    );
    if let Some(error) = report_error {
        return Err(error);
    }
    let stats = result?;
    output
        .flush()
        .context("could not flush native output to FFmpeg")?;
    Ok(stats)
}

pub(super) fn configure_fault_reporting() {
    #[cfg(unix)]
    {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: valid resource identifier and initialized limit. Only this
        // disposable worker loses core dumps; the supervising CLI is untouched.
        unsafe {
            libc::setrlimit(libc::RLIMIT_CORE, &limit);
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::{
            Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode},
            ErrorReporting::{WER_FAULT_REPORTING_NO_UI, WerSetFlags},
        };
        // SAFETY: process-local error-reporting flags, set before native work.
        unsafe {
            SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
            WerSetFlags(WER_FAULT_REPORTING_NO_UI);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn progress_is_immediate_then_coalesced_at_the_display_interval() {
        let start = Instant::now();
        let mut updates = ProgressUpdates::default();
        assert!(updates.is_due(start));
        for millisecond in 1..150 {
            assert!(!updates.is_due(start + Duration::from_millis(millisecond)));
        }
        assert!(updates.is_due(start + config::PROGRESS_INTERVAL));
        assert!(!updates.is_due(start + config::PROGRESS_INTERVAL));
        assert!(updates.is_due(start + config::PROGRESS_INTERVAL * 3));
    }
}
