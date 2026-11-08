// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    Engine, Frame, FrameSchedule, PlaybackOptions, Result, SceneDetector, scene::histogram,
};
use anyhow::{Context, bail, ensure};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::Duration,
};

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);
impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    pub fn check(&self) -> Result<()> {
        ensure!(!self.is_cancelled(), "cancelled");
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct StreamOptions {
    pub threshold: f64,
    /// A schedule using the verified normalized input count. None uses the
    /// default playback step, currently one output per input frame.
    pub schedule: Option<FrameSchedule>,
    /// Maximum number of pairs in flight, bounding buffers and reordering.
    pub buffer_capacity: Option<usize>,
}
impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            threshold: 0.3,
            schedule: None,
            buffer_capacity: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamStats {
    pub input_frames: u64,
    pub output_frames: u64,
    pub scene_cuts: u64,
    pub inferred_frames: u64,
}

struct Task {
    index: u64,
    pair_index: u64,
    timestep: f32,
    first: Arc<Frame>,
    second: Arc<Frame>,
    cut: bool,
    buffer: Vec<u8>,
}
struct Pair {
    index: u64,
    first: Arc<Frame>,
    middle: Option<Frame>,
    buffer: Vec<u8>,
}
enum Event {
    Pair(Pair),
    End(StreamStats),
}

fn receive<T>(receiver: &Receiver<T>, cancel: &CancellationToken) -> Result<T> {
    loop {
        cancel.check()?;
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(value) => return Ok(value),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => bail!("streaming channel closed unexpectedly"),
        }
    }
}

fn scene_detector_for_threshold(threshold: f64) -> Result<Option<SceneDetector>> {
    if threshold == 1.0 {
        Ok(None)
    } else {
        SceneDetector::new(threshold).map(Some)
    }
}

fn dispatch(
    mut frames: impl Iterator<Item = Result<Frame>>,
    tasks: SyncSender<Task>,
    buffers: Receiver<Vec<u8>>,
    detector: Option<SceneDetector>,
    schedule: Option<FrameSchedule>,
    cancel: &CancellationToken,
) -> Result<StreamStats> {
    let default_step = PlaybackOptions::default().sampling_step()?;
    let mut previous = Arc::new(frames.next().context("input contains no video frames")??);
    let mut previous_histogram = detector
        .as_ref()
        .filter(|detector| detector.threshold() > 0.0)
        .map(|_| histogram(&previous));
    let mut count = 1u64;
    let mut cuts = 0u64;
    let mut output = 0u64;
    let mut inferred = 0u64;
    loop {
        cancel.check()?;
        let (next, last) = match frames.next() {
            Some(frame) => (Arc::new(frame?), false),
            None => (Arc::clone(&previous), true),
        };
        ensure!(
            previous.width() == next.width() && previous.height() == next.height(),
            "video frame dimensions changed during decoding"
        );
        let cut = if last {
            true
        } else if let Some(detector) = &detector {
            if detector.threshold() == 0.0 {
                true
            } else {
                let current = histogram(&next);
                let cut =
                    detector.histograms_differ(previous_histogram.as_ref().unwrap(), &current);
                previous_histogram = Some(current);
                cut
            }
        } else {
            false
        };
        if !last {
            cuts += u64::from(cut);
        }
        let pair_index = count - 1;
        loop {
            let (pair, timestep) = match schedule {
                Some(schedule) => {
                    if output == schedule.output_frames {
                        break;
                    }
                    schedule.sample(output)?
                }
                None => default_step.sample(output)?,
            };
            if pair > pair_index {
                break;
            }
            ensure!(pair == pair_index, "non-monotonic frame schedule");
            let held = cut || timestep == 0.0;
            inferred += u64::from(!held);
            let buffer = receive(&buffers, cancel)?;
            tasks
                .send(Task {
                    index: output,
                    pair_index,
                    timestep,
                    first: Arc::clone(&previous),
                    second: Arc::clone(&next),
                    cut: held,
                    buffer,
                })
                .map_err(|_| anyhow::anyhow!("inference workers stopped"))?;
            output = output.checked_add(1).context("too many output frames")?;
        }
        if last {
            break;
        }
        previous = next;
        count = count.checked_add(1).context("too many input frames")?;
    }
    if let Some(schedule) = schedule {
        ensure!(
            count == schedule.input_frames,
            "normalized input count changed: counted {}, decoded {count}",
            schedule.input_frames
        );
        ensure!(
            output == schedule.output_frames,
            "frame schedule ended before its target count"
        );
    }
    Ok(StreamStats {
        input_frames: count,
        output_frames: output,
        scene_cuts: cuts,
        inferred_frames: inferred,
    })
}

