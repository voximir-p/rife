// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Container capabilities, presets and video options.
use super::quality::VideoQuality;
use crate::cli::Args;
use std::process::Command;

const PAD_EVEN: &str = "pad=ceil(iw/2)*2:ceil(ih/2)*2";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Container {
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
    pub(super) fn from_format(format: &str) -> Self {
        match format {
            "mp4" | "m4v" | "mov" => Self::Mp4,
            "mkv" | "matroska" => Self::Matroska,
            "ts" | "m2ts" | "mpegts" => Self::TransportStream,
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

    pub(super) fn has_video_only_output(self) -> bool {
        matches!(self, Self::Gif | Self::Apng | Self::Webp)
    }

    pub(super) fn uses_gif_palette(self, codec: Option<&str>) -> bool {
        self == Self::Gif && matches!(codec, None | Some("gif"))
    }

    pub(super) fn supports_faststart(self) -> bool {
        self == Self::Mp4
    }
}

pub(super) struct VideoPreset {
    pub(super) codec: Option<&'static str>,
    pub(super) options: &'static [&'static str],
    pad_even: bool,
}

impl VideoPreset {
    pub(super) fn for_container(container: Container) -> Self {
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

pub(super) fn append_video_options(
    cmd: &mut Command,
    container: Container,
    args: &Args,
    quality: &VideoQuality,
) {
    let preset = VideoPreset::for_container(container);
    let codec = args.video_codec().or(preset.codec);
    if let Some(codec) = codec {
        cmd.args(["-c:v", codec]);
    }
    if args.video_codec().is_none() {
        cmd.args(preset.options);
        if preset.pad_even {
            // H.264 4:2:0 and related formats need even dimensions.
            cmd.args(["-vf", PAD_EVEN]);
        }
    }
    if !container.has_video_only_output() || args.video_codec().is_some() {
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

pub(crate) fn carries_audio(format: &str) -> bool {
    !Container::from_format(format).has_video_only_output()
}

pub(crate) fn supports_timed_metadata(format: &str) -> bool {
    !Container::from_format(format).has_video_only_output()
}
