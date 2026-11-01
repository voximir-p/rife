// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Safe, bounded streaming interpolation with a persistent NCNN/Vulkan model.
//!
//! Frames contain packed RGB8 pixels on every platform. Model execution retains
//! the reference RIFE shaders and precision; no external RIFE process is used.
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
