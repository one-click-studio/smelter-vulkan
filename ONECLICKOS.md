# One-Click-OS: Winit + Smelter Integration Analysis

## Overview

This document provides a detailed analysis of how the one-click-os project (`/home/ocs-ubuntu/Desktop/one-click-os`) integrates winit for window management with the Smelter compositor framework. The analysis covers the complete initialization sequence, component dependencies, and architectural patterns.

## Project Architecture

The project uses a multi-crate architecture:
- **app**: Main application entry point
- **window-manager**: Winit integration and window lifecycle management
- **compositor**: Smelter compositor wrapper
- **graphics-core**: WGPU context definitions

## Complete Initialization Sequence

### 1. Main Entry Point

**File:** `crates/app/src/main.rs:61`

```rust
let app = tokio::task::block_in_place(|| App::new(CONFIG.clone(), args.unattended));
```

The initialization is executed synchronously within a Tokio async context to ensure proper ordering.

### 2. Application Initialization (App::new)

**File:** `crates/app/src/app.rs:36-44`

The `App::new()` method orchestrates the initialization in this critical order:

#### Step 2a: WindowManager Creation (MUST BE FIRST)

**File:** `crates/window-manager/src/lib.rs:30-41`

```rust
pub fn new() -> Result<Self, EventLoopError> {
    let winit_event_loop =
        winit::event_loop::EventLoop::<InternalCommand>::with_user_event().build()?;
    Ok(Self { winit_event_loop })
}
```

**Critical Details:**
- **MUST be created before WGPU/Compositor** (warning at line 36)
- Creates winit event loop with custom `InternalCommand` user events
- Uses winit 0.30.11 with wayland and x11 features
- Platform-specific initialization affects graphics stack

**Linux-specific variant:** `new_anythread()` (lines 46-85) for testing environments

#### Step 2b: Compositor Initialization

**File:** `crates/compositor/src/compositor.rs:297-346`

##### 2b.i: WGPU Context via Smelter (line 304-315)

```rust
let graphics_context = smelter_core::graphics_context::GraphicsContext::new(
    smelter_core::graphics_context::GraphicsContextOptions {
        force_gpu: false,
        features: wgpu::Features::empty(),
        limits: wgpu::Limits::default(),
        compatible_surface: None,     // No surface at initialization
        libvulkan_path: None,
        device_id: None,
        driver_name: None,
    },
).context("Cannot initialize WGPU")?;
```

**Key Points:**
- WGPU instance, adapter, device, and queue created through Smelter
- No compatible_surface provided (surfaces created per-window later)
- Uses wgpu 25.0.2
- Devices are Arc-wrapped for thread-safe sharing

##### 2b.ii: Smelter Pipeline Creation (line 317-318)

**File:** `crates/compositor/src/pipeline.rs:134-176`

**Chromium Context Setup (line 139-140):**
```rust
let framerate = smelter_render::Framerate { num: frame_rate, den: 1 };
let chromium_context = ChromiumContext::new(framerate, true)?;
```

**Pipeline Creation (line 142-160):**
```rust
let pipeline = smelter_core::Pipeline::new(smelter_core::PipelineOptions {
    default_buffer_duration: Duration::from_millis(100),
    ahead_of_time_processing: false,
    output_framerate: smelter_render::Framerate { num: frame_rate, den: 1 },
    run_late_scheduled_events: true,
    never_drop_output_frames: false,
    stream_fallback_timeout: Duration::from_millis(500),
    download_root: std::env::temp_dir().into(),
    mixing_sample_rate: 48_000,
    load_system_fonts: false,
    tokio_rt: None,
    rendering_mode: RenderingMode::GpuOptimized,
    wgpu_options: smelter_core::PipelineWgpuOptions::Context(graphics_context),
    chromium_context: Some(chromium_context.clone()),
    whip_whep_server: smelter_core::PipelineWhipWhepServerOptions::Disable,
    whip_whep_stun_servers: Default::default(),
})?;
```

**Configuration Highlights:**
- Framerate: 30fps (default from CompositorOptions)
- Rendering Mode: `GpuOptimized`
- Audio Sample Rate: 48kHz
- WGPU context passed in (shared with window manager)

**Pipeline Start (line 162-164):**
```rust
let events_rx = pipeline.subscribe_pipeline_events();
let pipeline = Arc::new(Mutex::new(pipeline));
smelter_core::Pipeline::start(&pipeline);
```

##### 2b.iii: WGPU Context Extraction (line 321-326)

```rust
let wgpu_context = graphics_core::WgpuContext {
    device: graphics_context.device,
    queue: graphics_context.queue,
    adapter: graphics_context.adapter,
    instance: graphics_context.instance,
};
```

This extracted context is passed to the window manager to share the same WGPU device/queue.

##### 2b.iv-ix: Additional Components (line 327-343)

