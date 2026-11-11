#!/bin/sh
# Copyright (C) 2026 Voximir.
# SPDX-License-Identifier: GPL-3.0-only
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
if command -v python3 >/dev/null 2>&1 && python3 --version >/dev/null 2>&1; then
    python=python3
else
    python=python
fi
exec "$python" - "$root" "$@" <<'PYTHON'
"""Exercise the runtime layout, interpolation, encoding and error handling."""
import argparse
from array import array
from fractions import Fraction
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

root = Path(sys.argv.pop(1)).resolve()
parser = argparse.ArgumentParser(prog="smoke.sh", description=__doc__)
parser.add_argument("runtime", nargs="?", type=Path, default=root / "runtime")
args = parser.parse_args()
runtime = args.runtime.resolve()
suffix = ".exe" if os.name == "nt" else ""
metadata = json.loads(subprocess.check_output([
    "cargo", "metadata", "--manifest-path", str(root / "Cargo.toml"),
    "--no-deps", "--format-version", "1", "--offline",
], text=True))
package, = [p for p in metadata["packages"] if p["id"] in metadata["workspace_default_members"]]
binary, = [t for t in package["targets"] if "bin" in t["kind"]]
app = binary["name"]
executable = runtime / (app + suffix)
ffmpeg = runtime / "ffmpeg" / ("ffmpeg" + suffix)
ffprobe = runtime / "ffmpeg" / ("ffprobe" + suffix)


def run(*command, succeeds=True):
    result = subprocess.run([str(arg) for arg in command], stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True)
    if succeeds:
        assert result.returncode == 0, f"{command!r}\n{result.stdout}"
    else:
        assert result.returncode != 0, f"Unexpected success: {command!r}"
    return result.stdout


def rife(*arguments, succeeds=True):
    return run(executable, "--gpu=" + os.environ.get("RIFE_SMOKE_GPU", "auto"),
               *arguments, succeeds=succeeds)


def hashes(path):
    return [line.rsplit(",", 1)[-1].strip() for line in run(
        ffmpeg, "-v", "error", "-i", path, "-map", "0:v:0", "-pix_fmt", "rgb24",
        "-f", "framemd5", "-").splitlines() if line and not line.startswith("#")]


def probe(path, *options):
    return json.loads(run(ffprobe, "-v", "error", *options, "-of", "json", path))


def frames(output, *options):
    rife("-i", source, "-m", "rife-v4.26", *options, "-e", "-c:v ffv1", "-e", "-c:a pcm_s16le", "-y", output)
    return hashes(output)


def check_cleanup():
    assert not list(test_root.rglob(f".{app}-*")), "Output staging was not cleaned"
    assert not list(test_root.rglob("*.lock")), "Output locks were not cleaned"
    assert not list(os_temp.glob(f".{app}-*")), "OS temporary staging was not cleaned"


def audio_check(path, expected_duration, start, end, tolerance):
    pcm = path.with_suffix(".pcm")
    run(ffmpeg, "-v", "error", "-i", path, "-map", "0:a:0", "-ac", "1", "-ar", "48000",
        "-c:a", "pcm_s16le", "-f", "s16le", "-y", pcm)
    samples = array("h", pcm.read_bytes())
    if sys.byteorder != "little":
        samples.byteswap()
    duration = len(samples) / 48000
    assert abs(duration - expected_duration) <= .01, f"Audio duration: {duration}"
    first, last = round(start * 48000), round(end * 48000)
    crossings = sum(samples[i - 1] < 0 <= samples[i] for i in range(first, last))
    frequency = crossings / ((last - first) / 48000)
    assert abs(frequency - 440) <= tolerance, f"Audio pitch: {frequency} Hz"


