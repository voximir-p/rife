// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use console::{Term, measure_text_width, style, truncate_str};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};
use std::{
    fmt, io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

const LABEL_WIDTH: usize = 12;
const REFRESH_INTERVAL: Duration = Duration::from_millis(150);
const PROGRESS_LABEL: &str = "Processing";
const MAX_BAR_WIDTH: usize = 32;
const MIN_BAR_WIDTH: usize = 8;

#[derive(Debug, Default)]
struct OutputState {
    interrupted: Arc<AtomicBool>,
    ended: AtomicBool,
}

impl OutputState {
    fn write_line(&self, term: &dyn indicatif::TermLike, line: &str) {
        let interrupted = self.interrupted.load(Ordering::Relaxed);
        if term.write_line(line.trim_end()).is_ok() && interrupted {
            self.ended.store(true, Ordering::Relaxed);
        }
        let _ = term.flush();
    }

    fn finish_interrupted(&self, term: &dyn indicatif::TermLike, interactive: bool) {
        if interactive
            && self.interrupted.load(Ordering::Relaxed)
            && !self.ended.load(Ordering::Relaxed)
        {
            self.write_line(term, "");
        }
    }
}

// Indicatif emits a separate space-only write to pad every redraw to the right
// edge. Suppress it so Ctrl+C echo cannot wrap the row before we clear it.
#[derive(Debug)]
struct UnpaddedTerm(Arc<dyn indicatif::TermLike>);

impl indicatif::TermLike for UnpaddedTerm {
    fn width(&self) -> u16 {
        self.0.width()
    }
    fn height(&self) -> u16 {
        self.0.height()
    }
    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_up(n)
    }
    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_down(n)
    }
    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_right(n)
    }
    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_left(n)
    }
    fn write_line(&self, line: &str) -> io::Result<()> {
        self.0.write_line(line)
    }
    fn write_str(&self, text: &str) -> io::Result<()> {
        if text.bytes().all(|byte| byte == b' ') {
            Ok(())
        } else {
            self.0.write_str(text)
        }
    }
    fn clear_line(&self) -> io::Result<()> {
        self.0.clear_line()
    }
    fn flush(&self) -> io::Result<()> {
        self.0.flush()
    }
}

fn draw_target(term: &Term) -> ProgressDrawTarget {
    if term.is_term() && !console::is_dumb() {
        ProgressDrawTarget::term_like_with_hz(Box::new(UnpaddedTerm(Arc::new(term.clone()))), 10)
    } else {
        ProgressDrawTarget::hidden()
    }
}

#[derive(Clone, Copy)]
enum Stage {
    Processing,
    Saving,
}

fn padded(value: impl fmt::Display) -> String {
    format!("{:>LABEL_WIDTH$}", value.to_string())
}

#[derive(Clone, Copy)]
struct Palette {
    enabled: bool,
}

impl Palette {
    fn stderr() -> Self {
        Self {
            enabled: Term::stderr().is_term()
                && std::env::var_os("NO_COLOR").is_none()
                && console::colors_enabled_stderr(),
        }
    }

    fn label(self, value: impl fmt::Display) -> String {
        style(padded(value))
            .green()
            .bold()
            .force_styling(self.enabled)
            .to_string()
    }

    fn value(self, value: impl fmt::Display) -> String {
        style(value).cyan().force_styling(self.enabled).to_string()
    }
}

pub(crate) fn warning(message: impl fmt::Display) {
    let prefix = style(padded("warning:"))
        .yellow()
        .bold()
        .force_styling(Palette::stderr().enabled);
    eprintln!("{prefix} {message}");
}

pub(crate) fn error(message: impl fmt::Display) {
    let prefix = style("error:")
        .red()
        .bold()
        .force_styling(Palette::stderr().enabled);
    eprintln!("{prefix} {message}");
}

pub(crate) struct Ui {
    verbose: bool,
    palette: Palette,
    term: Term,
    output: Arc<OutputState>,
}

