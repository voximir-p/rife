// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Reduce encoder and muxer delay without changing file encoding defaults.
use super::{
    Output,
    format::{Container, VideoPreset},
};
use crate::cli::Args;
use std::process::Command;

impl Output<'_> {
    pub(super) fn presets(&self) -> Container {
        if self.stdout && self.format == "nut" {
            // Keep the established H.264/CRF and audio policy when using NUT
            // for immediate packet delivery instead of Matroska's clusters.
            Container::Matroska
        } else {
            Container::from_format(self.format)
        }
    }

    pub(super) fn append_streaming_options(&self, command: &mut Command, args: &Args) {
        let codec = args
            .video_codec()
            .or(VideoPreset::for_container(self.presets()).codec);
        match codec {
            Some("libx264" | "libx264rgb" | "libx265") => {
                command.args(["-tune", "zerolatency"]);
            }
            Some("libvpx" | "libvpx-vp9") => {
                command.args(["-lag-in-frames", "0"]);
            }
            _ => {}
        }
        command.args(["-flush_packets", "1", "-max_interleave_delta", "1"]);
        match Container::from_format(self.format) {
            Container::Matroska | Container::WebM => {
                command.args(["-cluster_time_limit", "0", "-cluster_size_limit", "0"]);
            }
            Container::TransportStream | Container::Mpeg => {
                command.args(["-muxdelay", "0", "-muxpreload", "0"]);
            }
            _ => {}
        }
        command.args(["-f", self.format]);
    }
}
