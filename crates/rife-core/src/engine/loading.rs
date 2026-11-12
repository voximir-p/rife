// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

//! Cancellable model reads and checked native network construction.
use super::{
    Device, Network, cancellable_lock, cancellation_requested, gpu_gate, native_diagnostic,
    rife_create,
};
use crate::{CancellationToken, Model, Result};
use anyhow::{Context, bail, ensure};
use std::{ffi::CString, fs, io::Read, path::Path, ptr::NonNull, sync::Arc};

pub(super) struct ModelData {
    parameters: CString,
    weights: Vec<u8>,
    padding: u32,
}
impl ModelData {
    pub(super) fn read(directory: &Path, cancel: &CancellationToken) -> Result<Self> {
        cancel.check()?;
        let parameters = read_model_file(&directory.join(Model::PARAM_FILE), cancel)
            .context("could not read NCNN model parameters")?;
        let metadata = crate::model::validate(&parameters)?;
        let parameters =
            CString::new(parameters).context("NCNN parameter file contains a null byte")?;
        let weights = read_model_file(&directory.join(Model::WEIGHTS_FILE), cancel)
            .context("could not read NCNN model weights")?;
        ensure!(!weights.is_empty(), "empty NCNN model weights");
        Ok(Self {
            parameters,
            weights,
            padding: metadata.padding,
        })
    }
}

impl Network {
    pub(super) fn load(
        model: &ModelData,
        device: Device,
        threads: usize,
        cancel: &CancellationToken,
    ) -> Result<Arc<Self>> {
        let id = device.id;
        let gate = gpu_gate(id)?;
        let loading_permit = gate
            .as_ref()
            .map(|gate| cancellable_lock(gate, cancel))
            .transpose()?;
        let mut handle = std::ptr::null_mut();
        // SAFETY: buffers are valid for the call. The native loader copies
        // weights using a bounded reader, and never retains Rust pointers.
        let status = unsafe {
            rife_create(
                model.parameters.as_ptr(),
                model.weights.as_ptr(),
                model.weights.len(),
                id,
                model.padding as i32,
                threads as i32,
                &mut handle,
                cancellation_requested,
                std::ptr::from_ref(cancel).cast(),
            )
        };
        drop(loading_permit);
        if status != 0 {
            cancel.check()?;
            bail!(
                "could not load RIFE on {} (code {status}): {}",
                device.name,
                native_diagnostic()
            );
        }
        Ok(Arc::new(Network {
            handle: NonNull::new(handle).context("NCNN returned a null context")?,
            device,
            gpu_gate: gate,
        }))
    }
}

fn read_model_file(path: &Path, cancel: &CancellationToken) -> Result<Vec<u8>> {
    cancel.check()?;
    let mut file = fs::File::open(path)?;
    let mut data = Vec::new();
    data.try_reserve_exact(
        usize::try_from(file.metadata()?.len()).context("model file is too large")?,
    )?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancel.check()?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(data);
        }
        data.extend_from_slice(&buffer[..read]);
    }
}
