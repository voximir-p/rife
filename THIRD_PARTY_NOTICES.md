# Copyright and third-party notices

Copyright (C) 2026 Voximir.

The Rife project code is licensed under the GNU General Public License version
3 only; see [LICENSE](LICENSE). Release archives include the corresponding
source code, this notice, native component license files, and the generated
Rust dependency license report.

## Code included in the executable

- **RIFE reference implementation** — selected native inference code and
  shaders adapted from [Erlonealpha/rife-ncnn-vulkan-ex](https://github.com/Erlonealpha/rife-ncnn-vulkan-ex),
  which credits the Practical-RIFE project and the original ncnn port by nihui.
  The vendored reference files retain their MIT license and copyright notice;
  see [the included copy](LICENSES/native/RIFE-REFERENCE-MIT-LICENSE.txt).
- **RIFE model graph fixtures** — the three `.param` files in
  `crates/rife-core/tests/fixtures/` are copied from the upstream
  `rife-v4.25-lite`, `rife-v4.26`, and `rife-v4.26-large` model folders. Their
  upstream MIT notice is included above. No model weight `.bin` files are
  distributed by this project.
- **NCNN** — statically linked neural-network runtime, licensed primarily
  under BSD 3-Clause. Its full license and notices for its included components
  are in [NCNN-LICENSE.txt](LICENSES/native/NCNN-LICENSE.txt).
- **glslang** — statically linked shader compiler components, with multiple
  applicable licenses and notices. The complete license file from the pinned
  source is included at [GLSLANG-LICENSE.txt](LICENSES/native/GLSLANG-LICENSE.txt).
- **Vulkan-Headers** — Vulkan API headers, licensed under Apache-2.0 or MIT;
  the source license is included at
  [VULKAN-HEADERS-LICENSE.txt](LICENSES/native/VULKAN-HEADERS-LICENSE.txt).
- **Rust crates** — third-party Rust crate notices and license texts for the
  Windows x64 dependency graph are generated with `cargo-about` from
  `Cargo.lock` and included in `THIRD-PARTY-LICENSES.html`.

## Separately downloaded runtime files

Rife does not redistribute FFmpeg, FFprobe, or model weights. The user supplies
those files. Their licenses and any attribution requirements remain with their
respective distributions; verify the terms for the exact FFmpeg build and model
weights you download. FFmpeg's license can change when GPL-enabled components
are compiled in; for example, FFmpeg documents that enabling libx264 makes the
FFmpeg build GPL-licensed. Do not redistribute such a build without meeting its
license requirements.
