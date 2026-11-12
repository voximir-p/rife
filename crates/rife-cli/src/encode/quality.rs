// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Resolve explicit quality, source CRF and count-dependent bitrate fallback.
use super::{Output, format::VideoPreset};
use crate::cli::Args;
use anyhow::Context;
use rife_core::CancellationToken;
use std::{borrow::Cow, path::Path, process::Command};

const DEFAULT_CRF: &str = "20";

pub(crate) fn is_valid_crf(value: &str) -> bool {
    value
        .parse::<f64>()
        .is_ok_and(|crf| crf.is_finite() && crf >= 0.0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VideoQuality {
    Crf(String),
    Bitrate(u64),
    FormatDefault,
}

pub(crate) enum QualityPlan {
    Fixed(VideoQuality),
    SourceBitrate(u64),
}

impl QualityPlan {
    pub(crate) fn for_validation(&self) -> VideoQuality {
        match self {
            Self::Fixed(quality) => quality.clone(),
            // The final bitrate requires the actual normalized frame count.
            Self::SourceBitrate(_) => VideoQuality::FormatDefault,
        }
    }

    pub(crate) fn for_frames(&self, input: u64, output: u64) -> anyhow::Result<VideoQuality> {
        match self {
            Self::Fixed(quality) => Ok(quality.clone()),
            Self::SourceBitrate(bitrate) => Ok(VideoQuality::Bitrate(scale_fallback_bitrate(
                *bitrate, input, output,
            )?)),
        }
    }
}

pub(crate) struct VideoQualityInput<'a> {
    pub source_crf: Option<&'a str>,
    pub source_bitrate: Option<u64>,
}

pub(crate) fn prepare_video_quality(
    ffmpeg: &Path,
    output: Output<'_>,
    args: &Args,
    input: VideoQualityInput<'_>,
    cancel: &CancellationToken,
) -> anyhow::Result<QualityPlan> {
    let container = output.presets();
    if args.extra_video_rate_control()
        || (container.has_video_only_output() && args.video_codec().is_none())
        || container.uses_gif_palette(args.video_codec())
    {
        return Ok(QualityPlan::Fixed(VideoQuality::FormatDefault));
    }

    if let Some(crf) = args.extra_video_crf() {
        // Preserve VP9's unconstrained CRF companion option, while explicit
        // bitrate/quantizer flags above leave rate control entirely to FFmpeg.
        return Ok(QualityPlan::Fixed(VideoQuality::Crf(crf.to_owned())));
    }

    let crf = input.source_crf.unwrap_or(DEFAULT_CRF).to_owned();
    let codec = match args
        .video_codec()
        .or(VideoPreset::for_container(container).codec)
    {
        Some(codec) => Some(Cow::Borrowed(codec)),
        None => query_muxer_video_codec(ffmpeg, output.format, cancel)?.map(Cow::Owned),
    };

    if let Some(codec) = codec.as_deref()
        && encoder_supports_crf(ffmpeg, codec, cancel)? == Some(false)
    {
        let source_bitrate = input
            .source_bitrate
            .context("could not determine the source video bitrate for the encoder fallback")?;
        return Ok(QualityPlan::SourceBitrate(source_bitrate));
    }
    Ok(QualityPlan::Fixed(VideoQuality::Crf(crf)))
}

fn query_muxer_video_codec(
    ffmpeg: &Path,
    format: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<Option<String>> {
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-h", &format!("muxer={format}")]);
    let output = match crate::process::capture_output_named(command, "FFmpeg encoder query", cancel)
    {
        Ok(output) => String::from_utf8_lossy(&output).into_owned(),
        Err(error) if cancel.is_cancelled() => return Err(error),
        Err(_) => return Ok(None),
    };
    Ok(output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Default video codec:")
            .map(str::trim)
            .filter(|codec| !codec.is_empty())
            .map(str::to_owned)
    }))
}

fn encoder_supports_crf(
    ffmpeg: &Path,
    codec: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<Option<bool>> {
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-h", &format!("encoder={codec}")]);
    let output = match crate::process::capture_output_named(command, "FFmpeg encoder query", cancel)
    {
        Ok(output) => String::from_utf8_lossy(&output).into_owned(),
        Err(error) if cancel.is_cancelled() => return Err(error),
        Err(_) => return Ok(Some(false)),
    };
    if output.contains("is not recognized by FFmpeg") {
        return Ok(None);
    }
    Ok(Some(output.lines().any(|line| {
        line.trim_start()
            .strip_prefix("-crf")
            .is_some_and(|rest| rest.starts_with(char::is_whitespace))
    })))
}

pub(super) fn scale_fallback_bitrate(
    source_bitrate: u64,
    input_frames: u64,
    output_frames: u64,
) -> anyhow::Result<u64> {
    anyhow::ensure!(
        input_frames > 0 && output_frames > 0,
        "frame counts must be positive"
    );
    // Calibrated so a 2x output frame count targets 1.5x source bitrate:
    // bitrate = source bitrate * 3 * output frames / (4 * input frames).
    let denominator = u128::from(input_frames) * 4;
    let total = u128::from(source_bitrate) * u128::from(output_frames);
    let whole = total / denominator;
    let remainder = total % denominator;
    anyhow::ensure!(
        whole <= u128::from(u64::MAX) / 3,
        "fallback video bitrate is too large"
    );
    let scaled = whole * 3 + (remainder * 3).div_ceil(denominator);
    anyhow::ensure!(
        scaled > 0 && scaled <= u128::from(u64::MAX),
        "fallback video bitrate is outside the supported range"
    );
    Ok(scaled as u64)
}