- FaceZoom manager (line 327)
- FilterRegistry (line 328)
- FaceTracker client (line 329)
- InputCollection (line 331)
- OutputCollection (line 332)
- StyleCollection (line 333)
- Final Compositor::build() (line 335-343)

### 3. Compositor Start (Thread Spawning)

**File:** `crates/compositor/src/compositor.rs:389-412`

```rust
pub fn start(self) -> (CompositorHandle, EventHandle) {
    let (sender, command_rx) = flume::unbounded();
    let mut compositor_inner = self.inner;

    Self::warmup_face_trackers(&compositor_inner);  // Line 393

    thread::spawn(move || 'main: loop {
        while let Ok((msg, callback)) = command_rx.try_recv() {
            match msg {
                Msg::Kill => break 'main,
                Msg::UserMsg(command) => {
                    compositor_inner.command_queue.push_back((command, callback));
                }
            }
        }
        compositor_inner.progress();
        sleep(compositor_inner.options.refresh_rate);  // Default: 5ms
    });

    ((Proxy { tx: sender }).into(), EventHandle { event_loop: self.event_loop })
}
```

**Thread Details:**
- Spawns dedicated compositor thread (line 395)
- 5ms tick rate (line 409)
- Processes command queue
- Runs compositor progress loop

**Compositor Progress Loop** (`CompositorInner::progress()`, line 416-481):
- Process command queue
- Get pipeline events
- Update raw inputs (video/audio frames)
- Update filters
- Handle state transitions
- Update raw outputs
- Emit events

### 4. Compositor Adapter Creation

**File:** `crates/app/src/main.rs:85-91`

```rust
let compositor_adapter = CompositorAdapter::new(
    args.profile,
    compositor_handle,
    window_map.clone(),
    events_handle,
);
```

Loads blank profile and sets up event handling integration.

### 5. Window Manager Startup

**File:** `crates/window-manager/src/lib.rs:89-139`

#### 5a: Create WgpuWindowController (line 98)

**File:** `crates/window-manager/src/render/controller.rs:21-27`

```rust
pub fn new(wgpu_context: Arc<WgpuContext>) -> Self {
    Self { wgpu_context }
}
```

**WGPU Context Conversion** (line 115-121):

**File:** `crates/window-manager/src/render/context.rs:115-121`

```rust
impl From<graphics_core::WgpuContext> for WgpuContext {
    fn from(value: graphics_core::WgpuContext) -> Self {
        let renderer = SurfaceRenderer::new(&device);  // Creates render pipeline
        Self { instance, adapter, device, queue, renderer }
    }
}
```

**Surface Renderer Creation** (line 14-104):
- Creates shader module with WGSL shader (present.wgsl)
- Configures bind group layout for texture + sampler
- Creates render pipeline for texture presentation
- Format: `Bgra8UnormSrgb` (defined in mod.rs:9)
- Uses full-screen triangle technique

#### 5b: Create EventLoop (line 101)

**File:** `crates/window-manager/src/event_loop.rs:116-150`

```rust
pub fn new(
    controller: WgpuWindowController,
    window_map: WindowMap,
    compositor_input: Arc<InputCollection>,
) -> Self {
    let platform = PlatformImpl::new(window_map.clone());  // Line 126
    compositor_input.start();  // Line 140
    Self { controller, windows, connector_to_id, window_map, platform, compositor_input }
}
```

#### 5c: Start Winit Event Loop (line 136)

```rust
self.winit_event_loop
    .run_app_on_demand(&mut window_app)
    .context("Error running winit event loop")
```

## Window Lifecycle (Per Display Connector)

### Phase 1: Window Creation

**Trigger:** `connector_plugged` event

**File:** `crates/window-manager/src/event_loop.rs:162-176`

```rust
fn spawn_window(&mut self, event_loop: &WinitActiveEventLoop,
                connector_id: &ConnectorId) -> Result<(), Box<dyn Error>> {
    let name = connector_id.to_string();
    let window = self.controller.create_window(event_loop, connector_id, name)?;
    let winit_id = window.winit_id();
    self.windows.insert(winit_id, window);
    self.connector_to_id.insert(connector_id.clone(), winit_id);
    Ok(())
}
```

**File:** `crates/window-manager/src/render/window.rs:62-85`

```rust
pub fn create_window(
    &self,
    event_loop: &WinitActiveEventLoop,
    connector_id: &ConnectorId,
    name: String,
) -> Result<WgpuWindow, Box<dyn Error>> {
    let builder = PlatformImpl::setup_window_attributes(event_loop, name)
        .with_theme(Some(Theme::Light))  // Line 71: Force light theme
        .with_decorations(false);        // Line 74: No decorations (invisible on Weston)

    let winit_window = Arc::new(event_loop.create_window(builder)?);

    Ok(WgpuWindow {
        window: winit_window.clone(),
        surface: None,  // Surface NOT created yet
        wgpu_context: self.wgpu_context.clone(),
        connector_id: connector_id.clone(),
    })
}
```

