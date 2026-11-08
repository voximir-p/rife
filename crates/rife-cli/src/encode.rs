// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Args, config, media::Rate};
use anyhow::Context;
use rife_core::CancellationToken;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

const DEFAULT_AUDIO_BITRATE: &str = "192k";
const AAC_BITRATE_SCALE_NUM: u64 = 124;
const AAC_BITRATE_SCALE_DEN: u64 = 100;
const PAD_EVEN: &str = "pad=ceil(iw/2)*2:ceil(ih/2)*2";
const RETIMING_EPSILON: f64 = 1e-12;
pub(crate) const DEFAULT_CRF: &str = "20";
const INVALID_CRF: &str = "CRF must be a number greater than or equal to 0";

pub(crate) fn parse_crf(value: &str) -> std::result::Result<String, String> {
    let crf = value.parse::<f64>().map_err(|_| INVALID_CRF.to_owned())?;
    if crf >= 0.0 {
        Ok(value.to_owned())
    } else {
        Err(INVALID_CRF.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VideoQuality {
    Crf(String),
    Bitrate(u64),
    FormatDefault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    Mp4,
    Matroska,
    TransportStream,
    WebM,
    Avi,
    Mobile3gp,
    Flv,
    WindowsMedia,
    Mpeg,
    Ogg,
    Apng,
    Webp,
    Gif,
    Other,
}

impl Container {
    fn from_extension(extension: &str) -> Self {
        match extension {
            "mp4" | "m4v" | "mov" => Self::Mp4,
            "mkv" => Self::Matroska,
            "ts" | "m2ts" => Self::TransportStream,
            "webm" => Self::WebM,
            "avi" => Self::Avi,
            "3gp" | "3g2" => Self::Mobile3gp,
            "flv" => Self::Flv,
            "wmv" | "asf" => Self::WindowsMedia,
            "mpg" | "mpeg" | "vob" => Self::Mpeg,
            "ogv" | "ogg" => Self::Ogg,
            "apng" => Self::Apng,
            "webp" => Self::Webp,
            "gif" => Self::Gif,
            _ => Self::Other,
        }
    }

    fn has_video_only_output(self) -> bool {
        matches!(self, Self::Gif | Self::Apng | Self::Webp)
    }

    fn uses_gif_palette(self, codec: Option<&str>) -> bool {
        self == Self::Gif && matches!(codec, None | Some("gif"))
    }

    fn supports_faststart(self) -> bool {
        self == Self::Mp4
    }
}

struct VideoPreset {
    codec: Option<&'static str>,
    options: &'static [&'static str],
    pad_even: bool,
}

impl VideoPreset {
    fn for_container(container: Container) -> Self {
        let (codec, options, pad_even): (_, &[&str], _) = match container {
            Container::Mp4 | Container::Matroska | Container::TransportStream => (
                Some("libx264"),
                &["-preset", "medium", "-pix_fmt", "yuv420p"],
                true,
            ),
            Container::WebM => (
                Some("libvpx-vp9"),
                &[
                    "-deadline",
                    "good",
                    "-cpu-used",
                    "4",
                    "-row-mt",
                    "1",
                    "-pix_fmt",
                    "yuv420p",
                ],
                true,
            ),
            Container::Avi | Container::Mobile3gp => {
                (Some("mpeg4"), &["-pix_fmt", "yuv420p"], true)
            }
            Container::Flv => (Some("flv1"), &[], false),
            Container::WindowsMedia => (Some("wmv2"), &[], false),
            Container::Mpeg => (Some("mpeg2video"), &[], false),
            Container::Ogg => (Some("libtheora"), &[], false),
            Container::Apng => (Some("apng"), &["-plays", "0"], false),
            Container::Webp => (
                Some("libwebp_anim"),
                &["-lossless", "0", "-q:v", "85", "-loop", "0"],
                false,
            ),
            Container::Gif | Container::Other => (None, &[], false),
        };
        Self {
            codec,
            options,
            pad_even,
        }
    }
}

fn append_video_options(
    cmd: &mut Command,
    container: Container,
    args: &Args,
    quality: &VideoQuality,
) {
    let preset = VideoPreset::for_container(container);
    let codec = args.video_codec.as_deref().or(preset.codec);
    if let Some(codec) = codec {
        cmd.args(["-c:v", codec]);
    }
    if args.video_codec.is_none() {
        cmd.args(preset.options);
        if preset.pad_even {
            // H.264 4:2:0 and related formats need even dimensions.
            cmd.args(["-vf", PAD_EVEN]);
        }
    }
    if !container.has_video_only_output() || args.video_codec.is_some() {
        match quality {
            VideoQuality::Crf(crf) => {
                cmd.args(["-crf", crf]);
                if codec == Some("libvpx-vp9") {
                    // libvpx selects CRF mode when its bitrate target is zero.
                    cmd.args(["-b:v", "0"]);
                }
            }
            VideoQuality::Bitrate(bitrate) => {
                cmd.args(["-b:v", &bitrate.to_string()]);
            }
            VideoQuality::FormatDefault => {}
        }
    }
}

pub(crate) fn select_video_quality(
    ffmpeg: &Path,
    extension: &str,
    args: &Args,
    source_crf: Option<&str>,
    source_bitrate: Option<u64>,
    input_frames: u64,
    output_frames: u64,
    cancel: &CancellationToken,
) -> anyhow::Result<VideoQuality> {
    let container = Container::from_extension(extension);
    if (container.has_video_only_output() && args.video_codec.is_none())
        || container.uses_gif_palette(args.video_codec.as_deref())
    {
        return Ok(VideoQuality::FormatDefault);
    }

    let crf = args
        .crf
        .as_deref()
        .or(source_crf)
        .unwrap_or(DEFAULT_CRF)
        .to_owned();
    let codec = args
        .video_codec
        .as_deref()
        .or(VideoPreset::for_container(container).codec)
        .map(str::to_owned)
        .or(query_muxer_video_codec(ffmpeg, extension, cancel)?);

    if let Some(codec) = codec
        && encoder_supports_crf(ffmpeg, &codec, cancel)? == Some(false)
    {
        let source_bitrate = source_bitrate
            .context("could not determine the source video bitrate for the encoder fallback")?;
        return Ok(VideoQuality::Bitrate(scale_fallback_bitrate(
            source_bitrate,
            input_frames,
            output_frames,
        )?));
    }
    Ok(VideoQuality::Crf(crf))
}

fn query_muxer_video_codec(
    ffmpeg: &Path,
    extension: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<Option<String>> {
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-h", &format!("muxer={extension}")]);
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

fn scale_fallback_bitrate(
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

pub(crate) fn requires_retiming(speed: f64) -> bool {
    (speed - 1.0).abs() >= RETIMING_EPSILON
}

fn append_audio_options(
    cmd: &mut Command,
    container: Container,
    args: &Args,
    speed: f64,
    duration: f64,
    preserve_source: bool,
    source_bitrates: &[Option<u64>],
) {
    // Keep the original encoded stream intact at normal playback speed. The
    // rounded frame schedule can produce a tiny duration difference even when
    // --speed is 1, which should not trigger an audio transcode.
    let retiming = !preserve_source && requires_retiming(speed);
    if retiming {
        cmd.args([
            "-filter:a",
            &format!("{},apad,atrim=duration={duration:.17}", tempo_filter(speed)),
        ]);
    }
    if let Some(codec) = args.audio_codec.as_deref() {
        cmd.args(["-c:a", codec]);
        if codec != "copy" {
            append_source_bitrates(cmd, source_bitrates, codec);
        }
        return;
    }
    if !retiming {
        cmd.args(["-c:a", "copy"]);
        return;
    }
    match container {
        Container::Mp4 | Container::Mobile3gp | Container::TransportStream => {
            cmd.args(["-c:a", "aac", "-b:a", DEFAULT_AUDIO_BITRATE]);
        }
        Container::WebM => {
            cmd.args(["-c:a", "libopus", "-b:a", "160k"]);
        }
        Container::Matroska => {
            cmd.args(["-c:a", "aac", "-b:a", DEFAULT_AUDIO_BITRATE]);
        }
        Container::Avi | Container::Flv => {
            cmd.args(["-c:a", "libmp3lame", "-b:a", DEFAULT_AUDIO_BITRATE]);
        }
        Container::WindowsMedia => {
            cmd.args(["-c:a", "wmav2", "-b:a", DEFAULT_AUDIO_BITRATE]);
        }
        Container::Mpeg => {
            cmd.args(["-c:a", "mp2", "-b:a", DEFAULT_AUDIO_BITRATE]);
        }
        Container::Ogg => {
            cmd.args(["-c:a", "libvorbis", "-q:a", "5"]);
        }
        Container::Apng | Container::Webp | Container::Gif | Container::Other => {} // Use the muxer's default.
    }
    let encoder = match container {
        Container::Mp4
        | Container::Mobile3gp
        | Container::TransportStream
        | Container::Matroska => "aac",
        Container::WebM => "libopus",
        Container::Avi | Container::Flv => "libmp3lame",
        Container::WindowsMedia => "wmav2",
        Container::Mpeg => "mp2",
        Container::Ogg => "libvorbis",
        Container::Apng | Container::Webp | Container::Gif | Container::Other => "",
    };
    append_source_bitrates(cmd, source_bitrates, encoder);
}

fn append_source_bitrates(cmd: &mut Command, source_bitrates: &[Option<u64>], encoder: &str) {
    for (index, bitrate) in source_bitrates.iter().enumerate() {
        if let Some(bitrate) = bitrate {
            // FFmpeg's native AAC encoder's CBR stream averages below its
            // requested rate. This conservative margin keeps the measured
            // output from undershooting the input stream's bitrate.
            let target = if encoder == "aac" {
                bitrate
                    .saturating_mul(AAC_BITRATE_SCALE_NUM)
                    .div_ceil(AAC_BITRATE_SCALE_DEN)
            } else {
                *bitrate
            };
            cmd.arg(format!("-b:a:{index}")).arg(target.to_string());
        }
    }
}

// Each atempo stage stays within [0.5,2], preserving pitch over large slowdowns.
fn tempo_filter(mut speed: f64) -> String {
    let mut filters = Vec::new();
    while speed < 0.5 {
        filters.push("atempo=0.5".to_owned());
        speed *= 2.0;
    }
    while speed > 2.0 {
        filters.push("atempo=2".to_owned());
        speed /= 2.0;
    }
    filters.push(format!("atempo={speed:.17}"));
    filters.join(",")
}

pub(crate) struct Timing {
    pub rate: Rate,
    pub video_quality: VideoQuality,
    // Match all timed source streams to the actual whole-frame output duration.
    pub speed: f64,
    pub duration: f64,
    pub preserve_source_timing: bool,
    pub source_audio_bitrates: Vec<Option<u64>>,
    pub chapter_metadata: Option<PathBuf>,
}

pub(crate) fn supports_timed_metadata(extension: &str) -> bool {
    !Container::from_extension(extension).has_video_only_output()
}

pub(crate) fn command(
    ffmpeg: &Path,
    dimensions: (u32, u32),
    input: &Path,
    staged_output: &Path,
    timing: Timing,
    ext: &str,
    args: &Args,
) -> Command {
    let output_rate = timing.rate;
    let container = Container::from_extension(ext);
    let mut cmd = Command::new(ffmpeg);
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-y",
        "-nostats",
        "-progress",
        "pipe:1",
        "-stats_period",
        "0.15",
    ])
    .args([
        "-f",
        "rawvideo",
        "-pixel_format",
        config::PIXEL_FORMAT,
        "-video_size",
        &format!("{}x{}", dimensions.0, dimensions.1),
    ])
    .args(["-framerate", &output_rate.to_string(), "-i", "pipe:0"]);

    let video_only = container.has_video_only_output();
    // FFmpeg requires every input to appear before output-specific options such as -map.
    if !video_only {
        cmd.arg("-i").arg(input);
        if let Some(metadata) = &timing.chapter_metadata {
            cmd.args(["-f", "ffmetadata", "-i"]).arg(metadata);
        }
    }
    if container.uses_gif_palette(args.video_codec.as_deref()) {
        cmd.args([
            "-filter_complex",
            "[0:v]split[a][b];[a]palettegen[p];[b][p]paletteuse[v]",
            "-map",
            "[v]",
            "-loop",
            "0",
        ]);
    } else {
        cmd.args(["-map", "0:v:0"]);
    }

    append_video_options(&mut cmd, container, args, &timing.video_quality);
    if !video_only {
        let chapter_input = if timing.chapter_metadata.is_some() {
            "2"
        } else {
            "1"
        };
        cmd.args([
            "-map",
            "1:a?",
            "-map_metadata",
            "1",
            "-map_chapters",
            chapter_input,
        ]);
        if container == Container::Matroska {
            cmd.args([
                "-map", "1:s?", "-map", "1:t?", "-c:s", "copy", "-c:t", "copy",
            ]);
            if !timing.preserve_source_timing && requires_retiming(timing.speed) {
                // Stream copying retains subtitle styles and attachments. Scale
                // both timestamps and durations: -itsscale only changes the former.
                cmd.args([
                    "-bsf:s",
                    &format!(
                        "setts=ts=TS/{0:.17}:duration=DURATION/{0:.17}",
                        timing.speed
                    ),
                ]);
            }
        }
        append_audio_options(
            &mut cmd,
            container,
            args,
            timing.speed,
            timing.duration,
            timing.preserve_source_timing,
            &timing.source_audio_bitrates,
        );
    }
    if container.supports_faststart() {
        cmd.args(["-movflags", "+faststart"]);
    }
    // Some encoders choose a nearby conventional rate (119.88 -> 120).
    // Match the raw input clock explicitly, including packet duration.
    cmd.args([
        "-fps_mode:v",
        "cfr",
        "-r:v",
        &output_rate.to_string(),
        "-force_fps:v",
        "-enc_time_base:v",
        &format!("{}:{}", output_rate.den, output_rate.num),
    ]);
    cmd.arg(staged_output);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn crf_is_applied_to_video_streams_and_selects_vp9_constant_quality() {
        let args =
            Args::try_parse_from(["rife", "-i", "input.mp4", "--crf", "21", "out.mp4"]).unwrap();
        let mut h264 = Command::new("ffmpeg");
        append_video_options(
            &mut h264,
            Container::Mp4,
            &args,
            &VideoQuality::Crf(args.crf.clone().unwrap()),
        );
        let h264 = h264
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(h264.windows(2).any(|pair| pair == ["-crf", "21"]));
        assert!(!h264.iter().any(|arg| {
            matches!(
                arg.as_str(),
                "-b:v" | "-minrate:v" | "-maxrate:v" | "-bufsize:v" | "-x264-params"
            )
        }));

        let vp9_args = Args::try_parse_from([
            "rife",
            "-i",
            "input.mp4",
            "--video-codec",
            "libvpx-vp9",
            "-c",
            "31",
            "out.webm",
        ])
        .unwrap();
        let mut vp9 = Command::new("ffmpeg");
        append_video_options(
            &mut vp9,
            Container::WebM,
            &vp9_args,
            &VideoQuality::Crf(vp9_args.crf.clone().unwrap()),
        );
        let vp9 = vp9
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(vp9.windows(2).any(|pair| pair == ["-crf", "31"]));
        assert!(vp9.windows(2).any(|pair| pair == ["-b:v", "0"]));

        let image_args = Args::try_parse_from(["rife", "-i", "input.mp4", "out.gif"]).unwrap();
        let mut gif = Command::new("ffmpeg");
        append_video_options(
            &mut gif,
            Container::Gif,
            &image_args,
            &VideoQuality::FormatDefault,
        );
        let gif = gif
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(!gif.iter().any(|arg| arg == "-crf"));

        let webp = VideoPreset::for_container(Container::Webp);
        assert!(
            webp.options
                .windows(2)
                .any(|pair| pair == ["-lossless", "0"])
        );
    }

    #[test]
    fn explicit_gif_encoder_keeps_palette_quality_without_bitrate_queries() {
        for codec in [None, Some("gif")] {
            let mut arguments = vec!["rife", "-i", "input.mp4"];
            if let Some(codec) = codec {
                arguments.extend(["--video-codec", codec]);
            }
            arguments.push("out.gif");
            let args = Args::try_parse_from(arguments).unwrap();
            // GIF quality must not depend on source bitrate or launching an
            // encoder query, even when its encoder is selected explicitly.
            let quality = select_video_quality(
                Path::new("missing-ffmpeg"),
                "gif",
                &args,
                None,
                None,
                34,
                136,
                &CancellationToken::default(),
            )
            .unwrap();
            assert_eq!(quality, VideoQuality::FormatDefault);
            let cmd = command(
                Path::new("ffmpeg"),
                (64, 64),
                Path::new("input.mp4"),
                Path::new("out.gif"),
                Timing {
                    rate: Rate { num: 30, den: 1 },
                    video_quality: quality,
                    speed: 0.25,
                    duration: 136.0 / 30.0,
                    preserve_source_timing: false,
                    source_audio_bitrates: Vec::new(),
                    chapter_metadata: None,
                },
                "gif",
                &args,
            );
            let options = cmd
                .get_args()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>();
            assert!(options.windows(2).any(|pair| pair
                == [
                    "-filter_complex",
                    "[0:v]split[a][b];[a]palettegen[p];[b][p]paletteuse[v]"
                ]));
            assert!(options.windows(2).any(|pair| pair == ["-map", "[v]"]));
            assert!(options.windows(2).any(|pair| pair == ["-loop", "0"]));
            assert!(
                !options
                    .iter()
                    .any(|arg| matches!(arg.as_ref(), "-crf" | "-b:v"))
            );
        }
        assert!(!Container::Gif.uses_gif_palette(Some("png")));
        assert!(!Container::Matroska.uses_gif_palette(Some("gif")));
    }

    #[test]
    fn fallback_bitrate_scales_with_output_frame_count() {
        assert_eq!(scale_fallback_bitrate(1000, 100, 200).unwrap(), 1500);
        assert_eq!(scale_fallback_bitrate(1000, 100, 100).unwrap(), 750);
        assert_eq!(scale_fallback_bitrate(1000, 100, 50).unwrap(), 375);
    }

    #[test]
    fn tempo_chain_preserves_pitch_without_changing_sample_rate() {
        for speed in [0.125, 0.5, 0.73, 1.0, 2.0] {
            let filter = tempo_filter(speed);
            let product: f64 = filter
                .split(',')
                .map(|stage| {
                    let value = stage
                        .strip_prefix("atempo=")
                        .unwrap()
                        .parse::<f64>()
                        .unwrap();
                    assert!((0.5..=2.0).contains(&value));
                    value
                })
                .product();
            assert!((product - speed).abs() < 1e-12);
        }
    }
}