assert run(executable, "--version").strip() == f"{app} {package['version']}"
(root / "target").mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix="rife-smoke-", dir=root / "target") as directory:
    test_root = Path(directory)
    os_temp = test_root / "os-%temp"
    os_temp.mkdir()
    # Scope TMPDIR/TMP/TEMP to these test subprocesses, including Windows GetTempPath.
    os.environ.update({name: str(os_temp) for name in ["TMPDIR", "TMP", "TEMP"]})
    raw = test_root / "source.rgb"
    pixels = bytearray()
    for frame in range(6):
        rgb = bytearray(64 * 64 * 3)
        for y in range(64):
            for x in range(64):
                offset = (y * 64 + x) * 3
                rgb[offset + (0 if frame < 3 else 2)] = 240
                box = 5 + (frame % 3) * 16
                if 20 <= y < 36 and box <= x < box + 12:
                    rgb[offset:offset + 3] = bytes([245] * 3)
        pixels.extend(rgb)
    raw.write_bytes(pixels)
    source = test_root / "source.nut"
    run(ffmpeg, "-v", "error", "-f", "rawvideo", "-pixel_format", "rgb24", "-video_size", "64x64",
        "-framerate", "30000/1001", "-i", raw, "-f", "lavfi", "-i",
        "sine=frequency=440:sample_rate=48000:duration=0.2002", "-map", "0:v", "-map", "1:a",
        "-c:v", "ffv1", "-level", "3", "-c:a", "pcm_s16le", "-shortest", "-y", source)
    original = hashes(source)
    custom = test_root / "custom-options.mp4"
    title = "hello world; $(echo untouched) $HOME"
    log = rife("-i", source, "-m", "rife-v4.26", "-f", "2", "-s", "0.5",
               "-e", "-c:a copy", "-e", "-crf=23", "--extra-encoder-option",
               f"-crf 12 -c:a aac -b:a 10k -metadata 'title={title}'", custom)
    assert b"crf=12.0" in custom.read_bytes(), "Repeated CRF options did not override defaults"
    custom_info = probe(custom, "-show_streams", "-show_format")
    assert custom_info["format"]["tags"]["title"] == title, "Quoted metadata was expanded or split"
    assert any(stream["codec_name"] == "aac" for stream in custom_info["streams"])
    assert next(stream for stream in custom_info["streams"] if stream["codec_type"] == "video")["codec_name"] == "h264"
    single_crf = test_root / "single-crf.mp4"
    rife("-i", source, "-m", "rife-v4.26", "-e", "-crf=23", "-e", "-c:a aac", single_crf)
    assert b"crf=23.0" in single_crf.read_bytes(), "Inline CRF option was not passed to FFmpeg"
    raw_images = test_root / "uncompressed-png"
    raw_images.mkdir()
    rife("-i", source, "-m", "rife-v4.26", "-e", "-compression_level=0", raw_images / "%03d.png")
    assert hashes(raw_images / "%03d.png") == original, "Image encoder options changed RGB pixels"
    for option in ["-rife_missing_option 1", "-crf NaN", "-a:b 10k"]:
        failed_option = test_root / "bad-option.mp4"
        log = rife("-i", source, "-m", "rife-v4.26", "-v", "-e", option, failed_option, succeeds=False)
        assert log.startswith("error:") and log.endswith("\n\nerror: media processing failed\n"), log
        assert not re.search(r"^ *(?:Source|Target|Counted|Loaded|Processed|Saved)\b", log, re.M), log
        assert not failed_option.exists()
    for removed_flag in ["-c", "--crf", "--video-codec", "--audio-codec"]:
        log = rife("-i", source, removed_flag, "23", test_root / "removed-option.mp4", succeeds=False)
        assert "unexpected argument" in log and not (test_root / "removed-option.mp4").exists(), log
    check_cleanup()
    print("Passed: extra encoder options, repeated/inline CRF, literal quoted metadata, audio overrides, image settings and early errors.", flush=True)
    for threshold in ["0.3", "0", "1"]:
        output = test_root / f"threshold-{threshold}.nut"
        log = rife("-i", source, "-m", "rife-v4.26", "-f", "2", "-t", threshold,
                   "-e", "-c:v ffv1", "-y", output)
        lines = log.splitlines()
        assert len(lines) == 3 and re.fullmatch(r" +Counted 6 frames in total", lines[0]), log
        assert re.fullmatch(r"   Processed 12 frames in (?:[0-9]+\.[0-9]{2}s|[0-9]+m [0-9]+s)", lines[1]), log
        assert re.fullmatch(r" +Saved 12 frames in (?:[0-9]+\.[0-9]{2}s|[0-9]+m [0-9]+s)", lines[2]), log
        assert not re.search(r"Done:|~|\x1b|\| [0-9]+\.[0-9]+x", log), log
        result = hashes(output)
        assert len(result) == 12
        for frame in range(6):
            assert result[frame * 2] == original[frame], "Original frame changed"
            if threshold == "0":
                assert result[frame * 2 + 1] == original[frame]
        if threshold == "0.3":
            assert result[5] == original[2], "Cut was interpolated"
            assert result[7] not in original[3:5], "Interpolation did not resume"
        if threshold == "1":
            assert result[5] != original[2], "Threshold one detected a cut"
        info = probe(output, "-show_entries", "stream=codec_type,avg_frame_rate")
        assert next(s for s in info["streams"] if s["codec_type"] == "video")["avg_frame_rate"] == "60000/1001"
        assert any(s["codec_type"] == "audio" for s in info["streams"])
    log = rife("-i", source, "-m", "rife-v4.26", "-f", "2", "-v", "-e", "-c:v ffv1", "-y", test_root / "verbose.nut")
    labels = ["Source", "Target", "Counted", "Loaded", "Device", "Processed", "Saved"]
    for label in labels:
        assert re.search(rf"^ *{label} +", log, re.M), log
    assert re.fullmatch(r"Source Target Counted Loaded (?:Device )+Processed Saved",
                        " ".join(line.split()[0] for line in log.splitlines())), log
    assert not re.search(r"^ *(?:Counting|Loading|Processing|Saving)\b", log, re.M), log
    assert log.index("Counted 6 frames in total") < log.index("Loaded model")
    assert re.search(r"^ +Loaded model `rife-v4.26`$", log, re.M), log
    assert re.search(r"^ +Source 64x64 @ 29\.97 fps\n +Target 64x64 @ 59\.94 fps$", log, re.M), log
    assert not re.search(r"^ *Encoding|finalizing", log, re.M), log
    assert not re.search(r"Done:|^ *Video |^ *Model |Scenes|…|\.\.\.|Extracting frames:|Encoding video:|\x1b|\n\n", log, re.M), log
    expected_images = hashes(test_root / "threshold-0.3.nut")
    for extension in ["png", "PNG", "bmp", "tiff", "ppm"]:
        directory = test_root / f"frames-{extension}"
        directory.mkdir()
        pattern = directory / f"frame_%08d.{extension}"
        rife("-i", source, "-m", "rife-v4.26", "-f", "2",
             "-e", "-c:a rife_missing_encoder", pattern)
        assert sorted(path.name for path in directory.iterdir()) == [
            f"frame_{number:08d}.{extension}" for number in range(1, 13)], pattern
        assert hashes(pattern) == expected_images, f"Lossless {extension} changed RGB pixels"
        before = {path.name: path.read_bytes() for path in directory.iterdir()}
        log = rife("-i", source, "-m", "rife-v4.26", "-f", "2", pattern, succeeds=False)
        assert log == "error: output already exists (pass --overwrite to replace it)\n", log
        log = rife("-i", source, "-m", "rife-v4.26", "-f", "2", "-y",
                   "-e", "-c:v rife_missing_encoder", pattern, succeeds=False)
        assert "rife_missing_encoder" in log and log.endswith("\n\nerror: media processing failed\n"), log
        assert not re.search(r"^ *(?:Source|Target|Counted|Loaded|Processed|Saved)\b", log, re.M), log
        assert {path.name: path.read_bytes() for path in directory.iterdir()} == before
        # A successful replacement keeps the same numbering and raw RGB values.
        rife("-i", source, "-m", "rife-v4.26", "-f", "2", "-y", pattern)
        assert hashes(pattern) == expected_images
        check_cleanup()
    print("Passed: lossless image sequences, RGB equality, numbering, overwrite protection and encoder-failure cleanup.", flush=True)
    escaped = test_root / "escaped-frames"
    escaped.mkdir()
    pattern = escaped / "视%%频_%d.png"
    rife("-i", source, "-m", "rife-v4.26", "-s", "0.5", "-e", "-c:a copy", pattern)
    assert set(path.name for path in escaped.iterdir()) == {f"视%频_{number}.png" for number in range(1, 13)}
    assert hashes(pattern) == expected_images, "Speed-adjusted image schedule changed"
    odd_source = test_root / "odd.nut"
    run(ffmpeg, "-v", "error", "-f", "lavfi", "-i", "testsrc=size=65x67:rate=4:duration=1", "-c:v", "ffv1", odd_source)
    odd_video = test_root / "odd-output.nut"
    odd_images = test_root / "odd-frames"
    odd_images.mkdir()
    for output, codec in [(odd_video, ["-e", "-c:v ffv1"]), (odd_images / "%02d.png", [])]:
        rife("-i", odd_source, "-m", "rife-v4.26", "-f", "1.5", "-s", "0.75", *codec, output)
    assert len(list(odd_images.iterdir())) == 8
    assert hashes(odd_images / "%02d.png") == hashes(odd_video), "Odd-sized RGB frames changed"
    image_info = probe(odd_images / "01.png", "-show_entries", "stream=width,height")["streams"][0]
    assert (image_info["width"], image_info["height"]) == (65, 67), "Images were padded"
    jpeg = test_root / "jpeg-frames"
    jpeg.mkdir()
    rife("-i", source, "-m", "rife-v4.26", jpeg / "%03d.jpg")
    assert len(list(jpeg.iterdir())) == 6
    corrupt = test_root / "corrupt.mp4"
    corrupt.write_bytes(b"not a video")
    sparse = test_root / "sparse-frames"
    sparse.mkdir()
    (sparse / "frame_00000350.png").write_bytes(b"existing frame")
    log = rife("-i", corrupt, "-f", "20", "-v", sparse / "frame_%08d.png", succeeds=False)
    assert log == "error: output already exists (pass --overwrite to replace it)\n", log
    assert (sparse / "frame_00000350.png").read_bytes() == b"existing frame"
    for options, message in [
        (["-s", "0.5", "-e", "-c:a copy"], "audio codec copy cannot be used"),
        (["-j", "1:2,3:2"], "process worker counts must match"),
        (["-s", "1e-99"], "requested output frame count is too large"),
    ]:
        log = rife("-i", corrupt, "-v", *options, test_root / "invalid-early.mp4", succeeds=False)
        assert log.startswith("error:") and message in log and len(log.splitlines()) == 1, log
    for options, extension in [
        (["-e", "-c:v rife_missing_encoder"], "mp4"),
        (["-e", "-c:a rife_missing_encoder"], "mp4"),
        (["-e", "-c:v aac"], "mp4"),
        ([], "rife_missing_format"),
    ]:
        log = rife("-i", source, "-m", "rife-v4.26", "-v", *options,
                   test_root / f"invalid-early.{extension}", succeeds=False)
        assert log.startswith("error:") and log.endswith("\n\nerror: media processing failed\n"), log
        assert not re.search(r"^ *(?:Source|Target|Counted|Loaded|Processed|Saved)\b", log, re.M), log
    log = rife("-i", corrupt, test_root / "never.mp4", succeeds=False)
    assert "FFprobe failed" in log and log.endswith("\n\nerror: media processing failed\n"), log
    check_cleanup()
    print("Passed: escaped/unpadded patterns, odd dimensions, speed-adjusted image schedules, JPEG defaults and probe errors.", flush=True)
    default_gif = test_root / "default.gif"
    explicit_gif = test_root / "explicit.gif"
    for output, codec in [(default_gif, []), (explicit_gif, ["-e", "-c:v gif"])]:
        rife("-i", source, "-m", "rife-v4.26", "-t", "0", "-f", "1", "-s", "0.25",
             *codec, "-y", output)
    assert default_gif.read_bytes() == explicit_gif.read_bytes(), "Explicit GIF encoder changed palette quality"
    failed = test_root / "encoder-failure.mp4"
    log = rife("-i", source, "-m", "rife-v4.26", "-e", "-c:v rife_missing_encoder", "-y", failed, succeeds=False)
    assert re.search(r"^error:", log, re.M) and "rife_missing_encoder" in log and not failed.exists(), log
    assert log.endswith("\n\nerror: media processing failed\n"), log
    assert "Broken pipe" not in log and "FFmpeg decoding failed" not in log, log
    assert not re.search(r"^ *(?:Source|Target|Counted|Loaded|Processed|Saved)\b", log, re.M), log
    check_cleanup()
    for flag in ["-h", "--help"]:
        help_text = run(executable, flag)
        assert "Examples:" not in help_text and f"Usage: {app}" in help_text
    print("Passed: scene cuts, original pixels, rational FPS, audio, logging, help, and encoder cleanup.", flush=True)

    for scale, count, rate in [("4", 24, "120000/1001"), ("0.5", 3, "15000/1001"), ("1", 6, "30000/1001"), ("1.5", 9, "45000/1001")]:
        output = test_root / f"scale-{scale}.nut"
        assert len(frames(output, "-f", scale)) == count
        info = probe(output, "-select_streams", "v:0", "-show_packets", "-show_entries", "stream=time_base:packet=pts,duration")
        time_base = Fraction(info["streams"][0]["time_base"])
        assert all(int(p["duration"]) * time_base == 1 / Fraction(rate) for p in info["packets"])
    slow = test_root / "slow.nut"
    result = frames(slow, "-f", "2", "-s", "0.5")
    assert len(result) == 24 and all(result[i] == original[2] for i in [8, 9, 10, 11])
    assert probe(slow, "-select_streams", "v:0", "-show_entries", "stream=avg_frame_rate")["streams"][0]["avg_frame_rate"] == "60000/1001"
    for invalid in [["-f", "0"], ["-s", "0"], ["--slow", "2"], ["--num-frame", "24"], ["--time-step", "0.5"], ["--tta"], ["--temporal-tta"], ["--uhd"], ["-s", "0.5", "-e", "-c:a copy"]]:
        rife("-i", source, *invalid, "-y", test_root / "invalid.nut", succeeds=False)
    rife("-i", source, "-m", "rife-v4.26-large", "-f", "4", "-e", "-c:v ffv1", "-y", test_root / "large.nut")
    print("Passed: arbitrary counts, slow motion, cut timestamps, invalid arguments, and both models.", flush=True)

    audio_source = test_root / "audio-source.nut"
    run(ffmpeg, "-v", "error", "-f", "rawvideo", "-pixel_format", "rgb24", "-video_size", "64x64", "-framerate", "6", "-i", raw,
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1", "-map", "0:v", "-map", "1:a",
        "-c:v", "ffv1", "-c:a", "pcm_s16le", "-shortest", "-y", audio_source)
    for speed, duration, start, end, tolerance in [("0.5", 2, .2, 1.7, 3), ("2", .5, .1, .4, 5)]:
        output = test_root / f"pitch-{speed}.nut"
        rife("-i", audio_source, "-m", "rife-v4.26", "-f", "1", "-s", speed, "-e", "-c:v ffv1", "-e", "-c:a pcm_s16le", "-y", output)
        audio_check(output, duration, start, end, tolerance)
    for scale, speed, count in [("2", "0.25", 48), ("2", "0.125", 96), ("1", "0.5", 12), ("1", "1", 6), ("1", "2", 3)]:
        result = frames(test_root / f"scale-{scale}-speed-{speed}.nut", "-f", scale, "-s", speed)
        assert len(result) == count
        if scale == "1" and speed in ["1", "2"]:
            assert all(value == original[i * int(speed)] for i, value in enumerate(result))
    mp4 = test_root / "count-24.mp4"
    rife("-i", source, "-m", "rife-v4.26", "-f", "4", "-e", "-c:a aac", "-y", mp4)
    info = probe(mp4, "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=avg_frame_rate,nb_read_frames")["streams"][0]
    assert info["avg_frame_rate"] == "120000/1001" and info["nb_read_frames"] == "24"
    vfr = test_root / "vfr.nut"
    run(ffmpeg, "-v", "error", "-i", source, "-map", "0:v:0", "-vf", "setpts=PTS+if(gte(N\\,3)\\,2/(30000/1001*TB)\\,0)",
        "-fps_mode", "passthrough", "-enc_time_base:v", "1001:30000", "-c:v", "ffv1", "-y", vfr)
    output = test_root / "vfr-out.mp4"
    rife("-i", vfr, "-m", "rife-v4.26", "-f", "4", "-y", output)
    info = probe(output, "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=avg_frame_rate,nb_read_frames")["streams"][0]
    assert info["avg_frame_rate"] == "120000/1001" and info["nb_read_frames"] == "32"
    log = rife("-i", source, "-m", "rife-v4.28-large", "-y", test_root / "missing.nut", succeeds=False)
    assert re.search(r"models[\\/]rife-v4\.28-large", log) and "Unknown model" not in log, log
    check_cleanup()
    print("Passed: audio pitch/duration, independent FPS/speed, integer sampling, MP4 clocks, VFR counting, model lookup, and final cleanup.", flush=True)
PYTHON
