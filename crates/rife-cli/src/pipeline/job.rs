// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Prepare a destination and encoder before counting or loading the model.
use super::{
    output::{Destination, StagedOutput},
    preflight,
};
use crate::{
    chapters,
    cli::Args,
    encode,
    media::{self, Rate},
    paths,
    ui::{self, Ui},
};
use anyhow::{Context, Result, ensure};
use rife_core::{CancellationToken, DeviceSelection, FrameSchedule, PlaybackOptions};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

pub(super) struct PreparedJob {
    // Fields drop in declaration order: remove staging before releasing the lock.
    pub staging: TempDir,
    pub args: Args,
    pub input: PathBuf,
    pub ffmpeg: PathBuf,
    pub model: PathBuf,
    pub info: media::VideoInfo,
    output_rate: Rate,
    quality: encode::QualityPlan,
    chapter_metadata: Option<PathBuf>,
    pub staged_output: StagedOutput,
    pub destination: Destination,
}

pub(super) struct PlannedJob {
    pub prepared: PreparedJob,
    pub schedule: FrameSchedule,
    pub encoder: Command,
}

impl PreparedJob {
    pub fn prepare(args: Args, cancel: &CancellationToken, ui: &Ui) -> Result<Self> {
        let input = paths::resolve_input(&args.input)?;
        let destination = Destination::acquire(&args, &input, cancel, ui)?;
        let exe = env::current_exe().context("unable to locate the current executable")?;
        let root = dunce::simplified(&exe)
            .parent()
            .context("executable has no parent directory")?;
        let ffmpeg = PathBuf::from("ffmpeg");
        let ffprobe = Path::new("ffprobe");
        let model = paths::find_model(root, &args.model)?;
        validate_options(&args, &destination)?;
        let staging = paths::staging_directory(destination.parent())?;
        let staged_output = destination.stage(staging.path())?;
        let info = media::probe(ffprobe, &input, cancel, !destination.is_sequence())?;
        let (numerator, denominator) = playback(&args).fps_ratio()?;
        let output_rate = info.rate.scaled(numerator, denominator)?;
        validate_audio(&args, &destination, &info)?;
        let quality = if destination.is_sequence() {
            encode::QualityPlan::Fixed(encode::VideoQuality::FormatDefault)
        } else {
            encode::prepare_video_quality(
                &ffmpeg,
                destination.encoder_output(&args),
                &args,
                encode::VideoQualityInput {
                    source_crf: info.source_crf.as_deref(),
                    source_bitrate: info.video_bitrate,
                },
                cancel,
            )?
        };
        let mut job = Self {
            staging,
            args,
            input,
            ffmpeg,
            model,
            info,
            output_rate,
            quality,
            chapter_metadata: None,
            staged_output,
            destination,
        };
        job.prepare_chapters()?;
        job.validate_encoder(cancel)?;
        Ok(job)
    }

    fn prepare_chapters(&mut self) -> Result<()> {
        if !self.destination.is_sequence()
            && encode::requires_retiming(self.args.speed)
            && encode::supports_timed_metadata(self.destination.format(&self.args))
            && !self.info.chapters.is_empty()
        {
            let metadata = dunce::simplified(self.staging.path()).join("chapters.ffmetadata");
            // Whole-frame rounding can only reduce effective speed. Reject
            // invalid chapters at the requested speed before counting, then
            // rewrite with the actual output duration when the count is known.
            self.write_chapters(&metadata, self.args.speed)?;
            self.chapter_metadata = Some(metadata);
        }
        Ok(())
    }

    fn write_chapters(&self, path: &std::path::Path, speed: f64) -> Result<()> {
        chapters::write_retimed(
            path,
            &self.info.chapters,
            self.info.source_start_time,
            speed,
        )
    }

