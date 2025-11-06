# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust workspace project that integrates winit for window management with the Smelter compositor framework for DeckLink audio/video processing. The project uses `smelter-core` and `smelter-render` as external dependencies from the Software Mansion Smelter repository.

The architecture follows the one-click-os pattern with careful initialization ordering and shared WGPU context between the window manager and compositor.

### Workspace Structure

The project is organized as a Cargo workspace with two crates:

1. **`crates/smelter-vulkan`**: Main application binary
   - Window management with winit
   - Compositor integration
   - DeckLink video recording
   - Entry point and initialization logic

2. **`crates/process_helper`**: CEF subprocess helper binary
   - Required by Chromium/CEF for multi-process architecture
   - Spawned by CEF for renderer, GPU, and utility processes
   - Must be built before the main application

## Build Commands

**IMPORTANT**: Build `process_helper` first before building the main application!

```bash
# Build process_helper (required first step)
cargo build -p process_helper

# Build the main application
cargo build -p smelter_vulkan

# Or build everything at once
cargo build

# Build for release
cargo build -p process_helper --release
cargo build -p smelter_vulkan --release
```

### Running the Application

```bash
# Build both binaries first
cargo build -p process_helper
cargo build -p smelter_vulkan

# Run the main application (default workspace member)
cargo run

# Or run directly
./target/debug/smelter_vulkan
```

**Note**: `cargo run` without `-p` flag runs `smelter_vulkan` by default due to `default-members` in workspace `Cargo.toml`.

### Vulkan Library Patch

After building `process_helper`, you may need to run the libvulkan patch script:

```bash
# Build process_helper first
cargo build -p process_helper

# Run the patch to remove bundled libvulkan
./patches/libvulkan.sh

# Then build and run the main application
cargo build
cargo run
```

**Why this is needed:**
- CEF (Chromium) bundles its own `libvulkan.so.1` in `target/{debug|release}/lib/`
- This bundled version is missing required extensions like `VK_KHR_xlib_surface` for X11 window display
- The patch removes the bundled library so the system's full Vulkan library is used instead
- See `patches/libvulkan.sh` for details

### Other Commands

- **Check code (fast compile check)**: `cargo check`
- **Run tests**: `cargo test`
- **Lint code**: `cargo clippy`
- **Format code**: `cargo fmt`

### Testing specific items
- **Run a single test**: `cargo test test_name`
- **Run tests matching a pattern**: `cargo test pattern_name`

## Architecture

### Critical Initialization Order

The project follows a strict initialization sequence (see `src/main.rs:48-79`):

1. **WindowManager creation** (`src/window.rs:151`) - **MUST BE FIRST**
   - Creates winit event loop before any graphics initialization
   - Critical for proper platform-specific graphics stack setup

2. **Compositor initialization** (`src/compositor.rs:43`)
   - Initializes WGPU context via Smelter's GraphicsContext
   - Creates ChromiumContext for web rendering
   - Creates and starts Pipeline
   - Registers DeckLink devices
   - Returns both Compositor and WgpuContext

3. **Recording thread spawn** (`src/main.rs:65`)
   - Starts video recording in separate thread
   - Records from all detected DeckLink inputs

4. **Window event loop** (`src/main.rs:79`)
   - Blocking call to run the window manager
   - Uses shared WGPU context from compositor

### Core Components

- **`src/main.rs`**: Entry point that orchestrates the initialization sequence. Implements proper ordering: WindowManager → Compositor → Recording Thread → Event Loop. Logging filters out verbose output from `compositor_pipeline`, `compositor_render`, and `smelter_core`.

- **`src/window.rs`**: Window management using winit with lazy surface initialization
  - `WindowManager`: Wraps winit event loop (must be created first)
  - `WgpuContext`: Shared WGPU resources (instance, adapter, device, queue)
  - `WgpuWindow`: Individual window with lazy surface creation
  - `configure_surface()`: Surface configuration with Bgra8UnormSrgb format, AutoNoVsync present mode, 1 frame latency

  **Lazy Surface Initialization** (three-phase approach):
  1. Window creation (`src/window.rs:194`) - no surface yet
  2. Surface creation on first resize (`src/window.rs:40-50`)
  3. Surface configuration on first texture request (`src/window.rs:69-71`)

  **Vulkan Pipeline Refresh** (`src/window.rs:64-104`):
  - Handles `SurfaceError::Outdated` - reconfigures surface
  - Handles `SurfaceError::Lost` - reconfigures surface
  - Detects suboptimal surfaces - drops texture and reconfigures
  - Automatically retries texture acquisition after reconfiguration

