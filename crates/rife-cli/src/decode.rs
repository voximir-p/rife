// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! CFR normalization shared by frame counting and processing.
use crate::{
    config,
    media::{self, Rate},
    process::ManagedChild,
    ui::Ui,
};
use anyhow::{Context, Result, ensure};
use rife_core::{CancellationToken, Frame};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
};

pub(crate) fn command(ffmpeg: &Path, input: &Path, rate: Rate) -> Command {
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
pub(crate) fn count_frames(
    ffmpeg: &Path,
    input: &Path,
    info: &media::VideoInfo,
    cancel: &CancellationToken,
    ui: &Ui,
) -> Result<u64> {
    let mut counting = ui.counting();
    let mut child = ManagedChild::spawn(
        command(ffmpeg, input, info.rate),
        "FFmpeg counting",
        cancel.clone(),
    )?;
    let mut pipe = child.take_stdout();
    let bytes_per_frame = Frame::byte_len(info.width, info.height)? as u64;
    let result = count_rgb_frames(&mut pipe, bytes_per_frame, cancel, |frames| {
        counting.advance(frames)
    });
    drop(pipe);
    let finish = child.finish();
    cancel.check()?;
    match result {
        Ok(count) => {
            finish?;
            counting.finish();
            Ok(count)
        }
        Err(error) => Err(finish.err().unwrap_or(error)),
    }
}

fn count_rgb_frames(
    reader: &mut impl Read,
    bytes_per_frame: u64,
    cancel: &CancellationToken,
    mut progress: impl FnMut(u64),
) -> Result<u64> {
    let mut bytes = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancel.check()?;
        let n = reader
            .read(&mut buffer)
            .context("could not count normalized video")?;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .context("normalized video is too large")?;
        progress(bytes / bytes_per_frame);
    }
    ensure!(
        bytes.is_multiple_of(bytes_per_frame),
        "FFmpeg counting returned a truncated RGB frame"
    );
    let count = bytes / bytes_per_frame;
    ensure!(count > 0, "input contains no video frames");
    Ok(count)
}
