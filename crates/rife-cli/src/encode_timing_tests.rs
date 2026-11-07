// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Opt-in muxing regressions; these exercise the real encoder command without
//! requiring a model or an inference device.

use crate::{Args, chapters, encode, media};
use clap::Parser;
use rife_core::CancellationToken;
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn tool(name: &str) -> PathBuf {
    std::env::var_os(format!("RIFE_TEST_{}", name.to_ascii_uppercase()))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../runtime/ffmpeg")
                .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
        })
}

fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().expect("could not launch test tool");
    assert!(
        output.status.success(),
        "{command:?}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn probe(path: &Path) -> Value {
    serde_json::from_slice(&run(Command::new(tool("ffprobe"))
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_packets",
            "-show_chapters",
            "-show_format",
            "-show_data_hash",
            "sha256",
            "-of",
            "json",
        ])
        .arg(path)))
    .unwrap()
}

fn fixture(directory: &Path, offset: u32) -> PathBuf {
    let srt = directory.join("captions.srt");
    fs::write(
        &srt,
        "1\n00:00:00,500 --> 00:00:01,500\nFirst caption\n\n2\n00:00:02,000 --> 00:00:03,000\nSecond caption\n",
    )
    .unwrap();
    let ass = directory.join("styled.ass");
    fs::write(
        &ass,
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 16\nPlayResY: 16\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,12,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,0,0,0,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.75,0:00:01.75,Default,,0,0,0,,{\\i1}Styled caption\nDialogue: 0,0:00:02.50,0:00:03.50,Default,,0,0,0,,Last caption\n",
    )
    .unwrap();
    let metadata = directory.join("chapters.ffmeta");
    fs::write(
        &metadata,
        format!(
            ";FFMETADATA1\ntitle=Timing fixture\n[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle=Opening\\=one\\;\\#\\\\end\\\rcarriage\\\nline\n[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle=Second chapter\n",
            offset * 1000,
            (offset + 2) * 1000,
            (offset + 2) * 1000,
            (offset + 4) * 1000,
        ),
    )
    .unwrap();
    let attachment = directory.join("note.txt");
    fs::write(&attachment, "An attachment retained during retiming.\n").unwrap();
    let input = directory.join("source.mkv");
    run(Command::new(tool("ffmpeg"))
        .args([
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=size=16x16:rate=4:duration=4",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=4",
        ])
        .arg("-i")
        .arg(srt)
        .arg("-i")
        .arg(ass)
        .arg("-i")
        .arg(metadata)
        .args([
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:s",
            "-map",
            "3:s",
            "-map_metadata",
            "4",
            "-map_chapters",
            "4",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_s16le",
            "-c:s",
            "copy",
            "-metadata:s:s:0",
            "language=eng",
            "-metadata:s:s:0",
            "title=English captions",
            "-disposition:s:0",
            "default",
            "-metadata:s:s:1",
            "language=fra",
            "-disposition:s:1",
            "forced",
            "-attach",
        ])
        .arg(attachment)
        .args([
            "-metadata:s:t:0",
            "mimetype=text/plain",
            "-metadata:c:1",
            "title=Second chapter\\",
            "-output_ts_offset",
            &offset.to_string(),
        ])
        .arg(&input));
    input
}

fn encode_fixture(
    input: &Path,
    output: &Path,
    rate: media::Rate,
    frames: usize,
    speed: f64,
    preserve_source_timing: bool,
) -> Value {
    let ffmpeg = tool("ffmpeg");
    let info = media::probe(&tool("ffprobe"), input, &CancellationToken::default()).unwrap();
    let chapter_metadata = (!preserve_source_timing && !info.chapters.is_empty()).then(|| {
        let path = output.with_extension("ffmeta");
        chapters::write_retimed(&path, &info.chapters, info.source_start_time, speed).unwrap();
        path
    });
    let extension = output.extension().unwrap().to_str().unwrap();
    let args = Args::try_parse_from([
        "rife",
        "-i",
        "source.mkv",
        "--video-codec",
        if extension == "mkv" {
            "ffv1"
        } else {
            "libx264"
        },
        "--audio-codec",
        if extension == "mkv" {
            "pcm_s16le"
        } else {
            "aac"
        },
        "output.mkv",
    ])
    .unwrap();
    let mut command = encode::command(
        &ffmpeg,
        (16, 16),
        input,
        output,
        encode::Timing {
            rate,
            video_quality: encode::VideoQuality::FormatDefault,
            speed,
            duration: frames as f64 * rate.den as f64 / rate.num as f64,
            preserve_source_timing,
            source_audio_bitrates: info.audio_bitrates,
            chapter_metadata,
        },
        extension,
        &args,
    );
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&vec![0; 16 * 16 * 3 * frames])
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    probe(output)
}

fn streams<'a>(info: &'a Value, kind: &str) -> Vec<&'a Value> {
    info["streams"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|stream| stream["codec_type"] == kind)
        .collect()
}

fn packets<'a>(info: &'a Value, stream: &Value) -> Vec<&'a Value> {
    info["packets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|packet| packet["stream_index"] == stream["index"])
        .collect()
}

#[track_caller]
fn seconds(value: &Value) -> f64 {
    value.as_str().unwrap().parse().unwrap()
}

fn close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 0.002,
        "expected {expected}, got {actual}"
    );
}

fn check_chapters(source: &Value, output: &Value, offset: f64, speed: f64) {
    let original = source["chapters"].as_array().unwrap();
    let retimed = output["chapters"].as_array().unwrap();
    assert_eq!(original.len(), retimed.len());
    for (before, after) in original.iter().zip(retimed) {
        for field in ["start_time", "end_time"] {
            close(
                seconds(&after[field]),
                (seconds(&before[field]) - offset) / speed,
            );
        }
        assert_eq!(before["tags"]["title"], after["tags"]["title"]);
    }
}

