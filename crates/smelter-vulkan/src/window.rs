use anyhow::Result;
use smelter_core::protocols::RawDataOutputReceiver;
use std::sync::Arc;
use std::os::fd::RawFd;
use std::time::{Instant, Duration};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowId},
};

use crate::external_memory::{BridgeTextureImport, import_bridge_texture};

/// Command sent from compositor thread to window
#[derive(Debug, Clone)]
pub enum WindowCommand {
    /// Request a redraw with a new frame available
    RequestRedraw,
}

/// Type alias for the event loop proxy
pub type WindowCommandProxy = EventLoopProxy<WindowCommand>;

/// Independent WGPU context for window rendering (separate from Smelter's instance)
#[derive(Clone)]
pub struct WgpuContext {
    pub instance: Arc<wgpu::Instance>,
    pub adapter: Arc<wgpu::Adapter>,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
}

/// Manages a single window with its surface
pub struct WgpuWindow {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    wgpu_context: Arc<WgpuContext>,
}

impl WgpuWindow {
    /// Create a new window with its surface (eager initialization)
    pub fn new(window: Window, wgpu_context: Arc<WgpuContext>) -> Result<Self> {
        let window_arc = Arc::new(window);

        // Create and configure surface immediately
        tracing::info!("Creating WGPU surface");
        let surface = wgpu_context.instance.create_surface(window_arc.clone())?;

        let size = window_arc.inner_size();
        if size.width > 0 && size.height > 0 {
            tracing::info!("Configuring surface: {}x{}", size.width, size.height);
            configure_surface(&surface, size, &wgpu_context)?;
        }

        Ok(Self {
            window: window_arc,
            surface,
            wgpu_context,
        })
    }

    /// Resize the window and reconfigure the surface
    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            if let Err(e) = configure_surface(&self.surface, new_size, &self.wgpu_context) {
                tracing::error!("Failed to reconfigure surface on resize: {}", e);
            }
        }
    }

    /// Get the current surface texture, handling errors and suboptimal surfaces
    pub fn get_surface_texture(&mut self) -> Result<wgpu::SurfaceTexture> {
        // Try to get the current texture
        let mut texture = match self.surface.get_current_texture() {
            Ok(texture) => texture,
            Err(wgpu::SurfaceError::Outdated) => {
                // Surface is outdated, reconfigure and retry
                tracing::warn!("Outdated WGPU Surface detected, recreating it");
                let size = self.window.inner_size();
                configure_surface(&self.surface, size, &self.wgpu_context)?;
                self.surface.get_current_texture()?
            }
            Err(wgpu::SurfaceError::Lost) => {
                // Surface is lost, reconfigure and retry
                tracing::warn!("Lost WGPU Surface detected, recreating it");
                let size = self.window.inner_size();
                configure_surface(&self.surface, size, &self.wgpu_context)?;
                self.surface.get_current_texture()?
            }
            Err(e) => return Err(anyhow::anyhow!("Failed to get surface texture: {}", e)),
        };

        // Check if the texture is suboptimal
        if texture.suboptimal {
            tracing::warn!("Sub-optimal WGPU Surface detected, recreating it");
            drop(texture);
            let size = self.window.inner_size();
            configure_surface(&self.surface, size, &self.wgpu_context)?;
            texture = self.surface.get_current_texture()?;
        }

        Ok(texture)
    }

    /// Request a redraw of the window
    pub fn request_redraw(&self) {
        self.window.request_redraw();
    }
}

/// Configure a surface with the proper settings
fn configure_surface(
    surface: &wgpu::Surface,
    size: PhysicalSize<u32>,
    wgpu_context: &WgpuContext,
) -> Result<()> {
    let surface_caps = surface.get_capabilities(&wgpu_context.adapter);

    // Try to use Rgba8UnormSrgb to match the frame format, otherwise use any sRGB format
    let surface_format = surface_caps
        .formats
        .iter()
        .find(|f| **f == wgpu::TextureFormat::Rgba8UnormSrgb)
        .or_else(|| surface_caps.formats.iter().find(|f| f.is_srgb()))
        .copied()
        .unwrap_or(surface_caps.formats[0]);

    let surface_config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,
        width: size.width,
        height: size.height,
        present_mode: wgpu::PresentMode::AutoVsync,
        desired_maximum_frame_latency: 1,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: vec![],
    };

    surface.configure(&wgpu_context.device, &surface_config);
    Ok(())
}

