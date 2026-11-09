// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use crate::config::{ACTIVE_STYLE, SUCCESS_STYLE};
use clap::builder::styling::Style;
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

fn write_line(term: &dyn indicatif::TermLike, line: &str) {
    let _ = term.write_line(line.trim_end());
    let _ = term.flush();
}

fn clear_row(term: &dyn indicatif::TermLike) {
    let _ = term.clear_line();
    let _ = term.write_str("\r");
    let _ = term.flush();
}

// Indicatif emits a separate space-only write to pad every redraw to the right
// edge. Suppress it, and stop drawing on interruption so the terminal's own
// cancellation echo and cursor position survive cleanup.
#[derive(Debug)]
struct LiveTerm(Arc<dyn indicatif::TermLike>, Arc<AtomicBool>);

impl LiveTerm {
    fn draw(
        &self,
        action: impl FnOnce(&dyn indicatif::TermLike) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.1.load(Ordering::Relaxed) {
            Ok(())
        } else {
            action(self.0.as_ref())
        }
    }
}

impl indicatif::TermLike for LiveTerm {
    fn width(&self) -> u16 {
        self.0.width()
    }
    fn height(&self) -> u16 {
        self.0.height()
    }
    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.draw(|term| term.move_cursor_up(n))
    }
    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.draw(|term| term.move_cursor_down(n))
    }
    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.draw(|term| term.move_cursor_right(n))
    }
    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        self.draw(|term| term.move_cursor_left(n))
    }
    fn write_line(&self, line: &str) -> io::Result<()> {
        self.draw(|term| term.write_line(line))
    }
    fn write_str(&self, text: &str) -> io::Result<()> {
        if text.bytes().all(|byte| byte == b' ') {
            Ok(())
        } else {
            self.draw(|term| term.write_str(text))
        }
    }
    fn clear_line(&self) -> io::Result<()> {
        self.draw(|term| term.clear_line())
    }
    fn flush(&self) -> io::Result<()> {
        // Live drawing is an overlay on the next empty row, not a committed
        // line. Leave the cursor where the last completed line positioned it.
        self.draw(|term| {
            term.write_str("\r")?;
            term.flush()
        })
    }
}

fn draw_target(term: &Term, interrupted: Arc<AtomicBool>) -> ProgressDrawTarget {
    if term.is_term() && !console::is_dumb() && enable_live_rows(term) {
        ProgressDrawTarget::term_like_with_hz(
            Box::new(LiveTerm(Arc::new(term.clone()), interrupted)),
            10,
        )
    } else {
        ProgressDrawTarget::hidden()
    }
}

// Erasing the tail while preserving native cancellation echo requires VT
// output on Windows too, including when NO_COLOR disables colored labels.
#[cfg(windows)]
fn enable_live_rows(term: &Term) -> bool {
    if term.features().is_msys_tty() {
        return true;
    }
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_ERROR_HANDLE,
        SetConsoleMode,
    };
    let mut mode = 0;
    // SAFETY: the standard handle is borrowed, and mode points to a valid
    // writable DWORD. Only the console output mode is changed.
    unsafe {
        let handle = GetStdHandle(STD_ERROR_HANDLE);
        GetConsoleMode(handle, &mut mode) != 0
            && SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_live_rows(_: &Term) -> bool {
    true
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
        self.paint(padded(value), SUCCESS_STYLE)
    }

    fn value(self, value: impl fmt::Display) -> String {
        self.paint(value, ACTIVE_STYLE)
    }

    fn paint(self, value: impl fmt::Display, style: Style) -> String {
        if self.enabled {
            format!("{style}{value}{style:#}")
        } else {
            value.to_string()
        }
    }
}

pub(crate) fn warning(message: impl fmt::Display) {
    let prefix = style(padded("warning:"))
        .yellow()
        .bold()
        .force_styling(Palette::stderr().enabled);
    eprintln!("{prefix} {message}");
}

