// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    Args, chapters, config, encode,
    media::{self, Rate},
    output_lock::OutputLock,
    paths,
    ui::Ui,
};
use anyhow::{Context, Result, anyhow, ensure};
use rife_core::{CancellationToken, Engine, Frame, FrameSchedule, PlaybackOptions, StreamOptions};
use std::{
    collections::VecDeque,
    env, fmt, fs,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

struct ProcessExit {
    status: ExitStatus,
    terminated_on_cancel: bool,
}

#[derive(Debug)]
struct ProcessFailure {
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

fn attach_cleanup_failure(error: anyhow::Error, diagnostic: anyhow::Error) -> anyhow::Error {
    // A native failure stops the pipeline and closes/kills both FFmpeg pipes.
    // Their shutdown status and buffered warnings are secondary to that failure.
    if error.downcast_ref::<rife_core::InferenceError>().is_some() {
        return error;
    }

    if diagnostic
        .downcast_ref::<ProcessFailure>()
        .is_some_and(|failure| failure.terminated_on_cancel && failure.diagnostics.is_empty())
    {
        // An otherwise silent child killed during cleanup adds no diagnosis.
        return error;
    }
    error.context(diagnostic)
}

/// Reaps children on every exit path. A watchdog can terminate blocked FFmpeg
/// reads/writes on cancellation while Rust waits for in-flight GPU work to finish.
struct ManagedChild {
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    monitor: Option<JoinHandle<Result<ProcessExit>>>,
    diagnostics: Option<JoinHandle<VecDeque<String>>>,
    label: &'static str,
}

impl ManagedChild {
    fn spawn(mut command: Command, label: &'static str, cancel: CancellationToken) -> Result<Self> {
        cancel.check()?;
        command.stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("could not launch {label}"))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take().expect("configured stderr pipe");
        let diagnostics = thread::spawn(move || {
            let mut lines = VecDeque::with_capacity(config::DIAGNOSTIC_LINES);
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(line) => {
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
        });
        let child = Arc::new(Mutex::new(child));
        let watched = Arc::clone(&child);
        let monitor = thread::spawn(move || -> Result<ProcessExit> {
            let mut terminated_on_cancel = false;
            loop {
                {
                    let mut child = watched
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
        });
        Ok(Self {
            child,
            stdin,
            stdout,
            monitor: Some(monitor),
            diagnostics: Some(diagnostics),
            label,
        })
    }

    fn finish(&mut self) -> Result<()> {
        self.stdin.take();
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
        self.stdin.take();
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
    fn take_stdout(&mut self) -> ChildStdout {
        self.stdout
            .take()
            .expect("child stdout is configured as a pipe")
    }

    fn take_stdin(&mut self) -> ChildStdin {
        self.stdin
            .take()
            .expect("child stdin is configured as a pipe")
    }
}

pub(crate) fn capture_output(command: Command, cancel: &CancellationToken) -> Result<Vec<u8>> {
    capture_output_named(command, "FFprobe", cancel)
}

pub(crate) fn capture_output_named(
    mut command: Command,
    label: &'static str,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    command.stdin(Stdio::null()).stdout(Stdio::piped());
    let mut child = ManagedChild::spawn(command, label, cancel.clone())?;
    let mut output = Vec::new();
    let read = child.take_stdout().read_to_end(&mut output);
    let result = child.finish();
    cancel.check()?;
    result?;
    read.context("could not read FFprobe output")?;
    Ok(output)
}

struct RawFrames<R> {
    reader: R,
    width: u32,
    height: u32,
    bytes: usize,
    finished: bool,
}
impl<R: Read> RawFrames<R> {
    fn new(reader: R, width: u32, height: u32) -> Result<Self> {
        Ok(Self {
            reader,
            width,
            height,
            bytes: Frame::byte_len(width, height)?,
            finished: false,
        })
    }
}
impl<R: Read> Iterator for RawFrames<R> {
    type Item = Result<Frame>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let mut pixels = Vec::new();
        if let Err(error) = pixels.try_reserve_exact(self.bytes) {
            self.finished = true;
            return Some(Err(error).context("could not allocate decoded frame"));
        }
        pixels.resize(self.bytes, 0);
        match self.reader.read_exact(&mut pixels[..1]) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                self.finished = true;
                return None;
            }
            Err(error) => {
                self.finished = true;
                return Some(Err(error).context("could not read decoded frame"));
            }
        }
        if let Err(error) = self.reader.read_exact(&mut pixels[1..]) {
            self.finished = true;
            return Some(Err(error).context("FFmpeg returned a truncated RGB frame"));
        }
        Some(Frame::new(self.width, self.height, pixels))
    }
}

fn decode_command(ffmpeg: &Path, input: &Path, rate: Rate) -> Command {
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "warning", "-nostdin", "-i"])
        .arg(input)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &format!(
                "fps=fps={rate}:start_time=0,format={}",
                config::PIXEL_FORMAT
            ),
            "-fps_mode",
            "passthrough",
            "-c:v",
            "rawvideo",
            "-threads",
            "1",
            "-pix_fmt",
            config::PIXEL_FORMAT,
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    command
}

