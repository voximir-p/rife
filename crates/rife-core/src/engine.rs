// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

mod loading;

use crate::{
    CancellationToken, Frame, Result,
    settings::{DEVICE_NAME_CAPACITY, NATIVE_ERROR_CAPACITY},
};
use anyhow::{Context, bail, ensure};
use std::{
    collections::BTreeMap,
    ffi::{CStr, c_char, c_void},
    path::Path,
    ptr::NonNull,
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    thread,
    time::Duration,
};

unsafe extern "C" {
    fn rife_device_count() -> i32;
    fn rife_default_device() -> i32;
    fn rife_device_name(index: i32, name: *mut c_char, capacity: usize) -> i32;
    fn rife_create(
        param: *const c_char,
        weights: *const u8,
        len: usize,
        gpu: i32,
        padding: i32,
        threads: i32,
        out: *mut *mut c_void,
        cancel: unsafe extern "C" fn(*const c_void) -> i32,
        cancel_context: *const c_void,
    ) -> i32;
    fn rife_process(
        handle: *mut c_void,
        a: *const u8,
        b: *const u8,
        out: *mut u8,
        width: i32,
        height: i32,
        timestep: f32,
        cancel: unsafe extern "C" fn(*const c_void) -> i32,
        cancel_context: *const c_void,
    ) -> i32;
    fn rife_destroy(handle: *mut c_void);
    fn rife_error_message(destination: *mut c_char, capacity: usize);
}

// Serialize context creation because the reference shader caches are global.
static INITIALIZATION: Mutex<()> = Mutex::new(());
type GpuGate = Arc<Mutex<()>>;
static GPU_GATES: Mutex<BTreeMap<i32, GpuGate>> = Mutex::new(BTreeMap::new());

fn gpu_gate(device: i32) -> Result<Option<GpuGate>> {
    if device < 0 {
        return Ok(None);
    }
    let mut gates = GPU_GATES
        .lock()
        .map_err(|_| anyhow::anyhow!("GPU gate registry poisoned"))?;
    Ok(Some(Arc::clone(gates.entry(device).or_default())))
}

fn cancellable_lock<'a>(
    lock: &'a Mutex<()>,
    cancel: &CancellationToken,
) -> Result<MutexGuard<'a, ()>> {
    loop {
        cancel.check()?;
        match lock.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(10)),
            Err(TryLockError::Poisoned(_)) => bail!("NCNN execution lock poisoned"),
        }
    }
}

unsafe extern "C" fn cancellation_requested(context: *const c_void) -> i32 {
    // SAFETY: the caller passes a borrowed token that outlives the synchronous
    // native call. The C++ scope keeps the pointer thread-local and never stores
    // it in the model. Reading the token is atomic and cannot panic.
    i32::from(unsafe { &*context.cast::<CancellationToken>() }.is_cancelled())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: i32,
    pub name: String,
}

pub fn devices() -> Result<Vec<Device>> {
    devices_with_cancellation(&CancellationToken::default())
}

fn devices_with_cancellation(cancel: &CancellationToken) -> Result<Vec<Device>> {
    let _guard = cancellable_lock(&INITIALIZATION, cancel)?;
    // SAFETY: native registry initialization is internally synchronized.
    let count = unsafe { rife_device_count() };
    cancel.check()?;
    ensure!(
        count >= 0,
        "could not initialize the Vulkan device registry"
    );
    (0..count)
        .map(|id| {
            let mut name = [0 as c_char; DEVICE_NAME_CAPACITY];
            // SAFETY: name is a writable buffer of capacity bytes. A successful
            // native call writes a null-terminated device name within that buffer.
            ensure!(
                unsafe { rife_device_name(id, name.as_mut_ptr(), name.len()) } == 0,
                "could not query GPU {id}"
            );
            let name = unsafe { CStr::from_ptr(name.as_ptr()) }
                .to_string_lossy()
                .into_owned();
            Ok(Device { id, name })
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceSelection {
    #[default]
    Auto,
    Ids(Vec<i32>),
}

impl std::fmt::Display for DeviceSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Ids(ids) => {
                for (index, id) in ids.iter().enumerate() {
                    if index > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{id}")?;
                }
                Ok(())
            }
        }
    }
}

