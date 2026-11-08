// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

use clap::{
    CommandFactory, Parser,
    builder::styling::{AnsiColor, Color, Style, Styles as ClapStyles},
};
use config::{APP_NAME, VERSION};
use rife_core::{
    CancellationToken, DeviceSelection, EngineOptions, Jobs, Model, PlaybackOptions, SceneDetector,
    StreamOptions,
};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
mod chapters;
mod config;
mod encode;
#[cfg(test)]
mod encode_timing_tests;
mod media;
mod output_lock;
mod paths;
mod process;
mod ui;

pub(crate) fn make_styles() -> ClapStyles {
    let green = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
    let cyan = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)));
    let red = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red)));
    ClapStyles::styled()
        .header(green.bold())
        .usage(green.bold())
        .literal(cyan.bold())
        .placeholder(cyan)
        .error(red.bold())
}

#[derive(Debug, Parser)]
#[command(
    name = APP_NAME,
    version = VERSION,
    about = env!("CARGO_PKG_DESCRIPTION"),
    long_about = "Streaming video interpolation with embedded NCNN/Vulkan. Download FFmpeg and FFprobe into an adjacent ffmpeg/ directory, and compatible model files into an adjacent models/ directory.",
    styles = make_styles()
)]
struct Args {
    /// Input video (any format supported by the supplied FFmpeg build)
    #[arg(short, long, value_name = "FILE")]
    input: PathBuf,

    /// Model directory name beneath the executable-relative models folder
    #[arg(short = 'm', long, default_value = Model::DEFAULT)]
    model: Model,

    /// Output FPS relative to input FPS (1 = same FPS, 2 = double; finite and >0)
    #[arg(short = 'f', long, default_value_t = PlaybackOptions::default().fps_scale, value_name = "FACTOR", value_parser = parse_factor)]
    fps_scale: f64,

    /// Playback speed (finite and >0; 0.5 = half-speed, 2 = double-speed; preserves audio pitch)
    #[arg(short = 's', long, default_value_t = PlaybackOptions::default().speed, value_name = "FACTOR", value_parser = parse_factor)]
    speed: f64,

    /// Non-negative CRF override; default uses source metadata or 20
    #[arg(short = 'c', long, value_name = "CRF", value_parser = encode::parse_crf)]
    crf: Option<String>,

    /// Device selection: auto or comma-separated GPU IDs; use -1 for CPU (e.g. -1,0)
    #[arg(short, long, default_value_t = DeviceSelection::default(), value_name = "GPU", allow_hyphen_values = true)]
    gpu: DeviceSelection,

    /// Load buffering:inference workers per GPU:save buffering
    ///
    /// PROCESS counts follow --gpu order, e.g. 1:2,3:2 for two GPUs.
    /// One PROCESS count applies to all devices. LOAD and SAVE are shared.
    #[arg(
        short = 'j',
        long = "jobs",
        default_value_t = Jobs::default(),
        value_name = Jobs::SYNTAX
    )]
    jobs: Jobs,

    /// Scene cut threshold (0 = every pair is a cut; 1 = detection disabled)
    #[arg(short = 't', long, default_value_t = StreamOptions::default().threshold, value_parser = parse_threshold)]
    threshold: f64,

    #[arg(short = 'y', long = "overwrite", help = format!("Allow replacing an output, waiting if another {APP_NAME} instance is writing it"))]
    overwrite: bool,

    /// Override video encoder; unsupported CRF uses a frame-scaled bitrate
    #[arg(long, value_name = "ENCODER")]
    video_codec: Option<String>,

    /// Override audio handling (default: copy at speed 1; retiming targets source bitrate)
    #[arg(long, value_name = "ENCODER")]
    audio_codec: Option<String>,

    /// Show model, dimensions, frame rates, and device details
    #[arg(short, long)]
    verbose: bool,

    /// Output video filename (container detected from extension)
    #[arg(value_name = "OUTPUT")]
    output: PathBuf,
}

impl Args {
    fn engine_options(&self) -> EngineOptions {
        EngineOptions {
            devices: self.gpu.clone(),
            jobs: Some(self.jobs.clone()),
        }
    }
}

