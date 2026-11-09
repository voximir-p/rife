# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
