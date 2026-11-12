// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Application lifetime: cancellation, presentation and final exit handling.
use crate::{cli::Args, pipeline, ui};
use rife_core::CancellationToken;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) fn run(args: Args) {
    let cancel = CancellationToken::default();
    let interrupted = install_cancellation(&cancel);
    let ui = ui::Ui::new(args.verbose, Arc::clone(&interrupted));
    let result = pipeline::run(args, cancel, &ui);
    finish(result, interrupted.load(Ordering::Relaxed));
}

fn install_cancellation(cancel: &CancellationToken) -> Arc<AtomicBool> {
    let interrupted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&interrupted);
    let signal = cancel.clone();
    if let Err(error) = ctrlc::set_handler(move || {
        flag.store(true, Ordering::Relaxed);
        signal.cancel();
    }) {
        ui::warning(format_args!("unable to register Ctrl+C handler: {error}"));
    }
    interrupted
}

fn finish(result: anyhow::Result<()>, interrupted: bool) {
    if interrupted {
        std::process::exit(130);
    }
    if let Err(error) = result {
        ui::report_error(&error);
        std::process::exit(1);
    }
}
