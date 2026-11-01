// Copyright (C) 2026 Voximir.
// SPDX-License-Identifier: GPL-3.0-only

// Narrow C ABI: all buffers, dimensions and lifetimes are checked by the Rust API.
#include "rife.h"
#include "cancellation.h"
#include "settings.h"
#include "gpu.h"
#include <cstring>
#include <memory>
#include <new>
#include <cmath>

extern "C" {
int rife_device_count() noexcept {
    try { ncnn::create_gpu_instance(); return ncnn::get_gpu_count(); }
    catch (...) { return -1; }
}
int rife_default_device() noexcept {
    try { return ncnn::get_default_gpu_index(); } catch (...) { return -2; }
}
int rife_device_name(int index, char* name, size_t capacity) noexcept {
    try {
        if (rife_cancelled()) return -233;
        if (!name || capacity == 0 || index < 0 || index >= ncnn::get_gpu_count()) return -1;
        const char* source = ncnn::get_gpu_info(index).device_name();
        size_t len = strlen(source);
        if (len >= capacity) return -2;
        memcpy(name, source, len + 1);
        return 0;
    } catch (...) { return -3; }
}
int rife_create(const char* param, const unsigned char* weights, size_t weights_len,
                  int gpu, int padding, int threads, void** out,
                  RifeCancel cancel, const void* cancel_context) noexcept {
    rife_clear_native_error();
    RifeCancellationScope scope(cancel, cancel_context);
    if (!out) return -1;
    *out = nullptr;
    try {
        if (rife_cancelled()) return -233;
        if (!param || !weights || weights_len == 0 || threads < 1 || padding < 1 || padding > RIFE_MAX_PADDING || gpu < -1 || (gpu >= 0 && gpu >= ncnn::get_gpu_count())) return -1;
        std::unique_ptr<RIFE> model(new RIFE(gpu, threads, padding));
        int result = model->load(param, weights, weights_len);
        if (result != 0) return result;
        *out = model.release();
        return 0;
    } catch (...) { return -100; }
}
int rife_process(void* handle, const unsigned char* a, const unsigned char* b,
                   unsigned char* out, int width, int height, float timestep,
                   RifeCancel cancel, const void* cancel_context) noexcept {
    rife_clear_native_error();
    RifeCancellationScope scope(cancel, cancel_context);
    try {
        if (rife_cancelled()) return -233;
        if (!handle || !a || !b || !out || width <= 0 || height <= 0 || !std::isfinite(timestep) || timestep < 0.f || timestep > 1.f) return -1;
        if (timestep == 0.f || timestep == 1.f) {
            memcpy(out, timestep == 0.f ? a : b, size_t(width) * height * RIFE_RGB_CHANNELS);
            return 0;
        }
        ncnn::Mat first(width, height, const_cast<unsigned char*>(a), (size_t)RIFE_RGB_CHANNELS, RIFE_RGB_CHANNELS);
        ncnn::Mat second(width, height, const_cast<unsigned char*>(b), (size_t)RIFE_RGB_CHANNELS, RIFE_RGB_CHANNELS);
        ncnn::Mat result(width, height, out, (size_t)RIFE_RGB_CHANNELS, RIFE_RGB_CHANNELS);
        return static_cast<RIFE*>(handle)->process(first, second, timestep, result);
    } catch (...) { return -100; }
}
void rife_destroy(void* handle) noexcept {
    try { delete static_cast<RIFE*>(handle); } catch (...) {}
}
}
