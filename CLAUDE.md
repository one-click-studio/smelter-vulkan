# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust project that integrates with the Smelter compositor framework for video processing. The project uses `smelter-core` and `smelter-render` as external dependencies from the Software Mansion Smelter repository.

## Build Commands

- **Build the project**: `cargo build`
- **Run the project**: `cargo run`
- **Check code (fast compile check)**: `cargo check`
- **Run tests**: `cargo test`
- **Lint code**: `cargo clippy`
- **Format code**: `cargo fmt`

### Testing specific items
- **Run a single test**: `cargo test test_name`
- **Run tests matching a pattern**: `cargo test pattern_name`

## Architecture

### Core Components

The codebase is structured around a compositor abstraction that wraps the Smelter framework:

- **`src/main.rs`**: Entry point that initializes tracing/logging and creates a `Compositor` instance.

- **`src/compositor.rs`**: Contains the `Compositor` struct which manages:
  - `GraphicsContext`: Handles wgpu graphics context initialization (required by Smelter)
  - `Pipeline`: The core Smelter compositor pipeline wrapped in `Arc<Mutex<>>` for thread-safe access
  - `InputId`: Identifies the MP4 input source
  - `OutputId`: Identifies compositor outputs

  The `Compositor::new(input_path)` method is responsible for:
  1. Initializing the graphics context
  2. Creating and starting the pipeline
  3. Registering the MP4 input source

  The `start_recording()` method handles video recording to file with a specified duration.

- **`src/assets.rs`**: Handles downloading test assets (MP4 input file) if not present locally.

### External Dependencies

- **Smelter libraries** (`smelter-core`, `smelter-render`): Pinned to tag `v0.5.0` from the Software Mansion Smelter repository
  - `smelter-core`: Provides the core compositor functionality with vk-video feature enabled
  - `smelter-render`: Provides rendering capabilities with web-renderer feature enabled

- **wgpu 25.0.2**: Graphics library required by the compositor for GPU operations

- **reqwest**: HTTP client for downloading test assets

## Resolution Constants

The project defines `RESOLUTION` constant as `(3840, 2160)` in `compositor.rs` for 4K video resolution.
