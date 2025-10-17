# smelter-vulkan

A stress testing application for the Smelter compositor framework, designed to record from multiple DeckLink capture devices simultaneously using hardware-accelerated Vulkan H.264 encoding.

## Overview

This project demonstrates and stress tests the Smelter compositor's ability to handle high-throughput video workflows by:

- **Recording from up to 8 DeckLink capture devices simultaneously**
- **Capturing at 4K resolution (3840x2160) @ 30fps**
- **Using Vulkan-accelerated H.264 video encoding** for maximum performance
- **Writing each stream to separate MP4 files**

## Features

- **Multi-device capture**: Automatically detects and registers all available DeckLink devices
- **Hardware acceleration**: Leverages Vulkan video encoding for efficient GPU-based compression
- **High resolution**: Supports 4K (UHD) video recording at 3840x2160 resolution
- **Platform-specific**: Linux-only due to DeckLink SDK requirements

## Requirements

- **Platform**: Linux (x86_64)
- **Hardware**:
  - Vulkan-capable GPU with H.264 encoding support
  - One or more DeckLink capture cards
- **Dependencies**:
  - Rust toolchain
  - Vulkan drivers
  - DeckLink SDK (included via Smelter)

## Usage

```bash
# Build the project
cargo build --release

# Run the stress test (records for 10 minutes by default)
cargo run --release
```

The application will:
1. Initialize the Smelter compositor pipeline
2. Detect all available DeckLink devices
3. Register each device as a separate input
4. Start recording each input to `./recordings/recording_decklink_input_<ID>.mp4`
5. Record for the specified duration (default: 600 seconds / 10 minutes)
6. Clean up and exit

## Configuration

Key parameters can be adjusted in `src/compositor.rs`:

- **Resolution**: `RESOLUTION` constant (currently 3840x2160)
- **Framerate**: `output_framerate` in `PipelineOptions` (currently 30fps)
- **Sample rate**: `mixing_sample_rate` in `PipelineOptions` (currently 48kHz)
- **Recording duration**: Passed to `start_recording()` in `src/main.rs`

## Architecture

The application consists of two main components:

- **`compositor.rs`**: Manages the Smelter pipeline, DeckLink device registration, and recording setup
- **`main.rs`**: Entry point that initializes logging and coordinates the recording workflow

### Compositor Flow

1. Graphics context initialization (wgpu)
2. Pipeline configuration with Vulkan encoding support
3. DeckLink device enumeration and registration
4. Output registration for each input stream
5. Timed recording with automatic cleanup

## Performance Notes

This stress test is designed to push the limits of the system:

- 8 simultaneous 4K streams @ 30fps = ~240 fps total throughput
- Each stream generates approximately 150-300 Mbps (depending on encoder settings)
- Total bandwidth: 1.2-2.4 Gbps sustained write performance required
- Vulkan encoding offloads compression to GPU, reducing CPU load significantly

## Limitations

- **Linux only**: DeckLink SDK and Vulkan video encoding are only available on Linux
- **No audio**: Currently configured without audio capture to focus on video performance
- **Fixed codec**: Uses H.264 (no HEVC/AV1 support in this test)