fn error_line(message: impl fmt::Display, palette: Palette) -> String {
    let prefix = style("error:").red().bold().force_styling(palette.enabled);
    format!("{prefix} {message}")
}

pub(crate) fn error(message: impl fmt::Display) {
    eprintln!("{}", error_line(message, Palette::stderr()));
}

pub(crate) fn report_error(failure: &anyhow::Error) {
    if let Some(backend) = failure.downcast_ref::<crate::worker::Failure>() {
        let fallback = crate::worker::BackendReport {
            reason: backend.to_string(),
            details: Vec::new(),
        };
        report_backend_error(
            backend.report().unwrap_or(&fallback),
            "native backend failed",
        );
    } else if let Some(backend) = failure.downcast_ref::<crate::process::ProcessFailure>() {
        report_backend_error(&backend.report(), "media processing failed");
    } else {
        error(format_args!("{failure:#}"));
    }
}

fn report_backend_error(report: &crate::worker::BackendReport, summary: &str) {
    let term = Term::stderr();
    for line in backend_error_lines(report, summary, Palette::stderr()) {
        write_line(&term, &line);
    }
}

fn backend_error_lines(
    report: &crate::worker::BackendReport,
    summary: impl fmt::Display,
    palette: Palette,
) -> Vec<String> {
    let mut output = vec![error_line(&report.reason, palette)];
    output.extend(report.details.iter().cloned());
    output.push(String::new());
    output.push(error_line(summary, palette));
    output
}

pub(crate) struct Ui {
    verbose: bool,
    palette: Palette,
    term: Term,
    interrupted: Arc<AtomicBool>,
}

impl Ui {
    pub(crate) fn new(verbose: bool, interrupted: Arc<AtomicBool>) -> Self {
        Self {
            verbose,
            palette: Palette::stderr(),
            term: Term::stderr(),
            interrupted,
        }
    }

    pub(crate) fn info(&self, label: &str, value: impl fmt::Display) {
        if self.verbose {
            self.status(label, value);
        }
    }

    pub(crate) fn status(&self, label: &str, value: impl fmt::Display) {
        write_line(
            &self.term,
            &format!("{} {value}", self.palette.label(label)),
        );
    }

    pub(crate) fn counting(&self) -> Activity {
        self.activity(ActivityKind::Counting, true)
    }

    pub(crate) fn loading(&self, detail: &str) -> Activity {
        self.activity(ActivityKind::Loading(detail.to_owned()), self.verbose)
    }

    fn activity(&self, kind: ActivityKind, show_when_hidden: bool) -> Activity {
        let activity = Activity {
            bar: ProgressBar::with_draw_target(
                None,
                draw_target(&self.term, self.interrupted.clone()),
            ),
            kind,
            palette: self.palette,
            show_when_hidden,
            finished: false,
            term: Arc::new(self.term.clone()),
            interrupted: self.interrupted.clone(),
        };
        activity.bar.set_style(activity.style());
        if !activity.bar.is_hidden() {
            activity.bar.enable_steady_tick(REFRESH_INTERVAL);
            activity.bar.tick();
        }
        activity
    }

    pub(crate) fn progress(&self, total: Option<u64>) -> Progress {
        let bar =
            ProgressBar::with_draw_target(total, draw_target(&self.term, self.interrupted.clone()));
        let progress = Progress {
            bar,
            term: Arc::new(self.term.clone()),
            interrupted: self.interrupted.clone(),
            palette: self.palette,
            stage: Stage::Processing,
            encoded: Arc::new(AtomicU64::new(0)),
            finished: false,
        };
        progress.bar.set_style(progress.style());
        if !progress.bar.is_hidden() {
            progress.bar.enable_steady_tick(REFRESH_INTERVAL);
            progress.bar.tick();
        }
        progress
    }
}

#[derive(Clone)]
enum ActivityKind {
    Counting,
    Loading(String),
}

impl ActivityKind {
    fn running(&self, frames: u64, palette: Palette) -> String {
        let (label, detail) = match self {
            Self::Counting => ("Counting", format!("{frames} frames")),
            Self::Loading(detail) => ("Loading", detail.clone()),
        };
        format!("{} {detail}", palette.value(padded(label)))
    }

