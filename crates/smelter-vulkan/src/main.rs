use anyhow::Result;
use std::{fs, path::Path, thread};

#[cfg(target_os = "linux")]
mod compositor;
mod window;

const RECORDING_DURATION_SECS: u64 = 60;
const MAX_DECKLINKS_TO_RECORD: usize = 3;

/// Clean up existing .mp4 files in the recordings directory
fn cleanup() -> Result<()> {
    let recordings_dir = Path::new("./recordings");

    if !recordings_dir.exists() {
        return Ok(());
    }

    let entries = fs::read_dir(recordings_dir)?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("mp4") {
            match fs::remove_file(&path) {
                Ok(_) => {
                    tracing::info!("Removed: {}", path.display());
                }
                Err(e) => {
                    tracing::warn!("Failed to remove {}: {}", path.display(), e);
                }
            }
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    // Set up panic hook to ensure we exit cleanly on panic
    std::panic::set_hook(Box::new(|panic_info| {
        eprintln!("FATAL ERROR - Application panicked!");
        eprintln!("{}", panic_info);
        eprintln!("\nThe application will now exit.");
        std::process::exit(1);
    }));

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter("error,smelter_vulkan=debug,smelter_core=warn,compositor_pipeline=warn,compositor_render=warn")
        .init();

    // Note: Smelter/libcef automatically discovers process_helper in the same directory
    // as the main executable. The build script ensures process_helper is built.
    // See: libcef/src/settings.rs:executables_paths() for discovery logic

    // Clean up existing recordings at startup
    cleanup()?;

    // ========================================================================
    // CRITICAL INITIALIZATION ORDER (following one-click-os pattern)
    // ========================================================================

    // Step 1: Create WindowManager FIRST (REQUIRED before WGPU/Compositor)
    // This initializes the winit event loop which must exist before graphics
    tracing::info!("Step 1: Creating WindowManager (MUST be first)");
    let window_manager = window::WindowManager::new()?;

    #[cfg(target_os = "linux")]
    {
        // Step 2: Initialize Compositor and extract WGPU context
        tracing::info!("Step 2: Creating Compositor and initializing WGPU via Smelter");
        let (compositor, wgpu_context) = compositor::Compositor::new()?;

        // Step 3: Register window preview output with bounded(1) channel (if we have inputs)
        let frame_receiver = if let Some(input_id) = compositor.first_decklink_input() {
            tracing::info!("Step 3a: Registering window preview output with bounded(1) channel");
            let output_id = smelter_render::OutputId(std::sync::Arc::from("window_preview"));
            match compositor.register_window_preview_output(output_id, input_id) {
                Ok(receiver) => {
                    tracing::info!("Window preview output registered successfully");
                    Some(receiver)
                }
                Err(e) => {
                    tracing::warn!("Failed to register window preview output: {}", e);
                    None
                }
            }
        } else {
            tracing::warn!("No DeckLink inputs available - window will not display frames");
            None
        };

        // Step 4: Start recording in a separate thread (with 5 second delay for pipeline stabilization)
        thread::spawn(move || -> Result<()> {
            // Wait 5 seconds for pipeline to stabilize before starting recording
            thread::sleep(std::time::Duration::from_secs(5));

            let folder_path = "./recordings/".into();
            std::fs::create_dir_all(&folder_path)?;

            compositor.start_recording(
                folder_path,
                std::time::Duration::from_secs(RECORDING_DURATION_SECS),
                MAX_DECKLINKS_TO_RECORD,
            )?;

            Ok(())
        });

        // Step 5: Run window event loop with shared WGPU context and frame receiver (blocking)
        tracing::info!("Step 5: Starting window manager event loop");
        window_manager.run(wgpu_context, frame_receiver)?;
    }

    #[cfg(not(target_os = "linux"))]
    {
        tracing::warn!("Compositor is only supported on Linux.");
        // Still run the window manager, but without compositor
        let wgpu_context = window::WgpuContext {
            instance: std::sync::Arc::new(wgpu::Instance::new(wgpu::InstanceDescriptor::default())),
            adapter: std::sync::Arc::new(unsafe { std::mem::zeroed() }), // Dummy adapter
            device: std::sync::Arc::new(unsafe { std::mem::zeroed() }),   // Dummy device
            queue: std::sync::Arc::new(unsafe { std::mem::zeroed() }),    // Dummy queue
        };
        window_manager.run(wgpu_context, None)?;
    }

    Ok(())
}
