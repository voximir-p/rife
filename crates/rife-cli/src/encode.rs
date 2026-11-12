// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Build FFmpeg commands from borrowed, validated output settings.
mod audio;
mod format;
mod quality;
mod streaming;

use crate::{cli::Args, config, media::Rate};
use audio::append_audio_options;
pub(crate) use audio::requires_retiming;
use format::{Container, append_video_options};
pub(crate) use format::{carries_audio, supports_timed_metadata};
pub(crate) use quality::{
    QualityPlan, VideoQuality, VideoQualityInput, is_valid_crf, prepare_video_quality,
};
use std::{path::Path, process::Command};
#[cfg(test)]
use {
    audio::tempo_filter, format::VideoPreset, quality::scale_fallback_bitrate,
    rife_core::CancellationToken,
};

pub(crate) struct Output<'a> {
    pub format: &'a str,
    pub stdout: bool,
}

pub(crate) struct Timing<'a> {
    pub rate: Rate,
    pub video_quality: &'a VideoQuality,
    // Match all timed source streams to the actual whole-frame output duration.
    pub speed: f64,
    pub duration: f64,
    pub preserve_source_timing: bool,
    pub source_audio_bitrates: &'a [Option<u64>],
    pub chapter_metadata: Option<&'a Path>,
}

fn raw_video_command(
    ffmpeg: &Path,
    dimensions: (u32, u32),
    output_rate: Rate,
    streaming: bool,
) -> Command {
    let mut cmd = Command::new(ffmpeg);
    if streaming {
        // Dimensions, format and rate are explicit: no frame probing is needed.
        cmd.args([
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-fpsprobesize",
            "0",
        ]);
    }
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-y",
        "-nostats",
        "-progress",
        "pipe:2",
        "-stats_period",
        &config::PROGRESS_INTERVAL.as_secs_f64().to_string(),
        "-f",
        "rawvideo",
        "-pixel_format",
        config::PIXEL_FORMAT,
        "-video_size",
        &format!("{}x{}", dimensions.0, dimensions.1),
        "-framerate",
        &output_rate.to_string(),
        "-i",
        "pipe:0",
    ]);
    cmd
}

pub(crate) fn image_command(
    ffmpeg: &Path,
    dimensions: (u32, u32),
    rate: Rate,
    args: &Args,
) -> Command {
    let mut cmd = raw_video_command(ffmpeg, dimensions, rate, false);
    cmd.args([
        "-map",
        "0:v:0",
        "-fps_mode:v",
        "passthrough",
        "-f",
        "image2",
        "-start_number",
        "1",
    ]);
    if let Some(codec) = args.video_codec() {
        cmd.args(["-c:v", codec]);
    }
    // Pass packed RGB directly to the image encoder. FFmpeg chooses its codec,
    // supported pixel format and quality defaults; video presets do not apply.
    cmd.args(args.extra_encoder_args());
    cmd
}

