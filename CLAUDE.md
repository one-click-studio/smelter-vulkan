# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust project that integrates with the Smelter compositor framework for DeckLink audio/video processing. The project uses `smelter-core` and `smelter-render` as external dependencies from the Software Mansion Smelter repository.

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

- **`src/main.rs`**: Entry point that initializes tracing/logging and creates a `Compositor` instance. The logging configuration filters out verbose output from `compositor_pipeline` and `compositor_render` modules while keeping `smelter_decklink_audio` logs at info level.

- **`src/compositor.rs`**: Contains the `Compositor` struct which manages:
  - `GraphicsContext`: Handles wgpu graphics context initialization (required by Smelter)
  - `Pipeline`: The core Smelter compositor pipeline wrapped in `Arc<Mutex<>>` for thread-safe access
  - `OutputId`: Identifies compositor outputs (e.g., "raw_output")

  The `Compositor::new()` method is responsible for:
  1. Initializing the graphics context
  2. Creating and starting the pipeline
  3. Registering DeckLink devices

  The `record_main_output()` method handles video recording to file with a specified duration.

### External Dependencies

- **Smelter libraries** (`smelter-core`, `smelter-render`, `decklink`): Pinned to tag `v0.5.0` from the Software Mansion Smelter repository
  - `smelter-core`: Provides the core compositor functionality with DeckLink feature enabled
  - `smelter-render`: Provides rendering capabilities with web-renderer feature enabled
  - `decklink`: Optional DeckLink hardware integration

- **wgpu 25.0.2**: Graphics library required by the compositor for GPU operations

### Current State

The compositor implementation contains `todo!()` placeholders in `compositor.rs` for:
- Graphics context initialization (line 22)
- Pipeline creation and startup (line 25)
- DeckLink device registration (line 28)
- Recording functionality (line 38)

## Resolution Constants

The project defines `RESOLUTION_FHD` constant as `(1920, 1080)` in `compositor.rs:11` for Full HD video resolution.
