# rife

**rife** is a command-line video interpolation tool built around RIFE NCNN/Vulkan. It can increase a video's frame rate, change its playback speed, and create slow motion while preserving the motion smoothness.

## Features

- Increase or reduce output frame rate with `--fps-scale`.
- Change playback speed independently with `--speed`.
- Preserve audio pitch when changing playback speed; at normal speed, audio is copied by default.
- Retime chapter boundaries and MKV subtitle cue timestamps and durations when playback speed changes.
- Detect scene cuts to avoid interpolating across unrelated frames.
- Save interpolated frames as numbered images using printf-style output patterns.
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

1. Download an FFmpeg build from the [official FFmpeg download page](https://ffmpeg.org/download.html) that includes `ffmpeg.exe`, `ffprobe.exe`, and the `libx264` encoder. Copy both executables from its `bin/` directory into the `ffmpeg/` folder shown above.
2. Download a RIFE NCNN model from the [upstream models directory](https://github.com/Erlonealpha/rife-ncnn-vulkan-ex/tree/ex/models). For the model you want, copy `flownet.param` and `flownet.bin` into `models/<model-directory>/`, preserving the directory name. For example, the default model belongs in `models/rife-v4.25/`.
3. Install a current graphics driver with Vulkan support for GPU inference. CPU inference can be selected with `-g -1`.

The model directory name is selected with `-m`; rife checks the model files and graph when loading. A directory name alone does not guarantee compatibility. Do not download or run the upstream RIFE executable; this application contains its own inference backend.

The application has been built and runtime-tested on **Windows x64 and Linux x64**. macOS support has not yet been verified. The layout above shows Windows filenames; on Linux, use `rife`, `ffmpeg/ffmpeg`, and `ffmpeg/ffprobe` without the `.exe` suffix, with the same adjacent `models/` directory.

### Examples

```sh
# Double the frame rate without changing playback speed
rife -i input.mp4 -f 2 output.mp4

# Send the interpolated video directly to a player
rife -i input.mp4 -f 2 - | ffplay -

# Choose the streamed container and encoders
rife -i input.mp4 -f 2 -e "-f nut -c:v ffv1 -c:a pcm_s16le" - | ffplay -

# Create half-speed video while keeping the original frame rate
rife -i input.mp4 -s 0.5 slow.mp4

# Create half-speed video at 1.5 times the original frame rate
rife -i input.mp4 -f 1.5 -s 0.5 slow.mp4

# Choose a model and scene-cut threshold and double the frame rate
rife -i input.mp4 -m rife-v4.26-large -t 0.4 -f 2 -v output.mkv

# Use the CPU and GPU 0, with two workers on each device to double the frame rate
rife -i input.mp4 -g -1,0 -j 1:2,2:2 -f 2 output.mp4

# Save interpolated frames as lossless PNG images
mkdir -p frames
rife -i input.mp4 -f 2 "frames/frame_%08d.png"

# Pass a custom CRF to FFmpeg
rife -i input.mp4 -e -crf=23 output.mp4

# Pass several output options together
rife -i input.mp4 --extra-encoder-option "-crf 12 -c:a aac -b:a 10k" output.mp4

# Select video and audio encoders through FFmpeg options
rife -i input.mp4 -e "-c:v ffv1 -c:a pcm_s16le" output.mkv
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
| `OUTPUT`                                    | **Required.** Video filename, image sequence pattern such as `frames/frame_%08d.png`, or `-` for binary media on stdout. |
| `-m, --model MODEL`                         | Model directory name. Default: `rife-v4.25`.                                                                       |
| `-f, --fps-scale FACTOR`                    | Output FPS divided by input FPS. Default: `1`.                                                                     |
| `-s, --speed FACTOR`                        | Playback speed. Default: `1`; `0.5` is half-speed and `2` is double-speed.                                         |
| `-e, --extra-encoder-option OPTIONS`         | Extra FFmpeg output options, such as `-e -crf=23` or `-e "-crf 12 -b:a 10k"`. Repeatable.                         |
| `-g, --gpu GPU`                             | `auto` or comma-separated device IDs. Use `-1` for CPU; it can be combined with GPU IDs.                           |
| `-j, --jobs LOAD:PROCESS[,PROCESS...]:SAVE` | Shared input/output buffering budgets and per-device worker counts. Default: `1:2:2`.                              |
| `-t, --threshold THRESHOLD`                 | Scene-cut threshold from `0` to `1`. Default: `0.3`; `1` disables scene detection.                                 |
| `-y, --overwrite`                           | Replace an existing output. Waits if another rife process holds its output lock.                                   |
| `-v, --verbose`                             | Show model, dimensions, frame rates, and device details.                                                            |

FPS scale and playback speed must be finite, positive numbers. When speed changes, audio is re-encoded to match the final video duration while preserving pitch. At normal speed, audio is copied unless an encoder is selected with `-e "-c:a ENCODER"`. The output container must support a copied audio codec; otherwise, choose a compatible encoder.

Extra encoder options apply to both videos and image sequences. Each value uses POSIX-style quoting and escapes to split flags and values; no shell, variable expansion, or command substitution is used. Options are appended after FFmpeg's input declarations and rife's defaults, before the output filename. Repeat `-e` to append more options in order; later settings take precedence. For example, use `-e "-metadata title='Hello world'"` to keep a value containing spaces together. Input paths and progress options remain managed by rife.

Select video or image encoders with `-e "-c:v ENCODER"` and audio encoders with `-e "-c:a ENCODER"`. Without a rate-control override, video encoding still reuses source CRF metadata when available, otherwise `20`, or uses a frame-scaled bitrate for encoders without CRF. Explicit CRF, bitrate, or quantizer options override that automatic quality selection. A bitrate option alone does not change copied audio. FFmpeg's audio bitrate flag is `-b:a`.

Chapter boundaries and MKV subtitle cue timestamps and durations follow the same timing adjustment as audio. Chapter titles, subtitle styles, and MKV attachments are preserved. Changing only FPS leaves source subtitle and chapter timing unchanged. Subtitle streams are currently preserved only in MKV outputs.

GIF output uses a palette generated from the video for better colors, including with `-e "-c:v gif"`. Palette generation can add saving time and affects file size.

### Streaming output

Use `-` as the positional output to write encoded media to stdout. The default is
NUT with H.264 video and the usual CRF/bitrate and audio policy; `ffplay -` detects
it automatically. After the initial validation and frame-counting pass, original,
interpolated, and held frames are sent in timeline order as they become ready.
Frames travel through bounded memory buffers and OS pipes, without temporary
frame files or a completed video waiting on disk.

Streaming uses low-delay encoder settings and flushes each ready frame and
encoded packet. The default H.264 encoder uses zero-latency tuning with the same
quality target; file encoding keeps its existing settings. Progress, diagnostics,
and errors go to stderr, so they do not contaminate the media stream. Input remains
a file supplied with `-i`.

Override the container with `-e "-f FORMAT"` and encoders with the usual extra
options. The chosen format must support a non-seekable output. For example,
fragmented MP4 can use `-e "-f mp4 -movflags frag_every_frame+empty_moov"`. These
settings are checked before frame counting, using a temporary sample that is
never sent to stdout.

Extra options can override the low-delay defaults. Custom encoders, formats, and
the receiving player may introduce their own buffering. For example,
`-e "-f matroska"` selects the previous stdout container, whose muxer can delay
packets while completing a cluster. Processing runs as fast as possible; it does
not insert extra frames to cover playback stalls.

Stdout has no output-file lock or replacement step, and `--overwrite` has no
effect on it. Bytes are sent as encoding proceeds; an error or cancellation may
leave the receiver with a partial stream. Closing the receiving pipe stops the
job with a media-processing error and cleans up temporary processing files.

### Image sequences

Use exactly one `%d` or `%0Nd` placeholder in the output filename, following [FFmpeg's image-sequence syntax](https://ffmpeg.org/ffmpeg-formats.html#image2-2). For example, `frame_%08d.png` produces `frame_00000001.png`, `frame_00000002.png`, and so on. Numbering starts at 1; `%%` represents a literal percent sign. The output directory must already exist. `--fps-scale` and `--speed` determine which frames are generated as they do for video output; individual images do not store the playback frame rate.

FFmpeg selects the image encoder, pixel format, and quality defaults from the extension, unless extra encoder options override them. For example, `-e -compression_level=0` changes PNG compression. No video CRF, bitrate, chroma-subsampling, or padding presets are applied automatically. Lossless RGB formats such as PNG, BMP, TIFF, and PPM preserve the interpolated RGB8 pixels with their default settings; lossy formats such as JPEG use FFmpeg's defaults. Images contain no audio, subtitles, chapters, or attachments. The existing RGB8/HDR limitation still applies.

Frames are saved to their final names only after encoding succeeds. Any existing numbered file matching the pattern requires `--overwrite`, even if earlier frame numbers are missing. With `--overwrite`, unrelated files and frames beyond the new sequence's range are left untouched. Failed or cancelled saves roll back frames already moved into place. Image-sequence jobs writing to the same directory share an output lock.

### Progress and output safety

Output conflicts and invalid argument combinations are checked before frame counting. Encoding settings are checked with a temporary sample before counting or loading the model. Errors that depend on the decoded frame count or native model initialization are reported when those stages run; output paths are checked again before saving.

Temporary processing files use the OS temporary directory. For file outputs, if rife cannot create a temporary folder there, it uses the output's parent directory. Finished files are transferred to the output location after encoding succeeds, including when that location is on another drive. Streaming output requires a usable OS temporary directory for processing state.

Backend failures show the actual failure reason and raw diagnostics, followed by a blank line. FFmpeg and FFprobe failures end with `error: media processing failed`; native inference failures end with `error: native backend failed`. Failed jobs clean up their temporary files and any output locks, and preserve existing file outputs.

Verbose output shows Source and Target first, followed by frame counting, model loading, Device, and processing. Counting shows the number of video frames counted so far, then changes to green `Counted N frames in total` when finished. Loading changes to green Loaded when the model is ready. When interpolation ends, the Processed line reports its elapsed time. A separate Saving progress bar, in the same color as Processing, tracks output encoding with its own timer. The final green Saved line reports the saving time; both summaries remain visible. Live status rows keep the cursor at the start of the row and disappear on Ctrl+C while leaving the terminal’s own cancellation indicator and cursor position unchanged. Redirected logs contain completed messages rather than live progress.

## Models

The default model is `rife-v4.25`. Use `-m` to select another installed model. Model names are resolved directly as directory names under `models/`; for example, `-m rife-v4.28-large` looks for `models/rife-v4.28-large`.

Use compatible RIFE v4 models in NCNN format. Legacy RIFE models and arbitrary PyTorch weights are not supported.

## Current limitations

- macOS and architectures other than x64 have not been verified.
- Multi-GPU hardware and broad 4K performance/stress cases are not verified; HDR is not preserved.
- UHD, spatial TTA, and temporal TTA are not implemented for the supported RIFE v4 graphs.
- Timing embedded inside subtitle content, such as ASS karaoke/animation effects and DVD subtitle display commands, is not rewritten when playback speed changes.
- Container and encoder implementations impose their own frame-rate, time-base, and quality limits.

## License

Licensed under **GPL-3.0-only**; see [LICENSE](LICENSE) and [third-party notices](THIRD_PARTY_NOTICES.md). Each release provides a separate `rife-source.tar.gz` containing project and dependency sources.

## Contributing

Developers and contributors: see [CONTRIBUTING.md](CONTRIBUTING.md) for build, test, and contribution instructions.
