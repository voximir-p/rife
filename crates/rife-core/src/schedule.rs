// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::Result;
use anyhow::{Context, ensure};

/// Exact count scheduling, or fixed steps along the normalized input timeline.
#[derive(Debug, Clone, Copy)]
pub struct FrameSchedule {
    pub(crate) input_frames: u64,
    pub(crate) output_frames: u64,
    step: SamplingStep,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SamplingStep {
    numerator: u128,
    denominator: u128,
}

impl SamplingStep {
    fn new(numerator: u128, denominator: u128) -> Self {
        let common = gcd(numerator, denominator);
        Self {
            numerator: numerator / common,
            denominator: denominator / common,
        }
    }

    fn output_count(self, input_frames: u64) -> Result<u64> {
        ensure!(input_frames > 0, "input contains no video frames");
        let scaled = u128::from(input_frames)
            .checked_mul(self.denominator)
            .context("requested output frame count is too large")?;
        u64::try_from((scaled - 1) / self.numerator + 1)
            .context("requested output frame count is too large")
    }

    pub(crate) fn sample(self, index: u64) -> Result<(u64, f32)> {
        if self.numerator == 1 && self.denominator == 2 {
            return Ok((index / 2, if index.is_multiple_of(2) { 0.0 } else { 0.5 }));
        }
        let position = u128::from(index)
            .checked_mul(self.numerator)
            .context("input timeline is too large")?;
        let pair =
            u64::try_from(position / self.denominator).context("input timeline is too large")?;
        let fraction = ((position % self.denominator) as f64 / self.denominator as f64) as f32;
        Ok((pair, fraction))
    }

    fn sample_clamped(self, index: u64, input_frames: u64) -> Result<(u64, f32)> {
        ensure!(input_frames > 0, "input contains no video frames");
        let (pair, fraction) = self.sample(index)?;
        if pair >= input_frames - 1 {
            Ok((input_frames - 1, 0.0))
        } else {
            Ok((pair, fraction))
        }
    }
}

impl FrameSchedule {
    pub fn input_frames(self) -> u64 {
        self.input_frames
    }
    pub fn output_frames(self) -> u64 {
        self.output_frames
    }
    pub const DEFAULT_TIME_STEP: f64 =
        PlaybackOptions::DEFAULT_SPEED / PlaybackOptions::DEFAULT_FPS_SCALE;
    /// Independent FPS and playback-speed factors. A partial final output
    /// interval rounds up; input positions advance by speed/fps_scale.
    pub fn for_playback(input_frames: u64, options: PlaybackOptions) -> Result<Self> {
        ensure!(input_frames > 0, "input contains no video frames");
        let step = options.sampling_step()?;
        let output_frames = step.output_count(input_frames)?;
        Ok(Self {
            input_frames,
            output_frames,
            step,
        })
    }
    pub fn for_count(input_frames: u64, output_frames: u64) -> Result<Self> {
        ensure!(
            input_frames > 0 && output_frames > 0,
            "frame counts must be positive"
        );
        Ok(Self {
            input_frames,
            output_frames,
            step: SamplingStep::new(u128::from(input_frames), u128::from(output_frames)),
        })
    }
    pub fn for_step(input_frames: u64, step: f64) -> Result<Self> {
        ensure!(input_frames > 0, "input contains no video frames");
        Self::validate_step(step)?;
        // Treat the shortest decimal representation as a rational interval.
        // This avoids both .3 ending one sample late and counts above 2^53
        // silently rounding through f64. Float conversion happens only at NCNN.
        let (numerator, denominator) = decimal_ratio(step)?;
        let sampling_step = SamplingStep::new(numerator, denominator);
        let output_frames = sampling_step.output_count(input_frames)?;
        Ok(Self {
            input_frames,
            output_frames,
            step: sampling_step,
        })
    }

    pub fn validate_step(step: f64) -> Result<()> {
        ensure!(
            step.is_finite() && step > 0.0 && step <= 1.0,
            "time step must be finite and in (0, 1]"
        );
        Ok(())
    }
    /// Input pair and fraction for output index. Count mode uses integer
    /// arithmetic for timestamp boundaries, even beyond f64's exact range.
    pub(crate) fn sample(self, index: u64) -> Result<(u64, f32)> {
        ensure!(index < self.output_frames, "output index exceeds schedule");
        self.step.sample_clamped(index, self.input_frames)
    }
    pub fn default_count(input_frames: u64) -> Result<Self> {
        Self::for_playback(input_frames, PlaybackOptions::default())
    }
}
/// Output FPS and playback speed are independent positive finite factors.
#[derive(Debug, Clone, Copy)]
pub struct PlaybackOptions {
    pub fps_scale: f64,
    pub speed: f64,
}
impl Default for PlaybackOptions {
    fn default() -> Self {
        Self {
            fps_scale: Self::DEFAULT_FPS_SCALE,
            speed: Self::DEFAULT_SPEED,
        }
    }
}
impl PlaybackOptions {
    pub const DEFAULT_FPS_SCALE: f64 = 1.0;
    pub const DEFAULT_SPEED: f64 = 1.0;

