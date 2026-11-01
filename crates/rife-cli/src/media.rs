// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use anyhow::{Context, Result};
use rife_core::CancellationToken;
use serde::Deserialize;
use std::{collections::HashMap, path::Path, process::Command};

const PROBE_ENTRIES: &str = concat!(
    "stream=index,codec_type,width,height,avg_frame_rate,r_frame_rate,color_transfer,bit_rate,duration:",
    "stream_side_data=rotation:",
    "stream_tags=encoder,comment,description,title,handler_name,encoded_by,writing_library,encoding_tool,software,crf,encoder_settings,encoder_options,ENCODER,COMMENT,DESCRIPTION,TITLE,HANDLER_NAME,ENCODED_BY,WRITING_LIBRARY,ENCODING_TOOL,SOFTWARE,CRF,ENCODER_SETTINGS,ENCODER_OPTIONS:",
    "format=duration:",
    "format_tags=encoder,comment,description,title,encoded_by,writing_library,encoding_tool,software,crf,encoder_settings,encoder_options,ENCODER,COMMENT,DESCRIPTION,TITLE,ENCODED_BY,WRITING_LIBRARY,ENCODING_TOOL,SOFTWARE,CRF,ENCODER_SETTINGS,ENCODER_OPTIONS"
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rate {
    pub(crate) num: u64,
    pub(crate) den: u64,
}

impl Rate {
    pub(crate) fn parse(source: &str) -> Option<Self> {
        let (n, d) = source.split_once('/')?;
        let num = n.parse::<u64>().ok()?;
        let den = d.parse::<u64>().ok()?;
        if num == 0 || den == 0 {
            return None;
        }
        let gcd = gcd(num, den);
        Self {
            num: num / gcd,
            den: den / gcd,
        }
        .ffmpeg_safe()
        .ok()
    }

    pub(crate) fn scaled(self, multiplier: u64, divisor: u64) -> Result<Self> {
        anyhow::ensure!(
            multiplier > 0 && divisor > 0,
            "frame-rate multiplier and divisor must be positive"
        );
        let (mut a, mut b, mut c, mut d) = (self.num, self.den, multiplier, divisor);
        fn reduce(num: &mut u64, den: &mut u64) {
            let common = gcd(*num, *den);
            *num /= common;
            *den /= common;
        }
        reduce(&mut a, &mut d);
        reduce(&mut c, &mut b);
        reduce(&mut c, &mut d);
        Self {
            num: a.checked_mul(c).context("output frame rate is too large")?,
            den: b.checked_mul(d).context("output frame rate is too large")?,
        }
        .ffmpeg_safe()
    }

    fn ffmpeg_safe(self) -> Result<Self> {
        // FFmpeg stores AVRational components as signed 32-bit integers. Keep
        // exact rates when possible; approximate only fractions that cannot
        // fit in those fields, avoiding overflow in -r and -enc_time_base.
        const MAX_COMPONENT: u64 = i32::MAX as u64;
        const MAX_DENOMINATOR: u64 = 1_000_000;

        if self.num <= MAX_COMPONENT && self.den <= MAX_COMPONENT {
            return Ok(self);
        }

        let rate = self.num as f64 / self.den as f64;
        anyhow::ensure!(
            rate.is_finite() && rate > 0.0 && rate < MAX_COMPONENT as f64,
            "output frame rate is outside FFmpeg's supported range"
        );
        let component_limit = (MAX_COMPONENT as f64 / rate)
            .floor()
            .min(MAX_COMPONENT as f64) as u64;
        let denominator = if rate * (MAX_DENOMINATOR as f64) < 0.5 {
            component_limit
        } else {
            component_limit.min(MAX_DENOMINATOR)
        };
        anyhow::ensure!(
            denominator > 0,
            "output frame rate is outside FFmpeg's supported range"
        );

        let numerator = (rate * denominator as f64).round() as u64;
        anyhow::ensure!(
            numerator > 0 && numerator <= MAX_COMPONENT,
            "output frame rate is outside FFmpeg's supported range"
        );
        let common = gcd(numerator, denominator);
        Ok(Self {
            num: numerator / common,
            den: denominator / common,
        })
    }
}

impl std::fmt::Display for Rate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[derive(Deserialize)]
struct Probe {
    streams: Vec<MediaStream>,
    format: Option<FormatInfo>,
}
#[derive(Deserialize)]
struct FormatInfo {
    duration: Option<String>,
    #[serde(default)]
    tags: HashMap<String, String>,
}
#[derive(Deserialize)]
struct SideData {
    rotation: Option<f64>,
}
#[derive(Deserialize)]
struct MediaStream {
    index: Option<usize>,
    codec_type: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    color_transfer: Option<String>,
    bit_rate: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    tags: HashMap<String, String>,
    #[serde(default)]
    side_data_list: Vec<SideData>,
}
pub(crate) struct VideoInfo {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rate: Rate,
    pub(crate) hdr: bool,
    pub(crate) source_crf: Option<String>,
    pub(crate) video_bitrate: Option<u64>,
    pub(crate) audio_bitrates: Vec<Option<u64>>,
}
pub(crate) fn probe(ffprobe: &Path, input: &Path, cancel: &CancellationToken) -> Result<VideoInfo> {
    let mut command = Command::new(ffprobe);
    command
        .args(["-v", "error", "-show_entries", PROBE_ENTRIES, "-of", "json"])
        .arg(input);
    let output = crate::process::capture_output(command, cancel)?;
    let probe: Probe = serde_json::from_slice(&output).context("invalid JSON from FFprobe")?;
    let stream = probe
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"))
        .context("input contains no video stream")?;
    let rate = stream
        .avg_frame_rate
        .as_deref()
        .and_then(Rate::parse)
        .or_else(|| stream.r_frame_rate.as_deref().and_then(Rate::parse))
        .context("could not read a usable frame rate from FFprobe")?;
    let (width, height) = display_dimensions(
        stream
            .width
            .context("could not read video width from FFprobe")?,
        stream
            .height
            .context("could not read video height from FFprobe")?,
        stream.side_data_list.iter().find_map(|data| data.rotation),
    );
    rife_core::Frame::byte_len(width, height)?;
    let audio_streams = probe
        .streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("audio"))
        .collect::<Vec<_>>();
    let need_audio_packets = audio_streams
        .iter()
        .any(|stream| parse_bitrate(stream.bit_rate.as_deref()).is_none());
    let need_video_packets = parse_bitrate(stream.bit_rate.as_deref()).is_none();
    let audio_packet_sizes = if need_audio_packets {
        packet_stats(ffprobe, input, "a", cancel)?
    } else {
        HashMap::new()
    };
    let video_packet_sizes = if need_video_packets {
        packet_stats(ffprobe, input, "v:0", cancel)?
    } else {
        HashMap::new()
    };
    let format_duration = probe
        .format
        .as_ref()
        .and_then(|format| format.duration.as_deref());
    let video_bitrate = parse_bitrate(stream.bit_rate.as_deref()).or_else(|| {
        let index = stream.index?;
        bitrate_from_packets(
            video_packet_sizes.get(&index)?,
            stream.duration.as_deref(),
            format_duration,
        )
    });
    let audio_bitrates = audio_streams
        .iter()
        .map(|stream| {
            parse_bitrate(stream.bit_rate.as_deref()).or_else(|| {
                let index = stream.index?;
                bitrate_from_packets(
                    audio_packet_sizes.get(&index)?,
                    stream.duration.as_deref(),
                    format_duration,
                )
            })
        })
        .collect();
    Ok(VideoInfo {
        width,
        height,
        rate,
        hdr: matches!(
            stream.color_transfer.as_deref(),
            Some("smpte2084" | "arib-std-b67")
        ),
        source_crf: find_source_crf(
            &stream.tags,
            probe.format.as_ref().map(|format| &format.tags),
        ),
        video_bitrate,
        audio_bitrates,
    })
}
fn parse_bitrate(source: Option<&str>) -> Option<u64> {
    source?.parse::<u64>().ok().filter(|rate| *rate > 0)
}

