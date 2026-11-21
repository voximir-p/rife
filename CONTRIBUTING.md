# Contributing

This guide covers building, checking, and extending rife. For command-line usage and runtime setup, see the [README](README.md).

## Toolchain

- Rustup with the compiler and components in [`rust-toolchain.toml`](rust-toolchain.toml); the crates' minimum Rust version remains 1.88
- CMake 3.24 or newer
- Ninja on Windows; Make or Ninja on Linux (set `CMAKE_GENERATOR=Ninja` to select Ninja)
- Git, a POSIX shell, and standard Unix utilities (Git Bash on Windows)
- Python 3 for model fetching, smoke checks, source packaging, and compiler-library notices
- A C/C++ compiler: MSVC x64 on Windows, or a platform toolchain such as GCC/Clang on Linux
- The platform Vulkan development libraries and loader

Windows x64 and Linux x64 have been built and runtime-tested. macOS and other architectures remain unverified. Release targets are configured separately in [dist-workspace.toml](dist-workspace.toml).

## Build

Dependency revisions are pinned in [`third_party/dependencies.lock`](third_party/dependencies.lock). The native build script, `crates/rife-core/build.rs`, needs NCNN and Vulkan-Headers already present. The CLI build script, `crates/rife-cli/build.rs`, fetches missing selected model assets automatically. Run the shared fetch script below to prepare both native sources and models before a normal checkout build.

On Debian/Ubuntu, install the Vulkan loader development package before building:

```sh
sudo apt-get update
sudo apt-get install -y libvulkan-dev
```

The fetched Vulkan headers do not supply the system loader library. Linux
release builds install this package through `dist.dependencies.apt` in
`dist-workspace.toml`.

Run these commands from the repository root, using a Unix-like shell or Git Bash on Windows. On Windows, start the shell with the MSVC x64 build environment available:

```sh
sh scripts/fetch-dependencies.sh
cargo build --locked --release
```

The fetch script verifies the checked-out revisions and leaves unexpected existing sources untouched. Builds can use existing native source trees by setting `RIFE_NCNN_SOURCE` and `RIFE_VULKAN_HEADERS`. The executable is written to `target/release/rife` on Linux or `target/release/rife.exe` on Windows. FFmpeg and FFprobe are runtime and media-test requirements, not build prerequisites.

### Selecting bundled models

Edit [`third_party/models.txt`](third_party/models.txt) to choose which upstream model directories to embed. Use one name per line; blank lines and lines starting with `#` are ignored. Keep at least one unique name, using only letters, digits, underscores, hyphens, and internal dots. The first entry becomes the CLI default; it is separate from the core library's `Model::DEFAULT`. For example, a smaller build can contain just:

```text
rife-v4.25-lite
```

The upstream model repository and exact commit are pinned in `third_party/dependencies.lock`. `sh scripts/fetch-dependencies.sh` also fetches the selection; `sh scripts/fetch-models.sh` fetches only model assets. The CLI build script automatically runs the model fetcher if assets are missing and verifies existing Git checkouts. Git, Python 3 and `sh` (Git Bash on Windows) are needed for fetching. Only selected model directories are checked out for new clones; expanding an existing sparse checkout preserves its prior selection. Unexpected revisions and local changes are rejected without resetting user files. New checkouts are installed only after a successful fetch, so failed downloads can be retried. To change the pinned revision, move the old fetched checkout aside and fetch again; preserve any local work first.

`rust-embed` embeds only the selected `flownet.param`/`flownet.bin` pairs and their upstream MIT license. Debug builds also embed assets, so runtime behavior does not depend on the source checkout. Builds track selection and asset changes and validate fetched inputs before embedding them. Selection controls inclusion; validate loading and inference for every newly selected model. Complete Corresponding Source bundles include these assets and build offline without fetching or requiring Git/Python for model preparation. Model embedding increases executable size; removing a name from the selection excludes its bytes even if the fetched checkout still has that directory.

## Checks and runtime smoke test