    fn encoder_command(
        &self,
        quality: &encode::VideoQuality,
        speed: f64,
        duration: f64,
    ) -> Command {
        let dimensions = (self.info.width, self.info.height);
        if self.destination.is_sequence() {
            encode::image_command(&self.ffmpeg, dimensions, self.output_rate, &self.args)
        } else {
            encode::command(
                &self.ffmpeg,
                dimensions,
                &self.input,
                encode::Timing {
                    rate: self.output_rate,
                    video_quality: quality,
                    speed,
                    duration,
                    preserve_source_timing: !encode::requires_retiming(self.args.speed),
                    source_audio_bitrates: &self.info.audio_bitrates,
                    chapter_metadata: self.chapter_metadata.as_deref(),
                },
                self.destination.encoder_output(&self.args),
                &self.args,
            )
        }
    }

    fn validate_encoder(&self, cancel: &CancellationToken) -> Result<()> {
        // Use one blank RGB frame: some GIF/WebP encoders reject zero frames.
        let directory = dunce::simplified(self.staging.path()).join("check");
        fs::create_dir(&directory)?;
        let staged = self.destination.stage(&directory)?;
        let mut command =
            self.encoder_command(&self.quality.for_validation(), self.args.speed, 0.1);
        command
            .args([
                "-frames:v",
                "1",
                "-frames:a",
                "1",
                "-frames:s",
                "1",
                "-t",
                "0.1",
            ])
            .arg(staged.path());
        preflight::validate_encoding(command, (self.info.width, self.info.height), cancel)?;
        fs::remove_dir_all(directory).context("could not clean up output validation files")
    }

    pub fn report(&self, ui: &Ui) {
        if self.info.hdr {
            ui::warning(
                "HDR transfer detected; the RGB8 interpolation pipeline does not preserve HDR.",
            );
        }
        for (label, rate) in [("Source", self.info.rate), ("Target", self.output_rate)] {
            ui.info(
                label,
                format_args!(
                    "{}x{} @ {:.2} fps",
                    self.info.width,
                    self.info.height,
                    rate.num as f64 / rate.den as f64
                ),
            );
        }
    }

    pub fn plan(self, input_frames: u64, cancel: &CancellationToken) -> Result<PlannedJob> {
        let schedule = FrameSchedule::for_playback(input_frames, playback(&self.args))?;
        self.destination.validate_count(
            &self.input,
            schedule.output_frames(),
            self.args.overwrite,
            cancel,
        )?;
        let speed = self.args.fps_scale * input_frames as f64 / schedule.output_frames() as f64;
        if let Some(metadata) = &self.chapter_metadata {
            self.write_chapters(metadata, speed)?;
        }
        let quality = self
            .quality
            .for_frames(input_frames, schedule.output_frames())?;
        let duration = schedule.output_frames() as f64 * self.output_rate.den as f64
            / self.output_rate.num as f64;
        let mut encoder = self.encoder_command(&quality, speed, duration);
        encoder.arg(self.staged_output.path());
        Ok(PlannedJob {
            prepared: self,
            schedule,
            encoder,
        })
    }
}

fn playback(args: &Args) -> PlaybackOptions {
    PlaybackOptions {
        fps_scale: args.fps_scale,
        speed: args.speed,
    }
}

fn validate_options(args: &Args, destination: &Destination) -> Result<()> {
    let minimum = FrameSchedule::for_playback(1, playback(args))?;
    let device_count = match &args.gpu {
        DeviceSelection::Auto => 1,
        DeviceSelection::Ids(ids) => ids.len(),
    };
    args.jobs.clone().for_devices(device_count)?;
    destination.validate_minimum_count(minimum.output_frames())?;
    ensure!(
        !destination.carries_audio(args)
            || !encode::requires_retiming(args.speed)
            || args.audio_codec() != Some("copy"),
        "audio codec copy cannot be used when playback speed changes"
    );
    Ok(())
}

fn validate_audio(args: &Args, destination: &Destination, info: &media::VideoInfo) -> Result<()> {
    let encoded = destination.carries_audio(args)
        && args.audio_codec().map_or_else(
            || encode::requires_retiming(args.speed),
            |codec| codec != "copy",
        );
    ensure!(
        !encoded || info.audio_bitrates.iter().all(Option::is_some),
        "could not determine every source audio bitrate"
    );
    Ok(())
}
