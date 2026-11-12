// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Parent-owned processing workflow. Native work uses direct OS frame pipes.
mod job;
mod output;
mod preflight;

use crate::{
    cli::Args,
    decode,
    process::{ManagedChild, attach_cleanup_failure},
    ui::Ui,
    worker,
};
use anyhow::Result;
use job::{PlannedJob, PreparedJob};
use rife_core::CancellationToken;
use std::process::Stdio;

pub(crate) fn run(args: Args, cancel: CancellationToken, ui: &Ui) -> Result<()> {
    let job = PreparedJob::prepare(args, &cancel, ui)?;
    job.report(ui);
    let count = decode::count_frames(&job.ffmpeg, &job.input, &job.info, &cancel, ui)?;
    execute(job.plan(count, &cancel)?, &cancel, ui)
}

fn execute(job: PlannedJob, cancel: &CancellationToken, ui: &Ui) -> Result<()> {
    // Children die before job (and therefore staging and its output lock).
    let PlannedJob {
        prepared: job,
        schedule,
        mut encoder,
    } = job;
    let mut decoder = ManagedChild::spawn(
        decode::command(&job.ffmpeg, &job.input, job.info.rate),
        "FFmpeg decoding",
        cancel.clone(),
    )?;
    let mut worker = load_model(&job, schedule.input_frames(), &mut decoder, cancel, ui)?;
    cancel.check()?;
    encoder
        .stdin(Stdio::from(worker.take_stdout()))
        .stdout(if job.destination.is_stdout() {
            Stdio::inherit()
        } else {
            Stdio::null()
        });
    let mut progress = ui.progress(Some(schedule.output_frames()));
    let mut encoder =
        ManagedChild::encoder(encoder, cancel.clone(), Some(progress.encoded_frames()))?;
    let result = worker
        .process(|done| progress.advance(done))
        .and_then(|frames| {
            cancel.check()?;
            progress.saving(frames);
            decoder.finish()?;
            encoder.finish()?;
            cancel.check()?;
            job.destination.commit(
                &job.staged_output,
                &job.input,
                schedule.output_frames(),
                job.args.overwrite,
                cancel,
            )
        });
    match result {
        Ok(()) => {
            progress.finish();
            Ok(())
        }
        Err(error) => {
            progress.abandon();
            cancel.cancel();
            worker.request_cancel();
            Err(cleanup(error, [&mut decoder, &mut encoder]))
        }
    }
}

fn load_model(
    job: &PreparedJob,
    frames: u64,
    decoder: &mut ManagedChild,
    cancel: &CancellationToken,
    ui: &Ui,
) -> Result<worker::NativeWorker> {
    let mut loading = ui.loading(&format!("model `{}`", job.args.model));
    let config = worker::Config::new(&job.args, (job.info.width, job.info.height), frames);
    let mut worker = worker::NativeWorker::spawn(
        &config,
        &job.model,
        job.staging.path(),
        decoder.take_stdout(),
        cancel.clone(),
    )?;
    let devices = worker.loaded()?;
    cancel.check()?;
    loading.finish();
    for name in devices {
        ui.info("Device", name);
    }
    Ok(worker)
}

fn cleanup(mut error: anyhow::Error, children: [&mut ManagedChild; 2]) -> anyhow::Error {
    // Backend diagnostics take priority over secondary closed-pipe failures.
    for child in children {
        if !child.is_finished()
            && let Err(diagnostic) = child.finish()
        {
            error = attach_cleanup_failure(error, diagnostic);
        }
    }
    error
}