Run the formatting, lint, and workspace tests before submitting a change:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --release --locked -- -D warnings
cargo test --workspace --release --locked
```

The opt-in media regressions generate small fixtures and verify normalized frame
counts, incremental encoding, and subtitle/chapter/audio timing with FFmpeg and
FFprobe. They require no external model installation or GPU at test runtime; compiling the CLI test binary still prepares and embeds the selected models:

```sh
cargo test -p rife-cli --release --locked encode_timing_tests -- --ignored
```

The application and these tests launch `ffmpeg` and `ffprobe` through `PATH`.
Set `RIFE_TEST_FFMPEG` and `RIFE_TEST_FFPROBE` to select explicit binaries for
the opt-in tests only; these variables do not configure the application.

The smoke script exercises the built CLI with FFmpeg and FFprobe on `PATH`:

```sh
sh scripts/smoke.sh target/release
```

The first argument selects the directory containing the executable; omitting it
uses `runtime/`. No adjacent model directory is needed for bundled models. The
suite uses the CLI default model unless `--model MODEL` selects another bundled
or external model. The direct private-worker check uses fetched source model
files when no external model is installed.

The suite runs on CPU by default; use `--gpu 0` for GPU 0 (`RIFE_SMOKE_GPU` is
also supported). `--timeout SECONDS` changes the per-command deadlock guard from
120 seconds. Fixtures and outputs use an isolated OS temporary directory, so
the suite does not need a writable checkout or a particular Cargo target
directory. Run it separately for each model/device combination being validated.

The regular workspace test command skips the four opt-in native inference tests. The core library takes an explicit model directory, so point these tests at fetched or externally supplied compatible model files and run them serially:

```sh
RIFE_TEST_MODEL_DIR="$PWD/third_party/rife-ncnn-vulkan-ex/models/<model-directory>" \
  cargo test -p rife-core --release --lib engine::native_tests -- --ignored --test-threads=1
