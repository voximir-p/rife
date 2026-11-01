// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    Result,
    settings::{MAX_PADDING, RGB_CHANNELS},
};
use anyhow::{Context, ensure};

/// An owned frame of tightly packed RGB8 pixels, with no row padding.
#[derive(Debug)]
pub struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Frame {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self> {
        ensure!(
            pixels.len() == Self::byte_len(width, height)?,
            "RGB frame byte length does not match its dimensions"
        );
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn byte_len(width: u32, height: u32) -> Result<usize> {
        ensure!(
            width > 0
                && height > 0
                && width <= i32::MAX as u32 - (MAX_PADDING - 1) as u32
                && height <= i32::MAX as u32 - (MAX_PADDING - 1) as u32,
            "invalid frame dimensions"
        );
        (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(RGB_CHANNELS))
            .filter(|&n| n <= isize::MAX as usize)
            .context("frame is too large")
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }
    pub(crate) fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    pub(crate) fn recycled(width: u32, height: u32, mut pixels: Vec<u8>) -> Result<Self> {
        let len = Self::byte_len(width, height)?;
        if pixels.len() < len {
            pixels
                .try_reserve_exact(len - pixels.len())
                .context("could not allocate a frame buffer")?;
        }
        pixels.resize(len, 0);
        Self::new(width, height, pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_dimensions_and_buffer_lengths() {
        assert!(Frame::new(2, 2, vec![0; 11]).is_err());
        assert!(Frame::new(0, 2, Vec::new()).is_err());
        assert!(Frame::byte_len(u32::MAX, 1).is_err());
        assert_eq!(Frame::new(2, 2, vec![0; 12]).unwrap().pixels().len(), 12);
    }
}