fn find_source_crf(
    stream_tags: &HashMap<String, String>,
    format_tags: Option<&HashMap<String, String>>,
) -> Option<String> {
    find_crf_in_tags(stream_tags).or_else(|| format_tags.and_then(find_crf_in_tags))
}

fn find_crf_in_tags(tags: &HashMap<String, String>) -> Option<String> {
    for (key, value) in tags {
        if key.eq_ignore_ascii_case("crf") && value.parse::<f64>().is_ok_and(|number| number >= 0.0)
        {
            return Some(value.clone());
        }
    }
    let mut candidates = tags.iter().collect::<Vec<_>>();
    candidates.sort_by_key(|(key, _)| {
        let key = key.to_ascii_lowercase();
        (
            !key.contains("encoder") && !key.contains("writing_library"),
            key,
        )
    });
    candidates
        .into_iter()
        .find_map(|(_, value)| extract_crf(value))
}

fn extract_crf(metadata: &str) -> Option<String> {
    let lowercase = metadata.to_ascii_lowercase();
    for (position, _) in lowercase.match_indices("crf") {
        let rest = metadata[position + 3..].trim_start_matches(|character: char| {
            character.is_ascii_whitespace() || matches!(character, '=' | ':')
        });
        let length = rest
            .char_indices()
            .find_map(|(index, character)| {
                (!character.is_ascii_digit() && !matches!(character, '.' | '+' | '-' | 'e' | 'E'))
                    .then_some(index)
            })
            .unwrap_or(rest.len());
        if length == 0 {
            continue;
        }
        let candidate = &rest[..length];
        if candidate.parse::<f64>().is_ok_and(|number| number >= 0.0) {
            return Some(candidate.to_owned());
        }
    }
    None
}

