use anyhow::Result;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

/// Manages the application window using winit
pub struct WindowManager {
    window: Option<Window>,
}

impl WindowManager {
    pub fn new() -> Self {
        Self { window: None }
    }

    /// Run the window event loop
    pub fn run() -> Result<()> {
        let event_loop = EventLoop::new()?;
        event_loop.set_control_flow(ControlFlow::Poll);

        let mut app = WindowManager::new();
        event_loop.run_app(&mut app)?;

        Ok(())
    }
}

impl ApplicationHandler for WindowManager {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            let window_attributes = Window::default_attributes()
                .with_title("Smelter Vulkan");

            match event_loop.create_window(window_attributes) {
                Ok(window) => {
                    tracing::info!("Window created successfully");
                    self.window = Some(window);
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
            WindowEvent::RedrawRequested => {
                // Handle redraw if needed
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}
