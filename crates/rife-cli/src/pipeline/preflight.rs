// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Validate output encoding with a bounded synthetic sample.
use crate::process::ManagedChild;
use anyhow::{Context, Result};
use rife_core::{CancellationToken, Frame};
use std::{
    io::Write,
    process::{Command, Stdio},
};

pub(super) fn validate_encoding(
    mut command: Command,
    dimensions: (u32, u32),
    cancel: &CancellationToken,
) -> Result<()> {
    command.stdin(Stdio::piped()).stdout(Stdio::null());
    let mut child = ManagedChild::encoder(command, cancel.clone(), None)?;
    let mut pipe = child.take_stdin()?;
    let write = write_blank_frame(&mut pipe, dimensions, cancel);
    drop(pipe);
    let finish = child.finish();
    cancel.check()?;
    finish?;
    write
}

fn write_blank_frame(
    writer: &mut impl Write,
    dimensions: (u32, u32),
    cancel: &CancellationToken,
) -> Result<()> {
    let mut remaining = Frame::byte_len(dimensions.0, dimensions.1)?;
    let zeros = [0u8; 64 * 1024];
    while remaining > 0 {
        cancel.check()?;
        let count = remaining.min(zeros.len());
        writer
            .write_all(&zeros[..count])
            .context("could not validate output encoding")?;
        remaining -= count;
    }
    Ok(())
}