#[derive(Default)]
struct PacketStats {
    bytes: u64,
    start_time: Option<f64>,
    end_time: Option<f64>,
}

fn packet_stats(
    ffprobe: &Path,
    input: &Path,
    stream_selector: &str,
    cancel: &CancellationToken,
) -> Result<HashMap<usize, PacketStats>> {
    let mut command = Command::new(ffprobe);
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            stream_selector,
            "-show_packets",
            "-show_entries",
            "packet=stream_index,size,pts_time,duration_time",
            "-of",
            "compact=p=0:nk=0",
        ])
        .arg(input);
    let output = crate::process::capture_output(command, cancel)?;
    let mut stats = HashMap::<usize, PacketStats>::new();
    for line in String::from_utf8_lossy(&output).lines() {
        let mut index = None;
        let mut size = None;
        let mut pts = None;
        let mut duration = None;
        for field in line.split('|') {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            match key {
                "stream_index" => index = value.parse::<usize>().ok(),
                "size" => size = value.parse::<u64>().ok(),
                "pts_time" => pts = value.parse::<f64>().ok(),
                "duration_time" => duration = value.parse::<f64>().ok(),
                _ => {}
            }
        }
        let (Some(index), Some(size)) = (index, size) else {
            continue;
        };
        let packet = stats.entry(index).or_default();
        packet.bytes = packet
            .bytes
            .checked_add(size)
            .context("packet data is too large to measure its bitrate")?;
        if let (Some(pts), Some(duration)) = (pts, duration)
            && pts.is_finite()
            && duration.is_finite()
            && duration >= 0.0
        {
            packet.start_time = Some(packet.start_time.map_or(pts, |start| start.min(pts)));
            let end = pts + duration;
            packet.end_time = Some(packet.end_time.map_or(end, |previous| previous.max(end)));
        }
    }
    Ok(stats)
}