    fn completed(&self, frames: u64, palette: Palette) -> String {
        let (label, detail) = match self {
            Self::Counting => ("Counted", format!("{frames} frames in total")),
            Self::Loading(detail) => ("Loaded", detail.clone()),
        };
        format!("{} {detail}", palette.label(label))
    }
}

pub(crate) struct Activity {
    bar: ProgressBar,
    kind: ActivityKind,
    palette: Palette,
    show_when_hidden: bool,
    finished: bool,
    term: Arc<dyn indicatif::TermLike>,
    interrupted: Arc<AtomicBool>,
}

impl Activity {
    fn style(&self) -> ProgressStyle {
        let (kind, palette, term) = (self.kind.clone(), self.palette, self.term.clone());
        ProgressStyle::with_template("{activity}")
            .expect("valid activity template")
            .with_key(
                "activity",
                move |state: &ProgressState, output: &mut dyn fmt::Write| {
                    let width = usize::from(term.width()).saturating_sub(3);
                    let line = kind.running(state.pos(), palette);
                    let _ = output.write_str(&truncate_str(&line, width, ""));
                },
            )
    }

    pub(crate) fn advance(&self, frames: u64) {
        self.bar.set_position(frames);
    }

    pub(crate) fn finish(&mut self) {
        if self.finished {
            return;
        }
        if self.interrupted.load(Ordering::Relaxed) {
            clear_live_line(&self.bar, self.term.as_ref(), &self.interrupted);
            self.finished = true;
            return;
        }
        if !self.bar.is_hidden() || self.show_when_hidden {
            let line = self.kind.completed(self.bar.position(), self.palette);
            persist_line(&self.bar, self.term.as_ref(), &line);
        } else {
            clear_bar(&self.bar);
        }
        self.finished = true;
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        if !self.finished {
            clear_live_line(&self.bar, self.term.as_ref(), &self.interrupted);
        }
    }
}