impl Ui {
    pub(crate) fn new(verbose: bool, interrupted: Arc<AtomicBool>) -> Self {
        Self {
            verbose,
            palette: Palette::stderr(),
            term: Term::stderr(),
            output: Arc::new(OutputState {
                interrupted,
                ended: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn info(&self, label: &str, value: impl fmt::Display) {
        if self.verbose {
            self.status(label, value);
        }
    }

    pub(crate) fn finish_interrupted(&self) {
        // Static status rows already end in a newline, but a terminal may have
        // echoed ^C on the following row. Live rows finalized after the signal
        // already supply their own newline and must not gain a second one.
        self.output
            .finish_interrupted(&self.term, self.term.is_term());
    }

    pub(crate) fn status(&self, label: &str, value: impl fmt::Display) {
        self.output.write_line(
            &self.term,
            &format!("{} {value}", self.palette.label(label)),
        );
    }

    pub(crate) fn activity(&self, label: &str, detail: &str) -> Loading {
        let bar = ProgressBar::with_draw_target(None, draw_target(&self.term));
        if bar.is_hidden() {
            self.info(label, detail);
        } else {
            let (label, detail, term, palette) = (
                label.to_owned(),
                detail.to_owned(),
                self.term.clone(),
                self.palette,
            );
            bar.set_style(
                ProgressStyle::with_template("{activity}")
                    .expect("valid activity template")
                    .with_key(
                        "activity",
                        move |_: &ProgressState, output: &mut dyn fmt::Write| {
                            let width = usize::from(term.size().1).saturating_sub(3);
                            let line = format!("{} {detail}", palette.label(&label));
                            let line = truncate_str(&line, width, "");
                            let _ = output.write_str(&line);
                        },
                    ),
            );
            bar.enable_steady_tick(REFRESH_INTERVAL);
            bar.tick();
        }
        Loading {
            bar,
            keep: label == "Loading",
            line: format!("{} {detail}", self.palette.label(label)),
            term: Arc::new(self.term.clone()),
            output: self.output.clone(),
        }
    }

    pub(crate) fn progress(&self, total: Option<u64>) -> Progress {
        let bar = ProgressBar::with_draw_target(total, draw_target(&self.term));
        let progress = Progress {
            bar,
            term: Arc::new(self.term.clone()),
            output: self.output.clone(),
            palette: self.palette,
            stage: Stage::Processing,
            encoded: Arc::new(AtomicU64::new(0)),
            finished: false,
        };
        progress.bar.set_style(progress.style());
        if progress.bar.is_hidden() {
            self.output.write_line(
                &self.term,
                &format!("{} video", self.palette.value(padded(PROGRESS_LABEL))),
            );
        } else {
            progress.bar.enable_steady_tick(REFRESH_INTERVAL);
            progress.bar.tick();
        }
        progress
    }
}

pub(crate) struct Loading {
    bar: ProgressBar,
    keep: bool,
    line: String,
    term: Arc<dyn indicatif::TermLike>,
    output: Arc<OutputState>,
}

impl Drop for Loading {
    fn drop(&mut self) {
        if self.keep && !self.bar.is_hidden() {
            persist_line(&self.bar, self.term.as_ref(), &self.output, &self.line);
        } else {
            self.bar.disable_steady_tick();
            self.bar.finish_and_clear();
        }
    }
}

pub(crate) struct Progress {
    bar: ProgressBar,
    term: Arc<dyn indicatif::TermLike>,
    palette: Palette,
    stage: Stage,
    encoded: Arc<AtomicU64>,
    output: Arc<OutputState>,
    finished: bool,
}

impl Progress {
    fn style(&self) -> ProgressStyle {
        let (term, palette, stage, encoded) = (
            self.term.clone(),
            self.palette,
            self.stage,
            self.encoded.clone(),
        );
        ProgressStyle::with_template("{status}")
            .expect("valid progress template")
            .with_key(
                "status",
                move |state: &ProgressState, output: &mut dyn fmt::Write| {
                    let width = usize::from(term.width()).saturating_sub(3);
                    let line = render_status(
                        if matches!(stage, Stage::Saving) {
                            encoded
                                .load(Ordering::Relaxed)
                                .min(state.len().unwrap_or(u64::MAX))
                        } else {
                            state.pos()
                        },
                        state.len(),
                        state.elapsed(),
                        width,
                        palette,
                        stage,
                    );
                    let _ = output.write_str(&line);
                },
            )
    }

    pub(crate) fn advance(&self, frames: u64) {
        // The counter advances only after the encoder has accepted each frame.
        self.bar.set_position(frames);
    }

    pub(crate) fn saving(&mut self, frames: u64) {
        let hidden = self.bar.is_hidden();
        let message = processed(frames, self.bar.elapsed(), self.palette);
        persist_line(&self.bar, self.term.as_ref(), &self.output, &message);
        self.stage = Stage::Saving;
        self.bar = ProgressBar::with_draw_target(
            Some(frames),
            if hidden {
                ProgressDrawTarget::hidden()
            } else {
                ProgressDrawTarget::term_like_with_hz(Box::new(UnpaddedTerm(self.term.clone())), 10)
            },
        );
        self.bar.set_style(self.style());
        if !hidden {
            self.bar.enable_steady_tick(REFRESH_INTERVAL);
            self.bar.tick();
        }
    }

    pub(crate) fn encoded_frames(&self) -> Arc<AtomicU64> {
        self.encoded.clone()
    }

    pub(crate) fn finish(&mut self) {
        let message = saved(
            self.bar.length().unwrap_or(self.bar.position()),
            self.bar.elapsed(),
            self.palette,
        );
        persist_line(&self.bar, self.term.as_ref(), &self.output, &message);
        self.finished = true;
    }

    pub(crate) fn abandon(&mut self) {
        if self.finished {
            return;
        }
        let width = if self.bar.is_hidden() {
            usize::MAX
        } else {
            usize::from(self.term.width()).saturating_sub(3)
        };
        let line = render_status(
            if matches!(self.stage, Stage::Saving) {
                self.encoded
                    .load(Ordering::Relaxed)
                    .min(self.bar.length().unwrap_or(u64::MAX))
            } else {
                self.bar.position()
            },
            self.bar.length(),
            self.bar.elapsed(),
            width,
            self.palette,
            self.stage,
        );
        persist_line(&self.bar, self.term.as_ref(), &self.output, &line);
        self.finished = true;
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.abandon();
    }
}

fn persist_line(
    bar: &ProgressBar,
    term: &dyn indicatif::TermLike,
    output: &OutputState,
    line: &str,
) {
    let width = if bar.is_hidden() {
        usize::MAX
    } else {
        usize::from(term.width()).saturating_sub(3)
    };
    // Stop redraws before clearing and committing exactly one newline-terminated row.
    bar.disable_steady_tick();
    bar.finish_and_clear();
    output.write_line(term, &truncate_str(line, width, ""));
}

fn elapsed_time(elapsed: Duration) -> String {
    if elapsed.as_secs() >= 60 {
        let seconds = elapsed
            .as_secs()
            .saturating_add(u64::from(elapsed.subsec_nanos() >= 500_000_000));
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{:.2}s", elapsed.as_secs_f64())
    }
}

fn processed(frames: u64, elapsed: Duration, palette: Palette) -> String {
    format!(
        "{} {frames} frames in {}",
        palette.label("Processed"),
        elapsed_time(elapsed)
    )
}

fn saved(frames: u64, elapsed: Duration, palette: Palette) -> String {
    format!(
        "{} {frames} frames in {}",
        palette.label("Saved"),
        elapsed_time(elapsed)
    )
}

fn timestamp(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 3600 {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    } else {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    }
}

// Render at draw time, keeping elapsed time current even during slow calls.
// Choose fields by display width on every redraw so terminal resizing is handled.
fn render_status(
    frames: u64,
    total: Option<u64>,
    elapsed: Duration,
    width: usize,
    palette: Palette,
    stage: Stage,
) -> String {
    let separator = " | ";
    let time = timestamp(elapsed);
    // A stale underestimate cannot provide a meaningful percentage or ETA.
    // Saving retains the full bar until encoder shutdown and file commit finish.
    let total = total.filter(|&count| {
        count > 0
            && if matches!(stage, Stage::Saving) {
                frames <= count
            } else {
                frames < count
            }
    });
    let count = match total {
        Some(total) => format!("{frames}/{total}"),
        None => frames.to_string(),
    };
    let label = palette.value(padded(if matches!(stage, Stage::Saving) {
        "Saving"
    } else {
        PROGRESS_LABEL
    }));
    let mut percent = String::new();
    let mut eta = String::new();
    if let Some(total) = total {
        percent = format!(" ({:.1}%)", frames as f64 / total as f64 * 100.0);
        let remaining = if frames == total {
            Some(Duration::ZERO)
        } else if frames > 0 && !elapsed.is_zero() {
            Duration::try_from_secs_f64(
                elapsed.as_secs_f64() * total.saturating_sub(frames) as f64 / frames as f64,
            )
            .ok()
        } else {
            None
        };
        eta = format!(
            "{separator}ETA {}",
            remaining.map(timestamp).unwrap_or_else(|| "--:--".into())
        );
    }
    let suffixes = [
        format!("{count} frames{percent}: elapsed {time}{eta}"),
        format!("{count} frames{percent}: {time}{eta}"),
        format!("{count}{percent}: {time}{eta}"),
        format!("{count}{percent}{eta}"),
    ];
    if let Some(total) = total {
        for suffix in &suffixes {
            // Two spaces and the brackets surround the bar's interior cells.
            let fixed = measure_text_width(&label) + measure_text_width(suffix) + 4;
            let cells = width.saturating_sub(fixed).min(MAX_BAR_WIDTH);
            if cells >= MIN_BAR_WIDTH {
                let bar = progress_bar(frames, total, cells);
                return format!("{label} {bar} {suffix}");
            }
        }
    }
    fit(
        width,
        [
            format!("{label} {}", suffixes[0]),
            format!("{label} {}", suffixes[1]),
            format!("{label} {}", suffixes[2]),
            format!("{label} {count}"),
            count,
        ],
    )
}

fn progress_bar(frames: u64, total: u64, cells: usize) -> String {
    let filled =
        ((u128::from(frames) * cells as u128) / u128::from(total)).min(cells as u128) as usize;
    let mut bar = "=".repeat(filled);
    if filled < cells {
        bar.push('>');
        bar.extend(std::iter::repeat_n(' ', cells - filled - 1));
    }
    format!("[{bar}]")
}

fn fit<const N: usize>(width: usize, choices: [String; N]) -> String {
    for choice in &choices {
        if measure_text_width(choice) <= width {
            return choice.clone();
        }
    }
    truncate_str(&choices[N - 1], width, "").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Default)]
    struct RecordingTerm(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl indicatif::TermLike for RecordingTerm {
        fn width(&self) -> u16 {
            120
        }
        fn move_cursor_up(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn move_cursor_down(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn move_cursor_right(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn move_cursor_left(&self, _: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn write_line(&self, value: &str) -> std::io::Result<()> {
            self.write_str(&format!("{value}\n"))
        }
        fn write_str(&self, value: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push(value.to_owned());
            Ok(())
        }
        fn clear_line(&self) -> std::io::Result<()> {
            self.0.lock().unwrap().push("<clear>".into());
            Ok(())
        }
        fn flush(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl RecordingTerm {
        fn assert_last_line_retained(&self, text: &str) {
            let events = self.0.lock().unwrap();
            let last_line = events
                .iter()
                .rposition(|event| event.contains(text))
                .expect("missing retained line");
            assert!(
                events[last_line].ends_with('\n'),
                "missing explicit newline: {events:?}"
            );
            assert!(
                !events[last_line + 1..]
                    .iter()
                    .any(|event| event == "<clear>"),
                "{events:?}"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.contains(text) && event.ends_with('\n'))
                    .count(),
                1,
                "{events:?}"
            );
            assert!(
                !events
                    .iter()
                    .any(|event| !event.is_empty() && event.bytes().all(|byte| byte == b' ')),
                "unexpected padding: {events:?}"
            );
        }
    }

    #[test]
    fn interrupted_output_ends_once_without_trailing_spaces() {
        let recorder = RecordingTerm::default();
        let output = OutputState::default();
        output.write_line(&recorder, "Scanning video frames   ");
        output.interrupted.store(true, Ordering::Relaxed);
        output.finish_interrupted(&recorder, true);
        output.finish_interrupted(&recorder, true);
        assert_eq!(
            *recorder.0.lock().unwrap(),
            ["Scanning video frames\n", "\n"]
        );

        let recorder = RecordingTerm::default();
        let output = OutputState::default();
        output.interrupted.store(true, Ordering::Relaxed);
        output.write_line(&recorder, "Loading model   ");
        output.finish_interrupted(&recorder, true);
        assert_eq!(*recorder.0.lock().unwrap(), ["Loading model\n"]);

        let recorder = RecordingTerm::default();
        let output = OutputState::default();
        output.interrupted.store(true, Ordering::Relaxed);
        output.finish_interrupted(&recorder, false);
        assert!(recorder.0.lock().unwrap().is_empty());
    }

    #[test]
    fn saving_matches_processing_color_and_saved_has_a_green_summary() {
        let line = render_status(
            3,
            Some(272),
            Duration::from_secs(12),
            120,
            Palette { enabled: false },
            Stage::Saving,
        );
        assert_eq!(
            line,
            format!(
                "{} [>                               ] 3/272 frames (1.1%): elapsed 00:12 | ETA 17:56",
                padded("Saving")
            )
        );
        let colored = render_status(
            272,
            Some(272),
            Duration::from_secs(12),
            120,
            Palette { enabled: true },
            Stage::Saving,
        );
        assert!(colored.contains("\x1b[36m"), "{colored:?}");
        assert!(!colored.contains("\x1b[34m"), "{colored:?}");
        let summary = saved(136, Duration::from_millis(1940), Palette { enabled: true });
        assert!(summary.contains("\x1b[32m"), "{summary:?}");
        assert!(
            summary.ends_with("\x1b[0m 136 frames in 1.94s"),
            "{summary:?}"
        );
        for width in [0, 1, 10, 20, 40, 80] {
            let line = render_status(
                272,
                Some(272),
                Duration::from_secs(12),
                width,
                Palette { enabled: true },
                Stage::Saving,
            );
            assert!(measure_text_width(&line) <= width);
        }
    }

    #[test]
    fn cancelled_saving_keeps_one_status_line() {
        let recorder = RecordingTerm::default();
        let mut progress = Progress {
            bar: ProgressBar::with_draw_target(
                Some(272),
                ProgressDrawTarget::term_like(Box::new(UnpaddedTerm(Arc::new(recorder.clone())))),
            ),
            term: Arc::new(recorder.clone()),
            palette: Palette { enabled: false },
            stage: Stage::Processing,
            encoded: Arc::new(AtomicU64::new(0)),
            output: Arc::new(OutputState::default()),
            finished: false,
        };
        progress.bar.set_style(progress.style());
        progress.advance(272);
        progress.saving(272);
        progress.output.interrupted.store(true, Ordering::Relaxed);
        progress.abandon();
        progress.abandon();
        progress
            .output
            .finish_interrupted(progress.term.as_ref(), true);
        drop(progress);
        recorder.assert_last_line_retained("Saving");
        let events = recorder.0.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.contains("Processed") && event.ends_with('\n'))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| event.contains("Saved")));
    }

    #[test]
    fn completed_saving_keeps_both_summaries_without_duplicates() {
        let recorder = RecordingTerm::default();
        let mut progress = Progress {
            bar: ProgressBar::with_draw_target(
                Some(136),
                ProgressDrawTarget::term_like(Box::new(UnpaddedTerm(Arc::new(recorder.clone())))),
            ),
            term: Arc::new(recorder.clone()),
            palette: Palette { enabled: false },
            stage: Stage::Processing,
            encoded: Arc::new(AtomicU64::new(0)),
            output: Arc::new(OutputState::default()),
            finished: false,
        };
        progress.bar.set_style(progress.style());
        progress.advance(136);
        progress.saving(136);
        progress.encoded.store(136, Ordering::Relaxed);
        progress.finish();
        drop(progress);
        recorder.assert_last_line_retained("Saved");
        let events = recorder.0.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.contains("Processed") && event.ends_with('\n'))
                .count(),
            1
        );
    }

    #[test]
    fn stopped_or_dropped_progress_keeps_last_frame_status() {
        for explicitly_abandon in [false, true] {
            let recorder = RecordingTerm::default();
            let mut progress = Progress {
                bar: ProgressBar::with_draw_target(
                    Some(600),
                    ProgressDrawTarget::term_like(Box::new(UnpaddedTerm(Arc::new(
                        recorder.clone(),
                    )))),
                ),
                term: Arc::new(recorder.clone()),
                palette: Palette { enabled: false },
                stage: Stage::Processing,
                encoded: Arc::new(AtomicU64::new(0)),
                output: Arc::new(OutputState::default()),
                finished: false,
            };
            progress.bar.set_style(progress.style());
            progress.advance(300);
            if explicitly_abandon {
                progress.abandon();
            }
            drop(progress);
            recorder.assert_last_line_retained("300/600");
            let events = recorder.0.lock().unwrap();
            assert!(
                !events
                    .iter()
                    .any(|event| event.contains("Processed") || event.contains("Cancelling"))
            );
        }
    }

    #[test]
    fn model_loading_line_remains_after_loading_finishes() {
        let recorder = RecordingTerm::default();
        let bar = ProgressBar::with_draw_target(
            None,
            ProgressDrawTarget::term_like(Box::new(UnpaddedTerm(Arc::new(recorder.clone())))),
        );
        bar.set_style(ProgressStyle::with_template("{msg}").unwrap());
        let detail = format!("Loading model `{}`", crate::Model::default());
        let line = format!("{} model `{}`", padded("Loading"), crate::Model::default());
        bar.set_message(line.clone());
        let loading = Loading {
            bar,
            keep: true,
            line,
            term: Arc::new(recorder.clone()),
            output: Arc::new(OutputState::default()),
        };
        drop(loading);
        recorder.assert_last_line_retained(&detail);
    }

    fn status(frames: u64, total: Option<u64>, width: usize, color: bool) -> String {
        render_status(
            frames,
            total,
            Duration::from_secs(10),
            width,
            Palette { enabled: color },
            Stage::Processing,
        )
    }

    #[test]
    fn reports_actual_count_estimated_total_and_elapsed_time() {
        let line = status(300, Some(600), 120, false);
        assert!(
            line.starts_with(&format!("{} [", padded(PROGRESS_LABEL)))
                && line.contains("] 300/600 frames (50.0%): elapsed"),
            "{line}"
        );
        assert!(line.contains("elapsed 00:10 | ETA 00:10"), "{line}");
    }

    #[test]
    fn progress_bar_matches_frame_fraction_and_label_is_cyan() {
        let palette = Palette { enabled: false };
        assert_eq!(progress_bar(0, 424, 10), "[>         ]");
        assert_eq!(progress_bar(80, 424, 10), "[=>        ]");
        assert_eq!(progress_bar(424, 424, 10), "[==========]");
        assert_eq!(progress_bar(u64::MAX, u64::MAX, 10), "[==========]");
        let line = render_status(
            80,
            Some(424),
            Duration::from_secs(2),
            120,
            palette,
            Stage::Processing,
        );
        assert!(
            line.contains("80/424 frames (18.9%): elapsed 00:02 | ETA 00:08"),
            "{line}"
        );
        let colored = status(300, Some(600), 120, true);
        let label = padded(PROGRESS_LABEL);
        let colored_label = format!("\x1b[36m{label}\x1b[0m");
        assert!(colored.contains(&colored_label), "{colored:?}");
        let remainder = colored.replace(&colored_label, &label);
        assert!(!remainder.contains("\x1b["), "{colored:?}");
    }

    #[test]
    fn unknown_and_exceeded_totals_do_not_invent_percentages_or_eta() {
        for total in [None, Some(0), Some(200), Some(300)] {
            let line = status(300, total, 120, false);
            assert!(line.contains("300 frames"));
            assert!(!line.contains("ETA") || line.contains("--:--"), "{line}");
            assert!(!line.contains('%') && !line.contains('~'), "{line}");
        }
        let starting = render_status(
            0,
            Some(600),
            Duration::ZERO,
            120,
            Palette { enabled: false },
            Stage::Processing,
        );
        assert!(starting.contains("ETA --:--") && !starting.contains("0.00x"));
        let completed = render_status(
            544,
            Some(544),
            Duration::from_secs(31),
            120,
            Palette { enabled: false },
            Stage::Processing,
        );
        assert_eq!(
            completed,
            format!("{} 544 frames: elapsed 00:31", padded(PROGRESS_LABEL))
        );
    }

    #[test]
    fn completion_is_aligned_with_green_label_and_default_detail() {
        let palette = Palette { enabled: false };
        let message = processed(424, Duration::from_millis(120), palette);
        assert_eq!(
            message,
            format!("{} 424 frames in 0.12s", padded("Processed"))
        );
        let colored = processed(424, Duration::from_millis(120), Palette { enabled: true });
        assert!(colored.contains("\x1b[32m"), "{colored:?}");
        assert!(
            colored.ends_with("\x1b[0m 424 frames in 0.12s"),
            "{colored:?}"
        );
        for (milliseconds, expected) in [
            (59_990, "59.99s"),
            (60_000, "1m 0s"),
            (60_490, "1m 0s"),
            (60_500, "1m 1s"),
            (89_900, "1m 30s"),
            (119_490, "1m 59s"),
            (119_500, "2m 0s"),
            (3_600_000, "60m 0s"),
        ] {
            assert_eq!(
                processed(12820, Duration::from_millis(milliseconds), palette),
                format!("{} 12820 frames in {expected}", padded("Processed"))
            );
        }
        for label in ["Source", "Target", "Device", "Loading"] {
            let aligned = palette.label(label);
            assert_eq!(measure_text_width(&aligned), LABEL_WIDTH);
            assert!(aligned.ends_with(label));
        }
    }

    #[test]
    fn adapts_to_terminal_width_with_and_without_color() {
        for color in [false, true] {
            for width in [0, 1, 10, 20, 40, 60, 80, 120] {
                let line = status(1250, Some(2000), width, color);
                assert!(
                    measure_text_width(&line) <= width,
                    "width {width}: {line:?}"
                );
                if width >= 40 {
                    assert!(line.contains("1250/2000"), "{line:?}");
                    assert!(!line.contains('x'), "{line:?}");
                }
            }
        }
        assert!(status(300, Some(600), 120, true).contains("\x1b["));
        assert!(!status(300, Some(600), 120, false).contains("\x1b["));
        assert_eq!(timestamp(Duration::from_secs(3661)), "1:01:01");
    }
}
