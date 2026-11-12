# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Stream encoded media to stdout with `-` as the positional output, using NUT by default or a container selected through `-e "-f FORMAT"`.
- Export interpolated frames as numbered image sequences with printf-style output patterns, preserving RGB pixels in lossless image formats.

### Changed

- Remove `--video-codec` and `--audio-codec`; select encoders with `-e "-c:v ENCODER"` and `-e "-c:a ENCODER"` instead.
- Replace `-c`/`--crf` with repeatable `-e`/`--extra-encoder-option` for FFmpeg output settings. Migrate CRF overrides to `-e -crf=23`.
- Show Source and Target before counting frames, followed by model loading, Device, and processing in verbose output.
- End FFmpeg and FFprobe errors with `error: media processing failed` and keep cleanup errors from obscuring the original diagnostics.

### Fixed

- Send original and interpolated frames to stdout as they become ready, with low-delay encoding instead of waiting for buffered output to finish.
- Reject existing image sequences and invalid output settings before counting frames or loading the model.
- Report native backend crashes with formatted error details, clean up temporary files and output locks, and preserve existing outputs.

## [0.1.2] - 2026-11-09

### Changed

- Show live frame counting and model loading, followed by Counted and Loaded summaries.
- Separate Processing and Saving progress, retaining elapsed-time summaries for both stages and removing throughput multipliers.
- Use Cargo's bright green and cyan consistently in help and status output, and show Device above Source and Target details.
- Reduce overhead and memory use when inspecting videos before processing.

### Fixed

- Preserve GIF palette quality when `--video-codec gif` is selected explicitly.
- Clear unfinished status rows on Ctrl+C while preserving completed messages and the terminal's default cancellation indicator.
- Reject infinite CRF values before processing and ignore them in source metadata.

## [0.1.1] - 2026-11-07

### Changed

- Linux x64 is now tested alongside Windows x64.
- Store temporary processing files in the OS temporary directory, falling back to the output directory when unavailable. Outputs can be saved to another drive.
- Show completion times of at least one minute as minutes and rounded seconds, such as `1m 30s`.
- Source downloads now include the dependencies needed for offline builds.

### Fixed

- Keep chapters and MKV subtitle cues in sync with audio when playback speed changes, preserving their titles, styles, and metadata.

## [0.1.0] - 2026-11-04

### Added

- Command-line video interpolation with RIFE v4.
- Frame-rate and playback-speed changes with pitch-preserving audio, scene-cut detection, CPU/GPU selection, model selection, and video quality controls.

### Notes

- The initial release targets Windows x64.
- FFmpeg, FFprobe, and model files are downloaded and installed by the user; they are not included in the release archive.

[unreleased]: https://github.com/voximir-p/rife/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/voximir-p/rife/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/voximir-p/rife/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/voximir-p/rife/releases/tag/v0.1.0