pub(crate) fn command(
    ffmpeg: &Path,
    dimensions: (u32, u32),
    input: &Path,
    timing: Timing<'_>,
    output: Output<'_>,
    args: &Args,
) -> Command {
    let output_rate = timing.rate;
    let container = Container::from_format(output.format);
    let mut cmd = raw_video_command(ffmpeg, dimensions, output_rate, output.stdout);

    let video_only = container.has_video_only_output();
    // FFmpeg requires every input to appear before output-specific options such as -map.
    if !video_only {
        cmd.arg("-i").arg(input);
        if let Some(metadata) = &timing.chapter_metadata {
            cmd.args(["-f", "ffmetadata", "-i"]).arg(metadata);
        }
    }
    if container.uses_gif_palette(args.video_codec()) {
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

    let presets = output.presets();
    append_video_options(&mut cmd, presets, args, timing.video_quality);
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
            presets,
            args,
            timing.speed,
            timing.duration,
            timing.preserve_source_timing,
            timing.source_audio_bitrates,
        );
    }
    if !output.stdout && container.supports_faststart() {
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
    if output.stdout {
        // The explicit pipe protocol stays non-seekable even when stdout is
        // redirected to a regular file. Extra options can replace this format.
        output.append_streaming_options(&mut cmd, args);
    }
    cmd.args(args.extra_encoder_args());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn crf_is_applied_to_video_streams_and_selects_vp9_constant_quality() {
        let args = Args::try_parse_from(["rife", "-i", "input.mp4", "out.mp4"]).unwrap();
        let mut h264 = Command::new("ffmpeg");
        append_video_options(
            &mut h264,
            Container::Mp4,
            &args,
            &VideoQuality::Crf("21".into()),
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
            "-e",
            "-c:v libvpx-vp9",
            "out.webm",
        ])
        .unwrap();
        let mut vp9 = Command::new("ffmpeg");
        append_video_options(
            &mut vp9,
            Container::WebM,
            &vp9_args,
            &VideoQuality::Crf("31".into()),
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
        let directory = tempfile::tempdir().unwrap();
        let missing_ffmpeg = directory.path().join("missing-ffmpeg");
        for codec in [None, Some("-c:v gif")] {
            let mut arguments = vec!["rife", "-i", "input.mp4"];
            if let Some(codec) = codec {
                arguments.extend(["-e", codec]);
            }
            arguments.push("out.gif");
            let args = Args::try_parse_from(arguments).unwrap();
            // GIF quality must not depend on source bitrate or launching an
            // encoder query, even when its encoder is selected explicitly.
            let quality = prepare_video_quality(
                &missing_ffmpeg,
                Output {
                    format: "gif",
                    stdout: false,
                },
                &args,
                VideoQualityInput {
                    source_crf: None,
                    source_bitrate: None,
                },
                &CancellationToken::default(),
            )
            .unwrap()
            .for_frames(34, 136)
            .unwrap();
            assert_eq!(quality, VideoQuality::FormatDefault);
            let cmd = command(
                Path::new("ffmpeg"),
                (64, 64),
                Path::new("input.mp4"),
                Timing {
                    rate: Rate { num: 30, den: 1 },
                    video_quality: &quality,
                    speed: 0.25,
                    duration: 136.0 / 30.0,
                    preserve_source_timing: false,
                    source_audio_bitrates: &[],
                    chapter_metadata: None,
                },
                Output {
                    format: "gif",
                    stdout: false,
                },
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
    fn extra_options_follow_inputs_and_defaults_for_video_and_images() {
        let args = Args::try_parse_from([
            "rife",
            "-i",
            "input.mp4",
            "-e",
            "-c:a copy",
            "-e",
            "-crf=23",
            "-e",
            "-crf 12 -c:a aac -b:a 10k -metadata title='hello world'",
            "out.mp4",
        ])
        .unwrap();
        assert_eq!(args.audio_codec(), Some("aac"));
        let extra = args.extra_encoder_args().collect::<Vec<_>>();
        let mut command = command(
            Path::new("ffmpeg"),
            (64, 64),
            Path::new("input.mp4"),
            Timing {
                rate: Rate { num: 30, den: 1 },
                video_quality: &VideoQuality::Crf("20".into()),
                speed: 1.0,
                duration: 1.0,
                preserve_source_timing: true,
                source_audio_bitrates: &[Some(128_000)],
                chapter_metadata: None,
            },
            Output {
                format: "mp4",
                stdout: false,
            },
            &args,
        );
        command.arg("staged.mp4");
        let argv = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(&argv[argv.len() - extra.len() - 1..argv.len() - 1], extra);
        assert_eq!(argv.last(), Some(&"staged.mp4"));
        assert!(
            argv[..argv.len() - extra.len() - 1]
                .windows(2)
                .any(|pair| pair == ["-crf", "20"])
        );
        assert!(
            argv[..argv.len() - extra.len() - 1]
                .windows(2)
                .any(|pair| pair == ["-i", "input.mp4"])
        );

        let args = Args::try_parse_from([
            "rife",
            "-i",
            "input.mp4",
            "-e",
            "-compression_level=0",
            "frame_%03d.png",
        ])
        .unwrap();
        let command = image_command(
            Path::new("ffmpeg"),
            (65, 67),
            Rate { num: 30, den: 1 },
            &args,
        );
        let argv = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(&argv[argv.len() - 2..], ["-compression_level", "0"]);
        assert_eq!(argv.iter().filter(|&&arg| arg == "-i").count(), 1);
        assert!(!argv.contains(&"-pix_fmt") && !argv.contains(&"-crf"));
    }

    #[test]
    fn explicit_rate_control_does_not_require_automatic_bitrate_queries() {
        let directory = tempfile::tempdir().unwrap();
        let missing_ffmpeg = directory.path().join("missing-ffmpeg");
        for (extra, expected) in [
            ("-crf=23", VideoQuality::Crf("23".into())),
            ("-b:v 125k", VideoQuality::FormatDefault),
            ("-qp 10", VideoQuality::FormatDefault),
            ("-q:v 2", VideoQuality::FormatDefault),
        ] {
            let args =
                Args::try_parse_from(["rife", "-i", "in.mp4", "-e", extra, "out.mp4"]).unwrap();
            let quality = prepare_video_quality(
                &missing_ffmpeg,
                Output {
                    format: "mp4",
                    stdout: false,
                },
                &args,
                VideoQualityInput {
                    source_crf: Some("19"),
                    source_bitrate: None,
                },
                &CancellationToken::default(),
            )
            .unwrap();
            assert_eq!(quality.for_frames(2, 4).unwrap(), expected);
        }
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