impl FromStr for DeviceSelection {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        if value.eq_ignore_ascii_case("auto") {
            return Ok(Self::Auto);
        }
        let ids = value
            .split(',')
            .map(|device| {
                let id = device.parse::<i32>().context(
                    "device must be auto or a comma-separated list of GPU IDs; use -1 for CPU",
                )?;
                ensure!(
                    id >= -1,
                    "device IDs must be -1 for CPU or nonnegative GPU IDs"
                );
                Ok(id)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self::Ids(ids))
    }
}

#[derive(Debug, Clone)]
pub struct Jobs {
    /// Shared input buffering budget for all selected devices.
    pub load: usize,
    /// Inference workers in selected-device order.
    pub process: Vec<usize>,
    /// Shared output buffering budget for all selected devices.
    pub save: usize,
}

impl Default for Jobs {
    fn default() -> Self {
        Self {
            load: 1,
            process: vec![2],
            save: 2,
        }
    }
}
impl FromStr for Jobs {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        let parts = value.splitn(4, ':').collect::<Vec<_>>();
        ensure!(parts.len() == 3, "jobs must be {}", Self::SYNTAX);
        let jobs = Self {
            load: parts[0].parse()?,
            process: parts[1]
                .split(',')
                .take(Self::MAX_DEVICES + 1)
                .map(str::parse)
                .collect::<std::result::Result<Vec<_>, _>>()?,
            save: parts[2].parse()?,
        };
        jobs.validate()?;
        Ok(jobs)
    }
}
impl std::fmt::Display for Jobs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:", self.load)?;
        for (index, workers) in self.process.iter().enumerate() {
            if index > 0 {
                f.write_str(",")?;
            }
            write!(f, "{workers}")?;
        }
        write!(f, ":{}", self.save)
    }
}

impl Jobs {
    pub const SYNTAX: &str = "LOAD:PROCESS[,PROCESS...]:SAVE";
    pub const MAX_DEVICES: usize = 8;
    pub const MAX_COUNT: usize = 64;

    /// Apply one worker count to every device, or validate an explicit layout.
    pub fn for_devices(mut self, count: usize) -> Result<Self> {
        ensure!(
            (1..=Self::MAX_DEVICES).contains(&count),
            "select between one and {} devices",
            Self::MAX_DEVICES
        );
        self.validate()?;
        if self.process.len() == 1 {
            self.process.resize(count, self.process[0]);
        }
        ensure!(
            self.process.len() == count,
            "process worker counts must match the selected device count"
        );
        Ok(self)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            (1..=Self::MAX_COUNT).contains(&self.load)
                && (1..=Self::MAX_COUNT).contains(&self.save),
            "shared load/save buffer counts must be between 1 and {}",
            Self::MAX_COUNT
        );
        ensure!(
            !self.process.is_empty()
                && self.process.len() <= Self::MAX_DEVICES
                && self
                    .process
                    .iter()
                    .all(|n| (1..=Self::MAX_COUNT).contains(n)),
            "specify one to {} process worker counts, each between 1 and {}",
            Self::MAX_DEVICES,
            Self::MAX_COUNT
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    pub devices: DeviceSelection,
    pub jobs: Option<Jobs>,
}

/// The original inference failure, distinct from errors during pipeline cleanup.
#[derive(Debug)]
pub struct InferenceError {
    pub device: Device,
    pub width: u32,
    pub height: u32,
    pub status: i32,
    pub diagnostic: String,
}

impl std::fmt::Display for InferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "NCNN interpolation failed on {} at {}x{} (code {})",
            self.device.name, self.width, self.height, self.status
        )?;
        if !self.diagnostic.is_empty() {
            write!(formatter, ": {}", self.diagnostic)?;
        }
        Ok(())
    }
}
impl std::error::Error for InferenceError {}

