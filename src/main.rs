use anyhow::Result;

#[cfg(target_os = "linux")]
mod compositor;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_env_filter("error,smelter_vulkan=info")
        .init();

    #[cfg(target_os = "linux")]
    {
        let compositor = compositor::Compositor::new()?;

        let folder_path = "./recordings/".into();
        std::fs::create_dir_all(&folder_path)?;

        compositor.start_recording(folder_path, std::time::Duration::from_secs(600))?;
    }
    #[cfg(not(target_os = "linux"))]
    {
        println!("Test is only supported on Linux.");
    }

    Ok(())
}