fn parse_factor(value: &str) -> std::result::Result<f64, String> {
    let step = value
        .parse::<f64>()
        .map_err(|_| "factor must be a number".to_string())?;
    PlaybackOptions::validate_factor(step).map_err(|e| e.to_string())?;
    Ok(step)
}

fn parse_threshold(value: &str) -> std::result::Result<f64, String> {
    let threshold = value
        .parse::<f64>()
        .map_err(|_| "threshold must be a number from 0 to 1")?;
    SceneDetector::new(threshold).map_err(|error| error.to_string())?;
    Ok(threshold)
}

fn main() {
    if std::env::args_os().len() == 1 {
        let mut command = Args::command();
        let _ = command.print_help();
        return;
    }

    let args = Args::parse();
    let token = CancellationToken::default();
    let signal = token.clone();
    let interrupted = Arc::new(AtomicBool::new(false));
    let signal_flag = Arc::clone(&interrupted);
    if let Err(error) = ctrlc::set_handler(move || {
        signal_flag.store(true, Ordering::Relaxed);
        signal.cancel();
    }) {
        ui::warning(format_args!("unable to register Ctrl+C handler: {error}"));
    }
    let ui = ui::Ui::new(args.verbose, interrupted.clone());
    let result = process::process(args, token, &ui);
    if interrupted.load(Ordering::Relaxed) {
        ui.finish_interrupted();
        std::process::exit(130);
    }
    if let Err(error) = result {
        ui::error(format_args!("{error:#}"));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cli_defaults_are_supplied_by_clap() {
        let args = Args::try_parse_from([APP_NAME, "-i", "source.mp4", "out.mkv"]).unwrap();
        assert_eq!(args.model, Model::default());
        assert_eq!(args.gpu, DeviceSelection::default());
        let jobs = Jobs::default();
        assert_eq!(args.jobs.load, jobs.load);
        assert_eq!(args.jobs.process, jobs.process);
        assert_eq!(args.jobs.save, jobs.save);
        let matches = <Args as clap::CommandFactory>::command()
            .try_get_matches_from([APP_NAME, "-i", "source.mp4", "out.mkv"])
            .unwrap();
        for name in ["model", "gpu", "jobs", "threshold", "fps_scale", "speed"] {
            assert_eq!(
                matches.value_source(name),
                Some(clap::parser::ValueSource::DefaultValue)
            );
        }
        assert!(!args.verbose);
        assert_eq!(args.fps_scale, PlaybackOptions::default().fps_scale);
        assert_eq!(args.speed, PlaybackOptions::default().speed);
        assert_eq!(args.threshold, StreamOptions::default().threshold);
        assert!(args.crf.is_none());
    }

    #[test]
    fn cargo_identity_and_model_catalog_reach_clap() {
        use clap::CommandFactory;
        let command = Args::command();
        assert_eq!(command.get_name(), APP_NAME);
        assert_eq!(command.get_version(), Some(VERSION));
        assert_eq!(config::temp_prefix(), format!(".{APP_NAME}-"));
        for code in [
            "rife-v4.25",
            "rife-v4.25-lite",
            "rife-v4.25-heavy",
            "rife-v4.26",
            "rife-v4.26-large",
            "rife-v4.28-large",
        ] {
            let args = Args::try_parse_from([APP_NAME, "-i", "source.mp4", "-m", code, "out.mp4"])
                .unwrap();
            assert_eq!(args.model.directory(), code);
        }
        assert!(
            Args::try_parse_from([APP_NAME, "-i", "source.mp4", "-m", "../bad", "out.mp4"])
                .is_err()
        );
    }

    #[test]
    fn help_shows_all_defaults() {
        use clap::CommandFactory;

        for help in [
            Args::command().render_help(),
            Args::command().render_long_help(),
        ] {
            let text = help.to_string();
            let defaults = [
                Model::DEFAULT.to_owned(),
                DeviceSelection::default().to_string(),
                Jobs::default().to_string(),
                StreamOptions::default().threshold.to_string(),
                PlaybackOptions::default().fps_scale.to_string(),
                PlaybackOptions::default().speed.to_string(),
            ];
            for value in &defaults {
                let default = format!("[default: {value}]");
                let expected = defaults.iter().filter(|other| *other == value).count();
                assert_eq!(text.matches(&default).count(), expected, "{text}");
            }
            assert!(text.contains("--threshold <THRESHOLD>"), "{text}");
            assert!(text.contains("--speed <FACTOR>"), "{text}");
            assert!(text.contains("--crf <CRF>"), "{text}");
            assert!(!text.contains("--time-step"), "{text}");
        }
    }

    #[test]
    fn jobs_default_applies_to_each_selected_gpu() {
        for jobs in [None, Some("1:2:2"), Some("1:2,3:2")] {
            let mut values = vec![APP_NAME, "-i", "source.mp4", "-g", "0,1"];
            if let Some(jobs) = jobs {
                values.extend(["-j", jobs]);
            }
            values.push("out.mkv");
            let args = Args::try_parse_from(values).unwrap();
            let config = args.engine_options().jobs.unwrap().for_devices(2).unwrap();
            assert_eq!(config.load, 1);
            assert_eq!(
                config.process,
                if jobs == Some("1:2,3:2") {
                    vec![2, 3]
                } else {
                    vec![2, 2]
                }
            );
            assert_eq!(config.save, 2);
        }
    }

    #[test]
    fn compact_worker_counts_map_to_selected_gpus() {
        for (gpu, jobs, load, process, save) in [
            ("0,1", "1:2,2:2", 1, vec![2, 2], 2),
            ("1,0", "3:2,5:6", 3, vec![2, 5], 6),
            ("0,1,2", "1:2,3,4:2", 1, vec![2, 3, 4], 2),
        ] {
            let args = Args::try_parse_from([
                APP_NAME,
                "-i",
                "source.mp4",
                "-g",
                gpu,
                "-j",
                jobs,
                "out.mkv",
            ])
            .unwrap();
            let options = args.engine_options();
            assert_eq!(options.devices, gpu.parse::<DeviceSelection>().unwrap());
            let config = options
                .jobs
                .unwrap()
                .for_devices(gpu.split(',').count())
                .unwrap();
            assert_eq!(config.load, load);
            assert_eq!(config.process, process);
            assert_eq!(config.save, save);
        }
    }

    #[test]
    fn jobs_rejects_per_device_tuple_lists_at_argument_parsing() {
        let error = Args::try_parse_from([
            APP_NAME,
            "-i",
            "source.mp4",
            "-g",
            "0,1",
            "-j",
            "1:2:2,1:2:2",
            "out.mp4",
        ])
        .unwrap_err();
        assert!(error.to_string().contains(Jobs::SYNTAX));
        let help = <Args as clap::CommandFactory>::command()
            .render_long_help()
            .to_string();
        assert!(
            help.contains(&format!("--jobs <{}>", Jobs::SYNTAX)),
            "{help}"
        );
    }

    #[test]
    fn fps_scale_and_speed_are_independent_positive_finite_factors() {
        for flag in ["-s", "--speed", "-f", "--fps-scale"] {
            for value in ["0.125", "0.5", "1", "2", "8", "1.5"] {
                assert!(
                    Args::try_parse_from([APP_NAME, "-i", "in.mp4", flag, value, "out.mp4"])
                        .is_ok()
                );
            }
            for value in ["0", "NaN", "inf", "-0.1"] {
                assert!(
                    Args::try_parse_from([APP_NAME, "-i", "in.mp4", flag, value, "out.mp4"])
                        .is_err()
                );
            }
        }
        let args =
            Args::try_parse_from([APP_NAME, "-i", "in.mp4", "-f", "2", "-s", "0.5", "out.mp4"])
                .unwrap();
        assert_eq!((args.fps_scale, args.speed), (2.0, 0.5));
        for values in [
            vec!["-n", "24"],
            vec!["--num-frame", "24"],
            vec!["--slow", "2"],
            vec!["--frame-multiplier", "2"],
            vec!["--time-step", "0.5"],
            vec!["-x"],
            vec!["--tta"],
            vec!["-z"],
            vec!["--temporal-tta"],
            vec!["-u"],
            vec!["--uhd"],
        ] {
            let mut all = vec![APP_NAME, "-i", "in.mp4"];
            all.extend(values);
            all.push("out.mp4");
            assert!(Args::try_parse_from(all).is_err());
        }
    }

    #[test]
    fn threshold_flags_accept_only_finite_values_in_range() {
        for flag in ["-t", "--threshold"] {
            for value in ["0", "0.4", "1.0"] {
                let args =
                    Args::try_parse_from([APP_NAME, "-i", "source.mp4", flag, value, "out.mp4"])
                        .unwrap();
                assert_eq!(args.threshold, value.parse::<f64>().unwrap());
            }
            for value in ["NaN", "inf", "-0.1", "1.1", "invalid"] {
                assert!(
                    Args::try_parse_from([APP_NAME, "-i", "source.mp4", flag, value, "out.mp4"])
                        .is_err()
                );
            }
        }
    }

    #[test]
    fn verbose_flags_parse() {
        for flag in ["-v", "--verbose"] {
            let args =
                Args::try_parse_from([APP_NAME, "-i", "source.mp4", flag, "out.mkv"]).unwrap();
            assert!(args.verbose);
        }
    }

    #[test]
    fn help_and_errors_use_requested_palette_without_examples() {
        use clap::CommandFactory;

        let mut command = Args::command();
        for help in [command.render_help(), command.render_long_help()] {
            let text = help.ansi().to_string();
            assert!(!text.contains("Examples:"));
            assert!(text.contains("--verbose"));
            assert!(text.contains("\x1b[1m\x1b[32m"), "{text:?}");
            assert!(text.contains("\x1b[1m\x1b[36m"), "{text:?}");
            assert!(text.contains("\x1b[36m<FILE>"), "{text:?}");
        }
        let error = Args::try_parse_from([APP_NAME, "--invalid-flag"]).unwrap_err();
        let text = error.render().ansi().to_string();
        assert!(text.contains("\x1b[1m\x1b[31m"), "{text:?}");
    }

    #[test]
    fn numeric_cpu_and_default_model_parse() {
        let args =
            Args::try_parse_from([APP_NAME, "-i", "source.mp4", "-g", "-1", "out.mkv"]).unwrap();
        assert_eq!(args.model.directory(), "rife-v4.25");
        assert_eq!(args.gpu, DeviceSelection::Ids(vec![-1]));
    }

    #[test]
    fn accepts_numeric_cpu_sentinel_and_rejects_cpu_alias() {
        let cpu =
            Args::try_parse_from([APP_NAME, "-i", "source.mp4", "--gpu", "-1", "out.mkv"]).unwrap();
        assert_eq!(cpu.gpu, DeviceSelection::Ids(vec![-1]));
        assert!(
            Args::try_parse_from([APP_NAME, "-i", "source.mp4", "--gpu", "cpu", "out.mkv"])
                .is_err()
        );
        let args = Args::try_parse_from([
            APP_NAME,
            "-i",
            "source.mp4",
            "-g",
            "-1,0",
            "-j",
            "1:1,2:2",
            "out.mkv",
        ])
        .unwrap();
        let config = args.engine_options();
        assert_eq!(config.devices, DeviceSelection::Ids(vec![-1, 0]));
        assert_eq!(
            config.jobs.unwrap().for_devices(2).unwrap().process,
            vec![1, 2]
        );
    }

    #[test]
    fn threshold_parser_rejects_nonfinite_values() {
        assert!(parse_threshold("NaN").is_err());
        assert!(parse_threshold("inf").is_err());
    }

    #[test]
    fn crf_accepts_non_negative_values_and_preserves_input() {
        for value in ["0", "20", "51", "52", "1000000", "20.5"] {
            assert_eq!(encode::parse_crf(value).unwrap(), value);
            let args =
                Args::try_parse_from([APP_NAME, "-i", "source.mp4", "--crf", value, "out.mp4"])
                    .unwrap();
            assert_eq!(args.crf.as_deref(), Some(value));
        }
        for value in ["-1", "-0.5", "NaN", ""] {
            assert!(encode::parse_crf(value).is_err(), "accepted {value}");
        }
    }
}