#[test]
#[ignore = "requires FFmpeg and FFprobe in runtime/ffmpeg or RIFE_TEST_FFMPEG/RIFE_TEST_FFPROBE"]
fn subtitle_cues_and_chapters_follow_audio_timing() {
    let directory = tempfile::tempdir().unwrap();
    let input = fixture(directory.path(), 0);
    let original = probe(&input);
    for (name, rate, frames, speed, preserve) in [
        ("normal", media::Rate { num: 4, den: 1 }, 16, 1.0, true),
        ("slow", media::Rate { num: 4, den: 1 }, 32, 0.5, false),
        ("fast", media::Rate { num: 4, den: 1 }, 8, 2.0, false),
        (
            "rounded",
            media::Rate { num: 4, den: 1 },
            11,
            16.0 / 11.0,
            false,
        ),
        (
            "fps_only",
            media::Rate { num: 22, den: 5 },
            18,
            17.6 / 18.0,
            true,
        ),
    ] {
        let output = encode_fixture(
            &input,
            &directory.path().join(format!("{name}.mkv")),
            rate,
            frames,
            speed,
            preserve,
        );
        let factor = if preserve { 1.0 } else { speed };
        check_chapters(&original, &output, 0.0, factor);
        assert_eq!(output["format"]["tags"]["title"], "Timing fixture");
        let source_subtitles = streams(&original, "subtitle");
        let output_subtitles = streams(&output, "subtitle");
        assert_eq!(output_subtitles.len(), 2);
        for (before, after) in source_subtitles.iter().zip(output_subtitles) {
            assert_eq!(before["codec_name"], after["codec_name"]);
            assert_eq!(before["tags"]["language"], after["tags"]["language"]);
            assert_eq!(before["tags"]["title"], after["tags"]["title"]);
            assert_eq!(before["disposition"], after["disposition"]);
            let original_packets = packets(&original, before);
            let output_packets = packets(&output, after);
            assert_eq!(original_packets.len(), output_packets.len());
            for (source, result) in original_packets.iter().zip(output_packets) {
                close(
                    seconds(&result["pts_time"]),
                    seconds(&source["pts_time"]) / factor,
                );
                close(
                    seconds(&result["duration_time"]),
                    seconds(&source["duration_time"]) / factor,
                );
                assert_eq!(source["data_hash"], result["data_hash"]);
            }
        }
        let source_attachment = streams(&original, "attachment");
        let output_attachment = streams(&output, "attachment");
        assert_eq!(output_attachment.len(), 1);
        assert_eq!(
            source_attachment[0]["extradata_hash"],
            output_attachment[0]["extradata_hash"]
        );
        assert_eq!(source_attachment[0]["tags"], output_attachment[0]["tags"]);
        let audio = streams(&output, "audio")[0];
        assert_eq!(audio["codec_name"], "pcm_s16le");
        let audio_packets = packets(&output, audio);
        close(seconds(&audio_packets[0]["pts_time"]), 0.0);
        // A final PCM packet shorter than Matroska's millisecond time base has
        // no duration_time. Count samples instead of relying on rounded ticks.
        let audio_bytes: u64 = audio_packets
            .iter()
            .map(|packet| packet["size"].as_str().unwrap().parse::<u64>().unwrap())
            .sum();
        let sample_rate = seconds(&audio["sample_rate"]);
        let channels = audio["channels"].as_u64().unwrap();
        close(
            audio_bytes as f64 / (2.0 * channels as f64 * sample_rate),
            4.0 / factor,
        );
    }
}

#[test]
#[ignore = "requires FFmpeg and FFprobe in runtime/ffmpeg or RIFE_TEST_FFMPEG/RIFE_TEST_FFPROBE"]
fn chapter_retiming_normalizes_source_start_and_supports_mp4() {
    let directory = tempfile::tempdir().unwrap();
    let input = fixture(directory.path(), 10);
    let source = probe(&input);
    close(seconds(&source["format"]["start_time"]), 10.0);
    for extension in ["mkv", "mp4"] {
        let output = encode_fixture(
            &input,
            &directory.path().join(format!("retimed.{extension}")),
            media::Rate { num: 4, den: 1 },
            32,
            0.5,
            false,
        );
        check_chapters(&source, &output, 10.0, 0.5);
        if extension == "mkv" {
            let subtitle = streams(&output, "subtitle")[0];
            close(seconds(&packets(&output, subtitle)[0]["pts_time"]), 1.0);
        } else {
            assert!(streams(&output, "subtitle").is_empty());
        }
    }
}

#[test]
#[ignore = "requires FFmpeg and FFprobe in runtime/ffmpeg or RIFE_TEST_FFMPEG/RIFE_TEST_FFPROBE"]
fn retiming_accepts_inputs_without_subtitles_or_chapters() {
    let directory = tempfile::tempdir().unwrap();
    let source = fixture(directory.path(), 0);
    let input = directory.path().join("plain.mkv");
    run(Command::new(tool("ffmpeg"))
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:v",
            "-map",
            "0:a",
            "-map_metadata",
            "-1",
            "-map_chapters",
            "-1",
            "-c",
            "copy",
        ])
        .arg(&input));
    let result = encode_fixture(
        &input,
        &directory.path().join("retimed.mkv"),
        media::Rate { num: 4, den: 1 },
        32,
        0.5,
        false,
    );
    assert!(streams(&result, "subtitle").is_empty());
    assert!(streams(&result, "attachment").is_empty());
    assert!(result["chapters"].as_array().unwrap().is_empty());
}
