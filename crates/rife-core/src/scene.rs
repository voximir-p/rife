// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{Frame, Result, settings::RGB_CHANNELS};
use anyhow::ensure;

const SIDE: usize = 64;
const BINS: usize = 512;

/// Joint RGB histogram comparison on a fixed, evenly distributed 64×64 grid.
/// Threshold zero always cuts; threshold one never cuts.
#[derive(Debug)]
pub struct SceneDetector {
    threshold: f64,
}

impl SceneDetector {
    pub fn new(threshold: f64) -> Result<Self> {
        ensure!(
            threshold.is_finite() && (0.0..=1.0).contains(&threshold),
            "threshold must be finite and between 0 and 1"
        );
        Ok(Self { threshold })
    }

    pub fn is_cut(&self, first: &Frame, second: &Frame) -> bool {
        if self.threshold == 0.0 {
            return true;
        }
        if self.threshold == 1.0 {
            return false;
        }
        self.histograms_differ(&histogram(first), &histogram(second))
    }

    pub(crate) fn threshold(&self) -> f64 {
        self.threshold
    }
    pub(crate) fn histograms_differ(&self, first: &[u32; BINS], second: &[u32; BINS]) -> bool {
        let difference: u32 = first.iter().zip(second).map(|(a, b)| a.abs_diff(*b)).sum();
        self.threshold < 1.0 && difference as f64 / (2 * SIDE * SIDE) as f64 >= self.threshold
    }
}

pub(crate) fn histogram(frame: &Frame) -> [u32; BINS] {
    let mut bins = [0; BINS];
    let width = frame.width() as usize;
    let height = frame.height() as usize;
    for row in 0..SIDE {
        let y = ((row * 2 + 1) as u64 * height as u64 / (SIDE * 2) as u64) as usize;
        for column in 0..SIDE {
            let x = ((column * 2 + 1) as u64 * width as u64 / (SIDE * 2) as u64) as usize;
            let offset = (y * width + x) * RGB_CHANNELS;
            let rgb = &frame.pixels()[offset..offset + RGB_CHANNELS];
            let index = ((rgb[0] as usize >> 5) << 6)
                | ((rgb[1] as usize >> 5) << 3)
                | (rgb[2] as usize >> 5);
            bins[index] += 1;
        }
    }
    bins
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn threshold_endpoints_and_histogram_distance() {
        let a = Frame::new(64, 64, [255, 0, 0].repeat(4096)).unwrap();
        let b = Frame::new(64, 64, [0, 0, 255].repeat(4096)).unwrap();
        let mut pixels = a.pixels().to_vec();
        pixels[..6144].copy_from_slice(&b.pixels()[..6144]);
        let half = Frame::new(64, 64, pixels).unwrap();
        assert!(SceneDetector::new(0.0).unwrap().is_cut(&a, &a));
        assert!(!SceneDetector::new(1.0).unwrap().is_cut(&a, &b));
        assert!(SceneDetector::new(0.5).unwrap().is_cut(&a, &half));
        assert!(!SceneDetector::new(0.6).unwrap().is_cut(&a, &half));
        assert!(SceneDetector::new(f64::NAN).is_err());
        assert!(SceneDetector::new(-0.1).is_err());
    }
}
