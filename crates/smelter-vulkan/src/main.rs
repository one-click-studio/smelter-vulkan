use anyhow::Result;
use std::{fs, path::Path, thread};

#[cfg(target_os = "linux")]
mod compositor;
mod external_memory;
mod window;

const RECORDING_DURATION_SECS: u64 = 60;
const MAX_DECKLINKS_TO_RECORD: usize = 3;
const ENABLE_FRAME_STATS: bool = false;

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
    let window_manager = window::WindowManager::new(ENABLE_FRAME_STATS)?;

    // Get the event loop proxy for sending commands to the window
    let window_proxy = window_manager.proxy()
        .ok_or_else(|| anyhow::anyhow!("Failed to get event loop proxy"))?;

    #[cfg(target_os = "linux")]
    {
        use std::sync::{Arc, Mutex};

        // Step 2: Initialize Compositor with bridge texture
        tracing::info!("Step 2: Creating Compositor with bridge texture");
        let (compositor, compositor_context) = compositor::Compositor::new()?;

        // Step 3: Register window preview output with bounded(1) channel (if we have inputs)
        let frame_receiver = if let Some(input_id) = compositor.first_decklink_input() {
            tracing::info!("Step 3: Registering window preview output");
            let output_id = smelter_render::OutputId(Arc::from("window_preview"));
            match compositor.register_window_preview_output(output_id, input_id.clone()) {
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

        // Step 4: Start frame copy loop in separate thread
        // This receives frames from Smelter and copies them to the bridge texture
        if let Some(ref receiver) = frame_receiver {
            let receiver_clone = Arc::new(Mutex::new(receiver.clone()));
            let compositor_ref = Arc::new(Mutex::new(compositor));
            let compositor_clone = compositor_ref.clone();
            let window_proxy_clone = window_proxy.clone();

            thread::spawn(move || {
                tracing::info!("Frame copy loop started");
                loop {
                    // Receive frame from Smelter
                    let frame = {
                        let receiver_guard = receiver_clone.lock().unwrap();
                        match &receiver_guard.video {
                            Some(video_rx) => match video_rx.recv() {
                                Ok(smelter_core::PipelineEvent::Data(frame)) => Some(frame),
                                Ok(smelter_core::PipelineEvent::EOS) => {
                                    tracing::info!("End of stream in copy loop");
                                    break;
                                }
                                Err(e) => {
                                    tracing::error!("Error receiving frame: {}", e);
                                    break;
                                }
                            },
                            None => {
                                tracing::warn!("No video receiver in copy loop");
                                break;
                            }
                        }
                    };

                    if let Some(frame) = frame {
                        // Extract texture from frame
                        if let smelter_render::FrameData::Rgba8UnormWgpuTexture(texture) = &frame.data {
                            // Copy to bridge texture
                            let comp = compositor_clone.lock().unwrap();
                            if let Err(e) = comp.copy_frame_to_bridge(texture) {
                                tracing::error!("Failed to copy frame to bridge: {}", e);
                                continue;
                            }
                            // GPU sync completed in copy_frame_to_bridge via device.poll()

                            // Send redraw command to window via EventLoopProxy
                            if let Err(e) = window_proxy_clone.send_event(window::WindowCommand::RequestRedraw) {
                                tracing::error!("Failed to send redraw command: {}", e);
                                break;
                            }
                        }
                    }
                }
                tracing::info!("Frame copy loop exited");
            });

            // Step 5: Start recording in a separate thread
            thread::spawn(move || -> Result<()> {
                // Wait 5 seconds for pipeline to stabilize
                thread::sleep(std::time::Duration::from_secs(5));

                let folder_path = "./recordings/".into();
                std::fs::create_dir_all(&folder_path)?;

                // Start recording outputs (acquire lock briefly, then release)
                let output_ids = {
                    let comp = compositor_ref.lock().unwrap();
                    comp.start_recording_outputs(folder_path, MAX_DECKLINKS_TO_RECORD)?
                }; // Lock dropped here

                // Sleep for recording duration WITHOUT holding the lock
                tracing::info!("Recording for {} seconds", RECORDING_DURATION_SECS);
                thread::sleep(std::time::Duration::from_secs(RECORDING_DURATION_SECS));

                // Stop recording (acquire lock briefly again)
                {
                    let comp = compositor_ref.lock().unwrap();
                    comp.stop_recording_outputs(output_ids)?;
                }

                Ok(())
            });
        }

        // Step 6: Run window event loop with bridge texture FD (blocking)
        tracing::info!("Step 6: Starting window manager event loop with bridge texture");
        window_manager.run(
            compositor_context.bridge_memory_fd,
            None, // Don't pass frame_receiver to window anymore
        )?;
    }

    #[cfg(not(target_os = "linux"))]
    {
        tracing::warn!("Compositor is only supported on Linux.");
        tracing::error!("This application requires Linux with DeckLink and Vulkan video support");
        anyhow::bail!("Unsupported platform - Linux required");
    }

    Ok(())
}