impl Engine {
    /// Decode, compare histograms and infer concurrently, emitting RGB frames in
    /// timeline order. Blocking frame readers/sinks must unblock on cancellation
    /// (the CLI accomplishes this by terminating its FFmpeg children).
    /// On error, the supplied token is cancelled to stop upstream/downstream I/O.
    pub fn interpolate_stream<I, W, P>(
        &self,
        frames: I,
        options: StreamOptions,
        cancel: &CancellationToken,
        emit: W,
        progress: P,
    ) -> Result<StreamStats>
    where
        I: IntoIterator<Item = Result<Frame>>,
        I::IntoIter: Send,
        W: FnMut(&Frame) -> Result<()>,
        P: FnMut(u64),
    {
        run_stream(
            self.workers.len(),
            self.queue_depth,
            frames.into_iter(),
            options,
            cancel,
            emit,
            progress,
            |worker, first, second, out, timestep| {
                self.workers[worker].process_into(first, second, out, timestep, cancel)
            },
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn run_stream<I, W, P, F>(
    worker_count: usize,
    queue_depth: usize,
    frames: I,
    options: StreamOptions,
    cancel: &CancellationToken,
    mut emit: W,
    mut progress: P,
    process: F,
) -> Result<StreamStats>
where
    I: Iterator<Item = Result<Frame>> + Send,
    W: FnMut(&Frame) -> Result<()>,
    P: FnMut(u64),
    F: Fn(usize, &Frame, &Frame, &mut Frame, f32) -> Result<()> + Sync,
{
    cancel.check()?;
    // Threshold 1 disables detection before a detector or any histograms are
    // created. Other values still pass through the regular range validation.
    let detector = scene_detector_for_threshold(options.threshold)?;
    let capacity = options
        .buffer_capacity
        .unwrap_or(queue_depth + worker_count * 2);
    ensure!(
        (1..=1024).contains(&capacity) && worker_count > 0,
        "invalid streaming buffer capacity or worker count"
    );
    let (buffer_tx, buffer_rx) = mpsc::sync_channel(capacity);
    for _ in 0..capacity {
        buffer_tx.send(Vec::new()).unwrap();
    }
    let (task_tx, task_rx) = mpsc::sync_channel::<Task>(capacity);
    let task_rx = Arc::new(Mutex::new(task_rx));
    let (event_tx, event_rx) = mpsc::sync_channel::<Result<Event>>(capacity + 1);
    thread::scope(|scope| {
        for worker in 0..worker_count {
            let tasks = Arc::clone(&task_rx);
            let events = event_tx.clone();
            let process = &process;
            scope.spawn(move || {
                loop {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let task = {
                        let Ok(receiver) = tasks.lock() else {
                            break;
                        };
                        match receiver.recv_timeout(Duration::from_millis(50)) {
                            Ok(task) => task,
                            Err(RecvTimeoutError::Timeout) => continue,
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    };
                    let result = (|| -> Result<Event> {
                        cancel.check()?;
                        let mut buffer = task.buffer;
                        let middle = if task.cut {
                            None
                        } else {
                            let mut frame = Frame::recycled(
                                task.first.width(),
                                task.first.height(),
                                std::mem::take(&mut buffer),
                            )?;
                            process(worker, &task.first, &task.second, &mut frame, task.timestep)
                                .with_context(|| {
                                format!(
                                    "input frame pair {} -> {}",
                                    task.pair_index,
                                    task.pair_index + 1
                                )
                            })?;
                            Some(frame)
                        };
                        Ok(Event::Pair(Pair {
                            index: task.index,
                            first: task.first,
                            middle,
                            buffer,
                        }))
                    })();
                    let failed = result.is_err();
                    if events.send(result).is_err() || failed {
                        break;
                    }
                }
            });
        }
        drop(task_rx);
        scope.spawn(move || {
            let result = dispatch(
                frames,
                task_tx,
                buffer_rx,
                detector,
                options.schedule,
                cancel,
            )
            .map(Event::End);
            let _ = event_tx.send(result);
        });
        let result = (|| -> Result<StreamStats> {
            let mut pending = BTreeMap::new();
            let mut next = 0u64;
            let mut completed = 0u64;
            let mut finished = None;
            loop {
                match receive(&event_rx, cancel)?? {
                    Event::Pair(pair) => {
                        pending.insert(pair.index, pair);
                    }
                    Event::End(stats) => finished = Some(stats),
                }
                while let Some(pair) = pending.remove(&next) {
                    cancel.check()?;
                    emit(pair.middle.as_ref().unwrap_or(&pair.first))?;
                    completed += 1;
                    progress(completed);
                    let buffer = pair.middle.map(Frame::into_pixels).unwrap_or(pair.buffer);
                    // After the final task the producer may have dropped its
                    // buffer receiver; no further permits are needed then.
                    let _ = buffer_tx.send(buffer);
                    next += 1;
                }
                if let Some(stats) = finished
                    && next == stats.output_frames
                {
                    return Ok(stats);
                }
            }
        })();
        if result.is_err() {
            cancel.cancel();
        }
        // Unblock producer/workers before scoped threads are joined.
        drop(buffer_tx);
        drop(event_rx);
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(color: [u8; 3]) -> Result<Frame> {
        Frame::new(2, 2, color.repeat(4))
    }
    fn double_rate_schedule(input_frames: u64) -> FrameSchedule {
        FrameSchedule::for_playback(
            input_frames,
            PlaybackOptions {
                fps_scale: 2.0,
                speed: 1.0,
            },
        )
        .unwrap()
    }
    #[test]
    fn preserves_order_holds_cuts_and_resumes_interpolation() {
        let input = [[240, 0, 0], [230, 0, 0], [0, 0, 240], [0, 0, 230]]
            .into_iter()
            .map(frame);
        let mut output = Vec::new();
        let mut progress = Vec::new();
        let stats = run_stream(
            2,
            2,
            input,
            StreamOptions {
                schedule: Some(double_rate_schedule(4)),
                ..StreamOptions::default()
            },
            &CancellationToken::default(),
            |frame| {
                output.push(frame.pixels()[..3].to_vec());
                Ok(())
            },
            |count| progress.push(count),
            |_, a, b, out, _| {
                if a.pixels()[0] != 0 {
                    thread::sleep(Duration::from_millis(10));
                }
                for (out, (&a, &b)) in out
                    .pixels_mut()
                    .iter_mut()
                    .zip(a.pixels().iter().zip(b.pixels()))
                {
                    *out = ((a as u16 + b as u16) / 2) as u8;
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            output,
            [
                [240, 0, 0],
                [235, 0, 0],
                [230, 0, 0],
                [230, 0, 0],
                [0, 0, 240],
                [0, 0, 235],
                [0, 0, 230],
                [0, 0, 230]
            ]
        );
        assert_eq!(
            stats,
            StreamStats {
                input_frames: 4,
                output_frames: 8,
                scene_cuts: 1,
                inferred_frames: 2
            }
        );
        assert_eq!(progress, (1..=8).collect::<Vec<_>>());
    }
    #[test]
    fn threshold_one_skips_scene_detector_creation() {
        assert!(scene_detector_for_threshold(1.0).unwrap().is_none());
        assert!(scene_detector_for_threshold(0.3).unwrap().is_some());
        assert!(scene_detector_for_threshold(f64::NAN).is_err());
    }
    #[test]
    fn endpoints_and_sink_errors_do_not_deadlock() {
        for threshold in [0.0, 1.0] {
            let input = (0..20).map(|_| frame([240, 0, 0]));
            let stats = run_stream(
                2,
                1,
                input,
                StreamOptions {
                    threshold,
                    schedule: Some(double_rate_schedule(20)),
                    buffer_capacity: Some(1),
                },
                &CancellationToken::default(),
                |_| Ok(()),
                |_| {},
                |_, _, _, _, _| Ok(()),
            )
            .unwrap();
            assert_eq!(stats.inferred_frames, if threshold == 0.0 { 0 } else { 19 });
        }
        let token = CancellationToken::default();
        let result = run_stream(
            2,
            1,
            (0..20).map(|_| frame([0, 0, 0])),
            StreamOptions::default(),
            &token,
            |_| bail!("sink failed"),
            |_| {},
            |_, _, _, _, _| Ok(()),
        );
        assert!(result.unwrap_err().to_string().contains("sink failed"));
        assert!(token.is_cancelled());
    }
    #[test]
    fn truncated_input_and_inference_failures_are_reported() {
        let token = CancellationToken::default();
        assert!(
            run_stream(
                1,
                1,
                std::iter::empty(),
                StreamOptions::default(),
                &token,
                |_| Ok(()),
                |_| {},
                |_, _, _, _, _| Ok(())
            )
            .is_err()
        );
        let token = CancellationToken::default();
        let error = run_stream(
            2,
            1,
            (0..5).map(|_| frame([0, 0, 0])),
            StreamOptions {
                threshold: 1.0,
                schedule: Some(double_rate_schedule(5)),
                buffer_capacity: Some(2),
            },
            &token,
            |_| Ok(()),
            |_| {},
            |_, _, _, _, _| bail!("inference failed"),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("inference failed"));
        assert!(error.to_string().starts_with("input frame pair"));
    }
    #[test]
    fn arbitrary_output_counts_reuse_pairs_and_hold_exact_scene_boundaries() {
        for (count, threshold) in [(1, 1.0), (3, 1.0), (5, 1.0), (16, 1.0), (16, 0.0)] {
            let schedule = FrameSchedule::for_count(4, count).unwrap();
            let mut output = Vec::new();
            let mut calls = Vec::new();
            let token = CancellationToken::default();
            let stats = run_stream(
                3,
                1,
                (0..4).map(|v| frame([v * 40, 0, 0])),
                StreamOptions {
                    threshold,
                    schedule: Some(schedule),
                    buffer_capacity: Some(2),
                },
                &token,
                |f| {
                    output.push(f.pixels()[0]);
                    Ok(())
                },
                |_| {},
                |_, a, b, out, t| {
                    for v in out.pixels_mut() {
                        *v = ((1.0 - t) * a.pixels()[0] as f32 + t * b.pixels()[0] as f32).round()
                            as u8;
                    }
                    Ok(())
                },
            )
            .unwrap();
            for index in 0..count {
                let (pair, t) = schedule.sample(index).unwrap();
                let expected = if threshold == 0.0 || pair == 3 {
                    pair as u8 * 40
                } else {
                    (pair as f32 * 40.0 + t * 40.0).round() as u8
                };
                assert_eq!(output[index as usize], expected);
                if t > 0.0 && threshold == 1.0 {
                    calls.push(t);
                }
            }
            assert_eq!(stats.output_frames, count);
            assert_eq!(stats.inferred_frames, calls.len() as u64);
            if count == 16 && threshold == 1.0 {
                assert!(calls.contains(&0.25) && calls.contains(&0.75));
            }
        }
        let mut output = Vec::new();
        run_stream(
            2,
            1,
            [[240, 0, 0], [230, 0, 0], [0, 0, 240], [0, 0, 230]]
                .into_iter()
                .map(frame),
            StreamOptions {
                schedule: Some(FrameSchedule::for_count(4, 16).unwrap()),
                ..StreamOptions::default()
            },
            &CancellationToken::default(),
            |f| {
                output.push(f.pixels()[..3].to_vec());
                Ok(())
            },
            |_| {},
            |_, a, _, out, _| {
                out.pixels_mut().copy_from_slice(a.pixels());
                Ok(())
            },
        )
        .unwrap();
        for value in &output[4..8] {
            assert_eq!(value, &[230, 0, 0]);
        }
        assert_eq!(output[8], [0, 0, 240]);
    }
    #[test]
    fn count_changes_and_late_decode_errors_never_succeed() {
        for count in [2, 4] {
            let token = CancellationToken::default();
            let result = run_stream(
                2,
                1,
                (0..3).map(|_| frame([0, 0, 0])),
                StreamOptions {
                    schedule: Some(FrameSchedule::for_count(count, 1).unwrap()),
                    ..StreamOptions::default()
                },
                &token,
                |_| Ok(()),
                |_| {},
                |_, _, _, _, _| Ok(()),
            );
            assert!(format!("{:#}", result.unwrap_err()).contains("count changed"));
            assert!(token.is_cancelled());
        }
        let frames = [
            frame([0, 0, 0]),
            frame([0, 0, 0]),
            Err(anyhow::anyhow!("late decode error")),
        ];
        let result = run_stream(
            1,
            1,
            frames.into_iter(),
            StreamOptions {
                schedule: Some(FrameSchedule::for_count(3, 1).unwrap()),
                ..StreamOptions::default()
            },
            &CancellationToken::default(),
            |_| Ok(()),
            |_| {},
            |_, _, _, _, _| Ok(()),
        );
        assert!(format!("{:#}", result.unwrap_err()).contains("late decode error"));
    }
}
