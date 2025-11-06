use anyhow::Result;
use std::sync::Arc;
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
    let surface_format = surface_caps
        .formats
        .iter()
        .find(|f| f.is_srgb())
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
    tracing::debug!("Configured surface: {}x{}, format: {:?}", size.width, size.height, surface_format);
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

    /// Run the window event loop with the provided WGPU context
    pub fn run(self, wgpu_context: WgpuContext) -> Result<()> {
        let event_loop = self.event_loop
            .ok_or_else(|| anyhow::anyhow!("Event loop already consumed"))?;

        event_loop.set_control_flow(ControlFlow::Poll);

        let mut app = WindowApp::new(wgpu_context);
        event_loop.run_app(&mut app)?;

        Ok(())
    }
}

/// Application handler that manages window lifecycle
struct WindowApp {
    window: Option<WgpuWindow>,
    wgpu_context: Arc<WgpuContext>,
}

impl WindowApp {
    fn new(wgpu_context: WgpuContext) -> Self {
        Self {
            window: None,
            wgpu_context: Arc::new(wgpu_context),
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
                    self.window = Some(WgpuWindow::new(window, self.wgpu_context.clone()));
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
                if let Some(window) = &mut self.window {
                    // Only render if the window is ready (surface created on first resize)
                    if window.is_ready() {
                        // Get the surface texture and render
                        let texture = window.get_surface_texture()
                            .expect("Failed to get surface texture - this is a fatal error");

                        // For now, just present a black frame
                        // TODO: Render compositor output to this texture
                        texture.present();
                    }
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}
