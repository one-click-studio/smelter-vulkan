use anyhow::Result;
use smelter_core::{protocols::RawDataOutputReceiver, PipelineEvent};
use smelter_render::Frame;
use std::sync::{Arc, Mutex};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

/// Shared WGPU context for rendering
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
    surface: Option<wgpu::Surface<'static>>,
    surface_configured: bool,
    wgpu_context: Arc<WgpuContext>,
}

impl WgpuWindow {
    /// Create a new window without a surface (lazy initialization)
    pub fn new(window: Window, wgpu_context: Arc<WgpuContext>) -> Self {
        Self {
            window: Arc::new(window),
            surface: None,
            surface_configured: false,
            wgpu_context,
        }
    }

    /// Resize the window and configure/create the surface if needed
    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        match &self.surface {
            None => {
                // Create the surface on first resize (which always fires when window is created)
                tracing::info!("Creating WGPU surface on first resize");
                match self.wgpu_context.instance.create_surface(self.window.clone()) {
                    Ok(surface) => {
                        self.surface = Some(surface);
                        // Note: Surface is NOT configured here - it will be configured
                        // on first call to get_surface_texture()
                    }
                    Err(e) => {
                        tracing::error!("Failed to create surface: {}", e);
                    }
                }
            }
            Some(surface) => {
                // Only reconfigure if surface was already configured and size changed
                if self.surface_configured && new_size.width > 0 && new_size.height > 0 {
                    if let Err(e) = configure_surface(surface, new_size, &self.wgpu_context) {
                        tracing::error!("Failed to reconfigure surface on resize: {}", e);
                    }
                }
            }
        }
    }

    /// Check if the window is ready for rendering
    /// Returns true only after the surface has been created (on first resize)
    pub fn is_ready(&self) -> bool {
        self.surface.is_some()
    }

    /// Get the current surface texture, handling errors and suboptimal surfaces
    pub fn get_surface_texture(&mut self) -> Result<wgpu::SurfaceTexture> {
        let surface = self.surface.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Window is not ready to display textures yet"))?;

        // Configure the surface on first access (lazy configuration)
        // This is done here rather than in resize() to avoid Vulkan swapchain errors
        if !self.surface_configured {
            let size = self.window.inner_size();
            if size.width > 0 && size.height > 0 {
                tracing::info!("Configuring surface on first texture access: {}x{}", size.width, size.height);
                configure_surface(surface, size, &self.wgpu_context)?;
                self.surface_configured = true;
            }
        }

        // Try to get the current texture
        let mut texture = match surface.get_current_texture() {
            Ok(texture) => texture,
            Err(wgpu::SurfaceError::Outdated) => {
                // Surface is outdated, reconfigure and retry
                tracing::warn!("Outdated WGPU Surface detected, recreating it");
                let size = self.window.inner_size();
                configure_surface(surface, size, &self.wgpu_context)?;
                surface.get_current_texture()?
            }
            Err(wgpu::SurfaceError::Lost) => {
                // Surface is lost, reconfigure and retry
                tracing::warn!("Lost WGPU Surface detected, recreating it");
                let size = self.window.inner_size();
                configure_surface(surface, size, &self.wgpu_context)?;
                surface.get_current_texture()?
            }
            Err(e) => return Err(anyhow::anyhow!("Failed to get surface texture: {}", e)),
        };

        // Check if the texture is suboptimal
        if texture.suboptimal {
            tracing::warn!("Sub-optimal WGPU Surface detected, recreating it");
            drop(texture);
            let size = self.window.inner_size();
            configure_surface(surface, size, &self.wgpu_context)?;
            texture = surface.get_current_texture()?;
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
        present_mode: wgpu::PresentMode::AutoNoVsync,
        desired_maximum_frame_latency: 1,
        alpha_mode: wgpu::CompositeAlphaMode::Auto,
        view_formats: vec![],
    };

    surface.configure(&wgpu_context.device, &surface_config);
    tracing::info!("Configured surface: {}x{}, format: {:?}, present_mode: {:?}",
        size.width, size.height, surface_format, surface_config.present_mode);
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

    /// Run the window event loop with the provided WGPU context and frame receiver
    ///
    /// The receiver contains frames from the compositor with bounded(1) backpressure.
    /// The window will only redraw when frames are available, and consuming a frame
    /// allows the compositor to produce the next one.
    pub fn run(
        self,
        wgpu_context: WgpuContext,
        frame_receiver: Option<RawDataOutputReceiver>,
    ) -> Result<()> {
        let event_loop = self
            .event_loop
            .ok_or_else(|| anyhow::anyhow!("Event loop already consumed"))?;

        event_loop.set_control_flow(ControlFlow::Poll);

        let mut app = WindowApp::new(wgpu_context, frame_receiver);
        event_loop.run_app(&mut app)?;

        Ok(())
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
    frame_receiver: Option<Arc<Mutex<RawDataOutputReceiver>>>,
    blit_pipeline: Option<BlitPipeline>,
    pending_frame: Option<smelter_render::Frame>, // Frame being rendered (holds channel slot)
}

impl WindowApp {
    fn new(wgpu_context: WgpuContext, frame_receiver: Option<RawDataOutputReceiver>) -> Self {
        Self {
            window: None,
            wgpu_context: Arc::new(wgpu_context),
            frame_receiver: frame_receiver.map(|r| Arc::new(Mutex::new(r))),
            blit_pipeline: None,
            pending_frame: None,
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

    /// Render a frame from the compositor to the window surface
    fn render_frame(&mut self, frame: Frame) -> Result<()> {
        let window = self
            .window
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Window not created yet"))?;

        if !window.is_ready() {
            return Ok(()); // Window not ready, skip frame
        }

        // Get the surface texture
        let surface_texture = window.get_surface_texture()?;

        // Get the frame's WGPU texture
        let frame_texture = match &frame.data {
            smelter_render::FrameData::Rgba8UnormWgpuTexture(texture) => texture,
            _ => {
                tracing::error!("Unexpected frame format (not Rgba8UnormWgpuTexture)");
                surface_texture.present();
                return Ok(());
            }
        };

        // Create blit pipeline if not already created
        if self.blit_pipeline.is_none() {
            let surface_format = surface_texture.texture.format();
            self.blit_pipeline = Some(self.create_blit_pipeline(surface_format));
        }

        let blit = self.blit_pipeline.as_ref().unwrap();

        // Create bind group for this frame's texture
        let frame_view = frame_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.wgpu_context.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Frame Blit Bind Group"),
            layout: &blit.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&blit.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&frame_view),
                },
            ],
        });

        // Render the frame using a fullscreen blit
        let surface_view = surface_texture.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.wgpu_context.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("Frame Blit Encoder"),
            },
        );

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Frame Blit Pass"),
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

    /// Render pending frame and fetch next frame from channel
    ///
    /// With bounded(1) channels, this provides backpressure synchronization:
    /// 1. Render the pending_frame (if any) while holding the channel slot
    /// 2. After rendering, drop pending_frame to free the channel slot
    /// 3. Fetch next frame and store in pending_frame (blocks smelter until we render it)
    fn try_render_from_channel(&mut self) -> bool {
        // Step 1: Render the pending frame if we have one
        if let Some(frame) = self.pending_frame.take() {
            if let Err(e) = self.render_frame(frame) {
                tracing::error!("Failed to render frame: {}", e);
            }
            // pending_frame is now None, freeing the channel slot
        }

        // Step 2: Try to fetch the next frame from the channel
        let receiver = match &self.frame_receiver {
            Some(r) => r,
            None => return false,
        };

        let receiver_guard = receiver.lock().unwrap();
        let video_receiver = match &receiver_guard.video {
            Some(r) => r,
            None => return false,
        };

        // Non-blocking receive for the next frame
        match video_receiver.try_recv() {
            Ok(event) => {
                match event {
                    PipelineEvent::Data(frame) => {
                        // Store frame in pending_frame (holds channel slot)
                        self.pending_frame = Some(frame);
                        true // Frame is now pending
                    }
                    PipelineEvent::EOS => {
                        tracing::info!("End of stream received");
                        false
                    }
                }
            }
            Err(_) => {
                // No new frame available
                false
            }
        }
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
                    // Surface is NOT created yet - will be created on first resize
                    let wgpu_window = WgpuWindow::new(window, self.wgpu_context.clone());

                    // Request initial redraw to start frame polling
                    wgpu_window.request_redraw();

                    self.window = Some(wgpu_window);
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
                    tracing::debug!("Window resized to {}x{}", new_size.width, new_size.height);
                    window.resize(new_size);
                }
            }
            WindowEvent::RedrawRequested => {
                // Try to render from channel - only renders if frame is available
                #[allow(unused_variables)]
                let frame_was_rendered = self.try_render_from_channel();

                // Always request next redraw to keep polling for frames
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}
