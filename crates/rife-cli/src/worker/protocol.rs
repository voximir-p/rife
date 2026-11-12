// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Private worker configuration and structured status wire format.
use super::EVENT_PREFIX;
use crate::cli::Args;
use anyhow::{Context, Result};
use rife_core::{FrameSchedule, PlaybackOptions};
use serde::{Deserialize, Serialize};
use std::io::{self, Write};

#[derive(Serialize, Deserialize)]
pub(crate) struct Config {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) input_frames: u64,
    #[serde(default)]
    pub(super) stream_output: bool,
    pub(super) fps_scale: String,
    pub(super) speed: String,
    pub(super) threshold: String,
    pub(super) devices: String,
    pub(super) jobs: String,
}

impl Config {
    pub(crate) fn new(args: &Args, dimensions: (u32, u32), input_frames: u64) -> Self {
        let options = args.engine_options();
        Self {
            width: dimensions.0,
            height: dimensions.1,
            input_frames,
            stream_output: args.streams_output(),
            fps_scale: args.fps_scale.to_string(),
            speed: args.speed.to_string(),
            threshold: args.threshold.to_string(),
            devices: options.devices.to_string(),
            jobs: options.jobs.unwrap_or_default().to_string(),
        }
    }

    pub(super) fn schedule(&self) -> Result<FrameSchedule> {
        FrameSchedule::for_playback(
            self.input_frames,
            PlaybackOptions {
                fps_scale: self.fps_scale.parse()?,
                speed: self.speed.parse()?,
            },
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum Event {
    Loaded {
        devices: Vec<String>,
    },
    Progress {
        frames: u64,
    },
    Finished {
        frames: u64,
    },
    Error {
        message: String,
        native: bool,
        #[serde(default)]
        details: Vec<String>,
    },
}

/// Reuse the serialization buffer; status must stay on stderr, never RGB stdout.
#[derive(Default)]
pub(super) struct EventWriter {
    line: Vec<u8>,
}
impl EventWriter {
    pub(super) fn send(&mut self, event: &Event) -> Result<()> {
        self.line.clear();
        self.line.extend_from_slice(EVENT_PREFIX.as_bytes());
        serde_json::to_writer(&mut self.line, event)?;
        self.line.push(b'\n');
        io::stderr()
            .write_all(&self.line)
            .context("could not report native worker status")
    }
}

#[cfg(test)]
pub(super) fn send_event(event: &Event) -> Result<()> {
    EventWriter::default().send(event)
}
