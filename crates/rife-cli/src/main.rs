// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

mod app;
mod chapters;
mod cli;
mod config;
mod decode;
mod diagnostics;
mod encode;
#[cfg(test)]
mod encode_timing_tests;
mod encoder_options;
mod images;
mod media;
mod output_lock;
mod paths;
mod pipeline;
mod process;
mod ui;
mod worker;

fn main() {
    if let Some(code) = worker::entry() {
        std::process::exit(code);
    }
    if let Some(args) = cli::parse() {
        app::run(args);
    }
}