pub(crate) struct Progress {
    bar: ProgressBar,
    term: Arc<dyn indicatif::TermLike>,
    palette: Palette,
    stage: Stage,
    encoded: Arc<AtomicU64>,
    interrupted: Arc<AtomicBool>,
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
        if self.interrupted.load(Ordering::Relaxed) {
            self.abandon();
            return;
        }
        let hidden = self.bar.is_hidden();
        let message = processed(frames, self.bar.elapsed(), self.palette);
        persist_line(&self.bar, self.term.as_ref(), &message);
        self.stage = Stage::Saving;
        self.bar = ProgressBar::with_draw_target(
            Some(frames),
            if hidden {
                ProgressDrawTarget::hidden()
            } else {
                ProgressDrawTarget::term_like_with_hz(
                    Box::new(LiveTerm(self.term.clone(), self.interrupted.clone())),
                    10,
                )
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
        if self.finished {
            return;
        }
        if self.interrupted.load(Ordering::Relaxed) {
            self.abandon();
            return;
        }
        let message = saved(
            self.bar.length().unwrap_or(self.bar.position()),
            self.bar.elapsed(),
            self.palette,
        );
        persist_line(&self.bar, self.term.as_ref(), &message);
        self.finished = true;
    }

    pub(crate) fn abandon(&mut self) {
        if !self.finished {
            clear_live_line(&self.bar, self.term.as_ref(), &self.interrupted);
            self.finished = true;
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.abandon();
    }
}

fn clear_bar(bar: &ProgressBar) {
    bar.disable_steady_tick();
    bar.finish_and_clear();
}

fn clear_live_line(bar: &ProgressBar, term: &dyn indicatif::TermLike, interrupted: &AtomicBool) {
    clear_bar(bar);
    if !bar.is_hidden() {
        if interrupted.load(Ordering::Relaxed) {
            // The cursor was at column one before the terminal echoed the
            // interrupt. Erase only the remaining live text, leaving that echo
            // and its cursor position alone. Do not synthesize a newline or ^C.
            let _ = term.write_str("\x1b[K");
            let _ = term.flush();
        } else {
            clear_row(term);
        }
    }
}

fn persist_line(bar: &ProgressBar, term: &dyn indicatif::TermLike, line: &str) {
    let width = if bar.is_hidden() {
        usize::MAX
    } else {
        usize::from(term.width()).saturating_sub(3)
    };
    clear_bar(bar);
    write_line(term, &truncate_str(line, width, ""));
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

    fn assert_no_persisted_live_rows(recorder: &RecordingTerm) {
        let events = recorder.0.lock().unwrap();
        assert!(
            !events.iter().any(|event| event.ends_with('\n')
                && (event.contains("Loading")
                    || event.contains("Processing")
                    || event.contains("Saving")
                    || event.contains("Counting"))),
            "{events:?}"
        );
        assert!(
            events
                .iter()
                .any(|event| event == "<clear>" || event == "\x1b[K")
        );
        assert!(matches!(
            events.last().map(String::as_str),
            Some("\r" | "\x1b[K")
        ));
    }

    #[test]
    fn persistent_rows_end_with_newline_and_live_rows_restore_column_one() {
        let recorder = RecordingTerm::default();
        write_line(&recorder, "Completed task   ");
        assert_eq!(*recorder.0.lock().unwrap(), ["Completed task\n"]);
        let live = LiveTerm(Arc::new(recorder.clone()), Arc::new(AtomicBool::new(false)));
        indicatif::TermLike::write_str(&live, "Loading model").unwrap();
        indicatif::TermLike::flush(&live).unwrap();
        assert_eq!(recorder.0.lock().unwrap().last().unwrap(), "\r");
        clear_row(&recorder);
        let events = recorder.0.lock().unwrap();
        assert_eq!(
            events.iter().filter(|event| event.ends_with('\n')).count(),
            1
        );
        assert_eq!(events.last().unwrap(), "\r");
    }

    #[test]
    fn interrupted_live_draws_leave_terminal_echo_and_cursor_untouched() {
        let recorder = RecordingTerm::default();
        let interrupted = Arc::new(AtomicBool::new(true));
        let live = LiveTerm(Arc::new(recorder.clone()), interrupted);
        use indicatif::TermLike;
        live.move_cursor_up(1).unwrap();
        live.move_cursor_down(1).unwrap();
        live.move_cursor_left(1).unwrap();
        live.move_cursor_right(1).unwrap();
        live.write_str("Saving").unwrap();
        live.write_line("Saving").unwrap();
        live.clear_line().unwrap();
        live.flush().unwrap();
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
        assert!(colored.contains("\x1b[96m"), "{colored:?}");
        assert!(!colored.contains("\x1b[34m"), "{colored:?}");
        let summary = saved(136, Duration::from_millis(1940), Palette { enabled: true });
        assert!(summary.contains("\x1b[92m"), "{summary:?}");
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
    fn cancelled_saving_removes_the_live_row() {
        let recorder = RecordingTerm::default();
        let interrupted = Arc::new(AtomicBool::new(false));
        let mut progress = Progress {
            bar: ProgressBar::with_draw_target(
                Some(272),
                ProgressDrawTarget::term_like(Box::new(LiveTerm(
                    Arc::new(recorder.clone()),
                    interrupted.clone(),
                ))),
            ),
            term: Arc::new(recorder.clone()),
            palette: Palette { enabled: false },
            stage: Stage::Processing,
            encoded: Arc::new(AtomicU64::new(0)),
            interrupted,
            finished: false,
        };
        progress.bar.set_style(progress.style());
        progress.advance(272);
        progress.saving(272);
        // Model an echo supplied by the terminal, not by the application.
        indicatif::TermLike::write_str(&recorder, "^C").unwrap();
        let echo_index = recorder.0.lock().unwrap().len();
        progress.interrupted.store(true, Ordering::Relaxed);
        progress.abandon();
        progress.abandon();
        drop(progress);
        assert_no_persisted_live_rows(&recorder);
        let events = recorder.0.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.contains("Processed") && event.ends_with('\n'))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| event.contains("Saved")));
        assert_eq!(&events[echo_index..], ["\x1b[K"]);
    }