/// Manages the application window using winit
/// MUST be created BEFORE compositor initialization
pub struct WindowManager {
    event_loop: Option<EventLoop<WindowCommand>>,
    proxy: Option<WindowCommandProxy>,
    enable_frame_stats: bool,
    target_fps: f64,
}

impl WindowManager {
    /// Create a new WindowManager with an event loop
    /// CRITICAL: This MUST be called before WGPU/Compositor initialization
    pub fn new(enable_frame_stats: bool, target_fps: f64) -> Result<Self> {
        let event_loop = EventLoop::<WindowCommand>::with_user_event().build()?;
        let proxy = event_loop.create_proxy();
        Ok(Self {
            event_loop: Some(event_loop),
            proxy: Some(proxy),
            enable_frame_stats,
            target_fps,
        })
    }

    /// Get the event loop proxy for sending commands
    pub fn proxy(&self) -> Option<WindowCommandProxy> {
        self.proxy.clone()
    }

    /// Run the window event loop with bridge texture FD
    ///
    /// Creates an independent WGPU instance and imports the bridge texture
    /// using the provided file descriptor from the compositor.
    ///
    /// The event loop runs at a fixed frame rate using ControlFlow::WaitUntil.
    pub fn run(
        self,
        bridge_memory_fd: RawFd,
        frame_receiver: Option<RawDataOutputReceiver>,
    ) -> Result<()> {
        let event_loop = self
            .event_loop
            .ok_or_else(|| anyhow::anyhow!("Event loop already consumed"))?;

        // Create independent WGPU instance for window
        tracing::info!("Creating independent WGPU instance for window...");
        let wgpu_context = Self::create_wgpu_context()?;

        // Import bridge texture from file descriptor
        tracing::info!("Importing bridge texture from FD: {}", bridge_memory_fd);
        let bridge_texture = import_bridge_texture(&wgpu_context.device, bridge_memory_fd)
            .map_err(|e| anyhow::anyhow!("Failed to import bridge texture: {}", e))?;

        let target_frame_duration = Duration::from_secs_f64(1.0 / self.target_fps);
        tracing::info!("Window manager configured for {:.1} FPS (frame duration: {:.2}ms)",
            self.target_fps,
            target_frame_duration.as_secs_f64() * 1000.0);

        let mut app = WindowApp::new(
            wgpu_context,
            Some(bridge_texture),
            frame_receiver,
            self.enable_frame_stats,
            target_frame_duration,
        );

        event_loop.run_app(&mut app)?;

        Ok(())
    }

    /// Create an independent WGPU context (instance, adapter, device, queue)
    fn create_wgpu_context() -> Result<WgpuContext> {
        // Create WGPU instance
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..Default::default()
        });

        // Request adapter
        let adapter_result = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }));

        let adapter = adapter_result.map_err(|e| anyhow::anyhow!("Failed to request adapter: {:?}", e))?;

        tracing::info!(
            "Window using GPU: {:?} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );

        // Request device and queue
        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("Window Device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
                trace: wgpu_types::Trace::Off,
            },
        ))
        .map_err(|e| anyhow::anyhow!("Failed to create device: {}", e))?;

        // Set up device error handler
        device.on_uncaptured_error(Box::new(|error| {
            tracing::error!("Window WGPU Device Error: {:?}", error);
            panic!("Window WGPU device error detected: {:?}", error);
        }));

        Ok(WgpuContext {
            instance: Arc::new(instance),
            adapter: Arc::new(adapter),
            device: Arc::new(device),
            queue: Arc::new(queue),
        })
    }
}

