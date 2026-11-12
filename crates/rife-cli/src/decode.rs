// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! CFR normalization shared by exact counting and RGB processing.
use crate::{
    config,
    media::{self, Rate},
    process::ManagedChild,
    ui::Ui,
};
use anyhow::{Context, Result, ensure};
use rife_core::CancellationToken;
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
};

fn input_command(ffmpeg: &Path, input: &Path) -> Command {
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-loglevel", "warning", "-nostdin", "-i"])
        .arg(input)
        .args(["-map", "0:v:0", "-an", "-sn", "-dn"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    command
}

fn normalization_filter(rate: Rate) -> String {
    format!("fps=fps={rate}:start_time=0")
}

pub(crate) fn command(ffmpeg: &Path, input: &Path, rate: Rate) -> Command {
    let mut command = input_command(ffmpeg, input);
    command.args([
        "-vf",
        &format!(
            "{},format={}",
            normalization_filter(rate),
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
    ]);
    command
}

pub(crate) fn counting_command(ffmpeg: &Path, input: &Path, rate: Rate) -> Command {
    let mut command = input_command(ffmpeg, input);
    command.args([
        "-vf",
        &normalization_filter(rate),
        "-fps_mode",
        "passthrough",
        "-c:v",
        "wrapped_avframe",
        "-f",
        "null",
        "-nostats",
        "-progress",
        "pipe:1",
        "-stats_period",
        &config::PROGRESS_INTERVAL.as_secs_f64().to_string(),
        "-",
    ]);
    command
}

// Metadata cannot establish the normalized count: timestamp seeking is not a
// frame-index existence query. Keep the exact sequential pass, but let FFmpeg
// count filtered frames internally instead of converting/transferring RGB data.
pub(crate) fn count_frames(
    ffmpeg: &Path,
    input: &Path,
    info: &media::VideoInfo,
    cancel: &CancellationToken,
    ui: &Ui,
) -> Result<u64> {
    let mut counting = ui.counting();
    let mut child = ManagedChild::spawn(
        counting_command(ffmpeg, input, info.rate),
        "FFmpeg counting",
        cancel.clone(),
    )?;
    let result = read_count(&mut BufReader::new(child.take_stdout()), cancel, |frames| {
        counting.advance(frames)
    });
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

pub(crate) fn read_count(
    reader: &mut impl BufRead,
    cancel: &CancellationToken,
    mut progress: impl FnMut(u64),
) -> Result<u64> {
    let mut frames = 0;
    let mut complete = false;
    let mut line = String::new();
    loop {
        cancel.check()?;
        line.clear();
        match reader.read_line(&mut line) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => {
                if result.context("could not read normalized frame count")? == 0 {
                    break;
                }
            }
        }
        if let Some(value) = line.strip_prefix("frame=") {
            let count = value
                .trim()
                .parse()
                .context("FFmpeg returned an invalid frame count")?;
            ensure!(count >= frames, "FFmpeg returned a decreasing frame count");
            frames = count;
            progress(frames);
        } else if line.trim_end() == "progress=end" {
            complete = true;
        }
    }
    ensure!(
        complete,
        "FFmpeg counting did not finish its progress report"
    );
    ensure!(frames > 0, "input contains no video frames");
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn counting_requires_a_complete_valid_report_and_remains_cancellable() {
        let cancel = CancellationToken::default();
        let mut observed = Vec::new();
        let count = read_count(
            &mut &b"frame= 1\nfps=0\nprogress=continue\nframe=7\nprogress=end\n"[..],
            &cancel,
            |count| observed.push(count),
        )
        .unwrap();
        assert_eq!(count, 7);
        assert_eq!(observed, [1, 7]);
        for input in [
            "frame=7\n",
            "frame=0\nprogress=end\n",
            "frame=bad\nprogress=end\n",
            "frame=-1\nprogress=end\n",
            "frame=7\nframe=6\nprogress=end\n",
        ] {
            assert!(
                read_count(&mut input.as_bytes(), &cancel, |_| {}).is_err(),
                "{input:?}"
            );
        }
        assert!(
            read_count(&mut &b"frame=1\nprogress=end\n"[..], &cancel, |_| cancel
                .cancel())
            .unwrap_err()
            .to_string()
            .contains("cancelled")
        );
    }

    #[test]
    fn counting_preserves_reader_errors() {
        struct Failed;
        impl Read for Failed {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("test read failed"))
            }
        }
        let error = read_count(
            &mut BufReader::new(Failed),
            &CancellationToken::default(),
            |_| {},
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("test read failed"));
    }
}
