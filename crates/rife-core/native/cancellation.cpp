// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

#include "cancellation.h"
#include "settings.h"
#include <cstdarg>
#include <cstdio>
#include <cstring>
namespace {
thread_local RifeCancel current_callback = nullptr;
thread_local const void* current_context = nullptr;
}
bool rife_cancelled() noexcept {
    return current_callback && current_callback(current_context) != 0;
}
RifeCancellationScope::RifeCancellationScope(RifeCancel callback, const void* context) noexcept
    : previous_callback(current_callback), previous_context(current_context) {
    current_callback = callback;
    current_context = context;
}
RifeCancellationScope::~RifeCancellationScope() {
    current_callback = previous_callback;
    current_context = previous_context;
}

namespace {
thread_local char native_error[RIFE_NATIVE_ERROR_CAPACITY] = {};
thread_local size_t native_error_size = 0;
}
void rife_clear_native_error() noexcept {
    native_error_size = 0;
    native_error[0] = 0;
}
extern "C" void rife_record_native_error(const char* format, ...) noexcept {
    if (!format || native_error_size >= sizeof(native_error) - 1) return;
    if (native_error_size) native_error[native_error_size++] = '\n';
    const size_t available = sizeof(native_error) - native_error_size;
    va_list args;
    va_start(args, format);
    const int written = vsnprintf(native_error + native_error_size, available, format, args);
    va_end(args);
    if (written > 0) native_error_size += static_cast<size_t>(written) < available ? static_cast<size_t>(written) : available - 1;
    native_error[native_error_size] = 0;
}
extern "C" void rife_error_message(char* destination, size_t capacity) noexcept {
    if (!destination || !capacity) return;
    const size_t copied = native_error_size < capacity - 1 ? native_error_size : capacity - 1;
    memcpy(destination, native_error, copied);
    destination[copied] = 0;
}
