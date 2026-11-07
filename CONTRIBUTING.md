# Contributing

This guide covers building, checking, and extending rife. For command-line usage and runtime setup, see the [README](README.md).

## Toolchain

- Rust 1.88 or newer
- CMake 3.24 or newer
- Ninja
- A POSIX shell and standard Unix utilities (Git Bash on Windows)
- Python 3 for smoke checks, source packaging, and compiler-library notices
- On Windows, an MSVC C++ x64 toolchain
- On Linux or macOS, the platform Vulkan development libraries and loader

Windows x64 and Linux x64 have been built and runtime-tested. macOS and other architectures remain unverified. Release targets are configured separately in [dist-workspace.toml](dist-workspace.toml).

## Build

The native build uses pinned NCNN and Vulkan-Headers revisions from `third_party/dependencies.lock`. Fetch those sources before building; `build.rs` does not download them.

On Debian/Ubuntu, install the Vulkan loader development package before building:

```sh
sudo apt-get update
sudo apt-get install -y libvulkan-dev
```

The fetched Vulkan headers do not supply the system loader library. Linux
release builds install this package through `dist.dependencies.apt` in
`dist-workspace.toml`.

Run the same scripts on Unix-like systems and in Git Bash on Windows:

```sh
sh scripts/fetch-dependencies.sh
cargo build --locked --release
```

The fetch script verifies the checked-out revisions and leaves unexpected existing sources untouched. Builds can use existing source trees by setting `RIFE_NCNN_SOURCE` and `RIFE_VULKAN_HEADERS`.

## Checks and runtime smoke test

Run the formatting, lint, and workspace tests before submitting a change:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --release --locked -- -D warnings
cargo test --workspace --release --locked
```

The opt-in muxing regressions generate small media fixtures and verify subtitle,
chapter, and audio timing with FFmpeg/FFprobe. They do not require models or a GPU:

```sh
cargo test -p rife-cli --release --locked encode_timing_tests -- --ignored
```

These tests use `runtime/ffmpeg/ffmpeg` and `runtime/ffmpeg/ffprobe` (`.exe` on
Windows). Set `RIFE_TEST_FFMPEG` and `RIFE_TEST_FFPROBE` to use other test binaries.

The smoke script exercises the built CLI through the runtime directory layout. Put the executable, FFmpeg/FFprobe, and compatible `rife-v4.26` and `rife-v4.26-large` models under `runtime/` as shown in the README, then run:

```sh
# On Windows, copy target/release/rife.exe to runtime/rife.exe instead.
cp target/release/rife runtime/rife
sh scripts/smoke.sh
```

Pass a different runtime directory as the first argument to `smoke.sh`. Set
`RIFE_SMOKE_GPU=-1` to run on CPU; the default is automatic device selection.

The regular workspace test command does not run the opt-in native inference test. Point it at an installed compatible model and run it serially:

```sh
RIFE_TEST_MODEL_DIR="$PWD/runtime/models/rife-v4.26" \
  cargo test -p rife-core --release --test native -- --ignored --test-threads=1
```

Repeat with each model directory being validated. Native inference coverage requires a compatible model; GPU coverage also requires a Vulkan-capable driver.

## Project structure

- `crates/rife-core` provides reusable frame scheduling, model inference, scene detection, device management, and cancellation.
- `crates/rife-cli` provides the `rife` executable, CLI, FFmpeg/FFprobe pipeline, terminal UI, and output handling.
- `crates/rife-core/native` contains the C++/NCNN implementation and checked C ABI.
- `third_party` holds pinned native dependency metadata and local fetched sources.

Rust owns the public API and processing pipeline. The C++ backend is statically linked. Native build configuration and source overlays are maintained in `crates/rife-core/build.rs` and `crates/rife-core/native/CMakeLists.txt`.

## Documentation

- `README.md` covers installation, usage, options, and limitations for end users, with a short pointer to this guide for contributors.
- `CHANGELOG.md` describes user-visible changes. Keep CI setup, build-system changes, refactors, and test execution details out of release notes unless they have a user-visible effect; describe that effect rather than the implementation.
- This guide covers developer prerequisites, architecture, contribution workflows, tests, and release maintenance. Keep commands and procedures reproducible for other contributors.

## Changing model support or native code

Model files must remain user-provided. To add support for a model family, update the runtime graph validation, model fixtures, enabled native operators, and model compatibility documentation together. Validate both loading and inference using the native test and smoke workflow where applicable.

When changing the native dependency set or copied native code, update `third_party/dependencies.lock`, the relevant build configuration, `LICENSES/native/`, and `THIRD_PARTY_NOTICES.md`. Do not edit fetched dependency sources as part of the normal build; apply required local changes through the project’s build overlay.

## License report and notices

The Rust dependency license report is generated with cargo-about 0.9.3. The CI command installs only its `cli` feature:

```sh
cargo install --locked --features cli --version 0.9.3 cargo-about
sh scripts/generate-third-party-licenses.sh
sh scripts/generate-third-party-licenses.sh --check
```

Keep `THIRD-PARTY-LICENSES.html` current when workspace dependencies change. When a crate’s license changes, review `about.toml`. Native component attributions and license texts belong in `THIRD_PARTY_NOTICES.md` and `LICENSES/native/`; the release source archive includes these files and the GPL license text.

The Rust standard-library notices are separate from Cargo crates. Install
`rust-docs` and run `sh scripts/generate-rust-notices.sh` after a compiler
update; `--check` verifies the checked-in copy against the release compiler.
Release CI checks both reports before building the executable.

From a clean, committed checkout, run `sh scripts/package-source.sh` to
create `target/rife-source.tar.gz`. The release workflow publishes this bundle
alongside the binary. It includes pinned native sources, locked Rust crates,
their notices, and build scripts. Its generated Cargo configuration supports
`cargo build --release --locked --offline` after installing the documented
toolchains. Dependency downloads require network access when packaging;
FFmpeg, model weights, Git history, and machine-local files are excluded.

FFmpeg binaries and RIFE model weights are supplied separately by users. Do not add them to source or release artifacts. See the README and third-party notices for runtime and redistribution information.
