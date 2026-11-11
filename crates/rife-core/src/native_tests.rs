// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Opt-in inference checks with explicitly supplied model weights.
use super::{DeviceSelection, Engine, EngineOptions, Frame, Jobs};
use crate::Model;
use std::{env, fs, path::PathBuf};

fn model() -> PathBuf {
    env::var_os("RIFE_TEST_MODEL_DIR")
        .map(PathBuf::from)
        .expect("set RIFE_TEST_MODEL_DIR to an NCNN model directory")
}
fn options() -> EngineOptions {
    EngineOptions {
        devices: env::var("RIFE_TEST_GPU")
            .unwrap_or_else(|_| "-1".into())
            .parse()
            .expect("RIFE_TEST_GPU must be auto or comma-separated device IDs"),
        jobs: Some(Jobs {
            load: 1,
            process: vec![1],
            save: 1,
        }),
    }
}

#[test]
#[ignore = "requires RIFE_TEST_MODEL_DIR; CPU by default, optional RIFE_TEST_GPU"]
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
#[ignore = "requires RIFE_TEST_MODEL_DIR; checks native model loading on CPU"]
fn rejects_truncated_weights_without_escaping_the_native_boundary() {
    let temporary = tempfile::tempdir().unwrap();
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
    use std::{
        ffi::c_void,
        sync::atomic::{AtomicUsize, Ordering},
    };
    let engine = Engine::new(
        model(),
        EngineOptions {
            devices: DeviceSelection::Ids(vec![-1]),
            ..options()
        },
    )
    .unwrap();
    let first = Frame::new(64, 64, [100, 50, 25].repeat(4096)).unwrap();
    let second = Frame::new(64, 64, [25, 50, 100].repeat(4096)).unwrap();
    let mut output = vec![0; first.pixels().len()];
    let checks = AtomicUsize::new(0);
    unsafe extern "C" fn cancel_after_checks(context: *const c_void) -> i32 {
        // SAFETY: the synchronous call below borrows this atomic until return.
        let checks = unsafe { &*context.cast::<AtomicUsize>() };
        i32::from(checks.fetch_add(1, Ordering::Relaxed) >= 8)
    }
    // SAFETY: the engine owns a live CPU model; all frames are equal-sized RGB8
    // buffers, output is exclusive, and the callback context outlives this call.
    let status = unsafe {
        super::rife_process(
            engine.workers[0].handle.as_ptr(),
            first.pixels().as_ptr(),
            second.pixels().as_ptr(),
            output.as_mut_ptr(),
            64,
            64,
            0.5,
            cancel_after_checks,
            std::ptr::from_ref(&checks).cast(),
        )
    };
    assert!(
        checks.load(Ordering::Relaxed) > 8,
        "native cancellation checkpoints were not reached"
    );
    assert_ne!(status, 0, "cancelled native inference succeeded");
    // The scoped native callback must not poison subsequent model calls.
    assert!(engine.interpolate_pair(&first, &second, 0.5).is_ok());
}

#[test]
#[ignore = "requires RIFE_TEST_MODEL_DIR; checks graph-derived padding on the selected device"]
fn rectangular_pixels_match_explicitly_padded_inference() {
    let padding = crate::model::validate(&fs::read(model().join(Model::PARAM_FILE)).unwrap())
        .unwrap()
        .padding as usize;
    let engine = Engine::new(model(), options()).unwrap();
    let (w, h) = (65usize, 49usize);
    let (pw, ph) = (w.div_ceil(padding) * padding, h.div_ceil(padding) * padding);
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
            "row {y}, alignment {padding}"
        );
    }
}