- **`src/compositor.rs`**: Compositor wrapper for Smelter
  - `Compositor::new()`: Returns tuple of (Compositor, WgpuContext)
  - Initializes GraphicsContext through Smelter
  - Creates ChromiumContext for web rendering (30fps)
  - Configures Pipeline with:
    - Frame rate: 30fps (`compositor.rs:31`)
    - Audio sample rate: 48kHz (`compositor.rs:32`)
    - Rendering mode: GpuOptimized
    - 100ms buffer duration
    - 500ms stream fallback timeout
  - Registers all detected DeckLink devices
  - Records video with H.264 encoding at 4K resolution (3840x2160)

### External Dependencies

- **Smelter libraries** (`smelter-core`, `smelter-render`, `decklink`): Pinned to revision `005137d` from the Software Mansion Smelter repository
  - `smelter-core`: Provides the core compositor functionality with DeckLink and vk-video features
  - `smelter-render`: Provides rendering capabilities with web-renderer feature
  - `decklink`: DeckLink hardware integration (Linux only)

- **wgpu 25.0.2**: Graphics library required by the compositor for GPU operations
- **winit 0.30.12**: Cross-platform window management and event handling

### Process Helper Binary

**Location**: `crates/process_helper/src/main.rs`

The `process_helper` is a separate binary required by Chromium Embedded Framework (CEF) for its multi-process architecture:

**Purpose**:
- CEF spawns multiple `process_helper` subprocesses for different tasks:
  - **Renderer Process**: Executes JavaScript, renders HTML/CSS
  - **GPU Process**: Handles hardware-accelerated rendering
  - **Utility Processes**: Handle various browser tasks

**Implementation**:
```rust
fn main() -> Result<(), Box<dyn Error>> {
    let exit_code = smelter_render::web_renderer::process_helper::run_process_helper()?;
    std::process::exit(exit_code);
}
```

**Discovery**:
- Smelter/libcef automatically discovers `process_helper` in the same directory as the main executable
- See `libcef/src/settings.rs:executables_paths()` for discovery logic
- No manual environment variable configuration needed in production
- The build script (`crates/smelter-vulkan/build.rs`) ensures it's built before the main application

**Build Requirements**:
- Must be built with the same profile (debug/release) as the main application
- Located at `target/{debug|release}/process_helper`
- Build script will panic with helpful message if not found

### WGPU Context Sharing

The same WGPU device and queue are shared between:
1. **Compositor (Smelter)**: Renders video frames to textures
2. **Window Manager**: Presents textures to window surfaces

This design minimizes GPU memory transfers and synchronization overhead.

### Window Lifecycle

Each window follows a three-phase initialization:

1. **Window Creation** (`window.rs:194`):
   - Creates winit window with default attributes
   - Sets title to "Smelter Vulkan"
   - Sets initial size to 1920x1080
   - Surface is NOT created yet

2. **Lazy Surface Creation** (`window.rs:40-50`):
   - Triggered on first resize event (fires automatically)
   - Creates surface from window using shared WGPU instance
   - Avoids Vulkan swapchain errors on some compositors

3. **Surface Configuration** (`window.rs:113-139`):
   - Configures surface on first texture request
   - Format: Bgra8UnormSrgb (sRGB color space)
   - Present mode: AutoNoVsync (immediate presentation)
   - Frame latency: 1 frame maximum

## Configuration Constants

- `RESOLUTION`: 4K (3840x2160) - `compositor.rs:30`
- `FRAME_RATE`: 30fps - `compositor.rs:31`
- `AUDIO_SAMPLE_RATE`: 48kHz - `compositor.rs:32`
- `RECORDING_DURATION_SECS`: 60 seconds - `main.rs:8`

## Implementation Notes

- The WindowManager MUST be created before any WGPU/Compositor initialization (see `window.rs:150`)
- Surface creation is delayed to first resize event to avoid Vulkan errors
- Surface errors (Outdated, Lost, Suboptimal) are automatically handled with reconfiguration
- ChromiumContext is initialized for web rendering support
- All DeckLink devices are automatically detected and registered
- Recording runs in a separate thread to avoid blocking the event loop
- **Important**: Run `./patches/libvulkan.sh` after building `process_helper` to remove CEF's bundled libvulkan which lacks required X11 extensions