fn bitrate_from_packets(
    packets: &PacketStats,
    stream_duration: Option<&str>,
    format_duration: Option<&str>,
) -> Option<u64> {
    let packet_duration = packets
        .start_time
        .zip(packets.end_time)
        .map(|(start, end)| end - start)
        .filter(|duration| duration.is_finite() && *duration > 0.0);
    let valid_duration = |source: Option<&str>| {
        source
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
    };
    // The shortest valid span yields the conservative upper-bound estimate
    // when stream, container and packet timestamps disagree slightly.
    let duration = [
        packet_duration,
        valid_duration(stream_duration),
        valid_duration(format_duration),
    ]
    .into_iter()
    .flatten()
    .min_by(f64::total_cmp)?;
    let bitrate = (packets.bytes as f64 * 8.0 / duration).ceil();
    (bitrate.is_finite() && bitrate > 0.0 && bitrate <= u64::MAX as f64).then_some(bitrate as u64)
}

fn display_dimensions(width: u32, height: u32, rotation: Option<f64>) -> (u32, u32) {
    if rotation.is_some_and(|angle| (angle.rem_euclid(180.0) - 90.0).abs() < 0.01) {
        (height, width)
    } else {
        (width, height)
    }
}
pub(crate) fn realtime_factor(frames: u64, rate: Rate, seconds: f64) -> f64 {
    if seconds > 0.0 {
        frames as f64 * rate.den as f64 / rate.num as f64 / seconds
    } else {
        0.0
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_crf_values_only_when_metadata_exposes_them() {
        let tags = HashMap::from([
            ("encoder".to_owned(), "Lavc libx264 -crf 18.75".to_owned()),
            ("comment".to_owned(), "post-processed".to_owned()),
        ]);
        assert_eq!(find_crf_in_tags(&tags).as_deref(), Some("18.75"));
        assert!(
            find_crf_in_tags(&HashMap::from([(
                "encoder".to_owned(),
                "Lavc libx264".to_owned()
            )]))
            .is_none()
        );
    }

    #[test]
    fn accounts_for_ffmpeg_autorotation() {
        assert_eq!(display_dimensions(1920, 1080, Some(-90.0)), (1080, 1920));
        assert_eq!(display_dimensions(1920, 1080, Some(270.0)), (1080, 1920));
        assert_eq!(display_dimensions(1920, 1080, Some(180.0)), (1920, 1080));
        assert_eq!(display_dimensions(1920, 1080, None), (1920, 1080));
    }
    #[test]
    fn rational_rates_and_realtime_factor_are_exact() {
        assert_eq!(
            Rate::parse("30000/1001")
                .unwrap()
                .scaled(2, 1)
                .unwrap()
                .to_string(),
            "60000/1001"
        );
        assert!(Rate::parse("0/0").is_none());
        assert_eq!(realtime_factor(60, Rate::parse("30/1").unwrap(), 1.0), 2.0);
        assert_eq!(realtime_factor(30, Rate::parse("30/1").unwrap(), 10.0), 0.1);
    }
    #[test]
    fn arbitrary_fps_ratios_reduce_before_multiplication() {
        let rate = Rate::parse("30000/1001").unwrap();
        assert_eq!(rate.scaled(24, 6).unwrap().to_string(), "120000/1001");
        assert_eq!(rate.scaled(5, 6).unwrap().to_string(), "25000/1001");
        assert_eq!(rate.scaled(1, 6).unwrap().to_string(), "5000/1001");
        assert_eq!(
            Rate {
                num: u64::MAX,
                den: 1
            }
            .scaled(2, 2)
            .unwrap()
            .num,
            u64::MAX
        );
        assert!(
            Rate {
                num: u64::MAX,
                den: 1
            }
            .scaled(2, 1)
            .is_err()
        );
        assert!(rate.scaled(0, 1).is_err());
        assert!(rate.scaled(1, 0).is_err());
    }
}