// Run the exact normalization filter once to establish N. This uses one
// reusable byte buffer, never metadata estimates or a temporary image sequence.
fn count_normalized_frames(
    ffmpeg: &Path,
    input: &Path,
    info: &media::VideoInfo,
    cancel: &CancellationToken,
) -> Result<u64> {
    let mut child = ManagedChild::spawn(
        decode_command(ffmpeg, input, info.rate),
        "FFmpeg counting",
        cancel.clone(),
    )?;
    let mut pipe = child.take_stdout();
    let bytes_per_frame = Frame::byte_len(info.width, info.height)? as u64;
    let result = (|| -> Result<u64> {
        let mut bytes = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            cancel.check()?;
            let n = pipe
                .read(&mut buffer)
                .context("could not count normalized video")?;
            if n == 0 {
                break;
            }
            bytes = bytes
                .checked_add(n as u64)
                .context("normalized video is too large")?;
        }
        ensure!(
            bytes.is_multiple_of(bytes_per_frame),
            "FFmpeg counting returned a truncated RGB frame"
        );
        let count = bytes / bytes_per_frame;
        ensure!(count > 0, "input contains no video frames");
        Ok(count)
    })();
    drop(pipe);
    let finish = child.finish();
    cancel.check()?;
    match result {
        Ok(count) => {
            finish?;
            Ok(count)
        }
        Err(error) => Err(finish.err().unwrap_or(error)),
    }
}