**Window Properties:**
- Theme: Forced to `Light`
- Decorations: Initially disabled (enabled on first resize for dev)
- Window wrapped in `Arc<Window>` for shared ownership
- **Surface is NOT created at this point**

### Phase 2: Surface Creation (Lazy Initialization)

**Trigger:** First window resize event (fires automatically after window creation)

**File:** `crates/window-manager/src/render/window.rs:140-166`

```rust
pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
    // Create surface on first resize if not exists
    if self.surface.is_none() {
        tracing::info!("Creating WGPU surface for connector: {}", self.connector_id);
        let surface = self.wgpu_context.instance
            .create_surface(self.window.clone())
            .unwrap();
        self.surface = Some(surface);
    }

    // Configure surface if it exists
    if let Some(surface) = &self.surface {
        if new_size.width > 0 && new_size.height > 0 {
            configure_surface(surface, new_size, &self.wgpu_context).unwrap();
        }
    }
}
```

**Critical Design Decision (line 182-199):**

The two-step delayed surface creation avoids Vulkan swapchain errors on Weston compositor:

1. Surface created on first resize (always fires after window creation)
2. Surface configured when first texture is requested

### Phase 3: Surface Configuration

**Trigger:** First call to `get_surface_texture()` for rendering

**File:** `crates/window-manager/src/render/window.rs:15-38`

```rust
fn configure_surface(
    surface: &wgpu::Surface,
    size: PhysicalSize<u32>,
    wgpu_context: &WgpuContext,
) -> Result<(), wgpu::SurfaceError> {
    let surface_caps = surface.get_capabilities(&wgpu_context.adapter);
    let surface_format = surface_caps
        .formats
        .iter()
        .find(|f| f.is_srgb())
        .copied()
        .unwrap_or(surface_caps.formats[0]);

    let surface_config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,  // Bgra8UnormSrgb
        width: size.width,
        height: size.height,
        present_mode: wgpu::PresentMode::AutoNoVsync,  // Line 30
        desired_maximum_frame_latency: 1,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: vec![],
    };

    surface.configure(&wgpu_context.device, &surface_config);
    Ok(())
}
```

**Configuration Details:**
- Format: `Bgra8UnormSrgb` (sRGB color space)
- Present Mode: `AutoNoVsync` (no VSync, immediate presentation)
- Frame Latency: 1 frame maximum
- Alpha Mode: Auto

## Vulkan Pipeline Refresh (Handling Suboptimal/Outdated)

**File:** `crates/window-manager/src/render/window.rs:96-137`

The `get_surface_texture()` method handles three scenarios where the Vulkan surface becomes invalid:

### Scenario 1: Outdated Surface (line 117-122)

```rust
Err(wgpu::SurfaceError::Outdated) => {
    tracing::warn!("Outdated WGPU Surface detected, recreating it");
    let size = self.window.inner_size();
    configure_surface(surface, size, &self.wgpu_context).unwrap();
    surface.get_current_texture()?
}
```

**Cause:** Window resize, display mode change, or compositor reconfiguration

**Solution:** Reconfigure surface with current window size

### Scenario 2: Suboptimal Surface (line 128-134)

```rust
if texture.suboptimal {
    tracing::warn!("Sub-optimal WGPU Surface detected, recreating it");
    drop(texture);  // Drop old texture first
    let size = self.window.inner_size();
    configure_surface(surface, size, &self.wgpu_context).unwrap();
    texture = surface.get_current_texture()?;
}
```

**Cause:** Surface configuration no longer optimal for presentation

**Solution:** Drop texture, reconfigure surface, acquire new texture

### Scenario 3: Window Resize (line 155-159 in resize())

```rust
Some(surface) => {
    if new_size.width > 0 && new_size.height > 0 {
        configure_surface(surface, new_size, &self.wgpu_context).unwrap();
    }
}
```

**Cause:** User or system resizes window

**Solution:** Reconfigure surface with new dimensions

### Special Note: Present Warnings Silenced (line 45-49)

```rust
wgpu::SurfaceError::Present(present_error) => {
    // We get these errors due to sharing a queue between the winit event loop
    // and the compositor. We log them but don't propagate the error.
    tracing::trace!("Present warning: {}", present_error);
    return Ok(texture);
}
```

Present warnings are logged but ignored because the WGPU queue is shared between winit and the compositor.

## Component Dependencies

### Critical Initialization Order

```
WindowManager → WGPU Context (via Smelter) → Compositor → Pipeline
```

1. **WindowManager MUST be first** (warning at `window-manager/src/lib.rs:36`)
   - Reason: Platform-specific graphics stack initialization
   - Affects Vulkan/WGPU instance creation

