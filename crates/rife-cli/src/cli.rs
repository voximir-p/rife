// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Command-line declarations, parsing and option resolution.
use crate::config::{APP_NAME, VERSION};
use crate::{config, encoder_options};
use clap::{
    CommandFactory, Parser,
    builder::styling::{AnsiColor, Styles as ClapStyles},
};
use rife_core::{
    DeviceSelection, EngineOptions, Jobs, Model, PlaybackOptions, SceneDetector, StreamOptions,
};
use std::path::PathBuf;

pub(crate) fn parse() -> Option<Args> {
    if std::env::args_os().len() == 1 {
        let _ = Args::command().print_help();
        None
    } else {
        Some(Args::parse())
    }
}

pub(crate) fn make_styles() -> ClapStyles {
    ClapStyles::styled()
        .header(config::SUCCESS_STYLE)
        .usage(config::SUCCESS_STYLE)
        .literal(config::ACTIVE_STYLE)
        .placeholder(config::ACTIVE_STYLE)
        .error(AnsiColor::Red.on_default().bold())
}

#[derive(Debug, Parser)]
#[command(
    name = APP_NAME,
    version = VERSION,
    about = env!("CARGO_PKG_DESCRIPTION"),
    long_about = "Streaming video interpolation with embedded NCNN/Vulkan. Download FFmpeg and FFprobe into an adjacent ffmpeg/ directory, and compatible model files into an adjacent models/ directory.",
    styles = make_styles()
)]
pub(crate) struct Args {
    /// Input video (any format supported by the supplied FFmpeg build)
    #[arg(short, long, value_name = "FILE")]
    pub(crate) input: PathBuf,

    /// Model directory name beneath the executable-relative models folder
    #[arg(short = 'm', long, default_value = Model::DEFAULT)]
    pub(crate) model: Model,

    /// Output FPS relative to input FPS (1 = same FPS, 2 = double; finite and >0)
    #[arg(short = 'f', long, default_value_t = PlaybackOptions::default().fps_scale, value_name = "FACTOR", value_parser = parse_factor)]
    pub(crate) fps_scale: f64,

    /// Playback speed (finite and >0; 0.5 = half-speed, 2 = double-speed; preserves audio pitch)
    #[arg(short = 's', long, default_value_t = PlaybackOptions::default().speed, value_name = "FACTOR", value_parser = parse_factor)]
    pub(crate) speed: f64,

    /// Extra FFmpeg output options; repeatable (e.g. -e -crf=23 or -e "-crf 12 -b:a 10k")
    #[arg(
        short = 'e',
        long = "extra-encoder-option",
        value_name = "OPTIONS",
        allow_hyphen_values = true
    )]
    pub(crate) extra_encoder_options: Vec<encoder_options::EncoderOptions>,

    /// Device selection: auto or comma-separated GPU IDs; use -1 for CPU (e.g. -1,0)
    #[arg(short, long, default_value_t = DeviceSelection::default(), value_name = "GPU", allow_hyphen_values = true)]
    pub(crate) gpu: DeviceSelection,

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
    pub(crate) jobs: Jobs,

    /// Scene cut threshold (0 = every pair is a cut; 1 = detection disabled)
    #[arg(short = 't', long, default_value_t = StreamOptions::default().threshold, value_parser = parse_threshold)]
    pub(crate) threshold: f64,

    #[arg(short = 'y', long = "overwrite", help = format!("Allow replacing an output, waiting if another {APP_NAME} instance is writing it"))]
    pub(crate) overwrite: bool,

    /// Show model, dimensions, frame rates, and device details
    #[arg(short, long)]
    pub(crate) verbose: bool,

    /// Output video filename or image sequence pattern (e.g. frame_%08d.png)
    #[arg(value_name = "OUTPUT")]
    pub(crate) output: PathBuf,
}

impl Args {
    pub(crate) fn extra_encoder_args(&self) -> impl Iterator<Item = &str> {
        self.extra_encoder_options
            .iter()
            .flat_map(encoder_options::EncoderOptions::arguments)
    }

    fn encoder_option(&self, names: &[&str]) -> Option<&str> {
        self.extra_encoder_options
            .iter()
            .flat_map(encoder_options::EncoderOptions::entries)
            .filter(|(name, _)| names.contains(name))
            .last()
            .and_then(|(_, value)| value)
    }

