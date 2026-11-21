// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Safe, bounded streaming interpolation with a persistent NCNN/Vulkan model.
//!
//! Frames contain packed RGB8 pixels on every platform. Model execution retains
//! the reference RIFE shaders and precision; no external RIFE process is used.
//!
//! Library callers provide a directory containing `flownet.param` and
//! `flownet.bin`. Paths are used as supplied, relative to the caller's working
//! directory when not absolute. Model embedding and executable-relative lookup
//! belong to the CLI; this crate does not invoke FFmpeg or bundle model weights.
//!
//! Native inference runs in the calling process. The CLI provides its own
//! supervised worker process to contain native crashes. Cancellation is
//! cooperative at model-read and native-layer boundaries.
//!
//! ```no_run
//! use rife_core::{Engine, EngineOptions, Frame, Result};
//! fn midpoint(first: Frame, second: Frame) -> Result<Frame> {
//!     let model = Engine::new("models/rife-v4.25", EngineOptions::default())?;
//!     model.interpolate_pair(&first, &second, 0.5)
//! }
//! ```
mod cpu;
mod engine;
mod frame;
mod model;
mod scene;
mod schedule;
mod settings;
mod stream;

pub use engine::{Device, DeviceSelection, Engine, EngineOptions, InferenceError, Jobs, devices};
pub use frame::Frame;
pub use model::Model;
pub use scene::SceneDetector;
pub use schedule::{FrameSchedule, PlaybackOptions};
pub use stream::{CancellationToken, StreamOptions, StreamStats};

pub type Result<T> = anyhow::Result<T>;