2. **WGPU Context created by Compositor**
   - Single shared instance, adapter, device, queue
   - Extracted and passed to window manager

3. **Compositor requires Pipeline + ChromiumContext**
   - Pipeline requires WGPU context
   - ChromiumContext needed for web rendering

### Frame Flow Architecture

```
Compositor Output → FrameStream (tokio::sync::watch)
                     ↓
              InputCollection (monitors streams)
                     ↓
              EventLoop (via InternalCommand::RequestRedraw)
                     ↓
              Window (presents texture from shared WGPU queue)
```

**Key Components:**

1. **FrameStream** (`compositor/src/raw_inputs.rs`):
   - Tokio watch channel wrapping `Option<Arc<Texture>>`
   - Compositor writes output frames
   - InputCollection subscribes to changes

2. **InputCollection** (`window-manager/src/inputs/input_collection.rs:58-90`):
   - Spawns tokio tasks monitoring each connector's FrameStream
   - Sends `InternalCommand::RequestRedraw` to event loop when new frame available

3. **EventLoop** (`window-manager/src/event_loop.rs:179-260`):
   - Receives redraw commands via custom user events
   - Calls `window.request_redraw()` for appropriate window

4. **Window Rendering** (`window-manager/src/render/window.rs:168-208`):
   - Gets surface texture
   - Renders compositor frame using SurfaceRenderer
   - Presents to display

### Thread Architecture

| Thread | Purpose | Rate | File |
|--------|---------|------|------|
| Main | Winit event loop (blocking) | Event-driven | `window-manager/src/lib.rs:136` |
| Compositor | Command processing, progress loop | 5ms tick | `compositor/src/compositor.rs:395` |
| InputCollection | Async frame monitoring | On frame change | `window-manager/src/inputs/input_collection.rs:58` |
| Smelter Pipeline | Internal video processing | 30fps (default) | Smelter internals |

### WGPU Context Sharing

The same WGPU device and queue are used by:

1. **Compositor (Smelter)**: Renders video frames to textures
2. **Window Manager**: Presents textures to window surfaces
3. **Surface Renderer**: Blits textures to screen

This design minimizes GPU memory transfers and synchronization overhead.

## Key Configuration Values

| Setting | Value | Location |
|---------|-------|----------|
| Frame Rate | 30fps | `compositor/src/options.rs` (default) |
| Compositor Tick Rate | 5ms | `compositor/src/options.rs:9` |
| Audio Sample Rate | 48kHz | `compositor/src/pipeline.rs:151` |
| Surface Format | Bgra8UnormSrgb | `window-manager/src/render/mod.rs:9` |
| Present Mode | AutoNoVsync | `window-manager/src/render/window.rs:30` |
| Frame Latency | 1 frame | `window-manager/src/render/window.rs:31` |
| Rendering Mode | GpuOptimized | `compositor/src/pipeline.rs:155` |

## Key Files Reference

| File | Lines | Purpose |
|------|-------|---------|
| `app/src/main.rs` | 61, 203 | Entry point, initialization orchestration |
| `app/src/app.rs` | 36-44, 64-75 | App struct, coordinates components |
| `window-manager/src/lib.rs` | 30-41, 89-139 | Winit event loop setup |
| `window-manager/src/event_loop.rs` | 116-176, 179-260 | Window lifecycle, event handling |
| `window-manager/src/render/window.rs` | 62-85, 96-137, 140-166, 168-208 | Window creation, surface management, refresh, rendering |
| `window-manager/src/render/context.rs` | 14-104, 115-121 | Render pipeline, context conversion |
| `window-manager/src/render/controller.rs` | 21-27, 39-81 | Window controller |
| `window-manager/src/inputs/input_collection.rs` | 58-90 | Frame monitoring and redraw triggering |
| `compositor/src/compositor.rs` | 297-346, 389-412, 416-481 | Compositor setup, start, main loop |
| `compositor/src/pipeline.rs` | 134-176 | Smelter pipeline initialization |
| `compositor/src/raw_inputs.rs` | Frame stream implementation |
| `graphics-core/src/context.rs` | 14-60 | WGPU context definitions |

## Summary

The one-click-os project demonstrates a sophisticated integration of winit and Smelter:

1. **Careful Initialization Order**: WindowManager → WGPU → Compositor → Pipeline
2. **Lazy Surface Creation**: Surfaces created on first resize to avoid Weston issues
3. **Shared WGPU Context**: Single device/queue used by both compositor and window manager
4. **Robust Error Handling**: Handles outdated/suboptimal surfaces gracefully
5. **Thread Coordination**: Multiple threads (main event loop, compositor, input monitoring) work together
6. **Efficient Frame Flow**: Watch channels and custom events minimize latency

This architecture enables real-time video composition and presentation with minimal overhead and robust error recovery.