    pub(crate) fn video_codec(&self) -> Option<&str> {
        self.encoder_option(&[
            "-c",
            "-codec",
            "-vcodec",
            "-c:v",
            "-codec:v",
            "-c:v:0",
            "-codec:v:0",
            "-c:0",
            "-codec:0",
        ])
    }

    pub(crate) fn audio_codec(&self) -> Option<&str> {
        self.encoder_option(&["-c", "-codec", "-acodec", "-c:a", "-codec:a"])
    }

    pub(crate) fn extra_video_crf(&self) -> Option<&str> {
        self.encoder_option(&["-crf", "-crf:v", "-crf:v:0", "-crf:0"])
    }

    pub(crate) fn extra_video_rate_control(&self) -> bool {
        self.extra_encoder_options
            .iter()
            .flat_map(encoder_options::EncoderOptions::entries)
            .any(|(name, _)| {
                matches!(
                    name,
                    "-b" | "-b:v"
                        | "-b:v:0"
                        | "-b:0"
                        | "-vb"
                        | "-q"
                        | "-q:v"
                        | "-q:v:0"
                        | "-q:0"
                        | "-qscale"
                        | "-qscale:v"
                        | "-qscale:v:0"
                        | "-qp"
                        | "-qp:v"
                        | "-qp:v:0"
                )
            })
    }

    pub(crate) fn engine_options(&self) -> EngineOptions {
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
        assert!(args.extra_encoder_options.is_empty());
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
            assert!(text.contains("--extra-encoder-option <OPTIONS>"), "{text}");
            assert!(!text.contains("--crf"), "{text}");
            assert!(!text.contains("--video-codec"), "{text}");
            assert!(!text.contains("--audio-codec"), "{text}");
            assert!(!text.contains("--time-step"), "{text}");
        }
    }

    #[test]
    fn compact_worker_counts_map_to_selected_gpus() {
        for (gpu, jobs, load, process, save) in [
            ("0,1", None, 1, vec![2, 2], 2),
            ("0,1", Some("1:2:2"), 1, vec![2, 2], 2),
            ("1,0", Some("3:2,5:6"), 3, vec![2, 5], 6),
            ("-1,0", Some("1:1,2:2"), 1, vec![1, 2], 2),
            ("0,1,2", Some("1:2,3,4:2"), 1, vec![2, 3, 4], 2),
        ] {
            let mut values = vec![APP_NAME, "-i", "source.mp4", "-g", gpu];
            if let Some(jobs) = jobs {
                values.extend(["-j", jobs]);
            }
            values.push("out.mkv");
            let args = Args::try_parse_from(values).unwrap();
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
    fn help_and_errors_use_requested_palette_without_examples() {
        use clap::CommandFactory;

        let mut command = Args::command();
        for help in [command.render_help(), command.render_long_help()] {
            let text = help.ansi().to_string();
            assert!(!text.contains("Examples:"));
            assert!(text.contains("--verbose"));
            assert!(text.contains("\x1b[1m\x1b[92m"), "{text:?}");
            assert!(text.contains("\x1b[1m\x1b[96m"), "{text:?}");
            assert!(text.contains("\x1b[1m\x1b[96m<FILE>"), "{text:?}");
        }
        let error = Args::try_parse_from([APP_NAME, "--invalid-flag"]).unwrap_err();
        let text = error.render().ansi().to_string();
        assert!(text.contains("\x1b[1m\x1b[31m"), "{text:?}");
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
    }

    #[test]
    fn extra_encoder_options_accept_both_forms_and_repeat_in_order() {
        let args = Args::try_parse_from([
            APP_NAME,
            "-i",
            "source.mp4",
            "-e",
            "-crf=23",
            "--extra-encoder-option",
            "-crf 12 -a:b 10k",
            "-e",
            "-metadata title='hello world'",
            "out.mp4",
        ])
        .unwrap();
        assert_eq!(
            args.extra_encoder_args().collect::<Vec<_>>(),
            [
                "-crf",
                "23",
                "-crf",
                "12",
                "-a:b",
                "10k",
                "-metadata",
                "title=hello world",
            ]
        );
        assert_eq!(args.extra_video_crf(), Some("12"));
        for flag in ["-c", "--crf", "--video-codec", "--audio-codec"] {
            assert!(
                Args::try_parse_from([APP_NAME, "-i", "source.mp4", flag, "23", "out.mp4"])
                    .is_err()
            );
        }
        assert!(
            Args::try_parse_from([APP_NAME, "-i", "source.mp4", "-e", "-crf NaN", "out.mp4"])
                .is_ok()
        );
    }
}
