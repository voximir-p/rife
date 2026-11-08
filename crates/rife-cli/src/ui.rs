// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::media::{self, Rate};
use console::{Term, measure_text_width, style, truncate_str};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};
use std::{fmt, sync::Arc, time::Duration};

const LABEL_WIDTH: usize = 12;
const REFRESH_INTERVAL: Duration = Duration::from_millis(150);
const PROGRESS_LABEL: &str = "Processing";
const MAX_BAR_WIDTH: usize = 32;
const MIN_BAR_WIDTH: usize = 8;

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
}

impl Ui {
    pub(crate) fn new(verbose: bool) -> Self {
        Self {
            verbose,
            palette: Palette::stderr(),
            term: Term::stderr(),
        }
    }

    pub(crate) fn info(&self, label: &str, value: impl fmt::Display) {
        if self.verbose {
            self.status(label, value);
        }
    }

    pub(crate) fn status(&self, label: &str, value: impl fmt::Display) {
        eprintln!("{} {value}", self.palette.label(label));
    }

    pub(crate) fn activity(&self, label: &str, detail: &str) -> Loading {
        let bar = ProgressBar::new_spinner();
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
                            let width = usize::from(term.size().1).saturating_sub(1);
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
        }
    }

    pub(crate) fn progress(&self, rate: Rate, total: Option<u64>) -> Progress {
        let bar = ProgressBar::with_draw_target(total, ProgressDrawTarget::stderr_with_hz(10));
        let progress = Progress {
            bar,
            rate,
            term: Arc::new(self.term.clone()),
            palette: self.palette,
            finished: false,
        };
        progress.bar.set_style(progress.style());
        if progress.bar.is_hidden() {
            eprintln!("{} video", self.palette.value(padded(PROGRESS_LABEL)));
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
}

impl Drop for Loading {
    fn drop(&mut self) {
        if self.keep && !self.bar.is_hidden() {
            persist_line(&self.bar, self.term.as_ref(), &self.line);
        } else {
            self.bar.finish_and_clear();
        }
    }
}

pub(crate) struct Progress {
    bar: ProgressBar,
    rate: Rate,
    term: Arc<dyn indicatif::TermLike>,
    palette: Palette,
    finished: bool,
}

impl Progress {
    fn style(&self) -> ProgressStyle {
        let (term, palette, rate) = (self.term.clone(), self.palette, self.rate);
        ProgressStyle::with_template("{status}")
            .expect("valid progress template")
            .with_key(
                "status",
                move |state: &ProgressState, output: &mut dyn fmt::Write| {
                    let width = usize::from(term.width()).saturating_sub(1);
                    let line = render_status(
                        state.pos(),
                        state.len(),
                        state.elapsed(),
                        rate,
                        width,
                        palette,
                    );
                    let _ = output.write_str(&line);
                },
            )
    }

    pub(crate) fn advance(&self, frames: u64) {
        // The counter advances only after the encoder has accepted each frame.
        self.bar.set_position(frames);
    }

    pub(crate) fn finish(&mut self, frames: u64) {
        let message = finished(frames, self.bar.elapsed(), self.palette);
        let width = if self.bar.is_hidden() {
            usize::MAX
        } else {
            usize::from(self.term.width()).saturating_sub(1)
        };
        persist_line(
            &self.bar,
            self.term.as_ref(),
            &truncate_str(&message, width, ""),
        );
        self.finished = true;
    }

    pub(crate) fn abandon(&mut self) {
        if self.finished {
            return;
        }
        let width = if self.bar.is_hidden() {
            usize::MAX
        } else {
            usize::from(self.term.width()).saturating_sub(1)
        };
        let line = render_status(
            self.bar.position(),
            self.bar.length(),
            self.bar.elapsed(),
            self.rate,
            width,
            self.palette,
        );
        persist_line(&self.bar, self.term.as_ref(), &line);
        self.finished = true;
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.abandon();
    }
}

fn persist_line(bar: &ProgressBar, term: &dyn indicatif::TermLike, line: &str) {
    // Indicatif pads to the right margin without an explicit newline. Windows
    // hosts differ in wrapping behavior, so finish through a real line write
    // before ordinary stderr messages (details/errors) resume.
    bar.finish_and_clear();
    let _ = term.write_line(line);
    let _ = term.flush();
}

fn finished(frames: u64, elapsed: Duration, palette: Palette) -> String {
    let time = if elapsed.as_secs() >= 60 {
        let seconds = elapsed
            .as_secs()
            .saturating_add(u64::from(elapsed.subsec_nanos() >= 500_000_000));
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{:.2}s", elapsed.as_secs_f64())
    };
    let detail = style(format!("interpolating {frames} frames in {time}"));
    format!("{} {detail}", palette.label("Finished"))
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

// Render at draw time, keeping elapsed time/speed current even during slow calls.
// Choose fields by display width on every redraw so terminal resizing is handled.
fn render_status(
    frames: u64,
    total: Option<u64>,
    elapsed: Duration,
    rate: Rate,
    width: usize,
    palette: Palette,
) -> String {
    let separator = " | ";
    let time = timestamp(elapsed);
    let speed = format!(
        "{:.2}x",
        media::realtime_factor(frames, rate, elapsed.as_secs_f64())
    );
    // A stale underestimate cannot provide a meaningful percentage or ETA.
    let total = total.filter(|&count| count > 0 && frames < count);
    let count = match total {
        Some(total) => format!("{frames}/{total}"),
        None => frames.to_string(),
    };
    let label = palette.value(padded(PROGRESS_LABEL));
    let mut percent = String::new();
    let mut eta = String::new();
    if let Some(total) = total {
        percent = format!(" ({:.1}%)", frames as f64 / total as f64 * 100.0);
        let remaining = if frames > 0 && !elapsed.is_zero() {
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
        format!("{count} frames{percent}: elapsed {time}{eta}{separator}{speed}"),
        format!("{count} frames{percent}: {time}{eta}{separator}{speed}"),
        format!("{count}{percent}: {time}{eta}{separator}{speed}"),
        format!("{count}{percent}{eta}{separator}{speed}"),
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
            format!("{label} {count}{separator}{speed}"),
            format!("{count} {speed}"),
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
        }
    }

    #[test]
    fn stopped_or_dropped_progress_keeps_last_frame_status() {
        for explicitly_abandon in [false, true] {
            let recorder = RecordingTerm::default();
            let mut progress = Progress {
                bar: ProgressBar::with_draw_target(
                    Some(600),
                    ProgressDrawTarget::term_like(Box::new(recorder.clone())),
                ),
                rate: Rate { num: 60, den: 1 },
                term: Arc::new(recorder.clone()),
                palette: Palette { enabled: false },
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
                    .any(|event| event.contains("Finished") || event.contains("Cancelling"))
            );
        }
    }

    #[test]
    fn model_loading_line_remains_after_loading_finishes() {
        let recorder = RecordingTerm::default();
        let bar = ProgressBar::with_draw_target(
            None,
            ProgressDrawTarget::term_like(Box::new(recorder.clone())),
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
        };
        drop(loading);
        recorder.assert_last_line_retained(&detail);
    }

    fn status(frames: u64, total: Option<u64>, width: usize, color: bool) -> String {
        render_status(
            frames,
            total,
            Duration::from_secs(10),
            Rate { num: 60, den: 1 },
            width,
            Palette { enabled: color },
        )
    }

    #[test]
    fn reports_actual_count_estimated_total_and_video_throughput() {
        let line = status(300, Some(600), 120, false);
        assert!(
            line.starts_with(&format!("{} [", padded(PROGRESS_LABEL)))
                && line.contains("] 300/600 frames (50.0%): elapsed"),
            "{line}"
        );
        assert!(line.contains("elapsed 00:10 | ETA 00:10 | 0.50x"), "{line}");
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
            Rate { num: 30, den: 1 },
            120,
            palette,
        );
        assert!(
            line.contains("80/424 frames (18.9%): elapsed 00:02 | ETA 00:08 | 1.33x"),
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
            Rate { num: 60, den: 1 },
            120,
            Palette { enabled: false },
        );
        assert!(starting.contains("ETA --:--") && starting.contains("0.00x"));
        let completed = render_status(
            544,
            Some(544),
            Duration::from_secs(31),
            Rate { num: 63, den: 1 },
            120,
            Palette { enabled: false },
        );
        assert_eq!(
            completed,
            format!(
                "{} 544 frames: elapsed 00:31 | 0.28x",
                padded(PROGRESS_LABEL)
            )
        );
    }

    #[test]
    fn completion_is_aligned_with_green_label_and_default_detail() {
        let palette = Palette { enabled: false };
        let message = finished(424, Duration::from_millis(120), palette);
        assert_eq!(
            message,
            format!("{} interpolating 424 frames in 0.12s", padded("Finished"))
        );
        let colored = finished(424, Duration::from_millis(120), Palette { enabled: true });
        assert!(colored.contains("\x1b[32m"), "{colored:?}");
        assert!(
            colored.ends_with("\x1b[0m interpolating 424 frames in 0.12s"),
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
                finished(12820, Duration::from_millis(milliseconds), palette),
                format!(
                    "{} interpolating 12820 frames in {expected}",
                    padded("Finished")
                )
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
                    assert!(line.contains('x'), "{line:?}");
                }
            }
        }
        assert!(status(300, Some(600), 120, true).contains("\x1b["));
        assert!(!status(300, Some(600), 120, false).contains("\x1b["));
        assert_eq!(timestamp(Duration::from_secs(3661)), "1:01:01");
    }
}