/// Blit pipeline for rendering frames to surface
struct BlitPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// Frame timing statistics
struct FrameStats {
    last_frame_time: Option<Instant>,
    frame_count: u64,
    total_frame_time: f64,
    min_frame_time: f64,
    max_frame_time: f64,
    last_log_time: Instant,
}

impl FrameStats {
    fn new() -> Self {
        Self {
            last_frame_time: None,
            frame_count: 0,
            total_frame_time: 0.0,
            min_frame_time: f64::MAX,
            max_frame_time: 0.0,
            last_log_time: Instant::now(),
        }
    }

    /// Record a frame presentation and return the interval since last frame
    fn record_frame(&mut self, enable_frame_stats: bool) -> Option<f64> {
        // Early return if stats are disabled
        if !enable_frame_stats {
            return None;
        }

        let now = Instant::now();
        let interval = self.last_frame_time.map(|last| now.duration_since(last).as_secs_f64());

        if let Some(dt) = interval {
            self.frame_count += 1;
            self.total_frame_time += dt;
            self.min_frame_time = self.min_frame_time.min(dt);
            self.max_frame_time = self.max_frame_time.max(dt);

            // Log every second
            if now.duration_since(self.last_log_time).as_secs_f64() >= 1.0 {
                let avg_frame_time = self.total_frame_time / self.frame_count as f64;
                let avg_fps = 1.0 / avg_frame_time;

                tracing::info!(
                    "Frame stats: avg={:.1}fps ({:.2}ms) min={:.2}ms max={:.2}ms count={}",
                    avg_fps,
                    avg_frame_time * 1000.0,
                    self.min_frame_time * 1000.0,
                    self.max_frame_time * 1000.0,
                    self.frame_count
                );

                // Detect irregularities
                if self.max_frame_time > 2.0 * avg_frame_time {
                    tracing::warn!(
                        "Frame time irregularity detected: max frame time ({:.2}ms) is >2x average ({:.2}ms)",
                        self.max_frame_time * 1000.0,
                        avg_frame_time * 1000.0
                    );
                }

                // Reset stats for next interval
                self.frame_count = 0;
                self.total_frame_time = 0.0;
                self.min_frame_time = f64::MAX;
                self.max_frame_time = 0.0;
                self.last_log_time = now;
            }
        }

        self.last_frame_time = Some(now);
        interval
    }
}

/// Application handler that manages window lifecycle
struct WindowApp {
    window: Option<WgpuWindow>,
    wgpu_context: Arc<WgpuContext>,
    bridge_texture: Option<BridgeTextureImport>, // Imported bridge texture from compositor
    #[allow(dead_code)]
    frame_receiver: Option<RawDataOutputReceiver>, // Reserved for future use
    blit_pipeline: Option<BlitPipeline>,
    frame_stats: FrameStats, // Frame timing statistics
    enable_frame_stats: bool,
    target_frame_duration: Duration, // Target duration between frames
    next_frame_time: Instant, // Next scheduled frame time
}

impl WindowApp {
    fn new(
        wgpu_context: WgpuContext,
        bridge_texture: Option<BridgeTextureImport>,
        frame_receiver: Option<RawDataOutputReceiver>,
        enable_frame_stats: bool,
        target_frame_duration: Duration,
    ) -> Self {
        Self {
            window: None,
            wgpu_context: Arc::new(wgpu_context),
            bridge_texture,
            frame_receiver,
            blit_pipeline: None,
            frame_stats: FrameStats::new(),
            enable_frame_stats,
            target_frame_duration,
            next_frame_time: Instant::now(),
        }
    }

