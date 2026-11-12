// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Audio copying, pitch-preserving retiming and source bitrate selection.
use super::format::Container;
use crate::cli::Args;
use std::process::Command;

const DEFAULT_AUDIO_BITRATE: &str = "192k";
const AAC_BITRATE_SCALE_NUM: u64 = 124;
const AAC_BITRATE_SCALE_DEN: u64 = 100;
const RETIMING_EPSILON: f64 = 1e-12;

pub(crate) fn requires_retiming(speed: f64) -> bool {
    (speed - 1.0).abs() >= RETIMING_EPSILON
}

pub(super) fn append_audio_options(
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
    if let Some(codec) = args.audio_codec() {
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
pub(super) fn tempo_filter(mut speed: f64) -> String {
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