pub(crate) fn process(args: Args, cancel: CancellationToken) -> Result<()> {
    let exe = env::current_exe().context("unable to locate the current executable")?;
    let root = dunce::simplified(&exe)
        .parent()
        .context("executable has no parent directory")?;
    let (input, output, parent) = paths::resolve_paths(&args)?;
    let ui = Ui::new(args.verbose);
    let _output_lock = OutputLock::acquire(&output, args.overwrite, &cancel, || {
        ui.status("Waiting", format_args!("for locked `{}`", output.display()));
    })?;
    // The previous writer may have committed a file while this run was waiting.
    paths::validate_output(&input, &output, args.overwrite)?;
    let ffmpeg = paths::tool_path(root, "ffmpeg")?;
    let ffprobe = paths::tool_path(root, "ffprobe")?;
    let model = paths::find_model(root, &args.model)?;
    let extension = output
        .extension()
        .unwrap()
        .to_string_lossy()
        .to_ascii_lowercase();
    ui.status("Scanning", "video frames for output timing");
    let info = media::probe(&ffprobe, &input, &cancel)?;
    let input_frames = count_normalized_frames(&ffmpeg, &input, &info, &cancel)?;
    let playback = PlaybackOptions {
        fps_scale: args.fps_scale,
        speed: args.speed,
    };
    let schedule = FrameSchedule::for_playback(input_frames, playback)?;
    let video_quality = encode::select_video_quality(
        &ffmpeg,
        &extension,
        &args,
        info.source_crf.as_deref(),
        info.video_bitrate,
        input_frames,
        schedule.output_frames(),
        &cancel,
    )?;
    let (scale_num, scale_den) = playback.fps_ratio()?;
    let output_rate = info.rate.scaled(scale_num, scale_den)?;
    // Whole-frame rounding can extend the final interval. Match audio, subtitles,
    // and chapters to the actual video duration.
    let media_speed = args.fps_scale * input_frames as f64 / schedule.output_frames() as f64;
    let retiming = encode::requires_retiming(args.speed);
    let audio_is_encoded = args
        .audio_codec
        .as_deref()
        .map_or_else(|| retiming, |codec| codec != "copy");
    ensure!(
        !audio_is_encoded || info.audio_bitrates.iter().all(Option::is_some),
        "could not determine every source audio bitrate"
    );
    if info.hdr {
        crate::ui::warning(
            "HDR transfer detected; the RGB8 interpolation pipeline does not preserve HDR.",
        );
    }
    cancel.check()?;
    let engine = {
        let _loading = ui.activity("Loading", &format!("model `{}`", args.model));
        Engine::new_with_cancellation(&model, args.engine_options(), &cancel)?
    };
    cancel.check()?;
    for (label, rate) in [("Source", info.rate), ("Target", output_rate)] {
        ui.info(
            label,
            format_args!(
                "{}x{} @ {:.2} fps",
                info.width,
                info.height,
                rate.num as f64 / rate.den as f64
            ),
        );
    }
    for device in engine.selected_devices() {
        ui.info("Device", &device.name);
    }
    // Declare staging before subprocesses so they are reaped before its files
    // are removed, including on Windows and on early returns.
    let temp = paths::staging_directory(&parent)?;
    let staged = dunce::simplified(temp.path()).join(format!("encoded.{extension}"));
    let chapter_metadata =
        if retiming && encode::supports_timed_metadata(&extension) && !info.chapters.is_empty() {
            let metadata = dunce::simplified(temp.path()).join("chapters.ffmetadata");
            chapters::write_retimed(
                &metadata,
                &info.chapters,
                info.source_start_time,
                media_speed,
            )?;
            Some(metadata)
        } else {
            None
        };
    let mut decoder = ManagedChild::spawn(
        decode_command(&ffmpeg, &input, info.rate),
        "FFmpeg decoding",
        cancel.clone(),
    )?;
    let mut command = encode::command(
        &ffmpeg,
        (info.width, info.height),
        &input,
        &staged,
        encode::Timing {
            rate: output_rate,
            video_quality,
            speed: media_speed,
            duration: schedule.output_frames() as f64 * output_rate.den as f64
                / output_rate.num as f64,
            preserve_source_timing: !retiming,
            source_audio_bitrates: info.audio_bitrates.clone(),
            chapter_metadata,
        },
        &extension,
        &args,
    );
    command.stdin(Stdio::piped()).stdout(Stdio::null());
    let mut encoder = ManagedChild::spawn(command, "FFmpeg encoding", cancel.clone())?;
    let frames = RawFrames::new(decoder.take_stdout(), info.width, info.height)?;
    let mut sink = encoder.take_stdin();
    let estimated = Some(schedule.output_frames());
    let mut progress = ui.progress(output_rate, estimated);
    let result = engine.interpolate_stream(
        frames,
        StreamOptions {
            threshold: args.threshold,
            schedule: Some(schedule),
            ..StreamOptions::default()
        },
        &cancel,
        |frame| {
            sink.write_all(frame.pixels())
                .context("could not stream a frame to FFmpeg")
        },
        |done| progress.advance(done),
    );
    drop(sink); // EOF lets the encoder flush and finalize its container.
    let result = result.and_then(|stats| {
        progress.finalizing();
        decoder.finish()?;
        encoder.finish()?;
        cancel.check()?;
        ensure!(
            staged.is_file() && fs::metadata(&staged)?.len() > 0,
            "FFmpeg did not produce a valid output"
        );
        paths::commit_output(&staged, &output, args.overwrite, &cancel)?;
        Ok(stats)
    });
    match result {
        Ok(stats) => {
            progress.finish(stats.output_frames);
            Ok(())
        }
        Err(error) => {
            progress.abandon();
            // Watchdogs terminate both pipes even if a reader/writer was blocked.
            cancel.cancel();
            // A broken input/output pipe is often caused by FFmpeg rejecting a
            // codec or input. Preserve its bounded diagnostic tail, rather than
            // reporting only the secondary pipe error.
            let mut error = error;
            for child in [&mut decoder, &mut encoder] {
                if child.monitor.is_some()
                    && let Err(diagnostic) = child.finish()
                {
                    error = attach_cleanup_failure(error, diagnostic);
                }
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn silent_cleanup_termination_does_not_obscure_decoder_diagnostics() {
        let original = anyhow!("NCNN interpolation failed (code -3)");
        let decoder = failure("FFmpeg decoding", "Error muxing a packet", true);
        let heading = decoder.to_string();
        let error = attach_cleanup_failure(original, decoder);
        let error = attach_cleanup_failure(error, failure("FFmpeg encoding", "", true));
        assert_eq!(
            format!("{error:#}"),
            format!("{heading}: NCNN interpolation failed (code -3)")
        );
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
    fn independent_silent_failure_is_preserved_without_a_blank_line() {
        let failure = failure("FFmpeg encoding", "", false);
        let heading = failure.to_string();
        assert!(!heading.contains('\n'));
        let error = attach_cleanup_failure(anyhow!("Broken pipe"), failure);
        assert_eq!(format!("{error:#}"), format!("{heading}: Broken pipe"));
    }

    #[test]
    fn cleanup_preserves_encoder_diagnostics_and_standalone_inference_errors() {
        let original = anyhow!("NCNN interpolation failed (code -3)");
        let error = attach_cleanup_failure(original, failure("FFmpeg decoding", "", true));
        assert_eq!(format!("{error:#}"), "NCNN interpolation failed (code -3)");
        let encoder = failure("FFmpeg encoding", "Unknown encoder", true);
        let heading = encoder.to_string();
        let error = attach_cleanup_failure(error, encoder);
        assert_eq!(
            format!("{error:#}"),
            format!("{heading}: NCNN interpolation failed (code -3)")
        );
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
