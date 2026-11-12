// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! CLI identity comes from Cargo; runtime layout and shared process settings live here.
use clap::builder::styling::{AnsiColor, Style};
use std::time::Duration;

pub(crate) const SUCCESS_STYLE: Style = AnsiColor::BrightGreen.on_default().bold();
pub(crate) const ACTIVE_STYLE: Style = AnsiColor::BrightCyan.on_default().bold();

pub(crate) const APP_NAME: &str = env!("CARGO_BIN_NAME");
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) fn temp_prefix() -> String {
    format!(".{APP_NAME}-")
}
pub(crate) const MODELS_DIRECTORY: &str = "models";
pub(crate) const PIXEL_FORMAT: &str = "rgb24";
pub(crate) const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(50);
pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_millis(150);
pub(crate) const DIAGNOSTIC_LINES: usize = 20;
pub(crate) const DIAGNOSTIC_LINE_CHARS: usize = 1000;
