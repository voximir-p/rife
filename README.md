# rife

**rife** is a command-line video interpolation tool built around RIFE NCNN/Vulkan. It can increase a video's frame rate, change its playback speed, and create slow motion while preserving the motion smoothness.

The application is split into a reusable Rust library (`rife-core`) and the `rife` command-line program (`rife-cli`). Rust handles video processing and scheduling; a statically linked C++ backend runs the NCNN RIFE model on the CPU or Vulkan-capable GPUs.

## Features

- Increase or reduce output frame rate with `--fps-scale`.
- Change playback speed independently with `--speed`.
- Retiming audio uses pitch-preserving FFmpeg filters; at normal speed, audio is copied by default.
- Detect scene cuts to avoid interpolating across unrelated frames.
- Select the CPU, one or more GPUs, or automatic device selection.
- Choose models by their directory name without maintaining a model-name allowlist.
- Use user-provided FFmpeg, FFprobe, and model files beside the executable. Runtime paths are resolved relative to the executable, not the current working directory.
- No external RIFE executable, Python, CUDA, NCNN DLL, or OpenMP DLL is required at runtime.

## Running rife

### Required runtime downloads

The rife release does **not** include FFmpeg, FFprobe, or model weights. Download
and install these files yourself next to `rife.exe` (the directory name is
`rife/` below):

```text
rife/
├── rife.exe
├── ffmpeg/
│   ├── ffmpeg.exe
│   └── ffprobe.exe
└── models/
    └── <model-directory>/
        ├── flownet.param
        └── flownet.bin
```

1. Download an FFmpeg build from the [official FFmpeg download page](https://ffmpeg.org/download.html). FFmpeg links to third-party Windows builds; choose one that includes `ffmpeg.exe` and `ffprobe.exe`. The default MP4 encoder uses `libx264`; FFmpeg documents that builds with libx264 are GPL-licensed. This project does not redistribute FFmpeg, and you must follow the license for the exact build you download if you redistribute it. Copy both executables from its `bin/` directory into the `ffmpeg/` folder shown above.
2. Download a RIFE NCNN model from the [upstream models directory](https://github.com/Erlonealpha/rife-ncnn-vulkan-ex/tree/ex/models). For the model you want, copy `flownet.param` and `flownet.bin` into `models/<model-directory>/`, preserving the directory name. For example, the default model belongs in `models/rife-v4.25/`.
3. Install a current graphics driver with Vulkan support for GPU inference. CPU inference can be selected with `-g -1`.

The model directory name is selected with `-m`; rife checks the model files and graph when loading. A directory name alone does not guarantee compatibility. Do not download or run the upstream RIFE executable; this application contains its own inference backend.

## License

rife project code is licensed under **GPL-3.0-only**; see [LICENSE](LICENSE). Release archives include the corresponding source, third-party notices, and dependency license report. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for included components and attributions.

The currently compiled and runtime-tested platform is **Windows x64**. Linux and macOS support has not yet been verified; the layout above shows the Windows filenames.

### Examples

```sh
# Double the frame rate without changing playback speed
rife -i input.mp4 -f 2 output.mp4

# Create half-speed video while keeping the original frame rate
rife -i input.mp4 -s 0.5 slow.mp4

# Create half-speed video at 1.5 times the original frame rate
rife -i input.mp4 -f 1.5 -s 0.5 slow.mp4

# Choose a model and scene-cut threshold and double the frame rate
rife -i input.mp4 -m rife-v4.26-large -t 0.4 -f 2 -v output.mkv

# Use the CPU and GPU 0, with two workers on each device to double the frame rate
rife -i input.mp4 -g -1,0 -j 1:2,2:2 -f 2 output.mp4
```

`--fps-scale` and `--speed` are independent:

- `-f 2` makes the output frame rate twice the input frame rate.
- `-s 0.5` makes playback half as fast.
- `-f 2 -s 0.5` combines both: twice the output frame rate and half-speed playback.
- `-f 1 -s 1` keeps the original frame rate and playback speed, but still processes and re-encodes the video rather than copying its compressed video stream.

### Command-line options

| Option                                      | Description                                                                                                        |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `-i, --input FILE`                          | **Required.** Input video.                                                                                         |
| `OUTPUT`                                    | **Required.** Output filename and container.                                                                       |
| `-m, --model MODEL`                         | Model directory name. Default: `rife-v4.25`.                                                                       |
| `-f, --fps-scale FACTOR`                    | Output FPS divided by input FPS. Default: `1`.                                                                     |
| `-s, --speed FACTOR`                        | Playback speed. Default: `1`; `0.5` is half-speed and `2` is double-speed.                                         |
| `-c, --crf CRF`                             | Non-negative video CRF override. By default, source CRF metadata is reused when available, otherwise `20` is used. |
| `-g, --gpu GPU`                             | `auto` or comma-separated device IDs. Use `-1` for CPU; it can be combined with GPU IDs.                           |
| `-j, --jobs LOAD:PROCESS[,PROCESS...]:SAVE` | Shared input/output buffering budgets and per-device worker counts. Default: `1:2:2`.                              |
| `-t, --threshold THRESHOLD`                 | Scene-cut threshold from `0` to `1`. Default: `0.3`; `1` disables scene detection.                                 |
| `-y, --overwrite`                           | Replace an existing output. Waits if another rife process holds its output lock.                                   |
| `--video-codec ENCODER`                     | Override the FFmpeg video encoder. If it does not support CRF, rife uses a bitrate fallback.                       |
| `--audio-codec ENCODER`                     | Override the audio encoder. At normal speed, audio is copied by default.                                           |
| `-v, --verbose`                             | Show model, dimensions, frame rates, device names, and processing stages.                                          |

FPS scale and playback speed must be finite, positive numbers. When speed changes, audio is re-encoded to match the final video duration while preserving pitch. At normal speed, audio is copied unless `--audio-codec` is supplied. The output container must support a copied audio codec; otherwise, choose a compatible encoder.

## Models

The default model is `rife-v4.25`. Use `-m` to select another installed model. Model names are resolved directly as directory names under `models/`; for example, `-m rife-v4.28-large` looks for `models/rife-v4.28-large`.

Models must use a compatible NCNN RIFE v4 three-input `flownet` graph. Legacy `contextnet`/`fusionnet` model families and arbitrary PyTorch weights are not supported. A directory name being accepted does not guarantee that its model graph is compatible; the application validates the graph at load time.

## Contributing

Build prerequisites, architecture notes, test commands, and dependency/license maintenance are in [CONTRIBUTING.md](CONTRIBUTING.md).

## Current limitations

- Only Windows x64 has been compiled and runtime-tested so far.
- Multi-GPU hardware, Linux/macOS, broad 4K performance/stress cases, and HDR preservation are not verified; HDR is not preserved.
- UHD, spatial TTA, and temporal TTA are not implemented for the supported RIFE v4 graphs.
- Subtitle and chapter timestamps are not retimed when playback speed changes.
- Container and encoder implementations impose their own frame-rate, time-base, and quality limits.
