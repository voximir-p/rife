// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

#pragma once

// Shared ABI and model contract values. build.rs generates the matching Rust
// constants from this header so the native boundary has one source of truth.
#define RIFE_RGB_CHANNELS 3
#define RIFE_MAX_PADDING 1024
#define RIFE_NATIVE_ERROR_CAPACITY 4096
#define RIFE_DEVICE_NAME_CAPACITY 256
#define RIFE_INPUT_BLOB_0 "in0"
#define RIFE_INPUT_BLOB_1 "in1"
#define RIFE_INPUT_BLOB_2 "in2"
#define RIFE_OUTPUT_BLOB "out0"