```

Repeat with each model directory being validated. These checks use CPU unless
`RIFE_TEST_GPU=0` (or another device selection) requests GPU coverage. The weight
validation and native cancellation checks always use CPU. Padding comes from
the supplied model's validated graph. Native cancellation is triggered by a
checkpoint callback, without large frames, sleeps, or CPU-speed deadlines.
GPU coverage requires a Vulkan-capable driver.

Keep ordinary unit tests independent of externally installed models, FFmpeg, GPU drivers,
the current working directory, and a writable checkout. Use owned temporary
directories and synthetic fixtures; keep external-resource checks explicitly
opt-in. Embedded-asset tests may use the models compiled into the CLI. Synchronize concurrency tests with channels/acknowledgements rather
than sleeping to assume another task has run. Timeouts should guard deadlocks,
not assert machine performance. Self-executing child test fixtures are needed
for process/lock/crash coverage even though they do nothing when discovered
without their private environment settings.

## Project structure

- `crates/rife-core` provides reusable frame scheduling, model inference, scene detection, device management, and cancellation.
- `crates/rife-cli` provides the `rife` executable, CLI, FFmpeg/FFprobe pipeline, terminal UI, and output handling.
- `crates/rife-core/native` contains the C++/NCNN implementation and checked C ABI.
- `third_party` holds dependency revisions, the bundled model selection, and ignored fetched native/model sources.
- `crates/rife-cli/build.rs` prepares the selected assets and generates the embedded-model declaration and CLI default.

The core library does not bundle models or invoke FFmpeg. Its `Engine` takes a model directory supplied by the caller; the CLI owns embedded assets and external-model lookup.

The executable entry point only dispatches private worker mode or the CLI. Within
`crates/rife-cli/src`, responsibilities are divided as follows:

| Module | Responsibility |
| --- | --- |
| `cli.rs` | Argument parsing and effective encoder option selection |
| `app.rs` | Signal handling, application lifetime, and exit presentation |
| `pipeline.rs` and `pipeline/` | Job preparation, preflight, execution, and destination ownership |
| `process.rs` | Media child supervision, diagnostic capture, and cancellation |
| `decode.rs` | Shared temporal normalization, internal frame counting, and RGB decoding |
| `encode.rs` and `encode/` | Command construction, container presets, quality, and audio timing |
| `worker.rs` and `worker/` | Native process supervision, private protocol, runtime, and RGB frame reading |
| `diagnostics.rs` | Shared backend error presentation data |
| `models.rs` | Bundled asset resolution and cancellable extraction into job-owned staging |

Keep staging alive until all children are reaped, and release the destination
lock after staging cleanup. Prepared jobs own both resources; planned jobs add
the exact frame schedule and encoder command after counting. Encoder timing
borrows quality, bitrate, and metadata settings while building an owned command.
Worker progress messages use the same refresh interval as the terminal, while
loading, completion, and errors are reported immediately. Coalescing status must
never limit frame I/O or weaken final count/exit validation.

The positional output `-` selects a distinct stdout destination. Only file
destinations resolve output paths, acquire locks, validate image names, or commit
staged results. Input remains a canonical file; do not add stdin buffering or a
capture subprocess. Both paths still use the same normalized count and exact
schedule. Stdout defaults to NUT for immediate packet delivery, while retaining
the existing H.264/CRF and audio preset policy. `encode/streaming.rs` keeps that
default policy separate from the wire format and applies low-delay encoder,
packet-flush and muxer settings before extra options. Effective `-e "-f FORMAT"`
selection controls the chosen container's presets, quality probing, audio and timed-metadata handling.
File extensions still determine image-sequence staging names.

The worker config carries `stream_output` so its emit callback flushes every
scheduled frame, including copies and held endpoints. File jobs retain buffered
RGB writes. Raw encoder input has an explicit format, dimensions and clock, so
stdout jobs disable unnecessary probing of that pipe. Do not add `nobuffer` input
flags that can discard probed frames. Streaming has no frame or video cache on
disk; staging contains worker control state, optional retimed chapter metadata,
and the selected model files when a bundled model is used.
The native smoke regression withholds a third input frame until two output
frames arrive, then verifies original, interpolated and final held frames. The
FFmpeg opt-in regression requires a decoded frame before a second frame or EOF,
with and without source audio. Its receiver specifies NUT and a single decoding
thread to isolate sender delay from independent player probing/thread buffering.

Encoder stdout is inherited only for a stdout destination. FFmpeg progress uses
`pipe:2`; the existing stderr reader updates the saving counter and removes only
recognized progress records from the diagnostic tail. Keep genuine interleaved
errors. This avoids a media relay or another progress pipe/thread. Preflight also
uses `pipe:1` as the media destination when streaming, with its stdout discarded,
so non-seekable muxer restrictions are validated without sending sample bytes to
the receiver. Wait for both worker and encoder success, and preserve media-error
precedence on broken pipes. Stdout bytes cannot be rolled back. Stream jobs use
OS temporary storage for worker/chapter state and extracted bundled models,
with no destination-directory fallback.

In the core, `engine/loading.rs` owns cancellable model reads and native network
construction. Stream task processing and ordered output collection stay separate
from scoped thread orchestration. Keep their generic callbacks, recycled frame
buffers, permit bounds, and per-device gates when extending these stages.

Rust owns the public API and processing pipeline. The CLI runs model loading and native interpolation in a supervised worker using the same executable. The parent owns FFmpeg processes, output locks, staging, final commit, and the terminal UI; fatal worker exits must pass through that parent's normal error and cleanup path. Raw frames flow through OS pipes, while structured worker status and a bounded diagnostic tail drive progress and backend error reports. Keep public `rife-core` inference independent of this CLI process protocol. The worker privately enables native diagnostic mirroring so messages survive a subsequent crash; embedding applications retain quiet thread-local diagnostics. C++ exception messages are recorded before returning through the C ABI, and model-destruction failures must prevent output commit.

The C++ backend is statically linked. Native build configuration and source overlays are maintained in `crates/rife-core/build.rs` and `crates/rife-core/native/CMakeLists.txt`.

Image sequences use a separate `image2` encoder command: feed RGB8 directly, keep one image per scheduled frame, and leave image quality/pixel-format defaults to FFmpeg. Do not apply video presets or probe source bitrate packets for this path. `images.rs` parses numbered output patterns and commits a validated set of staged frames with bounded memory, same-filesystem preparation, backups, and rollback. Sequence jobs share a directory lock so differently padded or overlapping patterns cannot write concurrently. Preserve input protection and leave unrelated files untouched. The smoke suite compares decoded PNG/BMP/TIFF/PPM pixels against FFV1 output and checks numbering, replacement, errors, odd dimensions, and playback scheduling. Backend errors use one common terminal layout; failures caused by cleanup must not replace the original backend diagnosis.

Keep preflight validation before Source/Target status and the counting pass. Scan existing sequence names using canonical pattern expansion, including sparse sequences, and repeat validation after acquiring the output lock. Validate argument-only scheduling/worker/audio-copy constraints first, then source-dependent bitrate and chapter requirements. Before counting, run the same encoder options against one blank RGB frame in staging, limiting video/audio/subtitle packets and duration. Zero-frame validation incorrectly fails some GIF/WebP encoders, so do not substitute it. This check uses FFmpeg from `PATH` rather than an extension/codec allowlist and must remain cancellable with bounded buffering and normal diagnostic cleanup. `QualityPlan` defers frame-scaled bitrate calculation until the actual count is available; the sample omits that count-dependent bitrate. Rewrite chapter metadata with the final effective speed before loading the model. Encoding command builders return options without a destination so validation and production can append their own limits/path. Runtime decoding, model initialization, count-dependent limits, and output races still require their normal checks.

Counting uses the same `fps=fps=RATE:start_time=0` temporal filter and passthrough
output timing as processing, with `wrapped_avframe` into FFmpeg's null muxer.
Only machine progress records cross the pipe: no RGB conversion, raw-video
encoding, frame transfer or frame cache is needed for the count. Require a valid
final `progress=end` report and successful child exit; preserve backend errors
and cancellation ahead of secondary parser errors. Processing still verifies the
normalized count at EOF.

FFmpeg and FFprobe do not expose an exact random-access frame-index existence query
for this normalized stream. FFprobe `-read_intervals` seeks by time, with inexact
seek positions; `select`'s `n` is sequential, and repeating it would repeat decoding.
Metadata can also describe a different count from the normalized timeline, as
the VFR regression demonstrates. Keep the sequential fallback until an exact
index query is available. Plain FFprobe `-count_frames` counts decoded source
frames and does not apply the application's FPS filter. See the official
[FFprobe options](https://ffmpeg.org/ffprobe.html#Main-options) and
[select filter](https://ffmpeg.org/ffmpeg-filters.html#select_002c-aselect).

`encoder_options.rs` parses each repeatable `-e`/`--extra-encoder-option` value using `shlex` POSIX quoting, normalizing `-key=value` into separate FFmpeg arguments without a shell. Keep repetition order and append the resulting output options after all inputs/default options in both encoder builders; preflight must receive the same arguments. Codec and rate-control overrides are supplied through extra encoder options. Tests also reject the removed `-c`/`--crf`, `--video-codec`, and `--audio-codec` CLI flags. Retain finite CRF validation for source metadata, while explicit FFmpeg values are validated by the backend. Effective codec and rate-control selection must respect extra options so defaults and early audio-copy checks do not contradict overrides. The smoke suite verifies encoded CRF values, literal quoted metadata, audio overrides, image compression, early backend errors, and rejection of the removed flags. The runtime `shlex` dependency is covered by the generated license report.

## Documentation

Keep documentation consistent with CLI help, `third_party/models.txt`, the dependency lockfile, and actual runtime behavior. Verify examples and local links when changing them.

- `README.md` covers installation, usage, options, and limitations for end users, with a short pointer to this guide for contributors.
- `CHANGELOG.md` describes user-visible changes. Keep CI setup, build-system changes, refactors, and test execution details out of release notes unless they have a user-visible effect; describe that effect rather than the implementation.
- This guide covers developer prerequisites, architecture, contribution workflows, tests, and release maintenance. Keep commands and procedures reproducible for other contributors.

## Changing model support or native code

Bundled model selection belongs in `third_party/models.txt`; custom external models remain supported. Keep extracted assets in parent-owned staging until native workers have exited. Extract only the selected model, check cancellation between bounded writes, and retain its upstream license. To add support for a model family, update the runtime graph validation, model fixtures, enabled native operators, and model compatibility documentation together. Validate both loading and inference using the native test and smoke workflow where applicable.

When changing the native dependency set or copied native code, update `third_party/dependencies.lock`, the relevant build configuration, `LICENSES/native/`, and `THIRD_PARTY_NOTICES.md`. Do not edit fetched dependency sources as part of the normal build; apply required local changes through the project’s build overlay. Each overlay replacement must validate its own anchor; cancellation patches also check the expected overload count in the pinned source.

## License report and notices

The Rust dependency license report covers both release targets in `about.toml` and is generated with cargo-about 0.9.3. The CI command installs only its `cli` feature:

```sh
cargo install --locked --features cli --version 0.9.3 cargo-about
sh scripts/generate-third-party-licenses.sh
sh scripts/generate-third-party-licenses.sh --check
```

Keep `THIRD-PARTY-LICENSES.html` current when workspace dependencies change. When a crate’s license changes, review `about.toml`. Native component attributions and license texts belong in `THIRD_PARTY_NOTICES.md` and `LICENSES/native/`; the release source archive includes these files and the GPL license text.

The Rust standard-library notices are separate from Cargo crates and depend on
the compiler version. `rust-toolchain.toml` pins the same compiler on Windows
and Linux. The notice file is shipped in Rust's compiler package; installing
`rust-docs` alone does not select a matching compiler. Rustup applies the pin
to repository commands, including the existing release setup steps; the
`rust-version` in `Cargo.toml` is a minimum, not a release-compiler pin.

When updating the pinned compiler, regenerate and check its notices, then commit
the toolchain file and notice changes together:

```sh
rustc --version
sh scripts/generate-rust-notices.sh
sh scripts/generate-rust-notices.sh --check
```

Explicit `cargo +TOOLCHAIN` commands or `RUSTUP_TOOLCHAIN` can override the pin
for compatibility testing. Use the pinned compiler for release builds and
notice verification. Release CI checks both license reports before building.

With all tracked source changes committed, run `sh scripts/package-source.sh` to
create `target/rife-source.tar.gz`. Newly added project files must be committed too: the script exports `HEAD`, so untracked files are never included. The release workflow publishes this bundle
alongside the binary. It includes pinned native sources, selected model assets,
locked Rust crates, their notices, and build scripts. Its generated Cargo configuration supports
`cargo build --release --locked --offline` after installing the documented
toolchains. Dependency downloads require network access when packaging;
FFmpeg, unselected models, Git history, and machine-local files are excluded.
Untracked files, including cargo-dist's generated manifests, are excluded from
the source archive and do not block packaging. Staged or unstaged changes to
tracked files still fail the check, with the affected paths listed in the error.

Keep fetched model repositories and weights out of project Git history. Selected weights are embedded in release executables and included in the corresponding source bundle with their upstream MIT notice. FFmpeg binaries remain user-supplied and must not be added to source or release artifacts. See the README and third-party notices for runtime and redistribution information.


## Release workflow

Release targets and packaging are configured in [`dist-workspace.toml`](dist-workspace.toml). Project setup steps belong in [`.github/build-setup.yml`](.github/build-setup.yml). The generated `.github/workflows/release.yml` is maintained by cargo-dist; update its configuration and regenerate it when needed instead of editing it by hand.

The custom source artifact is built by `scripts/package-source.sh`; cargo-dist's default source tarball is disabled. Keep the source bundle, model selection, dependency revisions, and notices aligned with the executable being released.