    pub fn validate_factor(factor: f64) -> Result<()> {
        ensure!(
            factor.is_finite() && factor > 0.0,
            "factor must be finite and greater than zero"
        );
        Ok(())
    }
    pub fn validate(self) -> Result<()> {
        Self::validate_factor(self.fps_scale)?;
        Self::validate_factor(self.speed)
    }
    pub fn fps_ratio(self) -> Result<(u64, u64)> {
        Self::validate_factor(self.fps_scale)?;
        let (num, den) = decimal_ratio(self.fps_scale)?;
        let ratio = SamplingStep::new(num, den);
        Ok((
            u64::try_from(ratio.numerator).context("fps multiplier numerator is too large")?,
            u64::try_from(ratio.denominator).context("fps multiplier denominator is too large")?,
        ))
    }

    pub(crate) fn sampling_step(self) -> Result<SamplingStep> {
        self.validate()?;
        let (scale_num, scale_den) = decimal_ratio(self.fps_scale)?;
        let (speed_num, speed_den) = decimal_ratio(self.speed)?;
        let numerator = speed_num
            .checked_mul(scale_den)
            .context("playback ratio is too large")?;
        let denominator = speed_den
            .checked_mul(scale_num)
            .context("playback ratio is too large")?;
        Ok(SamplingStep::new(numerator, denominator))
    }
}
fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn decimal_ratio(value: f64) -> Result<(u128, u128)> {
    let text = value.to_string();
    let digits = text.replace('.', "");
    let numerator = digits
        .parse::<u128>()
        .context("scheduling value is too large or too small")?;
    let places = text
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let denominator = 10u128
        .checked_pow(places as u32)
        .context("requested output frame count is too large")?;
    Ok((numerator, denominator))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arbitrary_counts_and_quarter_steps() {
        let s = FrameSchedule::for_count(3, 12).unwrap();
        assert_eq!(
            (0..5).map(|i| s.sample(i).unwrap()).collect::<Vec<_>>(),
            vec![(0, 0.0), (0, 0.25), (0, 0.5), (0, 0.75), (1, 0.0)]
        );
        let slow = FrameSchedule::for_step(3, 0.25).unwrap();
        assert_eq!(slow.output_frames, 12);
        assert_eq!(slow.sample(3).unwrap(), (0, 0.75));
        assert_eq!(slow.sample(11).unwrap(), (2, 0.0));
        for n in [1, 2, 7, 100] {
            for m in [1, 2, 3, 9, 400] {
                let s = FrameSchedule::for_count(n, m).unwrap();
                for i in 0..m {
                    let (pair, t) = s.sample(i).unwrap();
                    assert!(pair < n && (0.0..=1.0).contains(&t));
                }
            }
        }
    }
    #[test]
    fn invalid_and_large_schedules_are_checked() {
        for s in [0.0, -0.1, 1.1, f64::NAN, f64::INFINITY, f64::MIN_POSITIVE] {
            assert!(FrameSchedule::for_step(3, s).is_err());
        }
        assert!(FrameSchedule::for_count(0, 1).is_err());
        assert!(FrameSchedule::for_count(1, 0).is_err());
        assert_eq!(
            FrameSchedule::default_count(u64::MAX)
                .unwrap()
                .output_frames(),
            u64::MAX
        );
        assert_eq!(FrameSchedule::for_step(3, 0.3).unwrap().output_frames, 10);
        assert_eq!(
            FrameSchedule::for_step(9_007_199_254_740_993, 0.5)
                .unwrap()
                .output_frames,
            18_014_398_509_481_986
        );
        let n = u64::MAX / 2;
        let schedule = FrameSchedule::for_count(n, n).unwrap();
        assert_eq!(schedule.sample(n - 2).unwrap(), (n - 2, 0.0));
    }
    #[test]
    fn independent_fps_and_speed_match_all_requested_examples() {
        for (fps_scale, speed, count, position) in [
            (2.0, 1.0, 12, (0, 0.5)),
            (2.0, 0.5, 24, (0, 0.25)),
            (1.0, 0.5, 12, (0, 0.5)),
            (1.0, 1.0, 6, (1, 0.0)),
            (1.0, 2.0, 3, (2, 0.0)),
            (2.0, 0.125, 96, (0, 0.0625)),
            (1.5, 1.0, 9, (0, 2.0 / 3.0)),
        ] {
            let options = PlaybackOptions { fps_scale, speed };
            let schedule = FrameSchedule::for_playback(6, options).unwrap();
            assert_eq!(schedule.output_frames(), count);
            assert_eq!(schedule.sample(1).unwrap(), position);
        }
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e100] {
            assert!(
                FrameSchedule::for_playback(
                    6,
                    PlaybackOptions {
                        fps_scale: value,
                        ..PlaybackOptions::default()
                    }
                )
                .is_err()
            );
            assert!(
                FrameSchedule::for_playback(
                    6,
                    PlaybackOptions {
                        speed: value,
                        ..PlaybackOptions::default()
                    }
                )
                .is_err()
            );
        }
        assert_eq!(
            FrameSchedule::for_playback(u64::MAX, PlaybackOptions::default())
                .unwrap()
                .output_frames(),
            u64::MAX
        );
        assert!(
            FrameSchedule::for_playback(
                u64::MAX,
                PlaybackOptions {
                    fps_scale: 2.0,
                    speed: 1.0
                }
            )
            .is_err()
        );
    }
}
