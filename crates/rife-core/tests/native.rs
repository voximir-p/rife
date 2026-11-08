// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Opt-in tests for a machine with compatible models and Vulkan drivers.
use rife_core::{DeviceSelection, Engine, EngineOptions, Frame, Jobs, Model};
use std::{env, fs, path::PathBuf};

fn model() -> PathBuf {
    env::var_os("RIFE_TEST_MODEL_DIR")
        .map(PathBuf::from)
        .expect("set RIFE_TEST_MODEL_DIR to an NCNN model directory")
}
fn options() -> EngineOptions {
    EngineOptions {
        jobs: Some(Jobs {
            load: 1,
            process: vec![1],
            save: 1,
        }),
        ..EngineOptions::default()
    }
}

#[test]
#[ignore = "requires RIFE_TEST_MODEL_DIR and Vulkan"]
fn persistent_models_preserve_rgb_and_vary_timesteps() {
    let red = Frame::new(64, 64, [230, 20, 10].repeat(4096)).unwrap();
    let blue = Frame::new(64, 64, [10, 20, 230].repeat(4096)).unwrap();
    let normal = Engine::new(model(), options()).unwrap();
    let red_out = normal.interpolate_pair(&red, &red, 0.5).unwrap();
    assert!(red_out.pixels()[0] > red_out.pixels()[2]);
    let expected = normal.interpolate_pair(&red, &blue, 0.5).unwrap();
    let second_model = Engine::new(model(), options()).unwrap();
    let blue_out = second_model.interpolate_pair(&blue, &blue, 0.5).unwrap();
    assert!(blue_out.pixels()[2] > blue_out.pixels()[0]);
    let result = normal.interpolate_pair(&red, &blue, 0.5).unwrap();
    assert_eq!(result.pixels(), expected.pixels());
    assert_eq!(
        normal.interpolate_pair(&red, &blue, 0.0).unwrap().pixels(),
        red.pixels()
    );
    assert_eq!(
        normal.interpolate_pair(&red, &blue, 1.0).unwrap().pixels(),
        blue.pixels()
    );
    for bad in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        assert!(normal.interpolate_pair(&red, &blue, bad).is_err());
    }
    let quarter = normal.interpolate_pair(&red, &blue, 0.25).unwrap();
    let three_quarters = normal.interpolate_pair(&red, &blue, 0.75).unwrap();
    assert_ne!(quarter.pixels(), three_quarters.pixels());
    let odd = Frame::new(65, 49, [30, 60, 90].repeat(65 * 49)).unwrap();
    let odd_out = normal.interpolate_pair(&odd, &odd, 0.25).unwrap();
    assert_eq!((odd_out.width(), odd_out.height()), (65, 49));
    assert!(odd_out.pixels()[2] > odd_out.pixels()[0]);
    let mismatch = Frame::new(32, 32, vec![0; 3072]).unwrap();
    assert!(normal.interpolate_pair(&red, &mismatch, 0.5).is_err());
}

#[test]
#[ignore = "requires RIFE_TEST_MODEL_DIR and Vulkan"]
fn rejects_truncated_weights_without_escaping_the_native_boundary() {
    let temporary =
        tempfile::tempdir_in(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"))
            .unwrap();
    fs::copy(
        model().join(Model::PARAM_FILE),
        temporary.path().join(Model::PARAM_FILE),
    )
    .unwrap();
    fs::write(temporary.path().join(Model::WEIGHTS_FILE), [0; 8]).unwrap();
    let options = EngineOptions {
        devices: DeviceSelection::Ids(vec![-1]),
        ..options()
    };
    assert!(Engine::new(temporary.path(), options).is_err());
}

#[test]
#[ignore = "requires RIFE_TEST_MODEL_DIR; exercises native CPU cancellation"]
fn cpu_inference_cancels_between_layers_and_model_remains_usable() {
    use rife_core::{CancellationToken, StreamOptions};
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let mut options = options();
    options.devices = "-1".parse().unwrap();
    let engine = Engine::new(model(), options).unwrap();
    let first = Frame::new(1280, 720, [100, 50, 25].repeat(1280 * 720)).unwrap();
    let second = Frame::new(1280, 720, [25, 50, 100].repeat(1280 * 720)).unwrap();
    let cancel = CancellationToken::default();
    let started = Instant::now();
    thread::scope(|scope| {
        let cancel = &cancel;
        scope.spawn(move || {
            thread::sleep(Duration::from_millis(100));
            cancel.cancel();
        });
        let result = engine.interpolate_stream(
            [Ok(first), Ok(second)],
            StreamOptions {
                threshold: 1.0,
                // Explicitly request interpolation: the default now copies
                // the input frame grid and can finish before cancellation.
                schedule: Some(rife_core::FrameSchedule::for_count(2, 4).unwrap()),
                ..StreamOptions::default()
            },
            cancel,
            |_| Ok(()),
            |_| {},
        );
        assert!(result.is_err());
    });
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "cancelled CPU inference took {:?}",
        started.elapsed()
    );
    // Scoped cancellation must not poison later calls on this same model/thread.
    let red = Frame::new(64, 64, [230, 20, 10].repeat(4096)).unwrap();
    assert!(engine.interpolate_pair(&red, &red, 0.5).is_ok());
}

#[test]
#[ignore = "requires RIFE_TEST_MODEL_DIR; checks model-specific padding on CPU and Vulkan"]
fn rectangular_pixels_match_explicitly_padded_inference() {
    for device in ["-1", "auto"] {
        let engine = Engine::new(
            model(),
            EngineOptions {
                devices: device.parse().unwrap(),
                ..options()
            },
        )
        .unwrap();
        let (w, h, pw, ph) = (65usize, 49usize, 128usize, 64usize);
        let mut pixels = vec![0; w * h * 3];
        let mut padded = vec![0; pw * ph * 3];
        for y in 0..h {
            for x in 0..w {
                let rgb = [(x * 3) as u8, (y * 4) as u8, 100];
                pixels[(y * w + x) * 3..(y * w + x + 1) * 3].copy_from_slice(&rgb);
                padded[(y * pw + x) * 3..(y * pw + x + 1) * 3].copy_from_slice(&rgb);
            }
        }
        let input = Frame::new(w as u32, h as u32, pixels).unwrap();
        let padded = Frame::new(pw as u32, ph as u32, padded).unwrap();
        let actual = engine.interpolate_pair(&input, &input, 0.25).unwrap();
        let expected = engine.interpolate_pair(&padded, &padded, 0.25).unwrap();
        for y in 0..h {
            assert_eq!(
                &actual.pixels()[y * w * 3..(y + 1) * w * 3],
                &expected.pixels()[y * pw * 3..(y * pw + w) * 3],
                "{device} row {y}"
            );
        }
    }
}