    fn create_blit_pipeline(&self, surface_format: wgpu::TextureFormat) -> BlitPipeline {
        let shader = self.wgpu_context.device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Blit Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
        });

        let bind_group_layout = self.wgpu_context.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Blit Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = self.wgpu_context.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Blit Pipeline Layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = self.wgpu_context.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Blit Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let sampler = self.wgpu_context.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Blit Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        BlitPipeline {
            pipeline,
            bind_group_layout,
            sampler,
        }
    }

    /// Render the bridge texture to the window surface
    /// This is called when the bridge is ready (compositor has copied a frame)
    fn render_bridge_texture(&mut self) -> Result<()> {
        let window = self
            .window
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Window not created yet"))?;

        let bridge_texture = self
            .bridge_texture
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Bridge texture not available"))?;

        // Get the surface texture
        let surface_texture = window.get_surface_texture()?;

        // Create blit pipeline if not already created
        if self.blit_pipeline.is_none() {
            let surface_format = surface_texture.texture.format();
            self.blit_pipeline = Some(self.create_blit_pipeline(surface_format));
        }

        let blit = self.blit_pipeline.as_ref().unwrap();

        // Create bind group for bridge texture
        let bridge_view = bridge_texture.wgpu_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.wgpu_context.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Bridge Blit Bind Group"),
            layout: &blit.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&blit.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&bridge_view),
                },
            ],
        });

        // Render the bridge texture using a fullscreen blit
        let surface_view = surface_texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.wgpu_context.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("Bridge Blit Encoder"),
            },
        );

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Bridge Blit Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &surface_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            render_pass.set_pipeline(&blit.pipeline);
            render_pass.set_bind_group(0, &bind_group, &[]);
            render_pass.draw(0..3, 0..1); // Draw fullscreen triangle
        }

        // Submit the command buffer
        self.wgpu_context.queue.submit(Some(encoder.finish()));

        // Present the frame
        surface_texture.present();

        // Record frame timing
        if self.enable_frame_stats {
            let interval = self.frame_stats.record_frame(self.enable_frame_stats);
            if let Some(dt) = interval {
                // Log individual frame times if they're unusually long (>50ms gap)
                if dt > 0.05 {
                    tracing::warn!("Large frame gap detected: {:.2}ms since last present", dt * 1000.0);
                }
            }
        }

        Ok(())
    }

}

impl ApplicationHandler<WindowCommand> for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let window_attributes = Window::default_attributes()
                .with_title("Smelter Vulkan")
                .with_inner_size(PhysicalSize::new(1920, 1080));

            match event_loop.create_window(window_attributes) {
                Ok(window) => {
                    tracing::info!("Window created successfully");

                    // Create window with surface (eager initialization)
                    match WgpuWindow::new(window, self.wgpu_context.clone()) {
                        Ok(wgpu_window) => {
                            tracing::info!("Surface created and configured");

                            self.window = Some(wgpu_window);

                            // Initialize the frame timing for the event loop
                            self.next_frame_time = Instant::now() + self.target_frame_duration;
                            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame_time));
                        }
                        Err(e) => {
                            tracing::error!("Failed to create window surface: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to create window: {}", e);
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Check if it's time for the next frame
        let now = Instant::now();
        if now >= self.next_frame_time {
            // Request redraw
            if let Some(window) = &self.window {
                window.request_redraw();
            }

            // Schedule next frame
            self.next_frame_time += self.target_frame_duration;
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame_time));
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                tracing::info!("Close requested, exiting");
                event_loop.exit();
            }
            WindowEvent::Resized(new_size) => {
                if let Some(window) = &mut self.window {
                    window.resize(new_size);
                }
            }
            WindowEvent::RedrawRequested => {
                // Render bridge texture if available
                if self.bridge_texture.is_some() {
                    if let Err(e) = self.render_bridge_texture() {
                        tracing::error!("Failed to render bridge texture: {}", e);
                    }
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: WindowCommand) {
        match event {
            WindowCommand::RequestRedraw => {
                // Compositor has written a new frame to the bridge texture
                // Note: Actual redraw is triggered by the fixed-rate timer in RedrawRequested
                // This event is kept for compatibility but doesn't trigger immediate redraws
            }
        }
    }
}
