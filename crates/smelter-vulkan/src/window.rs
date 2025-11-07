use anyhow::Result;
use smelter_core::protocols::RawDataOutputReceiver;
use std::sync::{Arc, Mutex, Condvar};
use std::os::fd::RawFd;
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

use crate::external_memory::{BridgeTextureImport, import_bridge_texture};

/// Independent WGPU context for window rendering (separate from Smelter's instance)
#[derive(Clone)]
pub struct WgpuContext {
    pub instance: Arc<wgpu::Instance>,
    pub adapter: Arc<wgpu::Adapter>,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
}

/// Synchronization primitive for bridge texture readiness (CPU-based)
pub type BridgeReadySignal = Arc<(Mutex<bool>, Condvar)>;

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
    event_loop: Option<EventLoop<()>>,
}

impl WindowManager {
    /// Create a new WindowManager with an event loop
    /// CRITICAL: This MUST be called before WGPU/Compositor initialization
    pub fn new() -> Result<Self> {
        let event_loop = EventLoop::builder().build()?;
        Ok(Self {
            event_loop: Some(event_loop),
        })
    }

    /// Run the window event loop with bridge texture FD and frame receiver
    ///
    /// Creates an independent WGPU instance and imports the bridge texture
    /// using the provided file descriptor from the compositor.
    ///
    /// The receiver contains frames from the compositor with bounded(1) backpressure.
    /// The window will only redraw when frames are available, and consuming a frame
    /// allows the compositor to produce the next one.
    pub fn run(
        self,
        bridge_memory_fd: RawFd,
        frame_receiver: Option<RawDataOutputReceiver>,
        bridge_ready_signal: BridgeReadySignal,
    ) -> Result<()> {
        let event_loop = self
            .event_loop
            .ok_or_else(|| anyhow::anyhow!("Event loop already consumed"))?;

        event_loop.set_control_flow(ControlFlow::Poll);

        // Create independent WGPU instance for window
        tracing::info!("Creating independent WGPU instance for window...");
        let wgpu_context = Self::create_wgpu_context()?;

        // Import bridge texture from file descriptor
        tracing::info!("Importing bridge texture from FD: {}", bridge_memory_fd);
        let bridge_texture = import_bridge_texture(&wgpu_context.device, bridge_memory_fd)
            .map_err(|e| anyhow::anyhow!("Failed to import bridge texture: {}", e))?;

        let mut app = WindowApp::new(wgpu_context, Some(bridge_texture), frame_receiver, bridge_ready_signal);
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

/// Application handler that manages window lifecycle
struct WindowApp {
    window: Option<WgpuWindow>,
    wgpu_context: Arc<WgpuContext>,
    bridge_texture: Option<BridgeTextureImport>, // Imported bridge texture from compositor
    #[allow(dead_code)]
    frame_receiver: Option<Arc<Mutex<RawDataOutputReceiver>>>, // Reserved for future use
    bridge_ready_signal: BridgeReadySignal, // CPU synchronization signal
    blit_pipeline: Option<BlitPipeline>,
}

impl WindowApp {
    fn new(
        wgpu_context: WgpuContext,
        bridge_texture: Option<BridgeTextureImport>,
        frame_receiver: Option<RawDataOutputReceiver>,
        bridge_ready_signal: BridgeReadySignal,
    ) -> Self {
        Self {
            window: None,
            wgpu_context: Arc::new(wgpu_context),
            bridge_texture,
            frame_receiver: frame_receiver.map(|r| Arc::new(Mutex::new(r))),
            bridge_ready_signal,
            blit_pipeline: None,
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

        Ok(())
    }

    /// Wait for bridge texture to be ready and render it
    ///
    /// The bridge is updated by the compositor in a separate thread.
    /// This method waits (with timeout) for the signal that a new frame
    /// has been copied to the bridge texture, then renders it.
    fn try_render_bridge(&mut self) -> bool {
        // Check if we have a bridge texture
        if self.bridge_texture.is_none() {
            // No bridge texture, nothing to render
            return false;
        }

        // Wait for bridge ready signal with timeout
        // This prevents blocking forever if compositor stops
        let _timed_out = {
            let (lock, cvar) = &*self.bridge_ready_signal;
            let result = cvar.wait_timeout_while(
                lock.lock().unwrap(),
                std::time::Duration::from_millis(100), // 100ms timeout
                |ready| !*ready
            ).unwrap();

            let (mut ready, timeout_result) = result;

            if timeout_result.timed_out() {
                // Timeout - will render what we have
                true
            } else {
                // Got signal - reset ready flag
                *ready = false;
                false
            }
        }; // Lock is dropped here

        // Now we can safely call render_bridge_texture (which needs &mut self)
        if let Err(e) = self.render_bridge_texture() {
            tracing::error!("Failed to render bridge texture: {}", e);
            return false;
        }

        true
    }
}

impl ApplicationHandler for WindowApp {
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

                            // Create blit pipeline now that we have the surface format
                            // Note: We'll still create it lazily on first frame since we need the actual surface texture format
                            // This is kept as-is to maintain the same behavior

                            // Request initial redraw to start frame polling
                            wgpu_window.request_redraw();

                            self.window = Some(wgpu_window);
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
                // Try to render from bridge texture (waits for compositor signal)
                #[allow(unused_variables)]
                let frame_was_rendered = self.try_render_bridge();

                // Always request next redraw to keep polling for frames
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}
