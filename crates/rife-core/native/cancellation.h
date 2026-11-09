// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

#pragma once

// Called synchronously on the Rust caller's thread. Neither pointer escapes the
// create/process call; the callback only reads a Rust atomic cancellation token.
using RifeCancel = int (*)(const void*);
bool rife_cancelled() noexcept;
class RifeCancellationScope {
public:
    RifeCancellationScope(RifeCancel callback, const void* context) noexcept;
    ~RifeCancellationScope();
    RifeCancellationScope(const RifeCancellationScope&) = delete;
    RifeCancellationScope& operator=(const RifeCancellationScope&) = delete;
private:
    RifeCancel previous_callback;
    const void* previous_context;
};

// Bounded thread-local diagnostics. Only an isolated CLI worker opts into
// stderr mirroring; embedding applications retain the quiet default.
extern "C" void rife_record_native_error(const char* format, ...) noexcept;
void rife_clear_native_error() noexcept;
