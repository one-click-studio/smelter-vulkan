# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a stress testing application for the Smelter compositor framework. It demonstrates high-throughput video workflows by recording from multiple DeckLink capture devices simultaneously using Vulkan-accelerated H.264 encoding.

**Platform**: Linux only (due to DeckLink SDK and Vulkan video encoding requirements)

## Build and Run Commands

```bash
# Build the project
cargo build --release

# Run the application (records for 60 seconds by default)
cargo run --release

# Build with debug logs
cargo build

# Run with verbose logging
RUST_LOG=debug cargo run

# Clean build artifacts
cargo clean
```

**Important**: After building, you may need to run the libvulkan patch to remove the bundled libvulkan.so.1 that's missing required extensions:

```bash
./patches/libvulkan.sh
```

This prevents CEF's bundled incomplete libvulkan from being loaded instead of the system version.

## Workspace Structure

This is a Cargo workspace with two crates:

- **`crates/smelter-vulkan`**: Main application that integrates winit, WGPU, and Smelter compositor
- **`crates/process_helper`**: Subprocess helper for Chromium/CEF browser processes (required by Smelter's web renderer)

The process_helper is automatically discovered by libcef when placed in the same directory as the main executable.

## Critical Architecture Patterns

### Initialization Order (DO NOT CHANGE)

The initialization sequence follows the one-click-os pattern and MUST be respected:

```
1. WindowManager (winit EventLoop) - MUST BE FIRST
2. Compositor + WGPU Context (via Smelter)
3. Pipeline Start
4. Window Creation
5. Event Loop Execution (blocking)
```

**Why WindowManager must be first**: Platform-specific graphics stack initialization in winit affects Vulkan/WGPU instance creation. Creating WGPU before the event loop can cause subtle initialization issues.

See `crates/smelter-vulkan/src/main.rs:65-114` for the exact sequence.

### Lazy Surface Creation

Surfaces are created in two stages to avoid Vulkan swapchain errors:

1. **Surface creation**: On first window resize (which automatically fires after window creation)
2. **Surface configuration**: On first call to `get_surface_texture()`

This pattern is critical for compatibility with Wayland compositors. See `crates/smelter-vulkan/src/window.rs:42-89`.

### Surface Error Recovery

The application handles three surface invalidation scenarios:

- **Outdated**: Reconfigure surface with current size
- **Lost**: Reconfigure surface and retry
- **Suboptimal**: Drop texture, reconfigure, acquire new texture

See `crates/smelter-vulkan/src/window.rs:76-121`.

### Shared WGPU Context

The same WGPU device and queue are used by:
- Smelter compositor (renders video frames)
- Window manager (presents frames to display)
- Blit pipeline (copies textures to surface)

This minimizes GPU memory transfers and synchronization overhead. The context is created by Smelter and extracted for sharing with the window manager.

## Configuration Constants

Key configuration is in `crates/smelter-vulkan/src/compositor.rs` and `src/main.rs`:

- **Resolution**: `RESOLUTION = (3840, 2160)` - 4K UHD
- **Frame rate**: `FRAME_RATE = 30` - 30 fps
- **Audio sample rate**: `AUDIO_SAMPLE_RATE = 48000` - 48 kHz
- **Recording duration**: `RECORDING_DURATION_SECS = 60` - 60 seconds
- **Max DeckLinks**: `MAX_DECKLINKS_TO_RECORD = 3` - Limit concurrent recordings

Surface configuration (`src/window.rs:130-161`):
- **Present mode**: `AutoNoVsync` - No vsync, immediate presentation
- **Frame latency**: 1 frame maximum
- **Format**: Prefers `Rgba8UnormSrgb`, falls back to any sRGB format

## Smelter Integration

This project uses the Smelter compositor framework at revision `005137d`:

```toml
smelter-core = { git = "https://github.com/software-mansion/smelter.git", rev = "005137d" }
```

**Linux-only features enabled**:
- `decklink` - DeckLink capture device support
- `vk-video` - Vulkan video encoding

**Key Smelter concepts**:
- **Pipeline**: Core compositor engine that processes inputs/outputs
- **Inputs**: Video sources (DeckLink devices in this project)
- **Outputs**: Destinations (MP4 files, raw data for window preview)
- **Components**: Scene graph elements (InputStreamComponent in this project)

### DeckLink Registration

DeckLinks are discovered and registered at startup:
1. Enumerate devices using `decklink::get_decklinks()`
2. Extract `persistent_id` for each device
3. Register each as a `ProtocolInputOptions::DeckLink`
4. Create corresponding outputs for recording

See `crates/smelter-vulkan/src/compositor.rs:101-157`.

### Output Types

Two output types are used:

1. **MP4 Output** (`start_record`): Vulkan H.264 encoding to file
2. **RawData Output** (`register_window_preview_output`): Frames for window display with bounded(1) backpressure

## Blit Pipeline (WGSL Shader)

The window rendering uses a simple fullscreen blit shader (`src/blit.wgsl`):

- Vertex shader generates a fullscreen triangle (no vertex buffer needed)
- Fragment shader samples the compositor frame texture
- Bind group: sampler (binding 0) + texture (binding 1)

The blit pipeline is created lazily on first frame render.

## Error Handling

### Panic Hook

A panic hook is installed at startup (`src/main.rs:42-47`) to ensure clean exit and clear error reporting.

### WGPU Device Errors

WGPU device errors trigger a panic with detailed error info (`src/compositor.rs:56-59`). This is intentional - device errors indicate unrecoverable GPU issues.

### Surface Errors

Surface errors are handled gracefully with automatic reconfiguration. Only fatal errors propagate.

## Thread Architecture

- **Main thread**: Winit event loop (blocking, event-driven)
- **Recording thread**: Started 5 seconds after compositor init to allow pipeline stabilization
- **Smelter internal threads**: Pipeline processing, encoding, device I/O

The 5-second delay before recording start is critical for pipeline stabilization.

## Development Notes

### Known Issues

- CEF bundles an incomplete libvulkan.so.1 missing X11 surface extensions
- Must run `./patches/libvulkan.sh` after building to remove it
- Recording only works on Linux due to DeckLink and vk-video dependencies

### Logging

Logging is configured with:
- Default level: ERROR for most crates
- `smelter_vulkan`: DEBUG
- `smelter_core`, `compositor_pipeline`, `compositor_render`: WARN

Adjust in `src/main.rs:49-52` as needed.

### Testing Without DeckLink Devices

The application will start without DeckLink devices but will only display a window (no recording). The window preview requires at least one DeckLink input.

### Recordings Directory

Recordings are saved to `./recordings/` and follow the naming pattern:
```
recording_decklink_input_<persistent_id>.mp4
```

Existing .mp4 files are cleaned up on startup by the `cleanup()` function.