    #[test]
    fn completed_saving_keeps_both_summaries_without_duplicates() {
        let recorder = RecordingTerm::default();
        let interrupted = Arc::new(AtomicBool::new(false));
        let mut progress = Progress {
            bar: ProgressBar::with_draw_target(
                Some(136),
                ProgressDrawTarget::term_like(Box::new(LiveTerm(
                    Arc::new(recorder.clone()),
                    interrupted.clone(),
                ))),
            ),
            term: Arc::new(recorder.clone()),
            palette: Palette { enabled: false },
            stage: Stage::Processing,
            encoded: Arc::new(AtomicU64::new(0)),
            interrupted,
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
    fn stopped_or_dropped_progress_clears_the_live_row() {
        for explicitly_abandon in [false, true] {
            let recorder = RecordingTerm::default();
            let interrupted = Arc::new(AtomicBool::new(false));
            let mut progress = Progress {
                bar: ProgressBar::with_draw_target(
                    Some(600),
                    ProgressDrawTarget::term_like(Box::new(LiveTerm(
                        Arc::new(recorder.clone()),
                        interrupted.clone(),
                    ))),
                ),
                term: Arc::new(recorder.clone()),
                palette: Palette { enabled: false },
                stage: Stage::Processing,
                encoded: Arc::new(AtomicU64::new(0)),
                interrupted,
                finished: false,
            };
            progress.bar.set_style(progress.style());
            progress.advance(300);
            if explicitly_abandon {
                progress.abandon();
            }
            drop(progress);
            assert_no_persisted_live_rows(&recorder);
            let events = recorder.0.lock().unwrap();
            assert!(
                !events
                    .iter()
                    .any(|event| event.contains("Processed") || event.contains("Cancelling"))
            );
        }
    }

    #[test]
    fn model_loading_keeps_one_line_and_only_reports_loaded_on_success() {
        for success in [false, true] {
            let recorder = RecordingTerm::default();
            let interrupted = Arc::new(AtomicBool::new(false));
            let bar = ProgressBar::with_draw_target(
                None,
                ProgressDrawTarget::term_like(Box::new(LiveTerm(
                    Arc::new(recorder.clone()),
                    interrupted.clone(),
                ))),
            );
            let palette = Palette { enabled: true };
            let detail = format!("model `{}`", crate::Model::default());
            let line = format!("{} {detail}", palette.value(padded("Loading")));
            let loaded_line = format!("{} {detail}", palette.label("Loaded"));
            assert!(line.contains("\x1b[96m"));
            assert!(loaded_line.contains("\x1b[92m"));
            let mut loading = Activity {
                bar,
                kind: ActivityKind::Loading(detail),
                palette,
                show_when_hidden: false,
                finished: false,
                term: Arc::new(recorder.clone()),
                interrupted,
            };
            loading.bar.set_style(loading.style());
            loading.bar.tick();
            if success {
                loading.finish();
                loading.finish();
            } else {
                loading.interrupted.store(true, Ordering::Relaxed);
            }
            drop(loading);
            if success {
                recorder.assert_last_line_retained("Loaded");
            } else {
                assert_no_persisted_live_rows(&recorder);
            }
            let events = recorder.0.lock().unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.contains("Loaded") && event.ends_with('\n'))
                    .count(),
                usize::from(success)
            );
        }
    }