fn native_diagnostic() -> String {
    let mut buffer = [0 as c_char; NATIVE_ERROR_CAPACITY];
    // SAFETY: the native function copies a bounded, null-terminated thread-local
    // log from the immediately preceding call on this same Rust worker thread.
    unsafe {
        rife_error_message(buffer.as_mut_ptr(), buffer.len());
        CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

pub(crate) struct Network {
    handle: NonNull<c_void>,
    device: Device,
    gpu_gate: Option<GpuGate>,
}
// SAFETY: processing creates per-call NCNN extractors and command buffers;
// immutable CPU model weights are shared, and NCNN's allocator pools use locks.
// Vulkan calls are additionally gated per physical GPU across all model instances,
// avoiding device loss observed with overlapping full-resolution jobs.
unsafe impl Send for Network {}
unsafe impl Sync for Network {}

impl Network {
    pub(crate) fn process_into(
        &self,
        a: &Frame,
        b: &Frame,
        out: &mut Frame,
        timestep: f32,
        cancel: &CancellationToken,
    ) -> Result<()> {
        cancel.check()?;
        ensure!(
            timestep.is_finite() && (0.0..=1.0).contains(&timestep),
            "timestep must be finite and between 0 and 1"
        );
        ensure!(
            a.width() == b.width()
                && a.height() == b.height()
                && a.width() == out.width()
                && a.height() == out.height(),
            "frame dimensions changed during interpolation"
        );
        let _permit = self
            .gpu_gate
            .as_ref()
            .map(|gate| cancellable_lock(gate, cancel))
            .transpose()?;
        cancel.check()?;
        // SAFETY: Frame validates dimensions and exact packed RGB lengths. All
        // inputs live through this synchronous call; out is exclusively borrowed.
        let status = unsafe {
            rife_process(
                self.handle.as_ptr(),
                a.pixels().as_ptr(),
                b.pixels().as_ptr(),
                out.pixels_mut().as_mut_ptr(),
                a.width() as i32,
                a.height() as i32,
                timestep,
                cancellation_requested,
                std::ptr::from_ref(cancel).cast(),
            )
        };
        cancel.check()?;
        if status != 0 {
            return Err(InferenceError {
                device: self.device.clone(),
                width: a.width(),
                height: a.height(),
                status,
                diagnostic: native_diagnostic(),
            }
            .into());
        }
        Ok(())
    }
}
impl Drop for Network {
    fn drop(&mut self) {
        // SAFETY: the last Arc owns this context; no processing calls remain.
        let _permit = self
            .gpu_gate
            .as_ref()
            .map(|gate| gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        unsafe { rife_destroy(self.handle.as_ptr()) };
    }
}

/// Persistent models, shared by a bounded number of inference workers.
pub struct Engine {
    pub(crate) workers: Vec<Arc<Network>>,
    pub(crate) queue_depth: usize,
    selected: Vec<Device>,
}

impl Engine {
    pub fn new(model_directory: impl AsRef<Path>, options: EngineOptions) -> Result<Self> {
        Self::new_with_cancellation(model_directory, options, &CancellationToken::default())
    }

    /// Load models with cooperative cancellation between reads and native layers.
    /// An active driver call or CPU operator must finish before cancellation returns.
    pub fn new_with_cancellation(
        model_directory: impl AsRef<Path>,
        options: EngineOptions,
        cancel: &CancellationToken,
    ) -> Result<Self> {
        let model = loading::ModelData::read(model_directory.as_ref(), cancel)?;
        let cpu_only =
            matches!(&options.devices, DeviceSelection::Ids(ids) if ids.iter().all(|&id| id == -1));
        let available = if cpu_only {
            Vec::new()
        } else {
            devices_with_cancellation(cancel)?
        };
        let ids = match options.devices {
            // SAFETY: devices() initialized NCNN's synchronized device registry.
            DeviceSelection::Auto => vec![unsafe { rife_default_device() }],
            DeviceSelection::Ids(ids) => ids,
        };
        let jobs = options.jobs.unwrap_or_default().for_devices(ids.len())?;
        let _guard = cancellable_lock(&INITIALIZATION, cancel)?;
        let mut workers = Vec::new();
        let mut selected = Vec::new();
        for (index, &id) in ids.iter().enumerate() {
            cancel.check()?;
            let device = if id == -1 {
                Device {
                    id,
                    name: crate::cpu::name(),
                }
            } else {
                available
                    .iter()
                    .find(|device| device.id == id)
                    .cloned()
                    .with_context(|| format!("invalid GPU ID {id}"))?
            };
            let threads = if id == -1 { jobs.process[index] } else { 1 };
            let network = Network::load(&model, device.clone(), threads, cancel)?;
            cancel.check()?;
            // Rust workers supply CPU parallelism too; no OpenMP DLL is needed.
            let count = jobs.process[index];
            for _ in 0..count {
                workers.push(Arc::clone(&network));
            }
            selected.push(device);
        }
        Ok(Self {
            workers,
            queue_depth: jobs.load + jobs.save,
            selected,
        })
    }

    pub fn selected_devices(&self) -> &[Device] {
        &self.selected
    }

    pub fn interpolate_pair(&self, first: &Frame, second: &Frame, timestep: f32) -> Result<Frame> {
        let mut result = Frame::recycled(first.width(), first.height(), Vec::new())?;
        self.workers[0].process_into(
            first,
            second,
            &mut result,
            timestep,
            &CancellationToken::default(),
        )?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    #[test]
    fn displayed_jobs_and_device_selections_round_trip() {
        for value in ["auto", "-1", "-1,0,1", "1,0"] {
            let selection = value.parse::<DeviceSelection>().unwrap();
            assert_eq!(
                selection.to_string().parse::<DeviceSelection>().unwrap(),
                selection
            );
        }
        for value in [Jobs::default().to_string(), "2:3,4:5".into()] {
            let jobs = value.parse::<Jobs>().unwrap();
            let restored = jobs.to_string().parse::<Jobs>().unwrap();
            assert_eq!(
                (restored.load, restored.process, restored.save),
                (jobs.load, jobs.process, jobs.save)
            );
        }
    }

    #[test]
    fn shared_worker_layout_preserves_counts_and_rejects_mismatches() {
        let defaults = Jobs::default();
        let expanded = defaults.clone().for_devices(3).unwrap();
        assert_eq!(
            (expanded.load, expanded.save),
            (defaults.load, defaults.save)
        );
        assert_eq!(expanded.process, vec![defaults.process[0]; 3]);
        assert_eq!(
            "1:2,3:4"
                .parse::<Jobs>()
                .unwrap()
                .for_devices(2)
                .unwrap()
                .process,
            vec![2, 3]
        );
        assert!("1:2,3:4".parse::<Jobs>().unwrap().for_devices(3).is_err());
        for count in [0, Jobs::MAX_DEVICES + 1] {
            assert!(Jobs::default().for_devices(count).is_err());
        }
    }

    #[test]
    fn gpu_gates_are_shared_per_device_and_cpu_has_no_gate() {
        let first = gpu_gate(0).unwrap().unwrap();
        let same = gpu_gate(0).unwrap().unwrap();
        let second = gpu_gate(1).unwrap().unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(gpu_gate(-1).unwrap().is_none());
        let held = first.lock().unwrap();
        assert!(same.try_lock().is_err());
        assert!(second.try_lock().is_ok());
        drop(held);
    }

    #[test]
    fn waiting_for_a_gpu_gate_is_cancellable() {
        let gate = Arc::new(Mutex::new(()));
        let held = gate.lock().unwrap();
        let cancel = CancellationToken::default();
        let signal = cancel.clone();
        let waiting = Arc::clone(&gate);
        let (tx, rx) = std::sync::mpsc::channel();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = cancellable_lock(&waiting, &signal).map(|_| ());
            tx.send(result.is_err()).unwrap();
        });
        started_rx.recv().unwrap();
        cancel.cancel();
        let result = rx.recv_timeout(Duration::from_secs(10));
        drop(held);
        worker.join().unwrap();
        assert!(result.unwrap());
    }

    #[test]
    fn native_diagnostics_are_thread_local_and_bounded() {
        unsafe extern "C" {
            fn rife_record_native_error(format: *const c_char, ...);
        }
        let barrier = Arc::new(std::sync::Barrier::new(4));
        thread::scope(|scope| {
            for index in 0..4 {
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    let message = CString::new(format!("worker {index}")).unwrap();
                    // SAFETY: both C strings remain valid for the synchronous call.
                    unsafe {
                        rife_record_native_error(c"%s".as_ptr(), message.as_ptr());
                    }
                    barrier.wait();
                    assert_eq!(native_diagnostic(), format!("worker {index}"));
                });
            }
        });
        thread::spawn(|| {
            let message = CString::new("x".repeat(NATIVE_ERROR_CAPACITY * 2)).unwrap();
            // SAFETY: valid format and string pointers live through the call.
            unsafe {
                rife_record_native_error(c"%s".as_ptr(), message.as_ptr());
            }
            assert_eq!(native_diagnostic(), "x".repeat(NATIVE_ERROR_CAPACITY - 1));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn cancelled_initialization_returns_before_file_access() {
        let cancel = CancellationToken::default();
        cancel.cancel();
        let error =
            Engine::new_with_cancellation("missing-model", EngineOptions::default(), &cancel)
                .err()
                .expect("initialization must be cancelled");
        assert_eq!(error.to_string(), "cancelled");
    }

    #[test]
    fn parses_shared_buffer_counts_and_ordered_per_device_workers() {
        for (value, load, process, save) in [
            ("1:2,2:2", 1, vec![2, 2], 2),
            ("1:2,5,8:3", 1, vec![2, 5, 8], 3),
            ("64:1,2:64", 64, vec![1, 2], 64),
        ] {
            let jobs = value.parse::<Jobs>().unwrap();
            assert_eq!((jobs.load, jobs.process, jobs.save), (load, process, save));
        }
        let counts = vec![Jobs::MAX_COUNT.to_string(); Jobs::MAX_DEVICES].join(",");
        let jobs = format!("{}:{counts}:{}", Jobs::MAX_COUNT, Jobs::MAX_COUNT)
            .parse::<Jobs>()
            .unwrap();
        assert_eq!((jobs.load, jobs.save), (Jobs::MAX_COUNT, Jobs::MAX_COUNT));
        assert_eq!(jobs.process, vec![Jobs::MAX_COUNT; Jobs::MAX_DEVICES]);
    }

    #[test]
    fn rejects_tuple_lists_and_malformed_worker_lists() {
        for value in [
            "1:2:2,1:2:2",
            "1:2:2,",
            ",1:2:2",
            "1:2:2,1:2",
            "1:2:2,1:2,3:2",
            "1:2,:2",
            "1:,2:2",
            "1:2,,3:2",
            "1::2",
            "1:0,2:2",
            "0:2:2",
            "1:2:0",
            "65:2:2",
            "1:2:65",
            "1:65:2",
            "1:2,bad:2",
            "1:2",
            "1:2:3:4",
        ] {
            assert!(value.parse::<Jobs>().is_err(), "accepted {value}");
        }
        let counts = ["2"; Jobs::MAX_DEVICES + 1].join(",");
        assert!(format!("1:{counts}:2").parse::<Jobs>().is_err());
    }

    #[test]
    fn parses_device_selections() {
        assert_eq!(
            "AUTO".parse::<DeviceSelection>().unwrap(),
            DeviceSelection::Auto
        );
        assert_eq!(
            "-1,1".parse::<DeviceSelection>().unwrap(),
            DeviceSelection::Ids(vec![-1, 1])
        );
        for value in ["", "cpu", "CPU", "-2", "1,", "bad", "auto,0"] {
            assert!(value.parse::<DeviceSelection>().is_err());
        }
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod native_tests;
