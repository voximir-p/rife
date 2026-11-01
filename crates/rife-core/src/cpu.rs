// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

// Query hardware metadata directly, without launching a helper process.
pub(crate) fn name() -> String {
    platform_name()
        .map(|name| name.trim_matches('\0').trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "CPU".into())
}

#[cfg(windows)]
fn platform_name() -> Option<String> {
    use std::ffi::c_void;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            key: *mut c_void,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            size: *mut u32,
        ) -> i32;
    }

    let subkey: Vec<u16> = "HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0\0"
        .encode_utf16()
        .collect();
    let value: Vec<u16> = "ProcessorNameString\0".encode_utf16().collect();
    let mut buffer = [0u16; 1024];
    let mut bytes = std::mem::size_of_val(&buffer) as u32;
    // SAFETY: HKEY_LOCAL_MACHINE is a predefined, sign-extended pseudo-handle.
    // Both names are NUL-terminated; buffer and size are writable for the call.
    // RRF_RT_REG_SZ restricts the result to a terminated UTF-16 string.
    let status = unsafe {
        RegGetValueW(
            (0x8000_0002u32 as i32 as isize) as *mut c_void,
            subkey.as_ptr(),
            value.as_ptr(),
            0x0000_0002,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != 0 {
        return None;
    }
    let end = buffer.iter().position(|&unit| unit == 0)?;
    Some(String::from_utf16_lossy(&buffer[..end]))
}

#[cfg(target_os = "linux")]
fn platform_name() -> Option<String> {
    let info = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    // Prefer the processor model over a board name on ARM systems.
    ["model name", "Hardware", "Model"]
        .into_iter()
        .find_map(|key| {
            info.lines().find_map(|line| {
                let (label, value) = line.split_once(':')?;
                (label.trim() == key && !value.trim().is_empty()).then(|| value.trim().to_owned())
            })
        })
}

#[cfg(target_os = "macos")]
fn platform_name() -> Option<String> {
    use std::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            old: *mut c_void,
            old_len: *mut usize,
            new: *const c_void,
            new_len: usize,
        ) -> c_int;
    }

    let mut buffer = [0u8; 512];
    let mut len = buffer.len();
    // SAFETY: the key is NUL-terminated; buffer/len are writable and bounded.
    // A null new pointer requests a read only query on Intel and Apple Silicon.
    let status = unsafe {
        sysctlbyname(
            c"machdep.cpu.brand_string".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null(),
            0,
        )
    };
    if status != 0 || len > buffer.len() {
        return None;
    }
    Some(String::from_utf8_lossy(&buffer[..len]).into_owned())
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn platform_name() -> Option<String> {
    None
}
