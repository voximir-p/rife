// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Read complete packed RGB frames and distinguish EOF from truncation.
use anyhow::{Context, Result};
use rife_core::Frame;
use std::io::{self, Read};

pub(super) struct RawFrames<R> {
    reader: R,
    width: u32,
    height: u32,
    bytes: usize,
    finished: bool,
}
impl<R: Read> RawFrames<R> {
    pub(super) fn new(reader: R, width: u32, height: u32) -> Result<Self> {
        Ok(Self {
            reader,
            width,
            height,
            bytes: Frame::byte_len(width, height)?,
            finished: false,
        })
    }
}
impl<R: Read> Iterator for RawFrames<R> {
    type Item = Result<Frame>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        let mut pixels = Vec::new();
        if let Err(error) = pixels.try_reserve_exact(self.bytes) {
            self.finished = true;
            return Some(Err(error).context("could not allocate decoded frame"));
        }
        pixels.resize(self.bytes, 0);
        match self.reader.read_exact(&mut pixels[..1]) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                self.finished = true;
                return None;
            }
            Err(error) => {
                self.finished = true;
                return Some(Err(error).context("could not read decoded frame"));
            }
        }
        if let Err(error) = self.reader.read_exact(&mut pixels[1..]) {
            self.finished = true;
            return Some(Err(error).context("FFmpeg returned a truncated RGB frame"));
        }
        Some(Frame::new(self.width, self.height, pixels))
    }
}
