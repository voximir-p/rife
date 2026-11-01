# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-04

### Added

- The reusable `rife-core` library and `rife` CLI for streaming RIFE v4 interpolation through a statically linked NCNN/Vulkan backend.
- Frame-rate scaling, playback-speed changes with pitch-preserving audio, scene-cut detection, CPU/GPU selection, dynamic model-directory selection, and CRF-aware video encoding.

### Notes

- Windows x64 is the only compiled and runtime-tested release target.
- FFmpeg, FFprobe, and model files are downloaded and installed by the user; they are not included in the release archive.

[unreleased]: https://github.com/voximir-p/rife/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/voximir-p/rife/releases/tag/v0.1.0