    #[test]
    fn counting_tracks_frames_and_only_retains_successful_totals() {
        for outcome in ["complete", "cancel", "error", "redirected"] {
            let recorder = RecordingTerm::default();
            let interrupted = Arc::new(AtomicBool::new(false));
            let mut counting = Activity {
                bar: ProgressBar::with_draw_target(
                    None,
                    if outcome == "redirected" {
                        ProgressDrawTarget::hidden()
                    } else {
                        ProgressDrawTarget::term_like(Box::new(LiveTerm(
                            Arc::new(recorder.clone()),
                            interrupted.clone(),
                        )))
                    },
                ),
                kind: ActivityKind::Counting,
                palette: Palette { enabled: true },
                show_when_hidden: true,
                finished: false,
                term: Arc::new(recorder.clone()),
                interrupted,
            };
            counting.bar.set_style(counting.style());
            counting.advance(1651);
            if outcome != "redirected" {
                let events = recorder.0.lock().unwrap();
                let line = events
                    .iter()
                    .find(|event| event.contains("Counting"))
                    .unwrap();
                assert!(line.contains("\x1b[96m"), "{events:?}");
                assert!(line.ends_with("1651 frames"), "{events:?}");
                assert_eq!(events.last().unwrap(), "\r");
            }
            counting.advance(13009);
            match outcome {
                "complete" | "redirected" => {
                    counting.finish();
                    counting.finish();
                }
                "cancel" => {
                    indicatif::TermLike::write_str(&recorder, "^C").unwrap();
                    counting.interrupted.store(true, Ordering::Relaxed);
                    counting.finish();
                }
                _ => {}
            }
            drop(counting);
            if matches!(outcome, "complete" | "redirected") {
                recorder.assert_last_line_retained("Counted");
                let events = recorder.0.lock().unwrap();
                let summary = events.last().unwrap();
                assert!(summary.contains("\x1b[92m"), "{summary:?}");
                assert!(summary.ends_with("13009 frames in total\n"), "{summary:?}");
                if outcome == "redirected" {
                    assert_eq!(events.len(), 1);
                }
            } else {
                assert_no_persisted_live_rows(&recorder);
                let events = recorder.0.lock().unwrap();
                assert!(!events.iter().any(|event| event.contains("Counted")));
                if outcome == "cancel" {
                    assert_eq!(events.last().unwrap(), "\x1b[K");
                }
            }
        }
    }

    #[test]
    fn backend_errors_show_reason_then_raw_diagnostics_and_final_summary() {
        let details = vec![
            "[0 NVIDIA GeForce RTX 3050 Ti Laptop GPU] queueC=2[8] queueG=0[16] queueT=1[2]".into(),
            "[0 NVIDIA GeForce RTX 3050 Ti Laptop GPU] bugsbn1=0 bugbilz=0 bugcopc=0 bugihfa=0".into(),
            "[0 NVIDIA GeForce RTX 3050 Ti Laptop GPU] fp16-p/s/a=1/1/1 int8-p/s/a=1/1/1".into(),
            "[0 NVIDIA GeForce RTX 3050 Ti Laptop GPU] subgroup=32 basic=1 vote=1 ballot=1 shuffle=1".into(),
            "vkAllocateMemory failed -2".into(),
        ];
        let report = crate::worker::BackendReport {
            reason: "segmentation fault (SIGSEGV)".into(),
            details,
        };
        let plain =
            backend_error_lines(&report, "native backend failed", Palette { enabled: false });
        assert_eq!(plain[0], "error: segmentation fault (SIGSEGV)");
        assert_eq!(&plain[1..6], report.details.as_slice());
        assert_eq!(plain[6], "");
        assert_eq!(plain[7], "error: native backend failed");
        assert_eq!(plain.len(), 8);
        let colored =
            backend_error_lines(&report, "native backend failed", Palette { enabled: true });
        assert!(colored[0].contains("\x1b[31m"));
        assert!(colored[7].contains("\x1b[31m"));
        assert_eq!(&colored[1..6], report.details.as_slice());
        assert_eq!(
            console::strip_ansi_codes(&colored.join("\n")),
            plain.join("\n")
        );
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
        let colored_label = format!("\x1b[1m\x1b[96m{label}\x1b[0m");
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
        assert!(colored.contains("\x1b[92m"), "{colored:?}");
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
