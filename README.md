# smelter-vulkan

A stress testing application for the Smelter compositor framework, designed to record video using hardware-accelerated Vulkan H.264 encoding.

## Overview

This project demonstrates and stress tests the Smelter compositor's ability to handle high-throughput video workflows by:

- **Recording from an MP4 input source (looped)**
- **Capturing at 4K resolution (3840x2160) @ 30fps**
- **Using Vulkan-accelerated H.264 video encoding** for maximum performance
- **Writing to MP4 output file**

## Features

- **Automatic asset download**: Downloads test input video if not present
- **Hardware acceleration**: Leverages Vulkan video encoding for efficient GPU-based compression
- **High resolution**: Supports 4K (UHD) video recording at 3840x2160 resolution
- **Platform-specific**: Linux-only due to Vulkan video encoding requirements

## Requirements

- **Platform**: Linux (x86_64)
- **Hardware**:
  - Vulkan-capable GPU with H.264 encoding support
- **Dependencies**:
  - Rust toolchain
  - Vulkan drivers

## Usage

```bash
# Build the project
cargo build --release

# Run the stress test (records for 10 minutes by default)
cargo run --release
```

The application will:
1. Download the test input video (if not present)
2. Initialize the Smelter compositor pipeline
3. Register the MP4 input source
4. Start recording to `./recordings/recording.mp4`
5. Record for the specified duration (default: 600 seconds / 10 minutes)
6. Clean up and exit

## Configuration

Key parameters can be adjusted in `src/compositor.rs`:

- **Resolution**: `RESOLUTION` constant (currently 3840x2160)
- **Framerate**: `output_framerate` in `PipelineOptions` (currently 30fps)
- **Sample rate**: `mixing_sample_rate` in `PipelineOptions` (currently 48kHz)
- **Recording duration**: Passed to `start_recording()` in `src/main.rs`

## Architecture

The application consists of three main components:

- **`compositor.rs`**: Manages the Smelter pipeline, input registration, and recording setup
- **`main.rs`**: Entry point that initializes logging and coordinates the recording workflow
- **`assets.rs`**: Handles downloading test input assets

### Compositor Flow

1. Graphics context initialization (wgpu)
2. Pipeline configuration with Vulkan encoding support
3. MP4 input registration (looped playback)
4. Output registration for recording
5. Timed recording with automatic cleanup

## Limitations

- **Linux only**: Vulkan video encoding is only available on Linux
- **No audio**: Currently configured without audio capture to focus on video performance
- **Fixed codec**: Uses H.264 (no HEVC/AV1 support in this test)
