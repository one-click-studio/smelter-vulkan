use anyhow::Result;
use std::{fs, path::Path, thread};

#[cfg(target_os = "linux")]
mod compositor;
mod window;

const RECORDING_DURATION_SECS: u64 = 60;

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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter("error,smelter_vulkan=info")
        .init();

    // Clean up existing recordings at startup
    cleanup()?;

    // Run compositor in a separate thread
    #[cfg(target_os = "linux")]
    thread::spawn(|| -> Result<()> {
        let compositor = compositor::Compositor::new()?;

        let folder_path = "./recordings/".into();
        std::fs::create_dir_all(&folder_path)?;

        compositor.start_recording(folder_path, std::time::Duration::from_secs(RECORDING_DURATION_SECS))?;

        Ok(())
    });

    #[cfg(not(target_os = "linux"))]
    {
        println!("Compositor is only supported on Linux.");
    }

    // Initialize and run the window (blocking) - at the very end
    window::WindowManager::run()?;

    Ok(())
}
